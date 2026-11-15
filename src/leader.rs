//! Which replica runs the bot. Every replica serves the web; only the one
//! holding a Postgres advisory lock connects to Discord and runs the task worker,
//! Google Calendar sync, the sign-in sweep and the Discord job worker. The
//! lock lives on a connection of its own, outside the pool: when that
//! connection ends, for any reason, Postgres lets the lock go and another
//! replica takes over.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use diesel::connection::{Connection as _, SimpleConnection};
use opentelemetry::metrics::Gauge;
use tokio::sync::watch;

use crate::store::Connection;

/// The advisory lock the leader holds: "marian" and a 1, as one bigint.
pub const LEADER_KEY: i64 = 0x6d61_7269_616e_0001;

/// How often a follower tries for the lock, and how often the leader checks
/// that its lock connection still answers.
const RETRY: Duration = Duration::from_secs(5);
const CHECK: Duration = Duration::from_secs(5);
/// How long the leader waits on its lock connection before it counts as lost.
const CHECK_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Whether this replica leads now, for `/healthz`.
#[derive(Clone, Default)]
pub struct Status(Arc<AtomicBool>);

impl Status {
    pub fn leading(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    fn set(&self, leading: bool) {
        self.0.store(leading, Ordering::Relaxed);
        LEADER_GAUGE.record(u64::from(leading), &[]);
    }
}

static LEADER_GAUGE: LazyLock<Gauge<u64>> = LazyLock::new(|| {
    opentelemetry::global::meter("marian")
        .u64_gauge("marian.leader")
        .with_description("1 while this replica leads: runs the Discord bot, the task worker and Discord jobs; 0 otherwise.")
        .build()
});

/// One spell as leader, for the work that runs only then. It's over when the
/// lock is lost or shutdown begins.
#[derive(Clone)]
pub struct Term(watch::Receiver<bool>);

impl Term {
    /// The lock was lost: stop now, another replica may already lead.
    pub fn lost(&self) -> bool {
        *self.0.borrow()
    }

    /// The lock was lost, or shutdown began.
    pub fn is_over(&self) -> bool {
        self.lost() || crate::shutdown::requested()
    }

    /// Returns once the term is over.
    pub async fn ended(&self) {
        let mut lost = self.0.clone();
        tokio::select! {
            _ = lost.wait_for(|lost| *lost) => {}
            _ = crate::shutdown::wait() => {}
        }
    }
}

/// Tries for leadership until shutdown. While it leads, `duties` runs with the
/// term: it should start the leader's work, wait for `Term::ended`, and stop
/// that work before returning. Only then is the lock let go, so a waiting
/// replica takes over once this one has finished.
pub async fn run<F, Fut>(url: String, key: i64, status: Status, mut duties: F)
where
    F: FnMut(Term) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    // A follower reports its 0 from the start.
    status.set(false);
    let mut said_waiting = false;
    while !crate::shutdown::requested() {
        match acquire(&url, key).await {
            Ok(Some(lock)) => {
                said_waiting = false;
                tracing::info!(
                    "This replica now leads: it runs the Discord bot, the task worker and Discord jobs."
                );
                status.set(true);
                let (lose, term) = watch::channel(false);
                let mut duty = tokio::spawn(duties(Term(term)));
                let kept = hold(lock, &mut duty).await;
                let lost = kept.is_none();
                // At shutdown the duties see it for themselves, and finish
                // their work rather than drop it.
                if lost {
                    tracing::warn!("This replica lost the leader lock; stopping the bot, the task worker and Discord jobs.");
                    let _ = lose.send(true);
                }
                if !duty.is_finished() {
                    let _ = duty.await;
                }
                status.set(false);
                // Closing the lock connection lets the lock go.
                if let Some(lock) = kept {
                    let _ = tokio::task::spawn_blocking(move || drop(lock)).await;
                }
                tracing::info!("This replica no longer leads.");
                // Whatever cut the lock off may still trouble this replica:
                // the others get the first go at it.
                if lost {
                    tokio::select! {
                        _ = tokio::time::sleep(RETRY * 2) => {}
                        _ = crate::shutdown::wait() => {}
                    }
                }
            }
            Ok(None) => {
                if !said_waiting {
                    tracing::info!(
                        "Another replica leads; this one serves the web and takes over if that one stops."
                    );
                    said_waiting = true;
                }
            }
            Err(err) => tracing::warn!("could not try for leadership: {err}"),
        }
        tokio::select! {
            _ = tokio::time::sleep(RETRY) => {}
            _ = crate::shutdown::wait() => {}
        }
    }
}

