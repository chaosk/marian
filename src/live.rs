//! Tells open dashboard pages what changed on a server, so they fetch only that.

use std::sync::{Arc, OnceLock};

use marian_types as dto;
use tokio::sync::broadcast;

/// A server's change, already written the way the stream sends it.
#[derive(Clone, Debug)]
pub struct News {
    pub guild_id: i64,
    pub body: Arc<str>,
}

fn bus() -> &'static broadcast::Sender<News> {
    static BUS: OnceLock<broadcast::Sender<News>> = OnceLock::new();
    BUS.get_or_init(|| broadcast::channel(256).0)
}

pub fn changed(guild_id: i64, change: &dto::Change) {
    let body = serde_json::to_string(change).unwrap_or_else(|_| everything().to_string());
    let _ = bus().send(News {
        guild_id,
        body: body.into(),
    });
}

/// Someone joined or left this event.
pub fn signups(guild_id: i64, event: uuid::Uuid) {
    changed(guild_id, &dto::Change::Signups { event: event.to_string() });
}

/// This event changed, came, or went.
pub fn event(guild_id: i64, event: uuid::Uuid) {
    changed(guild_id, &dto::Change::Event { event: event.to_string() });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_reaches_every_listener() {
        let mut first = subscribe();
        let mut second = subscribe();
        server(7);
        assert!(std::iter::from_fn(|| first.try_recv().ok()).any(|news| news.guild_id == 7));
        assert!(std::iter::from_fn(|| second.try_recv().ok()).any(|news| news.guild_id == 7));
    }

    #[test]
    fn a_change_names_what_changed() {
        let mut listener = subscribe();
        let id = uuid::Uuid::now_v7();
        signups(8, id);
        let news = std::iter::from_fn(|| listener.try_recv().ok())
            .find(|news| news.guild_id == 8)
            .unwrap();
        let change: dto::Change = serde_json::from_str(&news.body).unwrap();
        assert_eq!(change, dto::Change::Signups { event: id.to_string() });
        let fallback: dto::Change = serde_json::from_str(everything()).unwrap();
        assert_eq!(fallback, dto::Change::Server);
    }
}
