use leptos::prelude::*;

use crate::api::path;
use marian_types as dto;

use crate::format::{self, clock_of, spots, status_label};
use crate::widgets::{
    numbered, unsaved_signups, ActionButton, LeaveGuard, ApplyToSeries, EventAttendeeRole, EventChannel, EventColor, EventCutoff,
    EventDescription, EventLocation, EventMentions, EventReminders, EventRepeat, EventRestrictions, EventTitle, EventWhen,
    LocalDraft, MultipleToggle, PostButtons, SignupPanel, ThreadToggle,
};
use crate::cache::{self, show, when, Ready};
use crate::{base, border, title, A};

/// What differs between a saved event's page and a new one. The layout around
/// these is the same, so a new event looks the way it will once saved.
struct Slots {
    actions: AnyView,
    series_note: Option<AnyView>,
    attendance: AnyView,
    reminders: AnyView,
    roles: AnyView,
    embed: AnyView,
    channel: AnyView,
    status: AnyView,
    /// A link to the event's activity, once there is an event to have any.
    activity: Option<AnyView>,
}

fn layout(key: (String, String), event: &dto::Event, clock: format::Clock, slots: Slots) -> impl IntoView {
    let image = event.image_url.clone();
    view! {
        <div class="event-top">
            <div>
                <EventTitle key=key.clone() event=event.clone()/>
                <EventWhen key=key.clone() event=event.clone() clock=clock/>
                <EventLocation key=key.clone() event=event.clone()/>
            </div>
            <div class="actions">{slots.actions}</div>
        </div>
        {slots.series_note}
        {image.map(|url| view! { <img class="event-banner" src=url alt=""/> })}
        <div class="event-layout">
            <div class="event-main">
                <EventDescription key=key.clone() event=event.clone()/>
                {slots.attendance}
                <section>
                    <div class="section-head"><h2>"Notifications & access"</h2></div>
                    <div class="panel">
                        <div class="connection-grid">
                            <div>
                                <p class="kicker">"Reminders"</p>
                                {slots.reminders}
                            </div>
                            {slots.roles}
                        </div>
                    </div>
                </section>
                <section>
                    <h2>"Discord message"</h2>
                    {slots.embed}
                </section>
            </div>
            <aside class="panel event-facts">
                <p class="kicker">"Created by"</p>
                <p>{event.creator.clone()}</p>
                <p class="kicker">"Channel"</p>
                {slots.channel}
                <p class="kicker">"Color"</p>
                <EventColor key=key.clone() event=event.clone()/>
                <p class="kicker">"Multiple responses"</p>
                <MultipleToggle key=key.clone() event=event.clone()/>
                <p class="kicker">"Event thread"</p>
                <ThreadToggle key=key.clone() event=event.clone()/>
                <p class="kicker">"Signups close"</p>
                <EventCutoff key=key.clone() event=event.clone() clock=clock/>
                <p class="kicker">"Repeats"</p>
                <EventRepeat key=key.clone() event=event.clone() clock=clock/>
                <p class="kicker">"Status"</p>
                {slots.status}
                {slots.activity.map(|link| view! { <p class="kicker">"Activity"</p>{link} })}
            </aside>
        </div>
    }
}

fn localize_note(server: &dto::Server) -> &'static str {
    if server.localize_time {
        "Members see this time in their own zone on Discord."
    } else {
        "Times stay in the server zone."
    }
}