/// The lock connection, if the lock was free.
async fn acquire(url: &str, key: i64) -> Result<Option<Connection>, String> {
    let url = url.to_string();
    let attempt = tokio::task::spawn_blocking(move || -> Result<Option<Connection>, String> {
        let mut conn = Connection::establish(&url).map_err(|err| err.to_string())?;
        let got: bool = diesel::RunQueryDsl::get_result(
            diesel::select(diesel::dsl::sql::<diesel::sql_types::Bool>(&format!(
                "pg_try_advisory_lock({key})"
            ))),
            &mut conn,
        )
        .map_err(|err| err.to_string())?;
        Ok(got.then_some(conn))
    });
    // A connection that turns up after this gives up is dropped with the
    // task's result, and the lock with it.
    match tokio::time::timeout(CONNECT_TIMEOUT, attempt).await {
        Ok(Ok(result)) => result,
        Ok(Err(err)) => Err(err.to_string()),
        Err(_) => Err("the database didn't answer in time".into()),
    }
}

/// Holds the lock until shutdown begins, giving the connection back, or until
/// the connection stops answering or the duties stop on their own, giving
/// `None`.
async fn hold(mut lock: Connection, duty: &mut tokio::task::JoinHandle<()>) -> Option<Connection> {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(CHECK) => {}
            _ = crate::shutdown::wait() => return Some(lock),
            _ = &mut *duty => {
                if crate::shutdown::requested() {
                    return Some(lock);
                }
                tracing::warn!("the leader's work stopped on its own");
                return None;
            }
        }
        let check = tokio::task::spawn_blocking(move || {
            let alive = lock.batch_execute("SELECT 1").is_ok();
            (lock, alive)
        });
        match tokio::time::timeout(CHECK_TIMEOUT, check).await {
            Ok(Ok((back, true))) => lock = back,
            Ok(Ok((_, false))) | Ok(Err(_)) | Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// A node: runs `run` with duties that count how many of the nodes lead.
    fn node(
        url: String,
        key: i64,
        leaders: Arc<AtomicUsize>,
    ) -> (Status, tokio::task::JoinHandle<()>) {
        let status = Status::default();
        let task = tokio::spawn(run(url, key, status.clone(), move |term: Term| {
            let leaders = leaders.clone();
            async move {
                leaders.fetch_add(1, Ordering::SeqCst);
                term.ended().await;
                leaders.fetch_sub(1, Ordering::SeqCst);
            }
        }));
        (status, task)
    }

    fn lock_holder(admin: &mut Connection, key: i64) -> Option<i32> {
        use diesel::prelude::*;
        #[derive(QueryableByName)]
        struct Holder {
            #[diesel(sql_type = diesel::sql_types::Integer)]
            pid: i32,
        }
        diesel::sql_query(format!(
            "SELECT pid FROM pg_locks WHERE locktype = 'advisory' AND granted AND objsubid = 1 \
             AND ((classid::bigint << 32) | objid::bigint) = {key}"
        ))
        .get_result::<Holder>(admin)
        .optional()
        .unwrap()
        .map(|holder| holder.pid)
    }

    async fn until(what: &str, mut done: impl FnMut() -> bool) {
        for _ in 0..200 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("timed out waiting until {what}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_node_leads_and_the_other_takes_over_when_its_lock_goes() {
        let url = crate::store::database_url().unwrap();
        // A key of its own, so test runs and a local Marian don't meet.
        let key = 0x7465_7374_0000_0000 | i64::from(uuid::Uuid::now_v7().as_u128() as u32);
        let leaders = Arc::new(AtomicUsize::new(0));
        let (first, first_task) = node(url.clone(), key, leaders.clone());
        let (second, second_task) = node(url.clone(), key, leaders.clone());
        until("one leads", || first.leading() || second.leading()).await;
        // Both have tried by now; still only one.
        tokio::time::sleep(RETRY + Duration::from_secs(1)).await;
        assert_eq!(leaders.load(Ordering::SeqCst), 1);
        assert!(first.leading() != second.leading());
        let (leader, follower) = if first.leading() {
            (&first, &second)
        } else {
            (&second, &first)
        };

        // The leader's lock connection is cut, as a database restart or a
        // network fault would.
        let mut admin = Connection::establish(&url).unwrap();
        let pid = lock_holder(&mut admin, key).expect("the leader holds the lock");
        diesel::RunQueryDsl::execute(
            diesel::sql_query(format!("SELECT pg_terminate_backend({pid})")),
            &mut admin,
        )
        .unwrap();
        until("the leader steps down", || !leader.leading()).await;
        until("the other takes over", || follower.leading()).await;
        assert!(!leader.leading());
        assert_eq!(leaders.load(Ordering::SeqCst), 1);
        assert_ne!(lock_holder(&mut admin, key), Some(pid));

        first_task.abort();
        second_task.abort();
    }
}
