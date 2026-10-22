//! A routine's details changed in place, the way an event's are. A saved routine
//! sends each change at once; a new one keeps them until it is created.

use leptos::prelude::*;
use marian_types as dto;

use super::inline::{escape, focus_when_shown, pencil, LocalDraft};
use crate::format::{human_duration, weekday_name};
use crate::{api, cache, client};

const DURATIONS: [i64; 12] = [15, 30, 45, 60, 90, 120, 180, 240, 360, 480, 720, 1440];

/// The routine on this page and where its changes go.
#[derive(Clone, Copy)]
pub struct RoutineEdit {
    pub draft: RwSignal<dto::RoutineDraft>,
    /// The routine's address; none for a routine not created yet.
    pub api: StoredValue<Option<String>>,
    /// A new routine has something worth keeping.
    pub dirty: RwSignal<bool>,
    /// The routine being saved; its `pending` shows a save is on its way.
    pub saving: Action<dto::RoutineDraft, ()>,
    pub problem: RwSignal<String>,
    /// The event details the page edits with the event editors.
    pub local: StoredValue<Option<LocalDraft>>,
}

impl RoutineEdit {
    pub fn new(draft: dto::RoutineDraft, api: Option<String>) -> Self {
        let api = StoredValue::new(api);
        let problem = RwSignal::new(String::new());
        let server = cache::server_now();
        let saving = client::action(move |next: &dto::RoutineDraft| {
            let next = next.clone();
            let url = api.get_value();
            let queries = cache::client();
            let server = server.clone();
            async move {
                let Some(url) = url else {
                    return;
                };
                if let Err(failure) = api::send::<dto::Done, _>("PUT", &url, Some(&next)).await {
                    problem.try_set(failure.message);
                }
                cache::refresh_in(queries, &server);
            }
        });
        Self {
            draft: RwSignal::new(draft),
            api,
            dirty: RwSignal::new(false),
            saving,
            problem,
            local: StoredValue::new(None),
        }
    }

    /// The whole routine as the server takes it: wording, schedule, and event details.
    pub fn full(self) -> dto::RoutineDraft {
        let mut draft = self.draft.get_untracked();
        draft.event = self.local.get_value().map(routine_event);
        draft
    }

    /// Sends the whole routine. If the server says no, the reason shows and the
    /// page reloads what is really saved.
    pub fn push(self) {
        if self.api.with_value(Option::is_none) {
            self.dirty.set(true);
            return;
        }
        self.problem.set(String::new());
        self.saving.dispatch(self.full());
    }

    /// Applies a change at once. A saved routine sends it; if the server says no,
    /// the change is undone and the reason shows.
    fn change(self, apply: impl FnOnce(&mut dto::RoutineDraft)) {
        self.draft.update(apply);
        self.push();
    }

    pub fn error(self) -> impl IntoView {
        move || {
            let text = self.problem.get();
            (!text.is_empty()).then(|| view! { <p class="error">{text}</p> })
        }
    }
}

fn actions(cancel: impl Fn() + Copy + Send + Sync + 'static) -> impl IntoView {
    view! {
        <div class="inline-actions">
            <button type="submit" class="primary">"Save"</button>
            <button type="button" class="ghost" on:click=move |_| cancel()>"Cancel"</button>
        </div>
    }
}

fn weekdays(selected: u8) -> impl IntoView {
    (0..7u8)
        .map(|n| view! { <option value=n selected=n == selected>{weekday_name(n)}</option> })
        .collect_view()
}

