//! Tells open dashboard pages what changed on a server, so they fetch only that.
//! A change reaches pages open on this replica at once, and pages open on the
//! others through Postgres: each replica notifies the `marian_live` channel
//! and listens on it, on a connection of its own.

use std::collections::VecDeque;
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::Duration;

use diesel_async::{RunQueryDsl, SimpleAsyncConnection};
use marian_types as dto;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tokio::time::Instant;
use tokio_stream::StreamExt;

use crate::store::Connection;

/// A server's change, already written the way the stream sends it. No
/// server means every one.
#[derive(Clone, Debug)]
pub struct News {
    pub guild_id: Option<i64>,
    pub body: Arc<str>,
}

impl News {
    pub fn concerns(&self, guild_id: i64) -> bool {
        self.guild_id.is_none_or(|id| id == guild_id)
    }
}

fn bus() -> &'static broadcast::Sender<News> {
    static BUS: OnceLock<broadcast::Sender<News>> = OnceLock::new();
    BUS.get_or_init(|| broadcast::channel(256).0)
}

/// Where this replica's changes wait to be sent to the others, once the
/// bridge runs.
static OUTGOING: OnceLock<mpsc::Sender<String>> = OnceLock::new();

/// This process, so the bridge skips its own changes coming back.
static PROCESS: LazyLock<uuid::Uuid> = LazyLock::new(uuid::Uuid::now_v7);

/// The Postgres channel changes go out on.
const CHANNEL: &str = "marian_live";

/// A change as it travels between replicas: well under Postgres's 8000-byte
/// limit on a notification. The server's id is Marian's own, which never
/// leaves the database and the replicas.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Wire {
    from: uuid::Uuid,
    guild: i64,
    change: dto::Change,
}

pub fn changed(guild_id: i64, change: &dto::Change) {
    let body = serde_json::to_string(change).unwrap_or_else(|_| everything().to_string());
    let _ = bus().send(News {
        guild_id: Some(guild_id),
        body: body.into(),
    });
    if let Some(outgoing) = OUTGOING.get() {
        let wire = Wire {
            from: *PROCESS,
            guild: guild_id,
            change: change.clone(),
        };
        if let Ok(payload) = serde_json::to_string(&wire) {
            // Full only when the database has been away a long while; the
            // other replicas' pages refetch everything when it's back.
            let _ = outgoing.try_send(payload);
        }
    }
}

/// Someone joined or left this event.
pub fn signups(guild_id: i64, event: uuid::Uuid) {
    changed(
        guild_id,
        &dto::Change::Signups {
            event: event.to_string(),
        },
    );
}

/// This event changed, came, or went.
pub fn event(guild_id: i64, event: uuid::Uuid) {
    changed(
        guild_id,
        &dto::Change::Event {
            event: event.to_string(),
        },
    );
}

/// Something on this server that pages can't narrow down.
pub fn server(guild_id: i64) {
    changed(guild_id, &dto::Change::Server);
}

/// What a listener that fell behind is sent: it missed changes it can't name.
pub fn everything() -> &'static str {
    r#"{"kind":"server"}"#
}

pub fn subscribe() -> broadcast::Receiver<News> {
    bus().subscribe()
}

/// Starts this replica's bridge to the others, as a task with a connection
/// of its own: it sends this replica's changes, hands the others' to pages
/// open here, wakes the Discord job worker when any replica saves a job, and
/// wakes the task worker when a trigger marks a change. When the connection
/// drops it connects again, and pages open here refetch everything, since
/// changes made meanwhile never arrive.
pub fn bridge(url: String) {
    let (outgoing, pending) = mpsc::channel(1024);
    if OUTGOING.set(outgoing).is_err() {
        return;
    }
    tokio::spawn(async move {
        relay(&url, CHANNEL, *PROCESS, pending, &|news| {
            let _ = bus().send(news);
        })
        .await
    });
}

