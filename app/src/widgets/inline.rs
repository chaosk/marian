//! Event details changed in place on the event page: a pencil opens a small
//! form, Save sends only that detail, and Cancel or Escape puts it back.

use chrono::{Duration, TimeZone, Utc};
use leptos::prelude::*;
use marian_types as dto;
use marian_types::labels::PALETTE;

use crate::format::{self, cadence_label, color_name, human_duration, human_offset, Clock};
use crate::{api, cache, client};

const DURATIONS: [i64; 12] = [15, 30, 45, 60, 90, 120, 180, 240, 360, 480, 720, 1440];
const CUTOFFS: [i64; 11] = [0, 15, 30, 60, 120, 180, 360, 720, 1440, 2880, 10080];
const MAX_REMINDERS: usize = 5;

/// Whether this person can change the event from its page.
fn editable(event: &dto::Event) -> bool {
    event.can.edit && !matches!(event.status.as_str(), "archived" | "skipped" | "cancelled")
}

pub(super) fn pencil() -> impl IntoView {
    view! {
        <svg class="pencil" viewBox="0 0 24 24" aria-hidden="true">
            <path d="M3 17.25V21h3.75L17.81 9.94l-3.75-3.75L3 17.25zM20.71 7.04a1 1 0 0 0 0-1.41l-2.34-2.34a1 1 0 0 0-1.41 0l-1.83 1.83 3.75 3.75 1.83-1.83z"/>
        </svg>
    }
}

pub(super) fn escape(ev: &leptos::ev::KeyboardEvent) -> bool {
    ev.key() == "Escape"
}

/// A `datetime-local` field's value, with or without seconds.
fn parse_local(value: &str) -> Option<chrono::NaiveDateTime> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S"))
        .ok()
}

/// A new event kept in the browser until it is saved. When a page provides one,
/// the in-place editors write into it instead of sending anything to the server.
#[derive(Clone, Copy)]
pub struct LocalDraft {
    pub event: RwSignal<dto::Event>,
    pub reminders: RwSignal<Vec<dto::Reminder>>,
    pub repeat: RwSignal<Option<dto::Repeat>>,
    pub options: RwSignal<Vec<dto::SignupOption>>,
    pub channels: StoredValue<Vec<dto::Channel>>,
    pub clock: Clock,
    /// Something was set since the page opened, so leaving would lose it.
    pub dirty: RwSignal<bool>,
    /// The details are a routine's, for every event it posts, not one date's.
    pub template: bool,
    /// Called after each change, for a page that saves as it goes.
    pub sync: Option<Callback<()>>,
}

impl LocalDraft {
    /// Tells the page something changed.
    pub fn changed(self) {
        self.dirty.set(true);
        if let Some(sync) = self.sync {
            sync.run(());
        }
    }
}

/// A routine's page, where details hold for every event it posts.
fn for_template() -> bool {
    use_context::<LocalDraft>().is_some_and(|draft| draft.template)
}

impl LocalDraft {
    /// Keeps a change in the browser, as the server would have saved it.
    fn keep(self, patch: &dto::EventPatch) {
        let channel = patch.channel_id.as_ref().and_then(|id| {
            self.channels
                .with_value(|channels| channels.iter().find(|channel| &channel.id == id).map(|channel| channel.name.clone()))
        });
        let clock = self.clock;
        let repeat = patch.repeat.clone().filter(|repeat| repeat.cadence != "none");
        self.event.update(|event| {
            preview(event, patch, Some(clock));
            if let (Some(id), Some(name)) = (patch.channel_id.clone(), channel) {
                event.channel_id = id;
                event.channel = name;
            }
            if patch.repeat.is_some() {
                event.series = repeat.as_ref().map(|repeat| dto::Series {
                    id: String::new(),
                    title: event.title.clone(),
                    cadence: repeat.cadence.clone(),
                    active: true,
                    max_occurrences: repeat.count,
                    end_on: repeat.until.and_then(|day| {
                        clock.tz.from_local_datetime(&day.and_hms_opt(23, 59, 0)?).earliest()
                    }).map(|end| end.with_timezone(&Utc)),
                    next: None,
                    dates: 0,
                });
            }
        });
        if let Some(list) = &patch.reminders {
            self.reminders.set(list.clone());
        }
        if patch.repeat.is_some() {
            self.repeat.set(repeat);
        }
        self.changed();
    }
}

/// A request one in-place editor makes, and how its answer comes back.
#[derive(Clone)]
struct Request {
    method: &'static str,
    url: String,
    body: Option<serde_json::Value>,
    /// The answer is the event as it now reads, to put in the cache.
    answers_event: bool,
    /// A toast after it works.
    said: Option<&'static str>,
    /// Reload what is saved when it fails, because the page showed the change early.
    reload_on_failure: bool,
}

/// What a caller does once its request lands: on success, or when the server says no.
struct Hooks {
    done: Box<dyn FnOnce() + Send + Sync>,
    undo: Box<dyn FnOnce() + Send + Sync>,
}

/// One detail's change in flight: where it goes, whether it is sending, and why it failed.
#[derive(Clone, Copy)]
struct Sender {
    key: StoredValue<(String, String)>,
    problem: RwSignal<String>,
    /// The server's clock, for patches that carry a local start time.
    clock: Option<Clock>,
    /// A new event in the browser, which takes the changes instead of the server.
    local: Option<LocalDraft>,
    /// The request this editor has in flight; its `pending` keeps a second one waiting.
    request: Action<Request, ()>,
    hooks: StoredValue<Option<Hooks>>,
}

impl Sender {
    fn new(key: (String, String)) -> Self {
        let key = StoredValue::new(key);
        let problem = RwSignal::new(String::new());
        // Plain storage, not thread-local: the server renders editors on any thread.
        let hooks = StoredValue::new(None::<Hooks>);
        let request = client::action(move |request: &Request| {
            let request = request.clone();
            let (server, event) = key.get_value();
            let queries = cache::client();
            let toasts = client::toast_list();
            async move {
                let answer = if request.answers_event {
                    api::send::<dto::Event, _>(request.method, &request.url, request.body.as_ref())
                        .await
                        .map(Some)
                } else {
                    api::send::<dto::Done, _>(request.method, &request.url, request.body.as_ref())
                        .await
                        .map(|_| None)
                };
                let hooks = hooks.try_update_value(Option::take).flatten();
                match answer {
                    Ok(fresh) => {
                        if let Some(hooks) = hooks {
                            (hooks.done)();
                        }
                        if let (Some(said), Some(list)) = (request.said, toasts) {
                            client::push_toast(list, said);
                        }
                        if let Some(fresh) = fresh {
                            cache::apply_event(queries, &(server.clone(), event), &fresh);
                        }
                        cache::refresh_in(queries, &server);
                    }
                    Err(failure) => {
                        if let Some(hooks) = hooks {
                            (hooks.undo)();
                        }
                        problem.try_set(failure.message);
                        if request.reload_on_failure {
                            cache::refresh_in(queries, &server);
                        }
                    }
                }
            }
        });
        Self {
            key,
            problem,
            clock: None,
            local: use_context::<LocalDraft>(),
            request,
            hooks,
        }
    }

    fn in_zone(mut self, clock: Clock) -> Self {
        self.clock = Some(clock);
        self
    }

    /// A request is in flight.
    fn busy(self) -> Memo<bool> {
        self.request.pending()
    }

    fn dispatch(self, request: Request, done: impl FnOnce() + Send + Sync + 'static, undo: impl FnOnce() + Send + Sync + 'static) {
        self.problem.set(String::new());
        self.hooks.set_value(Some(Hooks {
            done: Box::new(done),
            undo: Box::new(undo),
        }));
        self.request.dispatch(request);
    }

    /// Sends the patch. The change goes into the cache first: the server tells
    /// open pages about it, and that refresh can come before this answer, so
    /// the page must already hold the new value. The answer replaces it after,
    /// and the page itself is the confirmation. `undo` runs when the server says
    /// no, and the page reloads what is really saved.
    fn send(self, patch: dto::EventPatch, done: impl FnOnce() + Send + Sync + 'static, undo: impl FnOnce() + Send + Sync + 'static) {
        if let Some(local) = self.local {
            let _ = undo;
            self.problem.set(String::new());
            done();
            local.keep(&patch);
            return;
        }
        let (server, event) = self.key.get_value();
        let queries = cache::client();
        let key = (server.clone(), event.clone());
        cache::preview_event(queries, &key, |cached| preview(cached, &patch, self.clock));
        if let Some(list) = &patch.reminders {
            cache::preview_reminders(queries, &key, list);
        }
        let request = Request {
            method: "PATCH",
            url: api::path::event(&server, &event),
            body: serde_json::to_value(&patch).ok(),
            answers_event: true,
            said: None,
            reload_on_failure: true,
        };
        self.dispatch(request, done, undo);
    }

