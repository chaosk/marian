use leptos::prelude::*;
use marian_types as dto;

use crate::format::{self, clock_of};
use crate::pages::event::role_cells;
use crate::widgets::{
    numbered, routine_problems, tally, template_event, TallyRow, unsaved_signups, ActionButton, EventColor, EventCutoff,
    EventLocation, EventReminders, LeaveGuard, LocalDraft, MultipleToggle, PollMoment, PollTally, RoutineChannel,
    RoutineDescription, RoutineEdit, RoutineTime, RoutineTitle, SignupPanel, ThreadToggle, UsualDays,
};
use crate::cache::{self, ask, when, Ready};
use crate::{base, client, title, A};

#[component]
pub fn Routines() -> impl IntoView {
    ask(cache::routines_scope(), cache::server_key, |rows| {
        let base = base();
        let cards = if rows.is_empty() {
            view! {
                <section class="empty">
                    <h2>"No routines yet."</h2>
                    <p>"A routine asks the server which day suits them each week, then posts the event."</p>
                </section>
            }
            .into_any()
        } else {
            let cards = rows
                .into_iter()
                .map(|row| {
                    let votes = (!row.choices.is_empty()).then(|| {
                        tally(
                            row.choices
                                .iter()
                                .map(|choice| TallyRow {
                                    label: choice.label.clone(),
                                    votes: choice.votes,
                                    voters: Vec::new(),
                                })
                                .collect(),
                        )
                    });
                    let badge = format!("badge {}", state_class(&row.badge));
                    let meta = [format!("#{}", row.channel), row.schedule.clone()]
                        .into_iter()
                        .filter(|part| part.len() > 1)
                        .collect::<Vec<_>>()
                        .join(" · ");
                    view! {
                        <A href=format!("{base}/routines/{}", row.id) attr:class="card routine-card">
                            <div class="card-top">
                                <h3>{row.title}</h3>
                                <span class=badge>{row.badge}</span>
                            </div>
                            <p class="meta">{meta}</p>
                            {votes}
                            <div class="routine-card-foot">
                                {(!row.poll.is_empty()).then(|| view! { <p class="meta">{row.poll.clone()}</p> })}
                                {(!row.event.is_empty()).then(|| view! { <p class="meta">{row.event.clone()}</p> })}
                            </div>
                        </A>
                    }
                })
                .collect_view();
            view! { <div class="routine-cards">{cards}</div> }.into_any()
        };
        title("Routines");
        view! {
            <div class="page-head">
                <div><h1>"Routines"</h1></div>
                <A href=format!("{base}/routines/new") attr:class="primary">"New routine"</A>
            </div>
            {cards}
        }
    })
}

/// The badge colour for a routine's state.
fn state_class(state: &str) -> &'static str {
    match state {
        "Voting" => "state-voting",
        "Stopped" => "state-stopped",
        _ => "state-waiting",
    }
}

/// A routine's details as its form sends them, from what its page shows.
fn draft_from(routine: &dto::RoutineView) -> dto::RoutineDraft {
    dto::RoutineDraft {
        channel_id: routine.channel_id.clone(),
        title: routine.title.clone(),
        description: routine.description.clone(),
        duration_minutes: routine.duration_minutes,
        start_time: routine.start_time.clone(),
        poll_weekday: routine.poll_weekday.parse().unwrap_or(6),
        poll_time: routine.poll_time.clone(),
        lock_weekday: routine.lock_weekday.parse().unwrap_or(1),
        lock_time: routine.lock_time.clone(),
        days: routine.usual.iter().filter(|day| day.on).map(|day| day.n).collect(),
        event: None,
    }
}

/// Sets up the event editors on a routine: a draft event holding the routine's
/// details, which the page reads and the routine saves from.
fn event_draft(
    edit: RoutineEdit,
    event: &dto::RoutineEvent,
    channels: &[dto::Channel],
    clock: format::Clock,
    saved: bool,
) -> LocalDraft {
    let draft = edit.draft.get_untracked();
    let channel = channels
        .iter()
        .find(|channel| channel.id == draft.channel_id)
        .map(|channel| channel.name.clone())
        .unwrap_or_default();
    let local = LocalDraft {
        event: RwSignal::new(template_event(&draft, event, channel)),
        reminders: RwSignal::new(event.reminders.clone()),
        repeat: RwSignal::new(None),
        options: RwSignal::new(numbered(event.options.clone())),
        channels: StoredValue::new(channels.to_vec()),
        clock,
        dirty: edit.dirty,
        template: true,
        sync: saved.then(|| Callback::new(move |_| edit.push())),
    };
    edit.local.set_value(Some(local));
    provide_context(local);
    local
}

