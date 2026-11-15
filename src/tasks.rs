//! Waking the leader's task worker and Google Calendar sync. Triggers notify
//! the `marian_tasks` channel when they mark a change; `crate::live`'s
//! listener, on the connection it already keeps, turns that into `wake`.

use std::sync::LazyLock;

use tokio::sync::watch;

/// The Postgres channel the task triggers notify.
pub const WAKE_CHANNEL: &str = "marian_tasks";

static TASKS: LazyLock<watch::Sender<u64>> = LazyLock::new(|| watch::channel(0).0);
static GOOGLE: LazyLock<watch::Sender<u64>> = LazyLock::new(|| watch::channel(0).0);

/// Something changed that may move a task: both loops look again.
pub fn wake() {
    TASKS.send_modify(|count| *count = count.wrapping_add(1));
    wake_google();
}

/// The task worker set a Google sync: only the Google loop looks again.
pub fn wake_google() {
    GOOGLE.send_modify(|count| *count = count.wrapping_add(1));
}

/// For the task worker: `changed()` returns once `wake` was called since
/// the last time it returned.
pub fn subscribe() -> watch::Receiver<u64> {
    TASKS.subscribe()
}

/// For the Google loop, like `subscribe`.
pub fn subscribe_google() -> watch::Receiver<u64> {
    GOOGLE.subscribe()
}
