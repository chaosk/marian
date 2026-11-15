use std::sync::LazyLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use diesel_async::SimpleAsyncConnection;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::error::OpError;
use crate::schema::discord_jobs;
use crate::store::Connection;

/// Work the web process hands to the Discord gateway. Kept in the
/// `discord_jobs` table until it's done, so a restart doesn't lose it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiscordJob {
    Sync {
        event_id: i64,
        tries: u8,
    },
    Remind {
        event_id: i64,
        text: String,
        message: String,
        tries: u8,
    },
    Attend {
        event_id: i64,
        tries: u8,
    },
    Routine {
        run_id: i64,
        tries: u8,
    },
    /// Register a server's slash commands again, in its language.
    Commands {
        guild_id: i64,
        tries: u8,
    },
}

impl DiscordJob {
    pub fn sync(event_id: i64) -> Self {
        Self::Sync { event_id, tries: 0 }
    }

    pub fn remind(event_id: i64, text: String, message: String) -> Self {
        Self::Remind {
            event_id,
            text,
            message,
            tries: 0,
        }
    }

    pub fn attend(event_id: i64) -> Self {
        Self::Attend { event_id, tries: 0 }
    }

    pub fn routine(run_id: i64) -> Self {
        Self::Routine { run_id, tries: 0 }
    }

    pub fn commands(guild_id: i64) -> Self {
        Self::Commands { guild_id, tries: 0 }
    }

    pub fn event_id(&self) -> i64 {
        match self {
            Self::Sync { event_id, .. }
            | Self::Remind { event_id, .. }
            | Self::Attend { event_id, .. } => *event_id,
            Self::Routine { .. } | Self::Commands { .. } => 0,
        }
    }

    /// How many times the job has failed so far.
    pub fn tries(&self) -> u8 {
        match self {
            Self::Sync { tries, .. }
            | Self::Remind { tries, .. }
            | Self::Attend { tries, .. }
            | Self::Routine { tries, .. }
            | Self::Commands { tries, .. } => *tries,
        }
    }

    pub fn bump(self) -> Option<Self> {
        match self {
            Self::Sync { event_id, tries } if tries < 8 => Some(Self::Sync {
                event_id,
                tries: tries + 1,
            }),
            Self::Remind {
                event_id,
                text,
                message,
                tries,
            } if tries < 8 => Some(Self::Remind {
                event_id,
                text,
                message,
                tries: tries + 1,
            }),
            Self::Attend { event_id, tries } if tries < 8 => Some(Self::Attend {
                event_id,
                tries: tries + 1,
            }),
            Self::Routine { run_id, tries } if tries < 8 => Some(Self::Routine {
                run_id,
                tries: tries + 1,
            }),
            Self::Commands { guild_id, tries } if tries < 8 => Some(Self::Commands {
                guild_id,
                tries: tries + 1,
            }),
            _ => None,
        }
    }
}

/// How long a job waits after its `tries`th failure: 2 seconds, doubling each
/// time, at most 5 minutes. Eight retries span about 8 minutes, enough to
/// ride out a Discord outage or a restart.
pub fn backoff(tries: u8) -> Duration {
    let seconds = 2u64.saturating_mul(1 << tries.saturating_sub(1).min(16));
    Duration::from_secs(seconds.min(300))
}

static WAKE: LazyLock<Notify> = LazyLock::new(Notify::new);

/// The Postgres channel `save` notifies, which `crate::live`'s listener on
/// every replica turns into `wake`.
pub const WAKE_CHANNEL: &str = "marian_jobs";

/// Tells the job worker new jobs are in the table.
pub fn wake() {
    WAKE.notify_one();
}

/// Waits for `wake`, or returns at once if it was called since the last wait.
pub async fn woken() {
    WAKE.notified().await;
}

/// Saves jobs to be done. Call it in the transaction of the change that needs
/// them, so they are kept exactly when the change is.
pub async fn save(conn: &mut Connection, jobs: &[DiscordJob]) -> Result<(), OpError> {
    if jobs.is_empty() {
        return Ok(());
    }
    // Due by this process's clock, which the worker reads them by.
    let now = Utc::now();
    let rows: Vec<_> = jobs
        .iter()
        .map(|job| {
            (
                discord_jobs::job.eq(serde_json::to_value(job).expect("a job serializes")),
                discord_jobs::run_at.eq(now),
            )
        })
        .collect();
    diesel::insert_into(discord_jobs::table)
        .values(&rows)
        .execute(conn)
        .await?;
    // Wakes the worker, on whichever replica leads, once this commits.
    conn.batch_execute(&format!("NOTIFY {WAKE_CHANNEL}"))
        .await?;
    Ok(())
}