/// The page a routine and a new one share: its wording and event details as an
/// event's page has them, with the poll leading the page.
fn routine_layout(
    edit: RoutineEdit,
    local: LocalDraft,
    channels: Vec<dto::Channel>,
    roles: Vec<dto::Role>,
    poll: AnyView,
) -> impl IntoView {
    let key = (cache::server_now(), "routine".to_string());
    let roles = StoredValue::new(roles);
    let attendance = {
        let key = key.clone();
        move || {
            let options = local.options.get();
            view! {
                <SignupPanel server=key.0.clone() event="routine".to_string() signups=unsaved_signups(&options) roles=roles.get_value()/>
            }
        }
    };
    let notices = {
        let key = key.clone();
        move || {
            let event = local.event.get();
            let reminders = local.reminders.get();
            view! {
                <section>
                    <div class="section-head"><h2>"Notifications & access"</h2></div>
                    <div class="panel">
                        <div class="connection-grid">
                            <div>
                                <p class="kicker">"Reminders"</p>
                                <EventReminders key=key.clone() event=event.clone() reminders/>
                            </div>
                            {role_cells(key.clone(), &event, roles.get_value())}
                        </div>
                    </div>
                </section>
            }
        }
    };
    let details = {
        let key = key.clone();
        move || {
            let event = local.event.get();
            let clock = local.clock;
            view! {
                <p class="kicker">"Color"</p>
                <EventColor key=key.clone() event=event.clone()/>
                <p class="kicker">"Multiple responses"</p>
                <MultipleToggle key=key.clone() event=event.clone()/>
                <p class="kicker">"Event thread"</p>
                <ThreadToggle key=key.clone() event=event.clone()/>
                <p class="kicker">"Signups close"</p>
                <EventCutoff key=key.clone() event=event.clone() clock=clock/>
            }
        }
    };
    view! {
        {edit.error()}
        <div class="event-layout">
            <div class="event-main">
                <RoutineDescription edit/>
                {poll}
                {attendance}
                {notices}
            </div>
            <aside class="panel event-facts">
                <p class="kicker">"Channel"</p>
                <RoutineChannel edit channels/>
                {details}
            </aside>
        </div>
    }
}

/// The routine's title, and where its events happen, as an event's page heads them.
fn routine_heading(local: LocalDraft, edit: RoutineEdit, lede: Option<&'static str>) -> impl IntoView {
    let key = (cache::server_now(), "routine".to_string());
    view! {
        <p class="kicker">"Routine"</p>
        <RoutineTitle edit/>
        {move || view! { <EventLocation key=key.clone() event=local.event.get()/> }}
        {lede.map(|text| view! { <p class="lede">{text}</p> })}
    }
}

/// The weekly cycle a routine runs: when the poll opens, when it locks, and
/// when the winning day's event starts, then the days each poll offers.
fn poll_schedule(edit: RoutineEdit) -> impl IntoView {
    view! {
        <div class="poll-part">
            <h3>"Every week"</h3>
            <ol class="poll-cycle">
                <li>
                    <p class="kicker">"Poll opens"</p>
                    <PollMoment edit locks=false/>
                </li>
                <li>
                    <p class="kicker">"Poll locks"</p>
                    <PollMoment edit locks=true/>
                </li>
                <li>
                    <p class="kicker">"Event starts"</p>
                    <RoutineTime edit/>
                    <p class="poll-cycle-note">"on the day with the most votes"</p>
                </li>
            </ol>
        </div>
        <div class="poll-part">
            <h3>"Days on each poll"</h3>
            <UsualDays edit/>
        </div>
    }
}

/// The poll section's heading, with where the poll stands.
fn poll_head(state: &'static str, note: String) -> impl IntoView {
    view! {
        <div class="section-head">
            <h2>"Poll"</h2>
            <p class="poll-state">
                {(!note.is_empty()).then(|| view! { <span class="meta">{note}</span> })}
                <span class=format!("badge {}", state_class(state))>{state}</span>
            </p>
        </div>
    }
}