    /// Asks the server to post or schedule the event, or take it back to a
    /// draft, at the path `route` names. The answer is the event as it now reads.
    fn go(
        self,
        route: fn(&str, &str) -> String,
        body: Option<serde_json::Value>,
        said: &'static str,
        done: impl FnOnce() + Send + Sync + 'static,
    ) {
        let (server, event) = self.key.get_value();
        let request = Request {
            method: "POST",
            url: route(&server, &event),
            body,
            answers_event: true,
            said: Some(said),
            reload_on_failure: false,
        };
        self.dispatch(request, done, || ());
    }

    /// Changes or ends the series this event belongs to; the page reloads after.
    fn series(
        self,
        method: &'static str,
        url: String,
        body: Option<serde_json::Value>,
        said: &'static str,
        done: impl FnOnce() + Send + Sync + 'static,
    ) {
        let request = Request {
            method,
            url,
            body,
            answers_event: false,
            said: Some(said),
            reload_on_failure: false,
        };
        self.dispatch(request, done, || ());
    }

    fn fail(self, message: &str) {
        self.problem.set(message.to_string());
    }

    fn error(self) -> impl IntoView {
        move || {
            let text = self.problem.get();
            (!text.is_empty()).then(|| view! { <p class="error inline-error">{text}</p> })
        }
    }

    fn actions(self, cancel: impl Fn() + Copy + Send + Sync + 'static) -> impl IntoView {
        view! {
            <div class="inline-actions">
                <button type="submit" class="primary" disabled=move || self.busy().get()>"Save"</button>
                <button type="button" class="ghost" on:click=move |_| cancel()>"Cancel"</button>
            </div>
        }
    }
}

/// What the event will read once `patch` is saved, as far as the page can tell.
fn preview(event: &mut dto::Event, patch: &dto::EventPatch, clock: Option<Clock>) {
    if let Some(title) = &patch.title {
        event.title = title.trim().to_string();
    }
    if let Some(description) = &patch.description {
        event.description = description.trim().to_string();
    }
    if let Some(color) = &patch.color {
        event.color = color.clone();
    }
    if let Some(place) = &patch.location {
        event.location = place.trim().to_string();
    }
    if let Some(on) = patch.thread_enabled {
        event.thread_enabled = on;
    }
    if let Some(on) = patch.multiple_signups {
        event.multiple_signups = on;
    }
    if let Some(on) = patch.mention_everyone {
        event.mention_everyone = on;
    }
    if let Some(ids) = &patch.mention_role_ids {
        event.mention_role_ids = ids.clone();
    }
    if let Some(ids) = &patch.restrict_role_ids {
        event.restrict_role_ids = ids.clone();
    }
    if let Some(role) = &patch.attendee_role_id {
        event.attendee_role_id = role.clone();
    }
    let start = patch
        .starts_local
        .zip(clock)
        .and_then(|(local, clock)| clock.tz.from_local_datetime(&local).earliest())
        .map(|start| start.with_timezone(&Utc));
    if let Some(start) = start {
        let shift = start - event.starts_at;
        event.cutoff_at = event.cutoff_at.map(|cutoff| cutoff + shift);
        event.starts_at = start;
    }
    if let Some(minutes) = patch.duration_minutes {
        event.duration_minutes = minutes;
    }
    match patch.cutoff {
        Some(dto::Cutoff::Never) => event.cutoff_at = None,
        Some(dto::Cutoff::Before { minutes }) => event.cutoff_at = Some(event.starts_at - Duration::minutes(minutes)),
        None => {}
    }
    if patch.cutoff.is_some() || start.is_some() {
        event.signups_closed = event.cutoff_at.is_some_and(|cutoff| format::now() >= cutoff);
    }
}

/// Focuses a field each time it appears.
pub(super) fn focus_when_shown<E>(node: NodeRef<E>)
where
    E: leptos::html::ElementType + 'static,
    E::Output: std::ops::Deref<Target = leptos::web_sys::HtmlElement> + Clone + 'static,
    NodeRef<E>: Get<Value = Option<E::Output>> + Copy + Send + Sync,
{
    Effect::new(move |_| {
        if let Some(field) = node.get() {
            let _ = field.focus();
        }
    });
}

#[component]
pub fn EventTitle(key: (String, String), event: dto::Event) -> impl IntoView {
    let shown = StoredValue::new(event.title.clone());
    if !editable(&event) {
        return view! { <h1>{event.title}</h1> }.into_any();
    }
    let sender = Sender::new(key);
    let open = RwSignal::new(false);
    let text = RwSignal::new(event.title);
    let field = NodeRef::<leptos::html::Input>::new();
    focus_when_shown(field);
    let cancel = move || {
        text.set(shown.get_value());
        sender.problem.set(String::new());
        open.set(false);
    };
    let save = move || {
        let next = text.get_untracked().trim().to_string();
        if next.is_empty() {
            return sender.fail("Give the event a title.");
        }
        if next == shown.get_value() {
            open.set(false);
            return;
        }
        sender.send(
            dto::EventPatch {
                title: Some(next),
                ..Default::default()
            },
            move || open.set(false),
            || (),
        );
    };
    view! {
        {move || if open.get() {
            view! {
                <form class="inline-form title-form" data-editor=""
                    on:submit=move |ev| { ev.prevent_default(); save(); }
                    on:keydown=move |ev| if escape(&ev) { cancel(); }>
                    <input node_ref=field class="title-input" maxlength="120" aria-label="Title"
                        prop:value=move || text.get() on:input=move |ev| text.set(event_target_value(&ev))/>
                    {sender.actions(cancel)}
                </form>
            }
            .into_any()
        } else {
            view! {
                <h1>
                    <button type="button" class="inline-edit" title="Change the title" on:click=move |_| open.set(true)>
                        {move || {
                            let text = shown.get_value();
                            if text.is_empty() {
                                view! { <span class="muted">"Name the event"</span> }.into_any()
                            } else {
                                view! { <span>{text}</span> }.into_any()
                            }
                        }}{pencil()}
                    </button>
                </h1>
            }
            .into_any()
        }}
        {sender.error()}
    }
    .into_any()
}

/// Post now and Schedule post, for an event that hasn't gone out to Discord yet.
#[component]
pub fn PostButtons(key: (String, String), event: dto::Event, clock: Clock) -> impl IntoView {
    if let Some(draft) = use_context::<LocalDraft>() {
        return new_event_buttons(key, draft, clock).into_any();
    }
    let waiting = matches!(event.status.as_str(), "draft" | "scheduled" | "failed");
    if !event.can.edit || !waiting {
        return ().into_any();
    }
    let sender = Sender::new(key);
    let open = RwSignal::new(false);
    let now = format::now();
    let first = event.post_at.filter(|at| *at > now).unwrap_or_else(|| {
        let day_before = event.starts_at - Duration::days(1);
        if day_before > now { day_before } else { now + Duration::hours(1) }
    });
    let initial = StoredValue::new(clock.input(first));
    let when = RwSignal::new(initial.get_value());
    let zone = clock.label(now);
    let starts = clock.when(event.starts_at);
    let scheduled = event.status == "scheduled";
    let field = NodeRef::<leptos::html::Input>::new();
    focus_when_shown(field);
    let cancel = move || {
        when.set(initial.get_value());
        sender.problem.set(String::new());
        open.set(false);
    };
    let post_now = move |_| {
        open.set(false);
        sender.go(api::path::event_publish, None, "Posted to Discord.", || ());
    };
    let schedule = move || {
        let Some(local) = parse_local(&when.get_untracked()) else {
            return sender.fail("Pick a date and time to post.");
        };
        let body = dto::PostAt { post_local: local };
        sender.go(api::path::event_schedule, serde_json::to_value(body).ok(), "Scheduled.", move || open.set(false));
    };
    view! {
        <div class="inline-anchor post-buttons">
            <button type="button" class="ghost" disabled=move || sender.busy().get()
                aria-expanded=move || if open.get() { "true" } else { "false" }
                on:click=move |_| if open.get_untracked() { cancel() } else { open.set(true) }>
                {if scheduled { "Reschedule" } else { "Schedule post" }}
            </button>
            {(event.status != "draft").then(|| view! {
                <button type="button" class="ghost" disabled=move || sender.busy().get()
                    on:click=move |_| sender.go(api::path::event_draft, None, "Back to a draft. It won't post on its own.", || ())>
                    "Back to draft"
                </button>
            })}
            <button type="button" class="primary" disabled=move || sender.busy().get() on:click=post_now>"Post now"</button>
            {move || open.get().then(|| view! {
                <form class="popover" data-editor=""
                    on:submit=move |ev| { ev.prevent_default(); schedule(); }
                    on:keydown=move |ev| if escape(&ev) { cancel(); }>
                    <label class="field">"Post to Discord at"
                        <input node_ref=field type="datetime-local" required
                            prop:value=move || when.get() on:input=move |ev| when.set(event_target_value(&ev))/>
                    </label>
                    <p class="popover-note">{zone.clone()}</p>
                    <p class="popover-note left">"The event starts "{starts.clone()}". The post has to go out before then."</p>
                    {sender.error()}
                    {sender.actions(cancel)}
                </form>
            })}
            {move || (!open.get()).then(|| sender.error())}
        </div>
    }
    .into_any()
}