/// How long a worker has a job it took to itself. No other worker takes it
/// until then, even if this one has gone. A run is cut off well before:
/// `RUN_LIMIT`.
pub const LEASE: Duration = Duration::from_secs(300);

/// The longest a job may run before it counts as failed, so it never outlives
/// its lease.
pub const RUN_LIMIT: Duration = Duration::from_secs(120);

/// Takes the job due soonest at `now`, if any, for `lease`: no other worker
/// takes it until the lease is up, by the database's clock. Two workers
/// taking at once get different jobs. A row this build can't read (from a
/// newer one) is deleted rather than left blocking the queue.
pub async fn claim(
    conn: &mut Connection,
    now: DateTime<Utc>,
    lease: Duration,
) -> Result<Option<(i64, DiscordJob)>, OpError> {
    #[derive(QueryableByName)]
    struct Claimed {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        id: i64,
        #[diesel(sql_type = diesel::sql_types::Jsonb)]
        job: serde_json::Value,
    }
    loop {
        let row: Option<Claimed> = diesel::sql_query(
            "UPDATE discord_jobs SET locked_until = now() + make_interval(secs => $2) \
             WHERE id = (SELECT id FROM discord_jobs \
                         WHERE run_at <= $1 AND (locked_until IS NULL OR locked_until < now()) \
                         ORDER BY run_at, id LIMIT 1 FOR UPDATE SKIP LOCKED) \
             RETURNING id, job",
        )
        .bind::<diesel::sql_types::Timestamptz, _>(now)
        .bind::<diesel::sql_types::Double, _>(lease.as_secs_f64())
        .get_result(conn)
        .await
        .optional()?;
        let Some(Claimed { id, job: value }) = row else {
            return Ok(None);
        };
        match serde_json::from_value(value) {
            Ok(job) => return Ok(Some((id, job))),
            Err(err) => {
                tracing::error!("dropping Discord job {id} this build can't read: {err}");
                done(conn, id).await?;
            }
        }
    }
}

/// When the next job not yet due, or not yet free, is, if there is one.
pub async fn next_run_at(conn: &mut Connection) -> Result<Option<DateTime<Utc>>, OpError> {
    Ok(discord_jobs::table
        // A job another worker has is next due when its lease is up.
        .select(diesel::dsl::sql::<
            diesel::sql_types::Nullable<diesel::sql_types::Timestamptz>,
        >("min(GREATEST(run_at, locked_until))"))
        .first(conn)
        .await?)
}

/// Jobs due at `now` that no worker has, and jobs waiting out a pause after failing.
pub async fn counts(conn: &mut Connection, now: DateTime<Utc>) -> Result<(i64, i64), OpError> {
    let due = discord_jobs::table
        .filter(discord_jobs::run_at.le(now))
        .filter(
            discord_jobs::locked_until
                .is_null()
                .or(discord_jobs::locked_until.lt(now)),
        )
        .count()
        .get_result(conn)
        .await?;
    let later = discord_jobs::table
        .filter(discord_jobs::run_at.gt(now))
        .count()
        .get_result(conn)
        .await?;
    Ok((due, later))
}

/// The job succeeded, or is out of tries.
pub async fn done(conn: &mut Connection, id: i64) -> Result<(), OpError> {
    diesel::delete(discord_jobs::table.filter(discord_jobs::id.eq(id)))
        .execute(conn)
        .await?;
    Ok(())
}