/// A new routine, laid out as its page will be. Nothing is saved until it is created.
#[component]
pub fn NewRoutine() -> impl IntoView {
    let server = cache::listen(cache::server_scope(), cache::server_key);
    let channels = cache::listen(cache::channels_scope(), cache::server_key);
    let roles = cache::listen(cache::roles_scope(), cache::server_key);
    let settings = cache::listen(cache::settings_scope(), cache::server_key);
    when(move || (server.get(), channels.get(), roles.get(), settings.get()).ready(), move |(server, channels, roles, settings)| {
        let draft = dto::RoutineDraft {
            channel_id: channels.first().map(|channel| channel.id.clone()).unwrap_or_default(),
            title: String::new(),
            description: String::new(),
            duration_minutes: 120,
            start_time: "19:00".into(),
            poll_weekday: 6,
            poll_time: "10:00".into(),
            lock_weekday: 1,
            lock_time: "18:00".into(),
            days: (0..7).collect(),
            event: None,
        };
        let starting = dto::RoutineEvent {
            color: "#1e3a6e".into(),
            thread_enabled: settings.event_threads,
            options: dto::default_signup_options(),
            reminders: (settings.default_reminder_minutes > 0)
                .then(|| dto::Reminder {
                    offset_minutes: settings.default_reminder_minutes,
                    message: String::new(),
                    sent: false,
                })
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let edit = RoutineEdit::new(draft, None);
        let local = event_draft(edit, &starting, &channels, clock_of(&server), false);
        let problems = RwSignal::new(Vec::<&'static str>::new());
        let go = client::use_go();
        let creating = client::action(move |draft: &dto::RoutineDraft| {
            let draft = draft.clone();
            let url = format!("/api{}/routines", base());
            let toasts = client::toast_list();
            let go = go.clone();
            async move {
                match crate::api::send::<dto::Done, _>("POST", &url, Some(&draft)).await {
                    Ok(done) => {
                        if let Some(list) = toasts {
                            client::push_toast(list, "Created the routine.");
                        }
                        edit.dirty.set(false);
                        if let Some(to) = done.location {
                            go(&to);
                        }
                    }
                    Err(failure) => edit.problem.set(failure.message),
                }
            }
        });
        let create = move |_| {
            let draft = edit.full();
            let mut found = routine_problems(&draft);
            if local.options.with_untracked(|options| options.is_empty()) {
                found.push("Add at least one attendance type.");
            }
            problems.set(found.clone());
            if !found.is_empty() {
                return;
            }
            edit.problem.set(String::new());
            creating.dispatch(draft);
        };
        title("New routine");
        view! {
            <LeaveGuard dirty=edit.dirty/>
            <div class="event-top">
                <div>{routine_heading(local, edit, Some("A weekly poll picks the day, and Marian posts the event."))}</div>
                <div class="actions post-buttons">
                    <button type="button" class="primary" disabled=move || creating.pending().get() on:click=create>"Create routine"</button>
                    {move || {
                        let list = problems.get();
                        (!list.is_empty()).then(|| view! {
                            <ul class="error save-problems" role="alert">
                                {list.into_iter().map(|problem| view! { <li>{problem}</li> }).collect_view()}
                            </ul>
                        })
                    }}
                </div>
            </div>
            <p class="notice info">"Not saved yet. Nothing goes to Discord until you create it."</p>
            {routine_layout(edit, local, channels, roles, view! {
                <section>
                    <div class="section-head"><h2>"Poll"</h2></div>
                    <div class="panel poll">{poll_schedule(edit)}</div>
                </section>
            }.into_any())}
        }
    })
}

#[component]
pub fn RoutinePage() -> impl IntoView {
    let server = cache::listen(cache::server_scope(), cache::server_key);
    let routine = cache::listen(cache::routine_scope(), cache::event_key);
    let channels = cache::listen(cache::channels_scope(), cache::server_key);
    let roles = cache::listen(cache::roles_scope(), cache::server_key);
    // Out here so it outlives the redraw each saved change brings.
    let editing_days = RwSignal::new(false);
    when(move || (server.get(), routine.get(), channels.get(), roles.get()).ready(), move |(server, routine, channels, roles)| {
        let base = base();
        let here = format!("{base}/routines/{}", routine.id);
        let api_here = format!("/api{here}");
        let edit = RoutineEdit::new(draft_from(&routine), Some(api_here.clone()));
        let local = event_draft(edit, &routine.event, &channels, clock_of(&server), true);
        let (state, note) = match &routine.run {
            _ if !routine.active => ("Stopped", String::new()),
            Some(run) if run.open => ("Voting", format!("Locks {}", run.locks)),
            Some(run) => ("Waiting", format!("Opens {}", run.opens)),
            None => ("Waiting", String::new()),
        };
        let current = routine.run.clone().filter(|_| routine.active).map(|run| {
            let heading = if run.open { "This week's votes" } else { "Next poll" };
            view! {
                <div class="poll-part">
                    <h3>{heading}</h3>
                    <PollTally run api=api_here.clone() editing=editing_days/>
                </div>
            }
        });
        let upcoming = routine.upcoming.clone().map(|event| {
            view! {
                <div class="poll-part poll-picked">
                    <h3>"Picked"</h3>
                    <A href=event.href>{event.when}</A>
                    <span class="muted">" · "{event.winner}" won the last poll"</span>
                </div>
            }
        });
        let history = (!routine.history.is_empty()).then(|| {
            let count = routine.history.len();
            let rows = routine
                .history
                .clone()
                .into_iter()
                .map(|row| view! { <li><A href=row.href>{row.when}</A><span class="muted">" · "{row.winner}</span></li> })
                .collect_view();
            view! {
                <details class="poll-part poll-history">
                    <summary>{format!("Past polls ({count})")}</summary>
                    <ul class="history-list">{rows}</ul>
                </details>
            }
        });
        let poll = view! {
            <section>
                {poll_head(state, note)}
                <div class="panel poll">
                    {current}
                    {upcoming}
                    {poll_schedule(edit)}
                    {history}
                </div>
            </section>
        }
        .into_any();
        let stopped = (!routine.active).then(|| view! {
            <p class="notice info">"This routine is stopped. No more polls go out."</p>
        });
        title(routine.title.clone());
        view! {
            <div class="event-top">
                <div>{routine_heading(local, edit, None)}</div>
                <div class="actions">
                    {routine.active.then(|| view! {
                        <ActionButton method="POST" url=format!("{api_here}/stop") label="Stop"
                            class="danger" confirm="Stop this routine? No more polls go out."/>
                    })}
                </div>
            </div>
            {stopped}
            {routine_layout(edit, local, channels, roles, poll)}
        }
    })
}