const CADENCES: [&str; 4] = ["daily", "weekly", "biweekly", "monthly"];

/// How a series reads in a line: its repeat and how long it runs.
fn repeat_line(series: &dto::Series, clock: &Clock) -> String {
    if !series.active {
        return "Ended".into();
    }
    let length = match (series.max_occurrences, series.end_on) {
        (Some(count), _) => format!("{count} dates"),
        (None, Some(end)) => format!("until {}", format::format_day(clock.day(end))),
        (None, None) => "keeps going".into(),
    };
    format!("{} · {length}", cadence_label(&series.cadence))
}

/// Whether the event repeats, and a form to start, change, or stop the repeat.
#[component]
pub fn EventRepeat(key: (String, String), event: dto::Event, clock: Clock) -> impl IntoView {
    let series = event.series.clone().filter(|series| series.cadence != "once");
    let face = series
        .as_ref()
        .map(|series| repeat_line(series, &clock))
        .unwrap_or_else(|| "Doesn't repeat".into());
    // A new event's planned repeat has no id yet; only a saved series has dates to show.
    let in_series = series.as_ref().is_some_and(|series| !series.id.is_empty());
    let all_dates = series
        .as_ref()
        .filter(|_| in_series)
        .map(|series| format!("{}/recurring/{}", crate::base(), series.id));
    let link = move || {
        all_dates
            .clone()
            .map(|href| view! { <p class="face-link"><crate::A href=href>"See all dates"</crate::A></p> })
    };
    let ended = series.as_ref().is_some_and(|series| !series.active);
    if !editable(&event) || ended {
        return view! { <p>{face}</p>{link()} }.into_any();
    }
    let sender = Sender::new(key);
    let open = RwSignal::new(false);
    let start = (
        series.as_ref().map(|series| series.cadence.clone()).unwrap_or_else(|| "none".into()),
        series
            .as_ref()
            .and_then(|series| series.max_occurrences)
            .map(|n| n.to_string())
            .unwrap_or_default(),
        series
            .as_ref()
            .and_then(|series| series.end_on)
            .map(|end| clock.date_input(end))
            .unwrap_or_default(),
    );
    let cadence = RwSignal::new(start.0.clone());
    let count = RwSignal::new(start.1.clone());
    let until = RwSignal::new(start.2.clone());
    let start = StoredValue::new(start);
    let series_id = StoredValue::new(series.as_ref().filter(|_| in_series).map(|series| series.id.clone()));
    let cancel = move || {
        let (first, stop, end) = start.get_value();
        cadence.set(first);
        count.set(stop);
        until.set(end);
        sender.problem.set(String::new());
        open.set(false);
    };
    let save = move || {
        let count = match count.get_untracked().trim() {
            "" => None,
            text => match text.parse::<i64>() {
                Ok(n) => Some(n),
                Err(_) => return sender.fail("The number of dates needs to be a number."),
            },
        };
        let until = match until.get_untracked().trim() {
            "" => None,
            text => match chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d") {
                Ok(date) => Some(date),
                Err(_) => return sender.fail("Use a calendar date."),
            },
        };
        let repeat = dto::Repeat {
            cadence: cadence.get_untracked(),
            count,
            until,
        };
        // Choosing no repeat changes nothing for a saved event that doesn't repeat.
        if repeat.cadence == "none" && sender.local.is_none() {
            open.set(false);
            return;
        }
        match series_id.get_value() {
            None => sender.send(
                dto::EventPatch {
                    repeat: Some(repeat),
                    ..Default::default()
                },
                move || open.set(false),
                || (),
            ),
            Some(id) => sender.series(
                "PATCH",
                api::path::series(&cache::server_now(), &id),
                serde_json::to_value(&repeat).ok(),
                "Saved the repeat.",
                move || open.set(false),
            ),
        }
    };
    let stopping = RwSignal::new(false);
    let stop = move || {
        let Some(id) = series_id.get_value() else {
            return;
        };
        stopping.set(false);
        sender.series(
            "POST",
            api::path::series_end(&cache::server_now(), &id),
            None,
            "Stopped repeating. Upcoming dates were skipped.",
            move || open.set(false),
        );
    };
    let cadences = move || {
        let now = cadence.get_untracked();
        let none = (!in_series).then(|| view! { <option value="none" selected=now == "none">"Doesn't repeat"</option> });
        let rest = CADENCES
            .iter()
            .map(|value| view! { <option value=*value selected=now == *value>{cadence_label(value)}</option> })
            .collect_view();
        view! { {none}{rest} }
    };
    view! {
        <div class="inline-anchor">
            <button type="button" class="inline-edit" title="Change the repeat"
                on:click=move |_| if open.get_untracked() { cancel() } else { open.set(true) }>
                {face}{pencil()}
            </button>
            {move || open.get().then(|| view! {
                <form class="popover" data-editor=""
                    on:submit=move |ev| { ev.prevent_default(); save(); }
                    on:keydown=move |ev| if escape(&ev) { cancel(); }>
                    <label class="field">"Repeats"
                        <select on:change=move |ev| cadence.set(event_target_value(&ev))>{cadences}</select>
                    </label>
                    <div class="grid">
                        <label class="field">"Stop after"
                            <input type="number" min="2" inputmode="numeric" placeholder="Keeps going"
                                prop:value=move || count.get() on:input=move |ev| count.set(event_target_value(&ev))/>
                        </label>
                        <label class="field">"Or end on"
                            <input type="date" prop:value=move || until.get() on:input=move |ev| until.set(event_target_value(&ev))/>
                        </label>
                    </div>
                    <p class="popover-note left">{if in_series {
                        "Upcoming dates follow the new repeat. Dates already posted stay as they are."
                    } else {
                        "This event is the first date. Marian adds the next ones ahead of time."
                    }}</p>
                    {sender.error()}
                    {move || if stopping.get() {
                        view! {
                            <div class="inline-actions">
                                <super::InlineConfirm
                                    question="Stop repeating? Dates that haven't gone out yet are skipped."
                                    yes="Stop repeating"
                                    on_yes=Callback::new(move |_| stop())
                                    on_no=Callback::new(move |_| stopping.set(false))/>
                            </div>
                        }
                        .into_any()
                    } else {
                        view! {
                            <div class="inline-actions">
                                {in_series.then(|| view! {
                                    <button type="button" class="danger repeat-stop" on:click=move |_| stopping.set(true)>"Stop repeating"</button>
                                })}
                                <button type="submit" class="primary" disabled=move || sender.busy().get()>"Save"</button>
                                <button type="button" class="ghost" on:click=move |_| cancel()>"Cancel"</button>
                            </div>
                        }
                        .into_any()
                    }}
                </form>
            })}
            {link()}
        </div>
    }
    .into_any()
}

/// Everything the browser holds for a new event, as the create endpoint takes it.
fn new_event_draft(draft: LocalDraft, action: dto::SaveAction, post_local: Option<chrono::NaiveDateTime>) -> dto::EventDraft {
    let event = draft.event.get_untracked();
    let local = |at: chrono::DateTime<Utc>| at.with_timezone(&draft.clock.tz).naive_local();
    dto::EventDraft {
        title: event.title,
        description: event.description,
        location: event.location,
        channel_id: event.channel_id,
        color: event.color,
        starts_local: local(event.starts_at),
        duration_minutes: event.duration_minutes,
        cutoff_local: event.cutoff_at.map(local),
        post_local,
        attendee_role_id: event.attendee_role_id,
        thread_enabled: event.thread_enabled,
        multiple_signups: event.multiple_signups,
        mention_everyone: event.mention_everyone,
        mention_role_ids: event.mention_role_ids,
        restrict_role_ids: event.restrict_role_ids,
        // Options get their numbers when the event is saved.
        options: draft
            .options
            .get_untracked()
            .into_iter()
            .map(|opt| dto::SignupOption { id: None, ..opt })
            .collect(),
        reminders: draft.reminders.get_untracked(),
        repeat: draft.repeat.get_untracked(),
        action,
        apply_series: false,
    }
}

/// What stops a new event from being saved, as the server would say it. Empty
/// means it can go. `post_at` is the chosen post time when scheduling.
fn unsaved_problems(draft: LocalDraft, post_at: Option<chrono::DateTime<Utc>>) -> Vec<&'static str> {
    let event = draft.event.get_untracked();
    let mut problems = Vec::new();
    let title = event.title.trim();
    if title.is_empty() {
        problems.push("Give the event a title.");
    } else if title.chars().count() > 120 {
        problems.push("Keep the title under 120 characters.");
    }
    if event.channel_id.is_empty() {
        problems.push("Pick a channel.");
    }
    if draft.options.with_untracked(|options| options.is_empty()) {
        problems.push("Add at least one attendance type.");
    }
    if !(15..=1440).contains(&event.duration_minutes) {
        problems.push("An event lasts between 15 minutes and 24 hours.");
    }
    if event.cutoff_at.is_some_and(|cutoff| cutoff > event.starts_at) {
        problems.push("Signups have to close before the event starts.");
    }
    if let Some(at) = post_at {
        if at <= format::now() {
            problems.push("Pick a post time that hasn't passed.");
        } else if at >= event.starts_at {
            problems.push("Post the event before it starts.");
        }
    }
    problems
}