/// The event title every poll's event gets.
#[component]
pub fn RoutineTitle(edit: RoutineEdit) -> impl IntoView {
    let open = RwSignal::new(false);
    let text = RwSignal::new(String::new());
    let fault = RwSignal::new(String::new());
    let field = NodeRef::<leptos::html::Input>::new();
    focus_when_shown(field);
    let start = move || {
        text.set(edit.draft.with_untracked(|draft| draft.title.clone()));
        fault.set(String::new());
        open.set(true);
    };
    let cancel = move || open.set(false);
    let save = move || {
        let next = text.get_untracked().trim().to_string();
        if next.is_empty() {
            return fault.set("Give the event a title.".into());
        }
        open.set(false);
        edit.change(|draft| draft.title = next);
    };
    view! {
        {move || if open.get() {
            view! {
                <form class="inline-form title-form"
                    on:submit=move |ev| { ev.prevent_default(); save(); }
                    on:keydown=move |ev| if escape(&ev) { cancel(); }>
                    <input node_ref=field class="title-input" maxlength="120" aria-label="Event title"
                        prop:value=move || text.get() on:input=move |ev| text.set(event_target_value(&ev))/>
                    {actions(cancel)}
                </form>
                {move || {
                    let text = fault.get();
                    (!text.is_empty()).then(|| view! { <p class="error inline-error">{text}</p> })
                }}
            }
            .into_any()
        } else {
            view! {
                <h1>
                    <button type="button" class="inline-edit" title="Change the event title" on:click=move |_| start()>
                        {move || {
                            let title = edit.draft.with(|draft| draft.title.clone());
                            if title.is_empty() {
                                view! { <span class="muted">"Name the event"</span> }.into_any()
                            } else {
                                view! { <span>{title}</span> }.into_any()
                            }
                        }}
                        {pencil()}
                    </button>
                </h1>
            }
            .into_any()
        }}
    }
}

/// The description each event starts with.
#[component]
pub fn RoutineDescription(edit: RoutineEdit) -> impl IntoView {
    let open = RwSignal::new(false);
    let text = RwSignal::new(String::new());
    let field = NodeRef::<leptos::html::Textarea>::new();
    focus_when_shown(field);
    let start = move || {
        text.set(edit.draft.with_untracked(|draft| draft.description.clone()));
        open.set(true);
    };
    let cancel = move || open.set(false);
    let save = move || {
        let next = text.get_untracked();
        open.set(false);
        edit.change(|draft| draft.description = next);
    };
    view! {
        <section class=move || if open.get() { "event-desc editable-desc open" } else { "event-desc editable-desc" }
            on:click=move |ev| {
                if !open.get_untracked() && !super::inline::clicked_inside(&ev, "a, form") {
                    start();
                }
            }>
            {move || if open.get() {
                view! {
                    <form class="inline-form desc-form"
                        on:submit=move |ev| { ev.prevent_default(); save(); }
                        on:keydown=move |ev| if escape(&ev) { cancel(); }>
                        <textarea node_ref=field rows="6" maxlength="2000" aria-label="Description"
                            prop:value=move || text.get() on:input=move |ev| text.set(event_target_value(&ev))></textarea>
                        <p class="meta">"Discord formatting works: **bold**, *italic*, __underline__, ~~strike~~, `code`, lists, and links."</p>
                        {actions(cancel)}
                    </form>
                }
                .into_any()
            } else {
                let body = edit.draft.with(|draft| draft.description.clone());
                view! {
                    <button type="button" class="icon-btn desc-edit" title="Change the description" aria-label="Change the description">{pencil()}</button>
                    {if body.is_empty() {
                        view! { <p class="muted">"Add a description"</p> }.into_any()
                    } else {
                        crate::md(&body).into_any()
                    }}
                }
                .into_any()
            }}
        </section>
    }
}

/// One sidebar detail: its face, and a small form in a popover.
fn fact(
    title: &'static str,
    face: impl Fn() -> String + Send + Sync + 'static,
    body: impl Fn() -> AnyView + Send + Sync + 'static,
    open: RwSignal<bool>,
    begin: impl Fn() + Copy + Send + Sync + 'static,
    save: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let cancel = move || open.set(false);
    view! {
        <div class="inline-anchor">
            <button type="button" class="inline-edit" title=title
                aria-expanded=move || if open.get() { "true" } else { "false" }
                on:click=move |_| if open.get_untracked() { cancel() } else { begin(); open.set(true) }>
                {face}{pencil()}
            </button>
            {move || open.get().then(|| view! {
                <form class="popover"
                    on:submit=move |ev| { ev.prevent_default(); save(); }
                    on:keydown=move |ev| if escape(&ev) { cancel(); }>
                    {body()}
                    {actions(cancel)}
                </form>
            })}
        </div>
    }
}