#[component]
pub fn EventPage() -> impl IntoView {
    let server = cache::listen(cache::server_scope(), cache::server_key);
    let event = cache::listen(cache::event_scope(), cache::event_key);
    let signups = cache::listen(cache::signups_scope(), cache::event_key);
    let members = cache::listen(cache::members_scope(), cache::server_key);
    let reminders = cache::listen(cache::reminders_scope(), cache::event_key);
    let roles = cache::listen(cache::roles_scope(), cache::server_key);
    let channels = cache::listen(cache::channels_scope(), cache::server_key);
    when(
        move || (server.get(), event.get()).ready(),
        move |(server, event)| {
            let clock = clock_of(&server);
            let now = format::now();
            let here = format!("{}/events/{}", base(), event.id);
            let server_id = cache::server_now();
            let at = |make: fn(&str, &str) -> String| make(&server_id, &event.id);
            let localize = localize_note(&server);
            let key = (cache::server_now(), event.id.clone());
            let series_note = event.series.as_ref().and_then(|series| {
                (series.cadence != "once").then(|| {
                    let href = format!("{}/recurring/{}", base(), series.id);
                    view! {
                        <p class="notice info">
                            "This is one date in a series. Changes here apply to this date only. "
                            <A href=href>"See all dates."</A>" "
                            <ApplyToSeries key=key.clone() event=event.clone()/>
                        </p>
                    }
                    .into_any()
                })
            });
            let can = event.can.clone();
            title(event.title.clone());
            let actions = view! {
                <PostButtons key=key.clone() event=event.clone() clock=clock/>
                {can.archive.then(|| view! {
                    <ActionButton method="POST" url=at(path::event_archive) label="Archive" class="ghost"/>
                })}
                <ActionButton method="POST" url=at(path::event_duplicate) label="Duplicate" class="ghost"
                    done=format!("Made a draft copy for {}, one week later.", clock.when(week_later(&clock, event.starts_at)))/>
                {can.skip.then(|| view! {
                    <ActionButton method="POST" url=at(path::event_skip) label="Skip this date" class="ghost"/>
                })}
                {can.delete.then(|| view! {
                    <ActionButton method="DELETE" url=at(path::event) label="Delete" class="danger" confirm="Remove this event?"/>
                })}
            }
            .into_any();
            let status = view! {
                <p>{status_label(&event.status)}</p>
                {(event.status == "scheduled").then(|| event.post_at).flatten().map(|at| view! {
                    <p class="muted">"Posts "{clock.when(at)}</p>
                })}
            }
            .into_any();
            let slots = Slots {
                actions,
                series_note,
                attendance: signups_cell(signups, members, roles).into_any(),
                reminders: reminders_cell(key.clone(), event.clone(), reminders).into_any(),
                roles: roles_cell(key.clone(), event.clone(), roles).into_any(),
                embed: embed_cell(signups, event.clone(), clock, here, at(path::event), localize, now).into_any(),
                channel: channel_cell(key.clone(), event.clone(), channels).into_any(),
                status,
                activity: Some(view! { <p><A href=format!("{}/events/{}/activity", base(), event.id)>"See what happened"</A></p> }.into_any()),
            };
            layout(key, &event, clock, slots)
        },
    )
}

/// A new event, laid out as its page will be. Nothing is saved until Save as
/// draft, Schedule post, or Post now; until then every change stays here.
#[component]
pub fn NewEvent() -> impl IntoView {
    let server = cache::listen(cache::server_scope(), cache::server_key);
    let start = cache::listen(cache::new_event_scope(), || (cache::server_key(), crate::query("preset")));
    let settings = cache::listen(cache::settings_scope(), cache::server_key);
    let roles = cache::listen(cache::roles_scope(), cache::server_key);
    // The draft lives in this page, so a refresh must not redraw it.
    cache::when_once(
        move || (server.get(), start.get(), settings.get(), roles.get()).ready(),
        move |(server, (channels, preset), settings, roles)| {
            let (options, multiple_signups) = preset
                .map(|preset| (preset.options, preset.multiple_signups))
                .unwrap_or_default();
            use chrono::TimeZone;
            let clock = clock_of(&server);
            let now = format::now();
            // Tomorrow evening in the server's zone, a likely time to start from.
            let tomorrow = clock.day(now) + chrono::Duration::days(1);
            let starts_at = tomorrow
                .and_hms_opt(19, 0, 0)
                .and_then(|at| clock.tz.from_local_datetime(&at).earliest())
                .map(|at| at.with_timezone(&chrono::Utc))
                .unwrap_or(now + chrono::Duration::days(1));
            let first = channels.first();
            let event = dto::Event {
                id: "new".into(),
                title: String::new(),
                description: String::new(),
                location: String::new(),
                status: "draft".into(),
                starts_at,
                duration_minutes: 60,
                cutoff_at: None,
                post_at: None,
                color: "#1e3a6e".into(),
                image_url: None,
                channel_id: first.map(|channel| channel.id.clone()).unwrap_or_default(),
                channel: first.map(|channel| channel.name.clone()).unwrap_or_default(),
                attendee_role_id: None,
                mention_everyone: false,
                mention_role_ids: Vec::new(),
                restrict_role_ids: Vec::new(),
                thread_enabled: settings.event_threads,
                multiple_signups,
                signups_closed: false,
                series: None,
                creator: "You".into(),
                discord_url: None,
                google_url: None,
                calendar_url: None,
                calendar_synced: false,
                google_synced: false,
                can: dto::Abilities {
                    edit: true,
                    ..Default::default()
                },
            };
            let reminders: Vec<dto::Reminder> = (settings.default_reminder_minutes > 0)
                .then(|| dto::Reminder {
                    offset_minutes: settings.default_reminder_minutes,
                    message: String::new(),
                    sent: false,
                })
                .into_iter()
                .collect();
            let draft = LocalDraft {
                event: RwSignal::new(event),
                reminders: RwSignal::new(reminders),
                repeat: RwSignal::new(None),
                options: RwSignal::new(numbered(options)),
                channels: StoredValue::new(channels.clone()),
                clock,
                dirty: RwSignal::new(false),
                template: false,
                sync: None,
            };
            provide_context(draft);
            let localize = localize_note(&server);
            let channels = StoredValue::new(channels);
            let roles = StoredValue::new(roles);
            title("New event".to_string());
            let page = move || {
                let event = draft.event.get();
                let options = draft.options.get();
                let reminders = draft.reminders.get();
                let key = (cache::server_now(), "new".to_string());
                let signups = unsaved_signups(&options);
                let slots = Slots {
                    actions: view! { <PostButtons key=key.clone() event=event.clone() clock=clock/> }.into_any(),
                    series_note: Some(view! {
                        <p class="notice info">"Not saved yet. Nothing goes to Discord until you save or post it."</p>
                    }
                    .into_any()),
                    attendance: view! {
                        <SignupPanel server=key.0.clone() event="new".to_string() signups=signups.clone() roles=roles.get_value()/>
                    }
                    .into_any(),
                    reminders: view! { <EventReminders key=key.clone() event=event.clone() reminders/> }.into_any(),
                    roles: role_cells(key.clone(), &event, roles.get_value()).into_any(),
                    embed: embed_view(&signups, &event, clock, String::new(), String::new(), localize, now).into_any(),
                    channel: view! { <EventChannel key=key.clone() event=event.clone() channels=channels.get_value()/> }.into_any(),
                    status: view! { <p>"Not saved yet"</p> }.into_any(),
                    activity: None,
                };
                layout(key, &event, clock, slots)
            };
            view! {
                <LeaveGuard dirty=draft.dirty/>
                {page}
            }
        },
    )
}