/// Asks before leaving a page with unsaved work: an app link opens a dialog here,
/// and closing or reloading the tab gets the browser's own warning.
#[component]
pub fn LeaveGuard(dirty: RwSignal<bool>) -> impl IntoView {
    let pending = RwSignal::new(None::<String>);
    let go = client::use_go();
    watch_leaving(dirty, pending);
    let stay = move || pending.set(None);
    let leave = move || {
        if let Some(to) = pending.get_untracked() {
            dirty.set(false);
            pending.set(None);
            go(&to);
        }
    };
    let keep = NodeRef::<leptos::html::Button>::new();
    Effect::new(move |_| {
        if let Some(button) = keep.get() {
            let _ = button.focus();
        }
    });
    move || {
        pending.get().is_some().then(|| {
            let leave = leave.clone();
            view! {
                <div class="modal-backdrop" on:click=move |_| stay()>
                    <div class="modal" role="alertdialog" aria-modal="true" aria-labelledby="leave-title"
                        on:click=|ev| ev.stop_propagation()
                        on:keydown=move |ev: leptos::ev::KeyboardEvent| if escape(&ev) { stay(); }>
                        <h2 id="leave-title">"Leave without saving?"</h2>
                        <p>"This event hasn't been saved. If you leave, what you set up here is lost."</p>
                        <div class="inline-actions">
                            <button type="button" class="primary" node_ref=keep on:click=move |_| stay()>"Keep editing"</button>
                            <button type="button" class="danger" on:click=move |_| leave()>"Leave"</button>
                        </div>
                    </div>
                </div>
            }
        })
    }
}

/// Catches app links before the router does, and the tab closing.
#[cfg(feature = "hydrate")]
fn watch_leaving(dirty: RwSignal<bool>, pending: RwSignal<Option<String>>) {
    use send_wrapper::SendWrapper;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;
    let on_click = Closure::<dyn FnMut(web_sys::MouseEvent)>::new(move |ev: web_sys::MouseEvent| {
        if !dirty.get_untracked()
            || ev.default_prevented()
            || ev.button() != 0
            || ev.meta_key()
            || ev.ctrl_key()
            || ev.shift_key()
            || ev.alt_key()
        {
            return;
        }
        let Some(element) = ev.target().and_then(|target| target.dyn_into::<web_sys::Element>().ok()) else {
            return;
        };
        let Ok(Some(link)) = element.closest("a[href]") else {
            return;
        };
        if link.get_attribute("target").is_some_and(|target| target == "_blank")
            || link.has_attribute("download")
            || link.get_attribute("rel").is_some_and(|rel| rel.contains("external"))
        {
            return;
        }
        let Ok(link) = link.dyn_into::<web_sys::HtmlAnchorElement>() else {
            return;
        };
        let here = leptos::prelude::window().location();
        if link.origin() != here.origin().unwrap_or_default() {
            return;
        }
        let to = format!("{}{}", link.pathname(), link.search());
        if to == format!("{}{}", here.pathname().unwrap_or_default(), here.search().unwrap_or_default()) {
            return;
        }
        ev.prevent_default();
        ev.stop_immediate_propagation();
        pending.set(Some(to));
    })
    .into_js_value();
    let on_unload = Closure::<dyn FnMut(web_sys::BeforeUnloadEvent)>::new(move |ev: web_sys::BeforeUnloadEvent| {
        if dirty.get_untracked() {
            ev.prevent_default();
            ev.set_return_value("");
        }
    })
    .into_js_value();
    let document = leptos::prelude::document();
    let window = leptos::prelude::window();
    let _ = document.add_event_listener_with_callback_and_bool("click", on_click.unchecked_ref(), true);
    let _ = window.add_event_listener_with_callback("beforeunload", on_unload.unchecked_ref());
    let handles = SendWrapper::new((on_click, on_unload));
    on_cleanup(move || {
        let (on_click, on_unload) = handles.take();
        let _ = leptos::prelude::document()
            .remove_event_listener_with_callback_and_bool("click", on_click.unchecked_ref(), true);
        let _ = leptos::prelude::window().remove_event_listener_with_callback("beforeunload", on_unload.unchecked_ref());
    });
}

#[cfg(not(feature = "hydrate"))]
fn watch_leaving(_: RwSignal<bool>, _: RwSignal<Option<String>>) {}

/// Save as draft, Schedule post, and Post now for an event that only lives in the
/// browser so far. Each one saves it, then opens its page.
fn new_event_buttons(key: (String, String), draft: LocalDraft, clock: Clock) -> impl IntoView {
    let sender = Sender::new(key);
    let go = client::use_go();
    let open = RwSignal::new(false);
    let now = format::now();
    let starts = draft.event.with_untracked(|event| event.starts_at);
    let first = {
        let day_before = starts - Duration::days(1);
        if day_before > now { day_before } else { now + Duration::hours(1) }
    };
    let initial = StoredValue::new(clock.input(first));
    let when = RwSignal::new(initial.get_value());
    let zone = clock.label(now);
    let field = NodeRef::<leptos::html::Input>::new();
    focus_when_shown(field);
    let problems = RwSignal::new(Vec::<&'static str>::new());
    // Creating is one request; its `pending` keeps the buttons from sending twice.
    let saving = client::action(move |(body, said): &(dto::EventDraft, &'static str)| {
        let (body, said) = (body.clone(), *said);
        let (server, _) = sender.key.get_value();
        let url = api::path::events(&server);
        let toasts = client::toast_list();
        let go = go.clone();
        async move {
            match api::send::<dto::Done, _>("POST", &url, Some(&body)).await {
                Ok(done) => {
                    if let Some(list) = toasts {
                        client::push_toast(list, said);
                    }
                    // Saved, so leaving is fine now.
                    draft.dirty.set(false);
                    if let Some(to) = done.location {
                        go(&to);
                    }
                }
                Err(failure) => sender.problem.set(failure.message),
            }
        }
    });
    let create = move |action: dto::SaveAction, post_local: Option<chrono::NaiveDateTime>, said: &'static str| {
        let post_at = post_local
            .and_then(|local| clock.tz.from_local_datetime(&local).earliest())
            .map(|at| at.with_timezone(&Utc));
        let found = unsaved_problems(draft, post_at);
        problems.set(found.clone());
        if !found.is_empty() {
            return;
        }
        sender.problem.set(String::new());
        saving.dispatch((new_event_draft(draft, action, post_local), said));
    };
    let cancel = move || {
        when.set(initial.get_value());
        sender.problem.set(String::new());
        open.set(false);
    };
    let schedule = {
        let create = create.clone();
        move || {
            let Some(local) = parse_local(&when.get_untracked()) else {
                return sender.fail("Pick a date and time to post.");
            };
            create(dto::SaveAction::Schedule, Some(local), "Scheduled.");
            if problems.with_untracked(|list| list.is_empty()) {
                open.set(false);
            }
        }
    };
    let save_draft = {
        let create = create.clone();
        move |_| create(dto::SaveAction::Draft, None, "Saved as a draft.")
    };
    let post_now = move |_| create(dto::SaveAction::Publish, None, "Posted to Discord.");
    let schedule = StoredValue::new(schedule);
    view! {
        <div class="inline-anchor post-buttons">
            <button type="button" class="ghost" disabled=move || saving.pending().get() on:click=save_draft>"Save as draft"</button>
            <button type="button" class="ghost" disabled=move || saving.pending().get()
                aria-expanded=move || if open.get() { "true" } else { "false" }
                on:click=move |_| if open.get_untracked() { cancel() } else { open.set(true) }>
                "Schedule post"
            </button>
            <button type="button" class="primary" disabled=move || saving.pending().get() on:click=post_now>"Post now"</button>
            {move || open.get().then(|| view! {
                <form class="popover" data-editor=""
                    on:submit=move |ev| { ev.prevent_default(); schedule.with_value(|schedule| schedule()); }
                    on:keydown=move |ev| if escape(&ev) { cancel(); }>
                    <label class="field">"Post to Discord at"
                        <input node_ref=field type="datetime-local" required
                            prop:value=move || when.get() on:input=move |ev| when.set(event_target_value(&ev))/>
                    </label>
                    <p class="popover-note">{zone.clone()}</p>
                    {sender.error()}
                    {sender.actions(cancel)}
                </form>
            })}
            {move || (!open.get()).then(|| sender.error())}
            {move || {
                let list = problems.get();
                (!list.is_empty()).then(|| view! {
                    <ul class="error save-problems" role="alert">
                        {list.into_iter().map(|problem| view! { <li>{problem}</li> }).collect_view()}
                    </ul>
                })
            }}
        </div>
    }
}

