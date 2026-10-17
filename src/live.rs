//! Tells open event pages that an event changed, so they fetch it again.

use std::sync::OnceLock;

use tokio::sync::broadcast;

fn bus() -> &'static broadcast::Sender<i64> {
    static BUS: OnceLock<broadcast::Sender<i64>> = OnceLock::new();
    BUS.get_or_init(|| broadcast::channel(256).0)
}

pub fn changed(event_id: i64) {
    let _ = bus().send(event_id);
}

pub fn subscribe() -> broadcast::Receiver<i64> {
    bus().subscribe()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_reaches_every_listener() {
        let mut first = subscribe();
        let mut second = subscribe();
        changed(7);
        assert!(std::iter::from_fn(|| first.try_recv().ok()).any(|id| id == 7));
        assert!(std::iter::from_fn(|| second.try_recv().ok()).any(|id| id == 7));
    }
}