fn signups_cell(
    signups: Resource<Result<dto::Signups, crate::api::Failure>>,
    members: Resource<Result<Vec<dto::Member>, crate::api::Failure>>,
    roles: Resource<Result<Vec<dto::Role>, crate::api::Failure>>,
) -> impl IntoView {
    when(
        move || (signups.get(), members.get(), roles.get()).ready(),
        |(signups, members, roles)| {
            view! {
                <SignupPanel
                    server=cache::server_now()
                    event=cache::event_now()
                    signups=signups.clone()
                    members=if signups.editable { members } else { Vec::new() }
                    roles=roles
                />
            }
        },
    )
}

fn channel_cell(
    key: (String, String),
    event: dto::Event,
    channels: Resource<Result<Vec<dto::Channel>, crate::api::Failure>>,
) -> impl IntoView {
    show(channels, move |channels| {
        view! { <EventChannel key=key.clone() event=event.clone() channels/> }
    })
}

fn reminders_cell(
    key: (String, String),
    event: dto::Event,
    reminders: Resource<Result<Vec<dto::Reminder>, crate::api::Failure>>,
) -> impl IntoView {
    show(reminders, move |reminders| {
        view! { <EventReminders key=key.clone() event=event.clone() reminders/> }
    })
}

fn roles_cell(
    key: (String, String),
    event: dto::Event,
    roles: Resource<Result<Vec<dto::Role>, crate::api::Failure>>,
) -> impl IntoView {
    show(roles, move |roles| role_cells(key.clone(), &event, roles))
}

pub(crate) fn role_cells(key: (String, String), event: &dto::Event, roles: Vec<dto::Role>) -> impl IntoView {
    view! {
        <div>
            <p class="kicker">"Mentions"</p>
            <EventMentions key=key.clone() event=event.clone() roles=roles.clone()/>
        </div>
        <div>
            <p class="kicker">"Attendee role"</p>
            <EventAttendeeRole key=key.clone() event=event.clone() roles=roles.clone()/>
        </div>
        <div>
            <p class="kicker">"Restrictions"</p>
            <EventRestrictions key=key event=event.clone() roles/>
        </div>
    }
}

/// The same time on the clock a week on, as a duplicate is made.
fn week_later(clock: &format::Clock, start: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    let local = start.with_timezone(&clock.tz).naive_local() + chrono::Duration::days(7);
    clock
        .tz
        .from_local_datetime(&local)
        .earliest()
        .map(|next| next.with_timezone(&chrono::Utc))
        .unwrap_or(start + chrono::Duration::days(7))
}