/// Where the event happens. People who can edit see a placeholder when there is none.
#[component]
pub fn EventLocation(key: (String, String), event: dto::Event) -> impl IntoView {
    let shown = StoredValue::new(event.location.clone());
    if !editable(&event) {
        return (!event.location.is_empty())
            .then(|| view! { <p class="event-place">"📍 "{event.location}</p> })
            .into_any();
    }
    let sender = Sender::new(key);
    let open = RwSignal::new(false);
    let text = RwSignal::new(event.location);
    let field = NodeRef::<leptos::html::Input>::new();
    focus_when_shown(field);
    let cancel = move || {
        text.set(shown.get_value());
        sender.problem.set(String::new());
        open.set(false);
    };
    let save = move || {
        let next = text.get_untracked().trim().to_string();
        if next == shown.get_value() {
            open.set(false);
            return;
        }
        sender.send(
            dto::EventPatch {
                location: Some(next),
                ..Default::default()
            },
            move || open.set(false),
            || (),
        );
    };
    view! {
        {move || if open.get() {
            view! {
                <form class="inline-form place-form" data-editor=""
                    on:submit=move |ev| { ev.prevent_default(); save(); }
                    on:keydown=move |ev| if escape(&ev) { cancel(); }>
                    <span aria-hidden="true">"📍"</span>
                    <input node_ref=field maxlength="200" aria-label="Location" placeholder="Where it happens"
                        prop:value=move || text.get() on:input=move |ev| text.set(event_target_value(&ev))/>
                    {sender.actions(cancel)}
                </form>
            }
            .into_any()
        } else {
            let place = shown.get_value();
            view! {
                <p class="event-place">
                    <button type="button" class="inline-edit" title="Change the location" on:click=move |_| open.set(true)>
                        {if place.is_empty() {
                            view! { <span class="muted">"📍 Add a location"</span> }.into_any()
                        } else {
                            view! { <span>"📍 "{place}</span> }.into_any()
                        }}
                        {pencil()}
                    </button>
                </p>
            }
            .into_any()
        }}
        {sender.error()}
    }
    .into_any()
}

/// The date and time lines, and a form for the start and the length.
#[component]
pub fn EventWhen(key: (String, String), event: dto::Event, clock: Clock) -> impl IntoView {
    let date = format::format_long_date(clock.day(event.starts_at));
    let time = format::time_line(&clock, event.starts_at, event.duration_minutes);
    if !editable(&event) {
        return view! {
            <p class="event-date">"📅 "{date}</p>
            <p class="event-time">"🕐 "{time}</p>
        }
        .into_any();
    }
    let sender = Sender::new(key).in_zone(clock);
    let open = RwSignal::new(false);
    let initial = (
        clock.input(event.starts_at),
        event.duration_minutes,
        clock.input(event.starts_at + Duration::minutes(event.duration_minutes)),
    );
    let start = RwSignal::new(initial.0.clone());
    let minutes = RwSignal::new(initial.1);
    let end = RwSignal::new(initial.2.clone());
    let by_end = RwSignal::new(false);
    let initial = StoredValue::new(initial);
    let zone = clock.label(format::now());
    let field = NodeRef::<leptos::html::Input>::new();
    focus_when_shown(field);

    let length = move || -> Result<i64, &'static str> {
        let begin = parse_local(&start.get()).ok_or("Pick a start date and time.")?;
        let mins = if by_end.get() {
            let finish = parse_local(&end.get()).ok_or("Pick an end date and time.")?;
            (finish - begin).num_minutes()
        } else {
            minutes.get()
        };
        if (15..=1440).contains(&mins) {
            Ok(mins)
        } else {
            Err("An event lasts between 15 minutes and 24 hours.")
        }
    };
    let ends_at = move || {
        let begin = parse_local(&start.get())?;
        let mins = length().ok()?;
        let finish = clock.tz.from_local_datetime(&(begin + Duration::minutes(mins))).earliest()?;
        Some(format!("ends at {}", clock.clock(finish.with_timezone(&Utc))))
    };
    let cancel = move || {
        let (first, mins, last) = initial.get_value();
        start.set(first);
        minutes.set(mins);
        end.set(last);
        by_end.set(false);
        sender.problem.set(String::new());
        open.set(false);
    };
    let save = move || {
        let Some(local) = parse_local(&start.get_untracked()) else {
            return sender.fail("Pick a start date and time.");
        };
        let mins = match length() {
            Ok(mins) => mins,
            Err(message) => return sender.fail(message),
        };
        sender.send(
            dto::EventPatch {
                starts_local: Some(local),
                duration_minutes: Some(mins),
                ..Default::default()
            },
            move || open.set(false),
            || (),
        );
    };
    let durations = move || {
        let now = minutes.get_untracked();
        let mut list = DURATIONS.to_vec();
        if !list.contains(&now) {
            list.push(now);
            list.sort_unstable();
        }
        list.into_iter()
            .map(|mins| view! { <option value=mins selected=mins == now>{human_duration(mins)}</option> })
            .collect_view()
    };

    view! {
        <div class="inline-anchor">
            <button type="button" class="inline-edit when-edit" title="Change the time"
                aria-expanded=move || if open.get() { "true" } else { "false" }
                on:click=move |_| if open.get_untracked() { cancel() } else { open.set(true) }>
                <span class="when-lines">
                    <span class="event-date">"📅 "{date}</span>
                    <span class="event-time">"🕐 "{time}</span>
                </span>
                {pencil()}
            </button>
            {move || open.get().then(|| view! {
                <form class="popover" data-editor=""
                    on:submit=move |ev| { ev.prevent_default(); save(); }
                    on:keydown=move |ev| if escape(&ev) { cancel(); }>
                    <label class="field">"Start time"
                        <input node_ref=field type="datetime-local" required
                            prop:value=move || start.get() on:input=move |ev| start.set(event_target_value(&ev))/>
                    </label>
                    <p class="popover-note">{zone.clone()}</p>
                    {move || if by_end.get() {
                        view! {
                            <label class="field">"End time"
                                <input type="datetime-local" required
                                    prop:value=move || end.get() on:input=move |ev| end.set(event_target_value(&ev))/>
                            </label>
                            <p class="popover-row">
                                <span>"or "<button type="button" class="link" on:click=move |_| by_end.set(false)>"choose a duration"</button></span>
                            </p>
                        }
                        .into_any()
                    } else {
                        view! {
                            <label class="field">"Duration"
                                <select on:change=move |ev| minutes.set(event_target_value(&ev).parse().unwrap_or(60))>{durations}</select>
                            </label>
                            <p class="popover-row">
                                <span>"or "<button type="button" class="link" on:click=move |_| {
                                    if let Some(begin) = parse_local(&start.get_untracked()) {
                                        end.set((begin + Duration::minutes(minutes.get_untracked())).format("%Y-%m-%dT%H:%M").to_string());
                                    }
                                    by_end.set(true);
                                }>"choose an end time"</button></span>
                                <span class="muted">{ends_at}</span>
                            </p>
                        }
                        .into_any()
                    }}
                    {sender.error()}
                    {sender.actions(cancel)}
                </form>
            })}
        </div>
    }
    .into_any()
}

#[component]
pub fn EventDescription(key: (String, String), event: dto::Event) -> impl IntoView {
    let can_edit = editable(&event);
    let shown = StoredValue::new(event.description.clone());
    let body = move || {
        let text = shown.get_value();
        if text.is_empty() {
            view! { <p class="muted">{if can_edit { "Add a description" } else { "No description" }}</p> }.into_any()
        } else {
            crate::md(&text).into_any()
        }
    };
    if !editable(&event) {
        return view! { <section class="event-desc">{body()}</section> }.into_any();
    }
    let sender = Sender::new(key);
    let open = RwSignal::new(false);
    let text = RwSignal::new(event.description);
    let field = NodeRef::<leptos::html::Textarea>::new();
    focus_when_shown(field);
    let cancel = move || {
        text.set(shown.get_value());
        sender.problem.set(String::new());
        open.set(false);
    };
    let save = move || {
        sender.send(
            dto::EventPatch {
                description: Some(text.get_untracked()),
                ..Default::default()
            },
            move || open.set(false),
            || (),
        );
    };
    view! {
        <section
            class=move || if open.get() { "event-desc editable-desc open" } else { "event-desc editable-desc" }
            on:click=move |ev| {
                // The whole box opens the editor, except a link in the text and
                // the editor's own buttons, which may be gone by the time this runs.
                if !open.get_untracked() && !clicked_inside(&ev, "a, form") {
                    open.set(true);
                }
            }>
            {move || if open.get() {
                view! {
                    <form class="inline-form desc-form" data-editor=""
                        on:submit=move |ev| { ev.prevent_default(); save(); }
                        on:keydown=move |ev| if escape(&ev) { cancel(); }>
                        <textarea node_ref=field rows="6" maxlength="2000" aria-label="Description"
                            prop:value=move || text.get() on:input=move |ev| text.set(event_target_value(&ev))></textarea>
                        <p class="meta">"Discord formatting works: **bold**, *italic*, __underline__, ~~strike~~, `code`, lists, and links."</p>
                        {sender.error()}
                        {sender.actions(cancel)}
                    </form>
                }
                .into_any()
            } else {
                view! {
                    <button type="button" class="icon-btn desc-edit" title="Change the description"
                        aria-label="Change the description">{pencil()}</button>
                    {body()}
                }
                .into_any()
            }}
        </section>
    }
    .into_any()
}

