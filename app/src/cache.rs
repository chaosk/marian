//! Cached REST reads. One request per key, shared across the shell and every page.

use std::hash::Hash;

use leptos::prelude::*;
use leptos_fetch::{QueryClient, QueryScope};
use marian_types as dto;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::api::{self, Failure};

/// Puts the cache on the app so every page sees the same server, event, and list.
pub fn provide() {
    QueryClient::new().provide();
}

pub fn client() -> QueryClient {
    expect_context()
}

/// Starts a cached read. Call this at the top of the page, not inside
/// another `ask`/`when` — a nested start is missing on the first hydrate
/// frame and the router dies.
pub fn listen<K, T>(
    scope: QueryScope<K, Result<T, Failure>>,
    key: impl Fn() -> K + Send + Sync + Clone + 'static,
) -> Resource<Result<T, Failure>>
where
    K: std::fmt::Debug + PartialEq + Hash + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    Result<T, Failure>: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
{
    client().resource(scope, key)
}

/// Marks every cached read stale. In-page changes, and live updates that
/// can't say what changed, call this instead of refetching each page's
/// private bundle. Only reads on screen refetch; the rest wait to be shown.
pub fn refresh(server: &str) {
    if server.is_empty() {
        return;
    }
    if let Some(client) = use_context::<QueryClient>() {
        refresh_in(client, server);
    }
}

/// `refresh` with a client captured earlier, for work that outlives its view.
pub fn refresh_in(client: QueryClient, server: &str) {
    if !server.is_empty() {
        client.invalidate_all_queries();
    }
}

/// Marks stale only what a live update says changed on this server.
#[cfg_attr(not(feature = "hydrate"), allow(dead_code))]
pub fn apply_change(client: QueryClient, server: &str, change: &dto::Change) {
    let here = server.to_string();
    let lists = |client: QueryClient| {
        client.invalidate_queries_with_predicate(events_scope(), |(s, _)| *s == here);
        client.invalidate_queries_with_predicate(activity_scope(), |(s, _, _)| *s == here);
        client.invalidate_query(routines_scope(), &here);
        client.invalidate_queries_with_predicate(routine_scope(), |(s, _)| *s == here);
    };
    match change {
        dto::Change::Signups { event } => {
            client.invalidate_query(signups_scope(), &(here.clone(), event.clone()));
            lists(client);
        }
        dto::Change::Event { event } => {
            let key = (here.clone(), event.clone());
            client.invalidate_query(event_scope(), &key);
            client.invalidate_query(signups_scope(), &key);
            client.invalidate_query(reminders_scope(), &key);
            lists(client);
            client.invalidate_query(server_scope(), &here);
            client.invalidate_query(pending_reminders_scope(), &here);
            client.invalidate_query(series_list_scope(), &here);
            client.invalidate_queries_with_predicate(series_scope(), |(s, _)| *s == here);
        }
        dto::Change::Server => refresh_in(client, server),
    }
}

/// Writes settings into the cache without remounting the form.
pub fn apply_settings(client: QueryClient, server: &str, next: &dto::Settings) {
    client.update_query(settings_scope(), &server.to_string(), |cached| {
        if let Some(Ok(settings)) = cached {
            *settings = next.clone();
        }
        client.untrack_update_query();
    });
    client.update_query(server_scope(), &server.to_string(), |cached| {
        if let Some(Ok(info)) = cached {
            info.hour_24 = next.hour_24;
            info.timezone = next.timezone.clone();
        }
        client.untrack_update_query();
    });
}

/// Writes an event's signups into the cache, so a refresh renders them while
/// the refetch is still on its way instead of the answer from before a change.
pub fn apply_signups(client: QueryClient, key: &(String, String), next: &dto::Signups) {
    client.update_query(signups_scope(), key, |cached| {
        if let Some(Ok(signups)) = cached {
            *signups = next.clone();
        }
        client.untrack_update_query();
    });
}

/// Writes an event as the server now reads it, for the same reason as `apply_signups`.
pub fn apply_event(client: QueryClient, key: &(String, String), next: &dto::Event) {
    client.update_query(event_scope(), key, |cached| {
        if let Some(Ok(event)) = cached {
            *event = next.clone();
        }
        client.untrack_update_query();
    });
}