/// How often the bridge checks the connection still answers when nothing
/// else happens on it.
const CHECK: Duration = Duration::from_secs(10);

/// Runs the bridge until shutdown, or until `pending`'s sender is gone.
async fn relay(
    url: &str,
    channel: &str,
    me: uuid::Uuid,
    mut pending: mpsc::Receiver<String>,
    deliver: &(dyn Fn(News) + Sync),
) {
    let mut unsent = VecDeque::new();
    let mut first = true;
    let mut failing = false;
    while !crate::shutdown::requested() {
        match listen(url, channel).await {
            Ok(mut conn) => {
                if failing {
                    tracing::info!("live updates reach the other replicas again");
                }
                failing = false;
                // Pages here missed whatever the others did meanwhile, and
                // the workers any jobs they saved and changes they marked.
                if !first {
                    deliver(News {
                        guild_id: None,
                        body: everything().into(),
                    });
                    crate::jobs::wake();
                    crate::tasks::wake();
                }
                match pump(&mut conn, channel, me, &mut pending, deliver, &mut unsent).await {
                    Ok(()) => return,
                    Err(err) => {
                        tracing::warn!("live updates lost the database, connecting again: {err}")
                    }
                }
            }
            Err(err) if !failing => {
                tracing::warn!("live updates can't reach the other replicas, trying again: {err}");
                failing = true;
            }
            Err(_) => {}
        }
        first = false;
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            _ = crate::shutdown::wait() => {}
        }
    }
}

async fn listen(url: &str, channel: &str) -> Result<Connection, String> {
    let mut conn = crate::store::connect(url)
        .await
        .map_err(|err| err.to_string())?;
    conn.batch_execute(&format!(
        "LISTEN {channel}; LISTEN {}; LISTEN {}",
        crate::jobs::WAKE_CHANNEL,
        crate::tasks::WAKE_CHANNEL
    ))
    .await
    .map_err(|err| err.to_string())?;
    Ok(conn)
}

/// Moves changes both ways until the connection fails, shutdown begins (`Ok`)
/// or `pending`'s sender is gone (`Ok`). What it couldn't send stays in
/// `unsent` for the next connection.
async fn pump(
    conn: &mut Connection,
    channel: &str,
    me: uuid::Uuid,
    pending: &mut mpsc::Receiver<String>,
    deliver: &(dyn Fn(News) + Sync),
    unsent: &mut VecDeque<String>,
) -> Result<(), String> {
    let mut checked = Instant::now();
    loop {
        while let Some(payload) = unsent.front() {
            notify(conn, channel, payload)
                .await
                .map_err(|err| err.to_string())?;
            unsent.pop_front();
            checked = Instant::now();
        }
        // Each arm only starts a wait, so whichever loses loses nothing:
        // what arrives meanwhile stays queued for the next turn.
        tokio::select! {
            _ = crate::shutdown::wait() => return Ok(()),
            payload = pending.recv() => match payload {
                Some(payload) => unsent.push_back(payload),
                None => return Ok(()),
            },
            note = async { std::pin::pin!(conn.notifications_stream()).next().await } => match note {
                Some(Ok(note)) => {
                    if note.channel == crate::jobs::WAKE_CHANNEL {
                        crate::jobs::wake();
                    } else if note.channel == crate::tasks::WAKE_CHANNEL {
                        crate::tasks::wake();
                    } else if let Some(news) = arrived(&note.payload, me) {
                        deliver(news);
                    }
                }
                Some(Err(err)) => return Err(err.to_string()),
                None => return Err("the connection closed".into()),
            },
            // A connection that died quietly shows up only when used.
            _ = tokio::time::sleep_until(checked + CHECK) => {
                conn.batch_execute("SELECT 1")
                    .await
                    .map_err(|err| err.to_string())?;
                checked = Instant::now();
            }
        }
    }
}