/// Whether a click landed inside an element matching `selector`.
#[cfg(feature = "hydrate")]
pub(super) fn clicked_inside(ev: &leptos::ev::MouseEvent, selector: &str) -> bool {
    use wasm_bindgen::JsCast;
    ev.target()
        .and_then(|target| target.dyn_into::<leptos::web_sys::Element>().ok())
        .and_then(|element| element.closest(selector).ok().flatten())
        .is_some()
}

#[cfg(not(feature = "hydrate"))]
pub(super) fn clicked_inside(_: &leptos::ev::MouseEvent, _: &str) -> bool {
    false
}

/// `#1E3A6E` or `1e3a6e` as `#1e3a6e`; anything else is not a color.
fn hex(text: &str) -> Option<String> {
    let digits = text.trim().trim_start_matches('#');
    (digits.len() == 6 && digits.chars().all(|ch| ch.is_ascii_hexdigit()))
        .then(|| format!("#{}", digits.to_ascii_lowercase()))
}

#[component]
pub fn EventColor(key: (String, String), event: dto::Event) -> impl IntoView {
    let color = event.color.clone();
    let face = {
        let color = color.clone();
        move || view! {
            <span class="color-face"><i class="dot" style=format!("background: {color}")></i>{color_name(&color)}</span>
        }
    };
    if !editable(&event) {
        return view! { <p>{face()}</p> }.into_any();
    }
    let sender = Sender::new(key);
    let open = RwSignal::new(false);
    let chosen = RwSignal::new(color.clone());
    let typed = RwSignal::new(color.clone());
    let shown = StoredValue::new(color);
    let cancel = move || {
        chosen.set(shown.get_value());
        typed.set(shown.get_value());
        sender.problem.set(String::new());
        open.set(false);
    };
    let save = move || {
        let Some(next) = hex(&typed.get_untracked()) else {
            return sender.fail("Use a color like #1e3a6e.");
        };
        if next == shown.get_value() {
            open.set(false);
            return;
        }
        sender.send(
            dto::EventPatch {
                color: Some(next),
                ..Default::default()
            },
            move || open.set(false),
            || (),
        );
    };
    let swatches = move || {
        PALETTE
            .iter()
            .map(|(value, label)| {
                let value = value.to_string();
                let pick = value.clone();
                let mine = value.clone();
                view! {
                    <button type="button" title=*label aria-label=*label
                        class=move || if chosen.get() == mine { "swatch on" } else { "swatch" }
                        style=format!("background: {value}")
                        on:click=move |_| {
                            chosen.set(pick.clone());
                            typed.set(pick.clone());
                        }></button>
                }
            })
            .collect_view()
    };
    view! {
        <div class="inline-anchor">
            <button type="button" class="inline-edit" title="Change the color"
                on:click=move |_| if open.get_untracked() { cancel() } else { open.set(true) }>
                {face}{pencil()}
            </button>
            {move || open.get().then(|| view! {
                <form class="popover" data-editor=""
                    on:submit=move |ev| { ev.prevent_default(); save(); }
                    on:keydown=move |ev| if escape(&ev) { cancel(); }>
                    <p class="popover-title">"Color"</p>
                    <div class="swatches">{swatches}</div>
                    <div class="hex-row">
                        <span class="swatch-preview" style=move || format!("background: {}", chosen.get())></span>
                        <input class="hex-input" maxlength="7" aria-label="Color code" spellcheck="false"
                            prop:value=move || typed.get()
                            on:input=move |ev| {
                                let text = event_target_value(&ev);
                                if let Some(color) = hex(&text) {
                                    chosen.set(color);
                                }
                                typed.set(text);
                            }/>
                    </div>
                    {sender.error()}
                    {sender.actions(cancel)}
                </form>
            })}
        </div>
    }
    .into_any()
}

/// A yes/no detail that saves the moment it is flipped.
fn patch_switch(
    key: (String, String),
    event: &dto::Event,
    start: bool,
    label: &'static str,
    patch: fn(bool) -> dto::EventPatch,
) -> AnyView {
    let on = RwSignal::new(start);
    let word = move || if on.get() { "Yes" } else { "No" };
    if !editable(event) {
        return view! { <p>{word}</p> }.into_any();
    }
    let sender = Sender::new(key);
    let flip = move |_| {
        // A second flip waits for the first to land; the switch stays bright meanwhile.
        if sender.busy().get_untracked() {
            return;
        }
        let next = !on.get_untracked();
        on.set(next);
        sender.send(patch(next), || (), move || {
            on.try_set(!next);
        });
    };
    view! {
        <div class="toggle-row">
            <button type="button" role="switch" aria-label=label
                class=move || if on.get() { "switch on" } else { "switch" }
                aria-checked=move || if on.get() { "true" } else { "false" }
                aria-busy=move || if sender.busy().get() { "true" } else { "false" }
                on:click=flip>
                <span></span>
            </button>
            <span>{word}</span>
        </div>
        {sender.error()}
    }
    .into_any()
}

#[component]
pub fn MultipleToggle(key: (String, String), event: dto::Event) -> impl IntoView {
    patch_switch(key, &event, event.multiple_signups, "Multiple responses", |on| dto::EventPatch {
        multiple_signups: Some(on),
        ..Default::default()
    })
}

#[component]
pub fn ThreadToggle(key: (String, String), event: dto::Event) -> impl IntoView {
    patch_switch(key, &event, event.thread_enabled, "Event thread", |on| dto::EventPatch {
        thread_enabled: Some(on),
        ..Default::default()
    })
}

/// The channel: a link to the post once it is out, and a choice before then.
#[component]
pub fn EventChannel(key: (String, String), event: dto::Event, channels: Vec<dto::Channel>) -> impl IntoView {
    let name = format!("#{}", event.channel);
    let unposted = matches!(event.status.as_str(), "draft" | "scheduled" | "failed") && event.discord_url.is_none();
    if !editable(&event) || !unposted {
        return match event.discord_url.clone() {
            Some(url) => view! { <p><a href=url title="Open in Discord">{name}</a></p> }.into_any(),
            None => view! { <p>{name}</p> }.into_any(),
        };
    }
    let sender = Sender::new(key);
    let open = RwSignal::new(false);
    // Stored so the closures below stay `Copy`.
    let current = StoredValue::new(event.channel_id.clone());
    let picked = RwSignal::new(current.get_value());
    let channels = StoredValue::new(channels);
    let cancel = move || {
        picked.set(current.get_value());
        sender.problem.set(String::new());
        open.set(false);
    };
    let save = move || {
        let next = picked.get_untracked();
        if next == current.get_value() {
            open.set(false);
            return;
        }
        sender.send(
            dto::EventPatch {
                channel_id: Some(next),
                ..Default::default()
            },
            move || open.set(false),
            || (),
        );
    };
    let choices = move || {
        channels.with_value(|channels| {
            channels
                .iter()
                .map(|channel| view! { <option value=channel.id.clone() selected=current.with_value(|now| &channel.id == now)>"#"{channel.name.clone()}</option> })
                .collect_view()
        })
    };
    view! {
        <div class="inline-anchor">
            <button type="button" class="inline-edit" title="Change the channel"
                on:click=move |_| if open.get_untracked() { cancel() } else { open.set(true) }>
                {name}{pencil()}
            </button>
            {move || open.get().then(|| view! {
                <form class="popover" data-editor=""
                    on:submit=move |ev| { ev.prevent_default(); save(); }
                    on:keydown=move |ev| if escape(&ev) { cancel(); }>
                    <label class="field">"Channel"
                        <select on:change=move |ev| picked.set(event_target_value(&ev))>{choices}</select>
                    </label>
                    <p class="popover-note left">"The channel is set once the event is posted."</p>
                    {sender.error()}
                    {sender.actions(cancel)}
                </form>
            })}
        </div>
    }
    .into_any()
}

/// Copies this date's details to the upcoming dates of its series.
#[component]
pub fn ApplyToSeries(key: (String, String), event: dto::Event) -> impl IntoView {
    if !editable(&event) {
        return ().into_any();
    }
    let sender = Sender::new(key);
    let asking = RwSignal::new(false);
    let apply = move || {
        asking.set(false);
        sender.send(
            dto::EventPatch {
                apply_series: true,
                ..Default::default()
            },
            || {
                client::toast("Upcoming dates now match this one.");
            },
            || (),
        );
    };
    view! {
        {move || if asking.get() {
            view! {
                <super::InlineConfirm
                    question="Copy this date's title, description, options, reminders, and roles to the upcoming dates?"
                    yes="Copy to upcoming dates"
                    class="primary"
                    on_yes=Callback::new(move |_| apply())
                    on_no=Callback::new(move |_| asking.set(false))/>
            }
            .into_any()
        } else {
            view! {
                <button type="button" class="link" disabled=move || sender.busy().get() on:click=move |_| asking.set(true)>
                    "Use these details for upcoming dates."
                </button>
            }
            .into_any()
        }}
        {sender.error()}
    }
    .into_any()
}