/// The job failed: it's tried again as `job` (its tries counted) at `run_at`,
/// by whichever worker takes it then.
pub async fn retry(
    conn: &mut Connection,
    id: i64,
    job: &DiscordJob,
    run_at: DateTime<Utc>,
) -> Result<(), OpError> {
    diesel::update(discord_jobs::table.filter(discord_jobs::id.eq(id)))
        .set((
            discord_jobs::job.eq(serde_json::to_value(job).expect("a job serializes")),
            discord_jobs::run_at.eq(run_at),
            discord_jobs::locked_until.eq(None::<DateTime<Utc>>),
        ))
        .execute(conn)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_is_tried_nine_times_then_dropped() {
        let mut job = Some(DiscordJob::sync(7));
        let mut runs = 0;
        while let Some(current) = job {
            assert_eq!(current.tries(), runs);
            assert_eq!(current.event_id(), 7);
            runs += 1;
            job = current.bump();
        }
        assert_eq!(runs, 9, "the first run and eight more");
    }

    #[test]
    fn retries_back_off_exponentially_up_to_five_minutes() {
        let waits: Vec<u64> = (1..=9).map(|tries| backoff(tries).as_secs()).collect();
        assert_eq!(waits, vec![2, 4, 8, 16, 32, 64, 128, 256, 300]);
        assert_eq!(backoff(u8::MAX).as_secs(), 300);
    }

    #[test]
    fn a_job_survives_the_table() {
        for job in [
            DiscordJob::sync(1),
            DiscordJob::remind(2, "<@u2>".into(), "Boots on.".into()),
            DiscordJob::attend(3),
            DiscordJob::routine(4),
            DiscordJob::commands(5).bump().unwrap(),
        ] {
            let value = serde_json::to_value(&job).unwrap();
            assert_eq!(serde_json::from_value::<DiscordJob>(value).unwrap(), job);
        }
    }

    #[tokio::test]
    async fn saved_jobs_come_back_in_order_and_wait_out_their_retries() {
        use chrono::SubsecRound;
        let mut conn = crate::store::open_memory().await.unwrap();
        assert_eq!(claim(&mut conn, Utc::now(), LEASE).await.unwrap(), None);
        save(&mut conn, &[DiscordJob::sync(1), DiscordJob::attend(1)])
            .await
            .unwrap();
        let now = Utc::now();
        let (first, job) = claim(&mut conn, now, LEASE).await.unwrap().unwrap();
        assert_eq!(job, DiscordJob::sync(1));
        let later = now + chrono::Duration::seconds(30);
        retry(&mut conn, first, &job.clone().bump().unwrap(), later)
            .await
            .unwrap();
        let (second, job) = claim(&mut conn, now, LEASE).await.unwrap().unwrap();
        assert_eq!(job, DiscordJob::attend(1));
        done(&mut conn, second).await.unwrap();
        assert_eq!(claim(&mut conn, now, LEASE).await.unwrap(), None);
        assert_eq!(counts(&mut conn, now).await.unwrap(), (0, 1));
        assert_eq!(
            next_run_at(&mut conn).await.unwrap(),
            Some(later).map(|at| at.trunc_subsecs(6))
        );
        let (id, job) = claim(&mut conn, later, LEASE).await.unwrap().unwrap();
        assert_eq!((id, job.tries()), (first, 1));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_workers_taking_at_once_never_get_the_same_job() {
        let mut conn = crate::store::open_memory().await.unwrap();
        let jobs: Vec<_> = (1..=40).map(DiscordJob::sync).collect();
        save(&mut conn, &jobs).await.unwrap();
        let db = crate::store::pool_for(&mut conn).await.unwrap();
        let now = Utc::now();
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let db = db.clone();
                tokio::spawn(async move {
                    let mut conn = db.get().await.unwrap();
                    let mut mine = Vec::new();
                    // Nothing is done or retried, so each job can be taken once.
                    while let Some((id, _)) = claim(&mut conn, now, LEASE).await.unwrap() {
                        mine.push(id);
                    }
                    mine
                })
            })
            .collect();
        let mut taken: Vec<Vec<i64>> = Vec::new();
        for worker in workers {
            taken.push(worker.await.unwrap());
        }
        let mut all: Vec<i64> = taken.concat();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 40, "every job taken once: {taken:?}");
        assert_eq!(taken[0].len() + taken[1].len(), 40);
    }

    #[tokio::test]
    async fn a_job_whose_lease_ran_out_is_taken_again() {
        let mut conn = crate::store::open_memory().await.unwrap();
        save(&mut conn, &[DiscordJob::sync(1)]).await.unwrap();
        let now = Utc::now();
        let (id, _) = claim(&mut conn, now, LEASE).await.unwrap().unwrap();
        // Leased: nobody else gets it, and it doesn't count as waiting.
        assert_eq!(claim(&mut conn, now, LEASE).await.unwrap(), None);
        assert_eq!(counts(&mut conn, now).await.unwrap(), (0, 0));
        // The worker that took it went away and its lease ran out.
        diesel::update(discord_jobs::table.filter(discord_jobs::id.eq(id)))
            .set(discord_jobs::locked_until.eq(Some(now - chrono::Duration::seconds(1))))
            .execute(&mut conn)
            .await
            .unwrap();
        assert_eq!(
            claim(&mut conn, Utc::now(), LEASE).await.unwrap(),
            Some((id, DiscordJob::sync(1)))
        );
    }
}