#[component]
pub fn RoutineChannel(edit: RoutineEdit, channels: Vec<dto::Channel>) -> impl IntoView {
    let channels = StoredValue::new(channels);
    let open = RwSignal::new(false);
    let picked = RwSignal::new(String::new());
    let face = move || {
        let id = edit.draft.with(|draft| draft.channel_id.clone());
        channels
            .with_value(|list| list.iter().find(|channel| channel.id == id).map(|channel| format!("#{}", channel.name)))
            .unwrap_or_else(|| "Pick a channel".into())
    };
    let body = move || {
        let now = picked.get_untracked();
        let options = channels.with_value(|list| {
            list.iter()
                .map(|channel| view! { <option value=channel.id.clone() selected=channel.id == now>"#"{channel.name.clone()}</option> })
                .collect_view()
        });
        view! {
            <label class="field">"Channel"
                <select on:change=move |ev| picked.set(event_target_value(&ev))>{options}</select>
            </label>
            <p class="popover-note left">"Polls and their events post here."</p>
        }
        .into_any()
    };
    let begin = move || picked.set(edit.draft.with_untracked(|draft| draft.channel_id.clone()));
    let save = move || {
        let next = picked.get_untracked();
        open.set(false);
        edit.change(|draft| draft.channel_id = next);
    };
    fact("Change the channel", face, body, open, begin, save)
}

/// When each event starts, and for how long.
#[component]
pub fn RoutineTime(edit: RoutineEdit) -> impl IntoView {
    let open = RwSignal::new(false);
    let start = RwSignal::new(String::new());
    let minutes = RwSignal::new(120_i64);
    let face = move || edit.draft.with(|draft| format!("{} · {}", draft.start_time, human_duration(draft.duration_minutes)));
    let body = move || {
        let now = minutes.get_untracked();
        let mut list = DURATIONS.to_vec();
        if !list.contains(&now) {
            list.push(now);
            list.sort_unstable();
        }
        let lengths = list
            .into_iter()
            .map(|mins| view! { <option value=mins selected=mins == now>{human_duration(mins)}</option> })
            .collect_view();
        view! {
            <label class="field">"Starts at"
                <input type="time" required prop:value=move || start.get() on:input=move |ev| start.set(event_target_value(&ev))/>
            </label>
            <label class="field">"Length"
                <select on:change=move |ev| minutes.set(event_target_value(&ev).parse().unwrap_or(120))>{lengths}</select>
            </label>
            <p class="popover-note left">"The winning day's event starts at this time, in the server's zone."</p>
        }
        .into_any()
    };
    let begin = move || {
        edit.draft.with_untracked(|draft| {
            start.set(draft.start_time.clone());
            minutes.set(draft.duration_minutes);
        })
    };
    let save = move || {
        let (at, mins) = (start.get_untracked(), minutes.get_untracked());
        open.set(false);
        edit.change(|draft| {
            draft.start_time = at;
            draft.duration_minutes = mins;
        });
    };
    fact("Change when the event starts", face, body, open, begin, save)
}

/// When the poll opens, or when it locks: a weekday and a time.
#[component]
pub fn PollMoment(edit: RoutineEdit, locks: bool) -> impl IntoView {
    let open = RwSignal::new(false);
    let day = RwSignal::new(0_u8);
    let time = RwSignal::new(String::new());
    let face = move || {
        edit.draft.with(|draft| {
            let (day, time) = if locks {
                (draft.lock_weekday, &draft.lock_time)
            } else {
                (draft.poll_weekday, &draft.poll_time)
            };
            format!("{} · {time}", weekday_name(day))
        })
    };
    let body = move || {
        view! {
            <div class="grid">
                <label class="field">"Day"
                    <select on:change=move |ev| day.set(event_target_value(&ev).parse().unwrap_or(0))>{weekdays(day.get_untracked())}</select>
                </label>
                <label class="field">"Time"
                    <input type="time" required prop:value=move || time.get() on:input=move |ev| time.set(event_target_value(&ev))/>
                </label>
            </div>
            <p class="popover-note left">{if locks {
                "Votes close then, and the day with the most votes becomes the event."
            } else {
                "Marian posts the poll in the channel then."
            }}</p>
        }
        .into_any()
    };
    let begin = move || {
        edit.draft.with_untracked(|draft| {
            if locks {
                day.set(draft.lock_weekday);
                time.set(draft.lock_time.clone());
            } else {
                day.set(draft.poll_weekday);
                time.set(draft.poll_time.clone());
            }
        })
    };
    let save = move || {
        let (d, t) = (day.get_untracked(), time.get_untracked());
        open.set(false);
        edit.change(|draft| {
            if locks {
                draft.lock_weekday = d;
                draft.lock_time = t;
            } else {
                draft.poll_weekday = d;
                draft.poll_time = t;
            }
        });
    };
    let title = if locks { "Change when the poll locks" } else { "Change when the poll opens" };
    fact(title, face, body, open, begin, save)
}

