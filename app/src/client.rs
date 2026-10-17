//! What islands do in the browser after a change.

use marian_types::Done;

#[cfg(feature = "hydrate")]
const FLASH: &str = "marian_flash";

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

/// Goes where a change points, or shows the current page again, with its notice.
pub fn follow(done: &Done) {
    flash(&done.notice);
    match &done.location {
        Some(to) => go(to),
        None => reload(),
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

pub fn go(to: &str) {
    #[cfg(feature = "hydrate")]
    let _ = leptos::prelude::window().location().assign(to);
    #[cfg(not(feature = "hydrate"))]
    let _ = to;
}

pub fn reload() {
    #[cfg(feature = "hydrate")]
    let _ = leptos::prelude::window().location().reload();
}

pub fn confirm(question: &str) -> bool {
    #[cfg(feature = "hydrate")]
    {
        leptos::prelude::window()
            .confirm_with_message(question)
            .unwrap_or(false)
    }
    #[cfg(not(feature = "hydrate"))]
    {
        let _ = question;
        false
    }
}

/// Runs `work` in the browser.
pub fn spawn(work: impl std::future::Future<Output = ()> + 'static) {
    #[cfg(feature = "hydrate")]
    leptos::task::spawn_local(work);
    #[cfg(not(feature = "hydrate"))]
    drop(work);
}
