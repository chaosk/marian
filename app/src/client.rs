//! What the browser does after a change.

use leptos::prelude::*;
use marian_types::Done;

#[cfg(feature = "hydrate")]
const FLASH: &str = "marian_flash";

const TOAST_MS: i32 = 4200;
const TOAST_OUT_MS: i32 = 180;

#[derive(Clone)]
pub(crate) struct Toast {
    pub id: u64,
    pub text: String,
    /// Reference counted rather than owned: a toast outlives the component that raised it.
    pub leaving: ArcRwSignal<bool>,
}

#[derive(Clone, Copy)]
struct Toasts(RwSignal<Vec<Toast>>);

thread_local! {
    static TOASTS: std::cell::Cell<Option<RwSignal<Vec<Toast>>>> = const { std::cell::Cell::new(None) };
    static WAITING: std::cell::Cell<Option<RwSignal<bool>>> = const { std::cell::Cell::new(None) };
    static OFFLINE_SAID: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Puts the toast list on the app so a notice survives a page change.
pub fn provide_toasts() {
    let list = RwSignal::new(Vec::new());
    let waiting = RwSignal::new(false);
    TOASTS.with(|slot| slot.set(Some(list)));
    WAITING.with(|slot| slot.set(Some(waiting)));
    OFFLINE_SAID.with(|slot| slot.set(false));
    provide_context(Toasts(list));
}

/// A read is backing off after Marian couldn't be reached.
#[cfg(not(feature = "ssr"))]
pub(crate) fn set_waiting(on: bool) {
    WAITING.with(|slot| {
        if let Some(waiting) = slot.get() {
            waiting.set(on);
        }
    });
}

pub(crate) fn waiting() -> bool {
    WAITING.with(|slot| slot.get().is_some_and(|waiting| waiting.get()))
}

/// A read got through; later outages can toast again.
#[cfg(not(feature = "ssr"))]
pub(crate) fn note_reached() {
    OFFLINE_SAID.with(|slot| slot.set(false));
    set_waiting(false);
}

/// One toast for this outage. Cells keep a still scaffold instead.
#[cfg(not(feature = "ssr"))]
pub(crate) fn say_offline(message: &str) {
    set_waiting(false);
    let first = OFFLINE_SAID.with(|slot| {
        let said = slot.get();
        slot.set(true);
        !said
    });
    if first {
        toast(message);
    }
}

pub(crate) fn toast_list() -> Option<RwSignal<Vec<Toast>>> {
    use_context::<Toasts>()
        .map(|toasts| toasts.0)
        .or_else(|| TOASTS.with(|slot| slot.get()))
}

/// Shows a notice in the corner. Empty text is ignored.
pub fn toast(notice: impl Into<String>) {
    match toast_list() {
        Some(list) => push_toast(list, notice),
        None => flash(&notice.into()),
    }
}

/// Same as `toast`, for a list captured before an async jump.
pub fn push_toast(list: RwSignal<Vec<Toast>>, notice: impl Into<String>) {
    let text = notice.into();
    if text.is_empty() {
        return;
    }
    let id = next_toast();
    let leaving = ArcRwSignal::new(false);
    list.update(|rows| {
        rows.push(Toast { id, text, leaving: leaving.clone() });
    });
    let live: Vec<(u64, ArcRwSignal<bool>)> = list.with_untracked(|rows| {
        rows.iter()
            .filter(|row| !row.leaving.get_untracked())
            .map(|row| (row.id, row.leaving.clone()))
            .collect()
    });
    if live.len() > 3 {
        if let Some((old, flag)) = live.into_iter().next() {
            dismiss_toast(list, old, flag);
        }
    }
    later(TOAST_MS, move || dismiss_toast(list, id, leaving));
}

fn dismiss_toast(list: RwSignal<Vec<Toast>>, id: u64, leaving: ArcRwSignal<bool>) {
    if leaving.get_untracked() {
        return;
    }
    leaving.set(true);
    later(TOAST_OUT_MS, move || {
        list.update(|rows| rows.retain(|row| row.id != id));
    });
}

fn next_toast() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn later(ms: i32, work: impl FnOnce() + 'static) {
    #[cfg(feature = "hydrate")]
    {
        use wasm_bindgen::prelude::Closure;
        use wasm_bindgen::JsCast;
        let cb = Closure::once_into_js(work);
        let _ = leptos::prelude::window().set_timeout_with_callback_and_timeout_and_arguments_0(
            cb.unchecked_ref(),
            ms,
        );
    }
    #[cfg(not(feature = "hydrate"))]
    {
        let _ = (ms, work);
    }
}

const LAST_SERVER: &str = "marian_last_server";

/// Remembers the server this browser opened last. It stays in the browser:
/// the server keeps no note of where anyone was.
pub fn remember_server(id: &str) {
    #[cfg(feature = "hydrate")]
    if let Ok(Some(storage)) = leptos::prelude::window().local_storage() {
        let _ = storage.set_item(LAST_SERVER, id);
    }
    #[cfg(not(feature = "hydrate"))]
    let _ = id;
}

/// The server this browser opened last, if it remembers one.
pub fn last_server() -> Option<String> {
    #[cfg(feature = "hydrate")]
    if let Ok(Some(storage)) = leptos::prelude::window().local_storage() {
        return storage.get_item(LAST_SERVER).ok().flatten().filter(|id| !id.is_empty());
    }
    None
}

/// Keeps a notice for the next page to show.
pub fn flash(notice: &str) {
    #[cfg(feature = "hydrate")]
    if let Ok(Some(storage)) = leptos::prelude::window().session_storage() {
        let _ = storage.set_item(FLASH, notice);
    }
    #[cfg(not(feature = "hydrate"))]
    let _ = notice;
}

/// The notice the last page left, once.
pub fn take_flash() -> String {
    #[cfg(feature = "hydrate")]
    if let Ok(Some(storage)) = leptos::prelude::window().session_storage() {
        let text = storage.get_item(FLASH).ok().flatten().unwrap_or_default();
        let _ = storage.remove_item(FLASH);
        return text;
    }
    String::new()
}

/// Goes to an address on this site without reloading the document.
pub fn use_go() -> impl Fn(&str) + Clone + Send + Sync + 'static {
    let navigate = leptos_router::hooks::use_navigate();
    move |to: &str| {
        navigate(
            to,
            leptos_router::NavigateOptions {
                resolve: false,
                scroll: true,
                ..Default::default()
            },
        );
    }
}