/// Changes the cached event in place before a save lands, so a refresh that
/// arrives first renders the new value, not the old one.
pub fn preview_event(client: QueryClient, key: &(String, String), change: impl FnOnce(&mut dto::Event)) {
    client.update_query(event_scope(), key, |cached| {
        if let Some(Ok(event)) = cached {
            change(event);
        }
        client.untrack_update_query();
    });
}

/// The same for an event's reminders, keeping "sent" on the ones that stay.
pub fn preview_reminders(client: QueryClient, key: &(String, String), next: &[dto::Reminder]) {
    client.update_query(reminders_scope(), key, |cached| {
        if let Some(Ok(list)) = cached {
            let sent = |rem: &dto::Reminder| {
                list.iter()
                    .any(|old| old.sent && old.offset_minutes == rem.offset_minutes && old.message == rem.message)
            };
            *list = next
                .iter()
                .map(|rem| dto::Reminder {
                    sent: sent(rem),
                    ..rem.clone()
                })
                .collect();
        }
        client.untrack_update_query();
    });
}

/// Writes a personal preference into the cached membership without remounting.
pub fn apply_membership(client: QueryClient, server: &str, change: &dto::MembershipPatch) {
    client.update_query(membership_scope(), &server.to_string(), |cached| {
        if let Some(Ok(membership)) = cached {
            if let Some(on) = change.reminders_enabled {
                membership.reminders_enabled = on;
            }
            if let Some(on) = change.google_auto_sync {
                membership.google.auto_sync = on;
            }
        }
        client.untrack_update_query();
    });
}

/// The server in the address, tracked so a switcher change refetches.
#[track_caller]
pub fn server_key() -> String {
    leptos_router::hooks::use_params_map().with(|params| params.get("server").unwrap_or_default())
}

#[track_caller]
pub fn event_key() -> (String, String) {
    leptos_router::hooks::use_params_map().with(|params| {
        (
            params.get("server").unwrap_or_default(),
            params.get("id").unwrap_or_default(),
        )
    })
}

/// The server in the address, read once. For handlers, actions, and the body
/// of a component; a query's key uses `server_key` so it follows the address.
#[track_caller]
pub fn server_now() -> String {
    leptos_router::hooks::use_params_map().with_untracked(|params| params.get("server").unwrap_or_default())
}

/// The event in the address, read once, as `server_now` reads the server.
#[track_caller]
pub fn event_now() -> String {
    leptos_router::hooks::use_params_map().with_untracked(|params| params.get("id").unwrap_or_default())
}

/// Starts a cached read and renders `render` when it is ready. Use this
/// only at the top of a page; sibling cells belong in `listen` + `show`.
pub fn ask<K, T, V>(
    scope: QueryScope<K, Result<T, Failure>>,
    key: impl Fn() -> K + Send + Sync + Clone + 'static,
    render: impl Fn(T) -> V + Send + Sync + 'static,
) -> impl IntoView
where
    K: std::fmt::Debug + PartialEq + Hash + Clone + Send + Sync + 'static,
    T: Clone + PartialEq + Send + Sync + Serialize + DeserializeOwned + 'static,
    V: IntoView + 'static,
    Result<T, Failure>: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
{
    show(listen(scope, key), render)
}

/// Renders a resource that was already started with `listen`.
pub fn show<T, V>(
    resource: Resource<Result<T, Failure>>,
    render: impl Fn(T) -> V + Send + Sync + 'static,
) -> impl IntoView
where
    T: Clone + Send + Sync + 'static,
    V: IntoView + 'static,
{
    view! {
        <Transition fallback=loading>
            <ErrorBoundary fallback=failed>
                {move || resource.get().map(|result| result.map(|value| render(value).into_any()))}
            </ErrorBoundary>
        </Transition>
    }
}

fn loading() -> impl IntoView {
    scaffold(false)
}

fn still() -> impl IntoView {
    scaffold(true)
}

fn scaffold(hold: bool) -> impl IntoView {
    view! {
        <div
            class=move || {
                if hold || crate::client::waiting() {
                    "scaffold still"
                } else {
                    "scaffold"
                }
            }
            aria-busy="true"
            aria-label="Loading"
        >
            <div class="scaffold-title"></div>
            <div class="scaffold-card">
                <div class="scaffold-line wide"></div>
                <div class="scaffold-line"></div>
                <div class="scaffold-line short"></div>
            </div>
            <div class="scaffold-card">
                <div class="scaffold-line"></div>
                <div class="scaffold-line short"></div>
            </div>
        </div>
    }
}