fn cutoff_label(minutes: i64) -> String {
    if minutes == 0 {
        "When it starts".into()
    } else {
        format!("{} before", human_offset(minutes))
    }
}

#[component]
pub fn EventCutoff(key: (String, String), event: dto::Event, clock: Clock) -> impl IntoView {
    let template = for_template();
    let face = if event.signups_closed {
        "Closed".to_string()
    } else if template {
        // A routine's cutoff is how long before each event, not a date.
        event
            .cutoff_at
            .map(|cutoff| cutoff_label((event.starts_at - cutoff).num_minutes().max(0)))
            .unwrap_or_else(|| "Never".into())
    } else {
        event
            .cutoff_at
            .map(|dt| clock.when(dt))
            .unwrap_or_else(|| "Never".into())
    };
    if !editable(&event) {
        return view! { <p>{face}</p> }.into_any();
    }
    let sender = Sender::new(key);
    let open = RwSignal::new(false);
    let current = event
        .cutoff_at
        .map(|cutoff| (event.starts_at - cutoff).num_minutes().max(0));
    let picked = RwSignal::new(current);
    let cancel = move || {
        picked.set(current);
        sender.problem.set(String::new());
        open.set(false);
    };
    let save = move || {
        let next = picked.get_untracked();
        if next == current {
            open.set(false);
            return;
        }
        let cutoff = match next {
            None => dto::Cutoff::Never,
            Some(minutes) => dto::Cutoff::Before { minutes },
        };
        sender.send(
            dto::EventPatch {
                cutoff: Some(cutoff),
                ..Default::default()
            },
            move || open.set(false),
            || (),
        );
    };
    let choices = move || {
        let now = picked.get_untracked();
        let mut list = CUTOFFS.to_vec();
        if let Some(mins) = current.filter(|mins| !list.contains(mins)) {
            list.push(mins);
            list.sort_unstable();
        }
        let never = view! { <option value="" selected=now.is_none()>"Never"</option> };
        let rest = list
            .into_iter()
            .map(|mins| view! { <option value=mins selected=now == Some(mins)>{cutoff_label(mins)}</option> })
            .collect_view();
        view! { {never}{rest} }
    };
    view! {
        <div class="inline-anchor">
            <button type="button" class="inline-edit" title="Change when signups close"
                on:click=move |_| if open.get_untracked() { cancel() } else { open.set(true) }>
                {face}{pencil()}
            </button>
            {move || open.get().then(|| view! {
                <form class="popover" data-editor=""
                    on:submit=move |ev| { ev.prevent_default(); save(); }
                    on:keydown=move |ev| if escape(&ev) { cancel(); }>
                    <label class="field">"Signups close"
                        <select on:change=move |ev| picked.set(event_target_value(&ev).parse().ok())>{choices}</select>
                    </label>
                    {sender.error()}
                    {sender.actions(cancel)}
                </form>
            })}
        </div>
    }
    .into_any()
}

/// A reminder offset as an amount and a unit, the largest that fits evenly.
fn split_offset(minutes: i64) -> (String, i64) {
    for unit in [1440, 60] {
        if minutes >= unit && minutes % unit == 0 {
            return ((minutes / unit).to_string(), unit);
        }
    }
    (minutes.to_string(), 1)
}

#[derive(Clone)]
struct ReminderRow {
    n: usize,
    amount: RwSignal<String>,
    unit: RwSignal<i64>,
    message: RwSignal<String>,
}

fn reminder_rows(list: &[dto::Reminder]) -> Vec<ReminderRow> {
    list.iter()
        .enumerate()
        .map(|(n, rem)| {
            let (amount, unit) = split_offset(rem.offset_minutes);
            ReminderRow {
                n,
                amount: RwSignal::new(amount),
                unit: RwSignal::new(unit),
                message: RwSignal::new(rem.message.clone()),
            }
        })
        .collect()
}

#[component]
pub fn EventReminders(key: (String, String), event: dto::Event, reminders: Vec<dto::Reminder>) -> impl IntoView {
    let template = for_template();
    let face = {
        let reminders = reminders.clone();
        move || {
            if reminders.is_empty() {
                return view! { <span class="muted">"No reminders"</span> }.into_any();
            }
            reminders
                .iter()
                .map(|rem| {
                    let message = rem.message.clone();
                    view! {
                        <span class="face-line">
                            {format::reminder_offset(rem.offset_minutes)}
                            {(!template).then(|| view! { <span class="muted">{if rem.sent { " · Sent" } else { " · Waiting" }}</span> })}
                        </span>
                        {(!message.is_empty()).then(|| view! { <span class="face-line muted">{message}</span> })}
                    }
                })
                .collect_view()
                .into_any()
        }
    };
    if !editable(&event) {
        return view! { <p class="face">{face()}</p> }.into_any();
    }
    let sender = Sender::new(key);
    let open = RwSignal::new(false);
    let saved = StoredValue::new(reminders.clone());
    let rows = RwSignal::new(reminder_rows(&reminders));
    // Rows made from a click belong to the editor, not to the popover that is
    // redrawn or closed around the click, or their fields vanish with it.
    let owner = StoredValue::new(Owner::current().expect("a component owner"));
    let cancel = move || {
        rows.set(owner.with_value(|owner| owner.with(|| reminder_rows(&saved.get_value()))));
        sender.problem.set(String::new());
        open.set(false);
    };
    let add = move |_| {
        rows.update(|list| {
            if list.len() < MAX_REMINDERS {
                let n = list.iter().map(|row| row.n + 1).max().unwrap_or(0);
                list.push(owner.with_value(|owner| {
                    owner.with(|| ReminderRow {
                        n,
                        amount: RwSignal::new("1".into()),
                        unit: RwSignal::new(60),
                        message: RwSignal::new(String::new()),
                    })
                }));
            }
        })
    };
    let save = move || {
        let mut list = Vec::new();
        for row in rows.get_untracked() {
            let Ok(amount) = row.amount.get_untracked().trim().parse::<i64>() else {
                return sender.fail("Each reminder needs a number.");
            };
            let offset_minutes = amount * row.unit.get_untracked();
            if !(1..=10_080).contains(&offset_minutes) {
                return sender.fail("Reminders go out between 1 minute and 7 days before.");
            }
            list.push(dto::Reminder {
                offset_minutes,
                message: row.message.get_untracked().trim().to_string(),
                sent: false,
            });
        }
        sender.send(
            dto::EventPatch {
                reminders: Some(list),
                ..Default::default()
            },
            move || open.set(false),
            || (),
        );
    };
    view! {
        <div class="inline-anchor">
            <button type="button" class="inline-edit" title="Change the reminders"
                on:click=move |_| if open.get_untracked() { cancel() } else { open.set(true) }>
                <span class="face">{face}</span>{pencil()}
            </button>
            {move || open.get().then(|| view! {
                <form class="popover wide" data-editor=""
                    on:submit=move |ev| { ev.prevent_default(); save(); }
                    on:keydown=move |ev| if escape(&ev) { cancel(); }>
                    <p class="popover-title">"Reminders"</p>
                    <For each=move || rows.get() key=|row| row.n children=move |row| {
                        let n = row.n;
                        view! {
                            <div class="reminder-edit">
                                <input type="number" min="1" inputmode="numeric" aria-label="How long before"
                                    prop:value=move || row.amount.get() on:input=move |ev| row.amount.set(event_target_value(&ev))/>
                                <select aria-label="Unit" on:change=move |ev| row.unit.set(event_target_value(&ev).parse().unwrap_or(60))>
                                    <option value="1" selected=row.unit.get_untracked() == 1>"minutes before"</option>
                                    <option value="60" selected=row.unit.get_untracked() == 60>"hours before"</option>
                                    <option value="1440" selected=row.unit.get_untracked() == 1440>"days before"</option>
                                </select>
                                <button type="button" class="icon-btn" title="Remove this reminder" aria-label="Remove this reminder"
                                    on:click=move |_| rows.update(|list| list.retain(|other| other.n != n))>
                                    <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M19 6.41 17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z"/></svg>
                                </button>
                                <input class="reminder-note" maxlength="300" placeholder="Optional note, such as flasks and food" aria-label="Reminder note"
                                    prop:value=move || row.message.get() on:input=move |ev| row.message.set(event_target_value(&ev))/>
                            </div>
                        }
                    }/>
                    {move || rows.with(|list| list.len() < MAX_REMINDERS).then(|| view! {
                        <p class="popover-row"><button type="button" class="link" on:click=add>"Add a reminder"</button></p>
                    })}
                    {sender.error()}
                    {sender.actions(cancel)}
                </form>
            })}
        </div>
    }
    .into_any()
}

fn role_names(roles: &[dto::Role], ids: &[String]) -> Vec<String> {
    ids.iter()
        .filter_map(|id| roles.iter().find(|role| &role.id == id))
        .map(|role| role.name.clone())
        .collect()
}