fn embed_cell(
    signups: Resource<Result<dto::Signups, crate::api::Failure>>,
    event: dto::Event,
    clock: format::Clock,
    here: String,
    delete_at: String,
    localize: &'static str,
    now: chrono::DateTime<chrono::Utc>,
) -> impl IntoView {
    show(signups, move |signups| embed_view(&signups, &event, clock, here.clone(), delete_at.clone(), localize, now))
}

/// The Discord post as it reads. `here` (the event's page) and `delete_at`
/// (its API path) are empty for an event not saved yet.
fn embed_view(
    signups: &dto::Signups,
    event: &dto::Event,
    clock: format::Clock,
    here: String,
    delete_at: String,
    localize: &'static str,
    now: chrono::DateTime<chrono::Utc>,
) -> impl IntoView {
    let event = event.clone();
    let description = event.description.clone();
    let location = event.location.clone();
    let image = event.image_url.clone();
    let color = event.color.clone();
    let can = event.can.clone();
    // A finished event's post has no buttons, as on Discord.
    let finished = matches!(event.status.as_str(), "archived" | "cancelled" | "skipped");
    {
        let fields = signups
            .options
            .iter()
            .map(|opt| {
                let lines: Vec<String> = opt
                    .confirmed
                    .iter()
                    .map(|person| person.name.clone())
                    .chain((opt.waiting_count > 0).then(|| format!("{} waiting", opt.waiting_count)))
                    .collect();
                let value = if lines.is_empty() {
                    view! { "-" }.into_any()
                } else {
                    lines
                        .iter()
                        .enumerate()
                        .map(|(i, line)| view! { {(i > 0).then(|| view! { <br/> })}{line.clone()} })
                        .collect_view()
                        .into_any()
                };
                view! {
                    <div class="d-field">
                        <p class="d-field-name">
                            {opt.emoji.clone()}" "
                            <strong>{opt.name.clone()}</strong>
                            " ("{spots(opt.confirmed_count, opt.capacity)}")"
                        </p>
                        <p class="d-field-value">{value}</p>
                    </div>
                }
            })
            .collect_view();
        let buttons = signups
            .options
            .iter()
            .map(|opt| {
                view! { <button class="d-btn" type="button" disabled>{opt.emoji.clone()}</button> }
            })
            .collect_view();
        view! {
            <div class="discord">
                <div class="embed" style=border(&color)>
                    <h3>{event.title.clone()}</h3>
                    {(!description.is_empty()).then(|| view! { <div class="d-desc md" inner_html=crate::markdown::render(&description)></div> })}
                    {(!location.is_empty()).then(|| view! {
                        <p class="d-kicker">"Location"</p>
                        <p class="d-desc">"📍 "{location.clone()}</p>
                    })}
                    <p class="d-kicker">"Time"</p>
                    <p class="d-desc">{clock.when(event.starts_at)}" - "{clock.end(event.starts_at, event.duration_minutes)}</p>
                    <p class="d-links">
                        {event.calendar_url.clone().map(|url| view! { <a href=url>"+"</a> })}
                        {(!here.is_empty()).then(|| view! { " · "<A href=here.clone()>"View on web"</A> })}
                    </p>
                    <p class="d-desc">"🕐 "{format::format_rel(event.starts_at, now)}</p>
                    {(event.status != "posted").then(|| view! { <p class="d-desc"><em>{status_label(&event.status)}</em></p> })}
                    <div class="d-fields">{fields}</div>
                    {image.as_ref().map(|url| view! { <img class="d-image" src=url.clone() alt=""/> })}
                    <p class="d-foot">"Created by "{event.creator.clone()}</p>
                </div>
                {(!finished).then(|| view! {
                    <div class="btn-row">
                        {buttons}
                        {if can.edit && !here.is_empty() {
                            view! { <A href=here.clone() attr:class="d-btn primary">"Edit"</A> }.into_any()
                        } else {
                            view! { <button class="d-btn primary" type="button" disabled>"Edit"</button> }.into_any()
                        }}
                        {if can.delete && !delete_at.is_empty() {
                            view! {
                                <ActionButton method="DELETE" url=delete_at.clone() label="Delete" class="d-btn danger" confirm="Remove this event?"/>
                            }
                            .into_any()
                        } else {
                            view! { <button class="d-btn danger" type="button" disabled>"Delete"</button> }.into_any()
                        }}
                    </div>
                })}
                <p class="why">"Times as they read on this site. "{localize}</p>
            </div>
        }
    }
}