/// What a page shows when a read failed: a still skeleton while Marian can't be
/// reached, otherwise the failure itself.
fn failed(errors: ArcRwSignal<Errors>) -> impl IntoView {
    move || {
        let failures: Vec<Failure> = errors.with(|errors| {
            errors
                .iter()
                .filter_map(|(_, error)| error.clone().into_inner().downcast_ref::<Failure>().cloned())
                .collect()
        });
        if failures.iter().any(Failure::unreachable) {
            return still().into_any();
        }
        match failures.into_iter().next() {
            Some(failure) => crate::failed(failure).into_any(),
            None => view! { <p class="error">"Something went wrong."</p> }.into_any(),
        }
    }
}

/// Turns a tuple of `resource.get()` values into one `Result` when every
/// read has finished. Works for 2–4 reads; add another `impl` if you need more.
pub trait Ready {
    type Output;
    fn ready(self) -> Option<Result<Self::Output, Failure>>;
}

macro_rules! impl_ready {
    ($($idx:tt : $T:ident),+) => {
        impl<$($T),+> Ready for ($(Option<Result<$T, Failure>>,)+) {
            type Output = ($($T,)+);
            fn ready(self) -> Option<Result<Self::Output, Failure>> {
                Some(Ok(($(
                    match self.$idx {
                        Some(Ok(value)) => value,
                        Some(Err(failure)) => return Some(Err(failure)),
                        None => return None,
                    },
                )+)))
            }
        }
    };
}

impl_ready!(0: A, 1: B);
impl_ready!(0: A, 1: B, 2: C);
impl_ready!(0: A, 1: B, 2: C, 3: D);
impl_ready!(0: A, 1: B, 2: C, 3: D, 4: E);

/// Renders when `gather` has every read it needs. Start the resources first
/// so they run together, then pass `(a.get(), b.get(), …).ready()`.
pub fn when<T, V>(
    gather: impl Fn() -> Option<Result<T, Failure>> + Send + Sync + 'static,
    render: impl Fn(T) -> V + Send + Sync + 'static,
) -> impl IntoView
where
    T: Clone + Send + Sync + 'static,
    V: IntoView + 'static,
{
    view! {
        <Transition fallback=loading>
            <ErrorBoundary fallback=failed>
                {move || gather().map(|result| result.map(|value| render(value).into_any()))}
            </ErrorBoundary>
        </Transition>
    }
}

/// `when` for a page that starts from server data and then holds someone's
/// unsaved work: it renders from the first complete read and lets later
/// refreshes pass by. Without it, any change elsewhere on the server would
/// redraw the page and lose that work.
pub fn when_once<T, V>(
    gather: impl Fn() -> Option<Result<T, Failure>> + Send + Sync + 'static,
    render: impl Fn(T) -> V + Send + Sync + 'static,
) -> impl IntoView
where
    T: Clone + PartialEq + Send + Sync + 'static,
    V: IntoView + 'static,
{
    let gather = std::sync::Arc::new(gather);
    let render = std::sync::Arc::new(render);
    view! {
        <Transition fallback=loading>
            <ErrorBoundary fallback=failed>
                {
                    // Made inside the Transition, so its resource reads count
                    // toward it. Once ready, the memo stops reading them, and so
                    // stops hearing their refreshes.
                    let gather = std::sync::Arc::clone(&gather);
                    let render = std::sync::Arc::clone(&render);
                    let kept = Memo::new(move |previous: Option<&Option<Result<T, Failure>>>| match previous {
                        Some(Some(Ok(value))) => Some(Ok(value.clone())),
                        _ => gather(),
                    });
                    move || kept.get().map(|result| result.map(|value| render(value).into_any()))
                }
            </ErrorBoundary>
        </Transition>
    }
}

/// The server's own read is a leaf, so its counts refetch without everything under the server.
fn link_server(id: &str) -> [String; 3] {
    ["s".into(), id.to_string(), "info".into()]
}

fn link_event(key: &(String, String)) -> [String; 4] {
    ["s".into(), key.0.clone(), "e".into(), key.1.clone()]
}