/// Role names on one line, separated by commas, so a long list wraps instead of growing tall.
fn name_lines(names: Vec<String>, empty: &'static str) -> impl IntoView {
    if names.is_empty() {
        return view! { <span class="muted">{empty}</span> }.into_any();
    }
    view! { <span class="face-line">{names.join(", ")}</span> }.into_any()
}

/// Checkboxes for a set of roles.
fn role_checks(roles: StoredValue<Vec<dto::Role>>, picked: RwSignal<Vec<String>>) -> impl IntoView {
    move || {
        roles.with_value(|roles| {
            if roles.is_empty() {
                return view! { <p class="popover-note left">"This server has no roles Marian can see."</p> }.into_any();
            }
            roles
                .iter()
                .map(|role| {
                    let id = StoredValue::new(role.id.clone());
                    view! {
                        <label class="check-row">
                            <input type="checkbox" prop:checked=move || picked.with(|ids| id.with_value(|id| ids.contains(id)))
                                on:change=move |ev| picked.update(|ids| {
                                    let id = id.get_value();
                                    ids.retain(|other| *other != id);
                                    if event_target_checked(&ev) {
                                        ids.push(id);
                                    }
                                })/>
                            <span>{role.name.clone()}</span>
                        </label>
                    }
                })
                .collect_view()
                .into_any()
        })
    }
}

#[component]
pub fn EventMentions(key: (String, String), event: dto::Event, roles: Vec<dto::Role>) -> impl IntoView {
    let mut shown = role_names(&roles, &event.mention_role_ids);
    if event.mention_everyone {
        shown.insert(0, "@everyone".into());
    }
    if !editable(&event) {
        return view! { <p class="face">{name_lines(shown, "No mentions")}</p> }.into_any();
    }
    let sender = Sender::new(key);
    let open = RwSignal::new(false);
    let start = (event.mention_everyone, event.mention_role_ids.clone());
    let everyone = RwSignal::new(start.0);
    let picked = RwSignal::new(start.1.clone());
    let start = StoredValue::new(start);
    let roles = StoredValue::new(roles);
    let cancel = move || {
        let (all, ids) = start.get_value();
        everyone.set(all);
        picked.set(ids);
        sender.problem.set(String::new());
        open.set(false);
    };
    let save = move || {
        sender.send(
            dto::EventPatch {
                mention_everyone: Some(everyone.get_untracked()),
                mention_role_ids: Some(picked.get_untracked()),
                ..Default::default()
            },
            move || open.set(false),
            || (),
        );
    };
    let shown = StoredValue::new(shown);
    view! {
        <div class="inline-anchor">
            <button type="button" class="inline-edit" title="Change who is mentioned"
                on:click=move |_| if open.get_untracked() { cancel() } else { open.set(true) }>
                <span class="face">{move || name_lines(shown.get_value(), "No mentions")}</span>{pencil()}
            </button>
            {move || open.get().then(|| view! {
                <form class="popover" data-editor=""
                    on:submit=move |ev| { ev.prevent_default(); save(); }
                    on:keydown=move |ev| if escape(&ev) { cancel(); }>
                    <p class="popover-title">"Mentions"</p>
                    <p class="popover-note left">"The Discord post mentions what you check when it goes out."</p>
                    <div class="check-list">
                        <label class="check-row">
                            <input type="checkbox" prop:checked=move || everyone.get()
                                on:change=move |ev| everyone.set(event_target_checked(&ev))/>
                            <span>"@everyone"</span>
                        </label>
                        {role_checks(roles, picked)}
                    </div>
                    {sender.error()}
                    {sender.actions(cancel)}
                </form>
            })}
        </div>
    }
    .into_any()
}

#[component]
pub fn EventRestrictions(key: (String, String), event: dto::Event, roles: Vec<dto::Role>) -> impl IntoView {
    let shown = role_names(&roles, &event.restrict_role_ids);
    if !editable(&event) {
        return view! { <p class="face">{name_lines(shown, "No restrictions")}</p> }.into_any();
    }
    let sender = Sender::new(key);
    let open = RwSignal::new(false);
    let start = StoredValue::new(event.restrict_role_ids.clone());
    let picked = RwSignal::new(event.restrict_role_ids);
    let roles = StoredValue::new(roles);
    let cancel = move || {
        picked.set(start.get_value());
        sender.problem.set(String::new());
        open.set(false);
    };
    let save = move || {
        sender.send(
            dto::EventPatch {
                restrict_role_ids: Some(picked.get_untracked()),
                ..Default::default()
            },
            move || open.set(false),
            || (),
        );
    };
    let shown = StoredValue::new(shown);
    view! {
        <div class="inline-anchor">
            <button type="button" class="inline-edit" title="Change who can sign up"
                on:click=move |_| if open.get_untracked() { cancel() } else { open.set(true) }>
                <span class="face">{move || name_lines(shown.get_value(), "No restrictions")}</span>{pencil()}
            </button>
            {move || open.get().then(|| view! {
                <form class="popover" data-editor=""
                    on:submit=move |ev| { ev.prevent_default(); save(); }
                    on:keydown=move |ev| if escape(&ev) { cancel(); }>
                    <p class="popover-title">"Restrictions"</p>
                    <p class="popover-note left">"Only people with a checked role can sign up. None checked lets everyone in."</p>
                    <div class="check-list">{role_checks(roles, picked)}</div>
                    {sender.error()}
                    {sender.actions(cancel)}
                </form>
            })}
        </div>
    }
    .into_any()
}

#[component]
pub fn EventAttendeeRole(key: (String, String), event: dto::Event, roles: Vec<dto::Role>) -> impl IntoView {
    let name = event
        .attendee_role_id
        .as_ref()
        .and_then(|id| roles.iter().find(|role| &role.id == id))
        .map(|role| role.name.clone());
    // Stored so the closures below stay `Copy`.
    let current = StoredValue::new(event.attendee_role_id.clone());
    let face = {
        let name = name.clone();
        move || match name.clone() {
            Some(name) => view! {
                <span class="face-line">{name.clone()}</span>
                <span class="face-line muted">"Confirmed people on an attending option get "{name}"."</span>
            }
            .into_any(),
            None => view! { <span class="muted">"No attendee role"</span> }.into_any(),
        }
    };
    if !editable(&event) {
        return view! { <p class="face">{face()}</p> }.into_any();
    }
    let sender = Sender::new(key);
    let open = RwSignal::new(false);
    let picked = RwSignal::new(current.get_value());
    let cancel = move || {
        picked.set(current.get_value());
        sender.problem.set(String::new());
        open.set(false);
    };
    let save = move || {
        let next = picked.get_untracked();
        if next == current.get_value() {
            open.set(false);
            return;
        }
        sender.send(
            dto::EventPatch {
                attendee_role_id: Some(next),
                ..Default::default()
            },
            move || open.set(false),
            || (),
        );
    };
    let roles = StoredValue::new(roles);
    let choices = move || {
        roles.with_value(|roles| {
            roles
                .iter()
                .map(|role| view! { <option value=role.id.clone() selected=current.with_value(|now| now.as_ref() == Some(&role.id))>{role.name.clone()}</option> })
                .collect_view()
        })
    };
    view! {
        <div class="inline-anchor">
            <button type="button" class="inline-edit" title="Change the attendee role"
                on:click=move |_| if open.get_untracked() { cancel() } else { open.set(true) }>
                <span class="face">{face}</span>{pencil()}
            </button>
            {move || open.get().then(|| view! {
                <form class="popover" data-editor=""
                    on:submit=move |ev| { ev.prevent_default(); save(); }
                    on:keydown=move |ev| if escape(&ev) { cancel(); }>
                    <label class="field">"Attendee role"
                        <select on:change=move |ev| picked.set(Some(event_target_value(&ev)).filter(|id| !id.is_empty()))>
                            <option value="" selected=current.with_value(Option::is_none)>"None"</option>
                            {choices}
                        </select>
                    </label>
                    <p class="popover-note left">"People confirmed on an attending option get this role in Discord."</p>
                    {sender.error()}
                    {sender.actions(cancel)}
                </form>
            })}
        </div>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_color_code_reads_with_or_without_the_hash() {
        assert_eq!(hex("#1E3A6E").as_deref(), Some("#1e3a6e"));
        assert_eq!(hex(" 2f6b4f ").as_deref(), Some("#2f6b4f"));
        assert_eq!(hex("#12345"), None);
        assert_eq!(hex("#12345g"), None);
    }

    #[test]
    fn a_reminder_offset_uses_the_largest_even_unit() {
        assert_eq!(split_offset(15), ("15".into(), 1));
        assert_eq!(split_offset(120), ("2".into(), 60));
        assert_eq!(split_offset(2880), ("2".into(), 1440));
        assert_eq!(split_offset(90), ("90".into(), 1));
    }

    #[test]
    fn a_cutoff_reads_as_time_before_the_start() {
        assert_eq!(cutoff_label(0), "When it starts");
        assert_eq!(cutoff_label(60), "1 hour before");
        assert_eq!(cutoff_label(2880), "2 days before");
    }
}