/// The weekdays people can vote for, as switches that save the moment they change.
#[component]
pub fn UsualDays(edit: RoutineEdit) -> impl IntoView {
    let chips = (0..7u8)
        .map(|n| {
            let on = move || edit.draft.with(|draft| draft.days.contains(&n));
            view! {
                <button type="button" class=move || if on() { "day-chip on" } else { "day-chip" }
                    aria-pressed=move || if on() { "true" } else { "false" }
                    on:click=move |_| edit.change(|draft| {
                        if let Some(at) = draft.days.iter().position(|day| *day == n) {
                            draft.days.remove(at);
                        } else {
                            draft.days.push(n);
                            draft.days.sort_unstable();
                        }
                    })>
                    {weekday_name(n)}
                </button>
            }
        })
        .collect_view();
    view! { <div class="day-chips">{chips}</div> }
}

/// One day on a poll: its date, its votes, and who cast them when known.
pub struct TallyRow {
    pub label: String,
    pub votes: i64,
    pub voters: Vec<String>,
}

/// Days and their votes as bars, the leader marked. A day nobody voted for
/// still shows, with an empty bar. A day with named voters opens to list them.
pub fn tally(rows: Vec<TallyRow>) -> impl IntoView {
    let most = rows.iter().map(|row| row.votes).max().unwrap_or(0);
    let rows = rows
        .into_iter()
        .map(|row| {
            let leads = most > 0 && row.votes == most;
            let width = if most > 0 { row.votes * 100 / most } else { 0 };
            let line = view! {
                <span class="tally-day">{row.label}</span>
                <span class="tally-track"><span class="tally-bar" style=format!("width: {width}%")></span></span>
                <span class="tally-votes">{row.votes}</span>
            };
            let body = if row.voters.is_empty() {
                view! { <div class="tally-line">{line}</div> }.into_any()
            } else {
                view! {
                    <details>
                        <summary class="tally-line" title="Show who voted">{line}</summary>
                        <p class="tally-voters">{row.voters.join(", ")}</p>
                    </details>
                }
                .into_any()
            };
            view! { <li class=if leads { "tally-row leads" } else { "tally-row" }>{body}</li> }
        })
        .collect_view();
    view! { <ol class="tally">{rows}</ol> }
}

/// This week's poll: its days with their votes, and a way to change which days
/// this one poll offers. A change saves at once and leaves the usual days alone.
/// `editing` belongs to the page: a saved change reloads the routine and draws
/// this again, and the days stay open for the next change.
#[component]
pub fn PollTally(run: dto::RunView, routine: String, editing: RwSignal<bool>) -> impl IntoView {
    let days = RwSignal::new(run.days.iter().filter(|day| day.on).map(|day| day.n).collect::<Vec<u8>>());
    let order = StoredValue::new(run.days.clone());
    let problem = RwSignal::new(String::new());
    let server = cache::server_now();
    let url = StoredValue::new(api::path::routine_days(&server, &routine));
    // Sends the poll's days; if the server says no, the days go back to `before`.
    let saving = client::action(move |(next, before): &(Vec<u8>, Vec<u8>)| {
        let body = dto::RoutineDays {
            days: next.clone(),
            scope: dto::DayScope::Run,
        };
        let before = before.clone();
        let url = url.get_value();
        let queries = cache::client();
        let server = server.clone();
        async move {
            match api::send::<dto::Done, _>("PUT", &url, Some(&body)).await {
                Ok(_) => cache::refresh_in(queries, &server),
                Err(failure) => {
                    days.try_set(before);
                    problem.try_set(failure.message);
                }
            }
        }
    });
    let toggle = move |n: u8| {
        let before = days.get_untracked();
        days.update(|list| {
            if let Some(at) = list.iter().position(|day| *day == n) {
                list.remove(at);
            } else {
                list.push(n);
            }
        });
        problem.set(String::new());
        saving.dispatch((days.get_untracked(), before));
    };
    let rows = move || {
        let on = days.get();
        let rows = order.with_value(|list| {
            list.iter()
                .filter(|day| on.contains(&day.n))
                .map(|day| TallyRow {
                    label: day.date.clone(),
                    votes: day.votes,
                    voters: day.voters.clone(),
                })
                .collect::<Vec<_>>()
        });
        if rows.is_empty() {
            view! { <p class="muted">"No days on this poll."</p> }.into_any()
        } else {
            tally(rows).into_any()
        }
    };
    let chips = move || {
        order.with_value(|list| {
            list.iter()
                .map(|day| {
                    let n = day.n;
                    let on = move || days.with(|list| list.contains(&n));
                    view! {
                        <button type="button" class=move || if on() { "day-chip on" } else { "day-chip" }
                            aria-pressed=move || if on() { "true" } else { "false" }
                            on:click=move |_| toggle(n)>
                            {day.date.clone()}
                        </button>
                    }
                })
                .collect_view()
        })
    };
    view! {
        {move || if editing.get() {
            view! {
                <div class="day-chips">{chips()}</div>
                <p class="settings-note">"Changes this poll only. Each change saves right away."</p>
            }
            .into_any()
        } else {
            rows().into_any()
        }}
        {move || {
            let text = problem.get();
            (!text.is_empty()).then(|| view! { <p class="error">{text}</p> })
        }}
        <button type="button" class="link-button poll-days-toggle" on:click=move |_| editing.update(|on| *on = !*on)>
            {move || if editing.get() { "Done" } else { "Change this poll's days" }}
        </button>
    }
}