pub fn me_scope() -> QueryScope<(), Result<dto::Me, Failure>> {
    QueryScope::new(get_me).with_invalidation_link(|_| ["me"])
}

async fn get_me() -> Result<dto::Me, Failure> {
    api::me().await
}

pub fn server_scope() -> QueryScope<String, Result<dto::Server, Failure>> {
    QueryScope::new(get_server).with_invalidation_link(|id| link_server(id))
}

async fn get_server(id: String) -> Result<dto::Server, Failure> {
    api::server(&id).await
}

pub fn membership_scope() -> QueryScope<String, Result<dto::Membership, Failure>> {
    QueryScope::new(get_membership).with_invalidation_link(|id| ["s".into(), id.clone(), "me".into()])
}

async fn get_membership(id: String) -> Result<dto::Membership, Failure> {
    api::membership(&id).await
}

pub fn settings_scope() -> QueryScope<String, Result<dto::Settings, Failure>> {
    QueryScope::new(get_settings).with_invalidation_link(|id| ["s".into(), id.clone(), "settings".into()])
}

async fn get_settings(id: String) -> Result<dto::Settings, Failure> {
    api::settings(&id).await
}

pub fn roles_scope() -> QueryScope<String, Result<Vec<dto::Role>, Failure>> {
    QueryScope::new(get_roles).with_invalidation_link(|id| ["s".into(), id.clone(), "roles".into()])
}

async fn get_roles(id: String) -> Result<Vec<dto::Role>, Failure> {
    api::roles(&id).await
}

pub fn members_scope() -> QueryScope<String, Result<Vec<dto::Member>, Failure>> {
    QueryScope::new(get_members).with_invalidation_link(|id| ["s".into(), id.clone(), "members".into()])
}

async fn get_members(id: String) -> Result<Vec<dto::Member>, Failure> {
    api::members(&id).await
}

pub fn channels_scope() -> QueryScope<String, Result<Vec<dto::Channel>, Failure>> {
    QueryScope::new(get_channels).with_invalidation_link(|id| ["s".into(), id.clone(), "channels".into()])
}

async fn get_channels(id: String) -> Result<Vec<dto::Channel>, Failure> {
    api::channels(&id).await
}

pub fn events_scope() -> QueryScope<(String, String), Result<dto::Paged<dto::EventSummary>, Failure>> {
    QueryScope::new(get_events).with_invalidation_link(|(id, _)| ["s".into(), id.clone(), "events".into()])
}

async fn get_events((server, qs): (String, String)) -> Result<dto::Paged<dto::EventSummary>, Failure> {
    let query = serde_json::from_str(&qs).unwrap_or_default();
    api::events(&server, &query).await
}

pub fn events_key(server: impl Into<String>, query: &dto::EventQuery) -> (String, String) {
    (
        server.into(),
        serde_json::to_string(query).unwrap_or_else(|_| "{}".into()),
    )
}

pub fn event_scope() -> QueryScope<(String, String), Result<dto::Event, Failure>> {
    QueryScope::new(get_event).with_invalidation_link(link_event)
}

async fn get_event((server, id): (String, String)) -> Result<dto::Event, Failure> {
    api::event(&server, &id).await
}

pub fn reminders_scope() -> QueryScope<(String, String), Result<Vec<dto::Reminder>, Failure>> {
    QueryScope::new(get_reminders).with_invalidation_link(|(s, id)| {
        ["s".into(), s.clone(), "e".into(), id.clone(), "reminders".into()]
    })
}

async fn get_reminders((server, id): (String, String)) -> Result<Vec<dto::Reminder>, Failure> {
    api::event_reminders(&server, &id).await
}

pub fn signups_scope() -> QueryScope<(String, String), Result<dto::Signups, Failure>> {
    QueryScope::new(get_signups).with_invalidation_link(|(s, id)| {
        ["s".into(), s.clone(), "e".into(), id.clone(), "signups".into()]
    })
}

async fn get_signups((server, id): (String, String)) -> Result<dto::Signups, Failure> {
    api::signups(&server, &id).await
}

pub fn pending_reminders_scope() -> QueryScope<String, Result<Vec<dto::PendingReminder>, Failure>> {
    QueryScope::new(get_pending).with_invalidation_link(|id| ["s".into(), id.clone(), "pending".into()])
}