async fn notify(
    conn: &mut Connection,
    channel: &str,
    payload: &str,
) -> Result<(), diesel::result::Error> {
    diesel::sql_query("SELECT pg_notify($1, $2)")
        .bind::<diesel::sql_types::Text, _>(channel)
        .bind::<diesel::sql_types::Text, _>(payload)
        .execute(conn)
        .await
        .map(drop)
}

/// Another replica's change, for pages open here; `None` for this replica's
/// own, which its pages already have, and for what it can't read.
fn arrived(payload: &str, me: uuid::Uuid) -> Option<News> {
    let wire: Wire = serde_json::from_str(payload).ok()?;
    if wire.from == me {
        return None;
    }
    Some(News {
        guild_id: Some(wire.guild),
        body: serde_json::to_string(&wire.change).ok()?.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_reaches_every_listener() {
        let mut first = subscribe();
        let mut second = subscribe();
        server(7);
        assert!(std::iter::from_fn(|| first.try_recv().ok()).any(|news| news.guild_id == Some(7)));
        assert!(std::iter::from_fn(|| second.try_recv().ok()).any(|news| news.guild_id == Some(7)));
    }

    #[test]
    fn a_change_names_what_changed() {
        let mut listener = subscribe();
        let id = uuid::Uuid::now_v7();
        signups(8, id);
        let news = std::iter::from_fn(|| listener.try_recv().ok())
            .find(|news| news.guild_id == Some(8))
            .unwrap();
        let change: dto::Change = serde_json::from_str(&news.body).unwrap();
        assert_eq!(
            change,
            dto::Change::Signups {
                event: id.to_string()
            }
        );
        let fallback: dto::Change = serde_json::from_str(everything()).unwrap();
        assert_eq!(fallback, dto::Change::Server);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_change_on_one_replica_reaches_pages_on_another() {
        let url = crate::store::database_url().unwrap();
        // A channel of its own, so test runs and a local Marian don't meet.
        let channel = format!("marian_live_t{}", uuid::Uuid::now_v7().simple());
        let replica = |me: uuid::Uuid| {
            let (outgoing, pending) = mpsc::channel::<String>(16);
            let (seen, news) = mpsc::unbounded_channel::<News>();
            let (url, channel) = (url.clone(), channel.clone());
            let task = tokio::spawn(async move {
                relay(&url, &channel, me, pending, &move |news| {
                    let _ = seen.send(news);
                })
                .await
            });
            (outgoing, news, task)
        };
        let (a, b) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
        let (a_out, mut a_news, a_task) = replica(a);
        let (b_out, mut b_news, b_task) = replica(b);
        let event = uuid::Uuid::now_v7();
        let wire = |from| {
            serde_json::to_string(&Wire {
                from,
                guild: 9,
                change: dto::Change::Signups {
                    event: event.to_string(),
                },
            })
            .unwrap()
        };
        // Both listen once a change sent after the start comes through.
        let mut arrived = None;
        for _ in 0..50 {
            a_out.send(wire(a)).await.unwrap();
            if let Ok(Some(news)) =
                tokio::time::timeout(Duration::from_millis(200), b_news.recv()).await
            {
                arrived = Some(news);
                break;
            }
        }
        let news = arrived.expect("B hears A's change");
        assert_eq!(news.guild_id, Some(9));
        let change: dto::Change = serde_json::from_str(&news.body).unwrap();
        assert_eq!(
            change,
            dto::Change::Signups {
                event: event.to_string()
            }
        );
        assert!(news.concerns(9) && !news.concerns(10));
        // A doesn't hear its own change come back.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(a_news.try_recv().is_err(), "A skips its own");

        drop((a_out, b_out));
        a_task.await.unwrap();
        b_task.await.unwrap();
    }

    #[test]
    fn news_for_no_server_reaches_every_page() {
        let news = News {
            guild_id: None,
            body: everything().into(),
        };
        assert!(news.concerns(1) && news.concerns(2));
        assert_eq!(
            arrived("not json", uuid::Uuid::now_v7()).map(|news| news.guild_id),
            None
        );
    }
}
