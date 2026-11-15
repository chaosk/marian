//! Tells open dashboard pages what changed on a server, so they fetch only that.
//! A change reaches pages open on this replica at once, and pages open on the
//! others through Postgres: each replica notifies the `marian_live` channel
//! and listens on it, on a connection of its own.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::{Duration, Instant};

use diesel::connection::{Connection as _, SimpleConnection};
use marian_types as dto;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

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
static OUTGOING: OnceLock<SyncSender<String>> = OnceLock::new();

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

/// Starts this replica's bridge to the others, on a thread of its own with a
/// connection of its own: it sends this replica's changes, hands the others'
/// to pages open here, and wakes the Discord job worker when any replica saves
/// a job. When the connection drops it connects again, and pages open here
/// refetch everything, since changes made meanwhile never arrive.
pub fn bridge(url: String) {
    let (outgoing, pending) = mpsc::sync_channel(1024);
    if OUTGOING.set(outgoing).is_err() {
        return;
    }
    let started = std::thread::Builder::new()
        .name("live-bridge".into())
        .spawn(move || {
            relay(&url, CHANNEL, *PROCESS, &pending, &|news| {
                let _ = bus().send(news);
            })
        });
    if let Err(err) = started {
        tracing::warn!("live updates stay on this replica: {err}");
    }
}

/// How often the bridge sends what's waiting and reads what arrived, and how
/// often it checks the connection still answers when nothing else happens.
const POLL: Duration = Duration::from_millis(200);
const CHECK: Duration = Duration::from_secs(10);

/// Runs the bridge until shutdown, or until `pending`'s sender is gone.
fn relay(
    url: &str,
    channel: &str,
    me: uuid::Uuid,
    pending: &Receiver<String>,
    deliver: &dyn Fn(News),
) {
    let mut unsent = VecDeque::new();
    let mut first = true;
    let mut failing = false;
    while !crate::shutdown::requested() {
        match listen(url, channel) {
            Ok(mut conn) => {
                if failing {
                    tracing::info!("live updates reach the other replicas again");
                }
                failing = false;
                // Pages here missed whatever the others did meanwhile, and
                // the worker any jobs they saved.
                if !first {
                    deliver(News {
                        guild_id: None,
                        body: everything().into(),
                    });
                    crate::jobs::wake();
                }
                match pump(&mut conn, channel, me, pending, deliver, &mut unsent) {
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
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn listen(url: &str, channel: &str) -> Result<Connection, diesel::result::Error> {
    let mut conn = Connection::establish(url)
        .map_err(|err| diesel::result::Error::QueryBuilderError(err.to_string().into()))?;
    conn.batch_execute(&format!(
        "LISTEN {channel}; LISTEN {}",
        crate::jobs::WAKE_CHANNEL
    ))?;
    Ok(conn)
}

/// Moves changes both ways until the connection fails, shutdown begins (`Ok`)
/// or `pending`'s sender is gone (`Ok`). What it couldn't send stays in
/// `unsent` for the next connection.
fn pump(
    conn: &mut Connection,
    channel: &str,
    me: uuid::Uuid,
    pending: &Receiver<String>,
    deliver: &dyn Fn(News),
    unsent: &mut VecDeque<String>,
) -> Result<(), diesel::result::Error> {
    let mut checked = Instant::now();
    loop {
        if crate::shutdown::requested() {
            return Ok(());
        }
        match pending.recv_timeout(POLL) {
            Ok(payload) => unsent.push_back(payload),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
        unsent.extend(pending.try_iter());
        while let Some(payload) = unsent.front() {
            notify(conn, channel, payload)?;
            unsent.pop_front();
            checked = Instant::now();
        }
        for note in conn.notifications_iter() {
            let note = note?;
            if note.channel == crate::jobs::WAKE_CHANNEL {
                crate::jobs::wake();
            } else if let Some(news) = arrived(&note.payload, me) {
                deliver(news);
            }
        }
        // A connection that died quietly shows up only when used.
        if checked.elapsed() >= CHECK {
            conn.batch_execute("SELECT 1")?;
            checked = Instant::now();
        }
    }
}

fn notify(
    conn: &mut Connection,
    channel: &str,
    payload: &str,
) -> Result<(), diesel::result::Error> {
    diesel::RunQueryDsl::execute(
        diesel::sql_query("SELECT pg_notify($1, $2)")
            .bind::<diesel::sql_types::Text, _>(channel)
            .bind::<diesel::sql_types::Text, _>(payload),
        conn,
    )
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

    #[test]
    fn a_change_on_one_replica_reaches_pages_on_another() {
        let url = crate::store::database_url().unwrap();
        // A channel of its own, so test runs and a local Marian don't meet.
        let channel = format!("marian_live_t{}", uuid::Uuid::now_v7().simple());
        let replica = |me: uuid::Uuid| {
            let (outgoing, pending) = mpsc::sync_channel::<String>(16);
            let (seen, news) = mpsc::channel::<News>();
            let (url, channel) = (url.clone(), channel.clone());
            let thread = std::thread::spawn(move || {
                relay(&url, &channel, me, &pending, &move |news| {
                    let _ = seen.send(news);
                })
            });
            (outgoing, news, thread)
        };
        let (a, b) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
        let (a_out, a_news, a_thread) = replica(a);
        let (b_out, b_news, b_thread) = replica(b);
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
            a_out.send(wire(a)).unwrap();
            if let Ok(news) = b_news.recv_timeout(Duration::from_millis(200)) {
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
        std::thread::sleep(Duration::from_millis(500));
        assert!(a_news.try_recv().is_err(), "A skips its own");

        drop((a_out, b_out));
        a_thread.join().unwrap();
        b_thread.join().unwrap();
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