async fn get_pending(server: String) -> Result<Vec<dto::PendingReminder>, Failure> {
    api::pending_reminders(&server, 6).await
}

pub fn activity_scope() -> QueryScope<(String, u32, u32), Result<dto::Paged<dto::Activity>, Failure>> {
    QueryScope::new(get_activity).with_invalidation_link(|(id, _, _)| ["s".into(), id.clone(), "activity".into()])
}

async fn get_activity((server, page, limit): (String, u32, u32)) -> Result<dto::Paged<dto::Activity>, Failure> {
    api::activity(&server, page, limit).await
}

pub fn series_list_scope() -> QueryScope<String, Result<Vec<dto::Series>, Failure>> {
    QueryScope::new(get_series_list).with_invalidation_link(|id| ["s".into(), id.clone(), "series".into()])
}

async fn get_series_list(server: String) -> Result<Vec<dto::Series>, Failure> {
    api::series_list(&server).await
}

pub fn series_scope() -> QueryScope<(String, String), Result<dto::Series, Failure>> {
    QueryScope::new(get_series).with_invalidation_link(|(s, id)| {
        ["s".into(), s.clone(), "series".into(), id.clone()]
    })
}

async fn get_series((server, id): (String, String)) -> Result<dto::Series, Failure> {
    api::series(&server, &id).await
}

pub fn presets_scope() -> QueryScope<String, Result<Vec<dto::Preset>, Failure>> {
    QueryScope::new(get_presets).with_invalidation_link(|id| ["s".into(), id.clone(), "presets".into()])
}

async fn get_presets(server: String) -> Result<Vec<dto::Preset>, Failure> {
    api::presets(&server).await
}

pub fn library_scope() -> QueryScope<String, Result<Vec<dto::LibraryItem>, Failure>> {
    QueryScope::new(get_library).with_invalidation_link(|id| ["s".into(), id.clone(), "library".into()])
}

async fn get_library(server: String) -> Result<Vec<dto::LibraryItem>, Failure> {
    api::library(&server).await
}

pub fn commands_scope() -> QueryScope<String, Result<Vec<dto::CommandLog>, Failure>> {
    QueryScope::new(get_commands).with_invalidation_link(|id| ["s".into(), id.clone(), "commands".into()])
}

async fn get_commands(server: String) -> Result<Vec<dto::CommandLog>, Failure> {
    api::commands(&server).await
}

pub fn routines_scope() -> QueryScope<String, Result<Vec<dto::RoutineSummary>, Failure>> {
    QueryScope::new(get_routines).with_invalidation_link(|id| ["s".into(), id.clone(), "routines".into()])
}

async fn get_routines(server: String) -> Result<Vec<dto::RoutineSummary>, Failure> {
    api::routines(&server).await
}

pub fn routine_scope() -> QueryScope<(String, String), Result<dto::RoutineView, Failure>> {
    QueryScope::new(get_routine).with_invalidation_link(|(s, id)| {
        ["s".into(), s.clone(), "routines".into(), id.clone()]
    })
}

async fn get_routine((server, id): (String, String)) -> Result<dto::RoutineView, Failure> {
    api::routine(&server, &id).await
}

pub fn timezones_scope() -> QueryScope<String, Result<Vec<dto::ZoneGroup>, Failure>> {
    QueryScope::new(get_timezones)
}

async fn get_timezones(selected: String) -> Result<Vec<dto::ZoneGroup>, Failure> {
    api::timezones(&selected).await
}

/// What a new event starts from: the server's channels, and the preset named in
/// the address or, without one, the server's default.
pub fn new_event_scope() -> QueryScope<(String, String), Result<(Vec<dto::Channel>, Option<dto::Preset>), Failure>>
{
    QueryScope::new(get_new_event).with_invalidation_link(|(s, _)| ["s".into(), s.clone(), "form".into()])
}

async fn get_new_event((server, preset): (String, String)) -> Result<(Vec<dto::Channel>, Option<dto::Preset>), Failure>
{
    let channels = api::channels(&server).await?;
    let presets = api::presets(&server).await?;
    let chosen = presets
        .iter()
        .find(|shown| !preset.is_empty() && shown.id == preset)
        .or_else(|| presets.iter().find(|shown| shown.default))
        .or_else(|| presets.first())
        .cloned();
    Ok((channels, chosen))
}