/// The event details as the routine keeps them, from the page's draft event.
pub fn routine_event(local: LocalDraft) -> dto::RoutineEvent {
    let event = local.event.get_untracked();
    dto::RoutineEvent {
        location: event.location,
        color: event.color,
        thread_enabled: event.thread_enabled,
        multiple_signups: event.multiple_signups,
        mention_everyone: event.mention_everyone,
        attendee_role_id: event.attendee_role_id,
        mention_role_ids: event.mention_role_ids,
        restrict_role_ids: event.restrict_role_ids,
        cutoff_minutes: event
            .cutoff_at
            .map(|cutoff| (event.starts_at - cutoff).num_minutes().max(0)),
        options: local
            .options
            .get_untracked()
            .into_iter()
            .map(|opt| dto::SignupOption { id: None, ..opt })
            .collect(),
        reminders: local.reminders.get_untracked(),
    }
}

/// A stand-in event for the event editors to work on: a routine's details on
/// a date a week out. Only the details matter; the date is never shown.
pub fn template_event(draft: &dto::RoutineDraft, event: &dto::RoutineEvent, channel: String) -> dto::Event {
    let starts_at = crate::format::now() + chrono::Duration::days(7);
    dto::Event {
        id: "routine".into(),
        title: draft.title.clone(),
        description: draft.description.clone(),
        location: event.location.clone(),
        status: "draft".into(),
        starts_at,
        duration_minutes: draft.duration_minutes,
        cutoff_at: event.cutoff_minutes.map(|mins| starts_at - chrono::Duration::minutes(mins)),
        post_at: None,
        color: event.color.clone(),
        image_url: None,
        channel_id: draft.channel_id.clone(),
        channel,
        attendee_role_id: event.attendee_role_id.clone(),
        mention_everyone: event.mention_everyone,
        mention_role_ids: event.mention_role_ids.clone(),
        restrict_role_ids: event.restrict_role_ids.clone(),
        thread_enabled: event.thread_enabled,
        multiple_signups: event.multiple_signups,
        signups_closed: false,
        series: None,
        creator: String::new(),
        discord_url: None,
        google_url: None,
        calendar_url: None,
        calendar_synced: false,
        google_synced: false,
        can: dto::Abilities {
            edit: true,
            ..Default::default()
        },
    }
}

/// What stops a new routine from being created.
pub fn routine_problems(draft: &dto::RoutineDraft) -> Vec<&'static str> {
    let mut problems = Vec::new();
    if draft.title.trim().is_empty() {
        problems.push("Give the event a title.");
    }
    if draft.channel_id.is_empty() {
        problems.push("Pick a channel.");
    }
    if draft.days.is_empty() {
        problems.push("Pick at least one day people can vote for.");
    }
    if draft.poll_weekday == draft.lock_weekday && draft.poll_time == draft.lock_time {
        problems.push("The poll has to stay open for a while before it locks.");
    }
    problems
}