/// Goes where a change points, or refreshes this page.
pub fn use_follow() -> impl Fn(Done) + Clone + Send + Sync + 'static {
    let go = use_go();
    let params = leptos_router::hooks::use_params_map();
    move |done: Done| {
        match done.location {
            Some(to) => go(&to),
            None => {
                let server = params.with_untracked(|params| params.get("server").unwrap_or_default());
                crate::cache::refresh(&server);
            }
        }
    }
}

pub fn path() -> String {
    #[cfg(feature = "hydrate")]
    {
        leptos::prelude::window()
            .location()
            .pathname()
            .unwrap_or_else(|_| "/".into())
    }
    #[cfg(not(feature = "hydrate"))]
    {
        "/".into()
    }
}

/// An action for a change the browser sends. The browser runs it on its one
/// thread, where requests aren't `Send`. The server never sends changes, but it
/// renders pages on any thread, so it gets the thread-safe kind of action.
#[cfg(feature = "hydrate")]
pub fn action<I, O, F, Fu>(run: F) -> Action<I, O>
where
    I: Send + Sync + 'static,
    O: Send + Sync + 'static,
    F: Fn(&I) -> Fu + 'static,
    Fu: std::future::Future<Output = O> + 'static,
{
    Action::new_unsync(run)
}

#[cfg(not(feature = "hydrate"))]
pub fn action<I, O, F, Fu>(run: F) -> Action<I, O>
where
    I: Send + Sync + 'static,
    O: Send + Sync + 'static,
    F: Fn(&I) -> Fu + Send + Sync + 'static,
    Fu: std::future::Future<Output = O> + Send + 'static,
{
    Action::new(run)
}

/// Runs `work` in the browser.
pub fn spawn(work: impl std::future::Future<Output = ()> + 'static) {
    #[cfg(feature = "hydrate")]
    leptos::task::spawn_local(work);
    #[cfg(not(feature = "hydrate"))]
    drop(work);
}
