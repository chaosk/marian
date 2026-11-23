use leptos::prelude::*;

use crate::api::path;
use crate::cache::{self, Ready, ask};
use crate::format;
use crate::head;
use crate::widgets::{ActionButton, JsonForm, RolePicker, SaveState, ZonePicker};

/// A switch: its field name, title, and what it does.
type Toggle = (&'static str, &'static str, &'static str);

const TIME_TOGGLES: &[Toggle] = &[
    (
        "hour_24",
        "24-hour time",
        "This site and the digest use a 24-hour clock.",
    ),
    (
        "localize_time",
        "Members see their own zone",
        "Discord shows the start time in each member's own zone.",
    ),
];

const POST_TOGGLES: &[Toggle] = &[
    (
        "show_attendees",
        "Show names on the post",
        "The Discord post lists who accepted each option.",
    ),
    (
        "purge_channels",
        "Purge event channels",
        "Posting or deleting an event clears other unpinned messages from its channel.",
    ),
    (
        "calendar_auto_sync",
        "Add new posts to calendar feeds",
        "Each new post appears on members' personal calendar feeds.",
    ),
];

const THREAD_TOGGLES: &[Toggle] = &[
    (
        "event_threads",
        "Threads on new events",
        "A new event opens a Discord thread.",
    ),
    (
        "thread_reminders",
        "Reminders in the thread",
        "Reminders post in the event thread instead of the channel.",
    ),
    (
        "auto_join_threads",
        "Pull attendees into the thread",
        "Off, reminders mention attendees in the thread, which brings them in. On, Marian also adds people on an attending option right away and removes them when they leave it.",
    ),
];

/// A permission: its field, title, note, and what choosing no role means.
const ROLE_PICKS: &[(&str, &str, &str, &str)] = &[
    (
        "role_event",
        "Create events",
        "Choose roles to limit who can.",
        "Everyone",
    ),
    (
        "role_edit",
        "Edit any event",
        "An event's creator can always edit it.",
        "Nobody extra",
    ),
    (
        "role_delete",
        "Delete any event",
        "An event's creator can always delete it.",
        "Nobody extra",
    ),
    (
        "role_channel",
        "Create channels",
        "Choose roles to limit who can.",
        "Same as creating events",
    ),
    (
        "role_signup",
        "Change signup options and presets",
        "Choose roles to limit who can.",
        "Same as creating events",
    ),
];

#[component]
pub fn Settings() -> impl IntoView {
    let membership = cache::listen(cache::membership_scope(), cache::server_key);
    let settings = cache::listen(cache::settings_scope(), cache::server_key);
    let roles = cache::listen(cache::roles_scope(), cache::server_key);
    // The list doesn't depend on the server's zone: keyed on it, it was asked
    // for once with no zone while settings loaded, and that list, with nothing
    // marked as chosen, could be the one the form kept.
    let zones = cache::listen(cache::timezones_scope(), String::new);
    // The first full answer stays, so a save that refetches settings doesn't
    // remount the form under the person using it.
    cache::when_once(
        move || (membership.get(), settings.get(), roles.get(), zones.get()).ready(),
        |(membership, settings, roles, zones)| settings_form(membership, settings, roles, zones),
    )
}

/// The last change's progress, by the field it was to.
type Status = RwSignal<Option<(String, SaveState)>>;

fn settings_form(
    membership: marian_types::Membership,
    settings: marian_types::Settings,
    roles: Vec<marian_types::Role>,
    zones: Vec<marian_types::ZoneGroup>,
) -> impl IntoView {
    let server_manager = membership.server_manager;
    let status: Status = RwSignal::new(None);
    // What the controls are drawn from. A change the server turns down puts
    // them all back to what's saved, rather than leaving the refused value
    // showing as if it stuck.
    let shown = StoredValue::new(settings);
    // Bumped to draw the controls afresh. Redrawn in place, a control whose
    // saved value didn't change would keep the refused one showing, so each
    // round is keyed and mounts new controls.
    let round = RwSignal::new(0_u32);
    let roles = StoredValue::new(roles);
    let zones = StoredValue::new(zones);
    // Read now: the answer comes after the request, away from the page's context.
    let queries = cache::client();
    let server = StoredValue::new(cache::server_now());
    let on_status = Callback::new(move |(field, state): (String, SaveState)| {
        match &state {
            SaveState::Failed(_) => {
                let saved = untrack(|| {
                    queries.get_cached_query(cache::settings_scope(), server.get_value())
                });
                if let Some(Ok(saved)) = saved {
                    shown.set_value(saved);
                }
                round.update(|round| *round += 1);
                focus_setting(field.clone());
            }
            SaveState::Saved => {
                // "Saved" shows for a moment, unless something newer replaced it.
                let said = (field.clone(), SaveState::Saved);
                crate::client::later(2500, move || {
                    if status.try_with_untracked(|now| now.as_ref() == Some(&said)) == Some(true) {
                        status.try_set(None);
                    }
                });
            }
            SaveState::Saving => {}
        }
        status.set(Some((field, state)));
    });
    view! {
        {head("Settings")}
        {(!server_manager).then(|| view! { <p class="notice info">"Only people who can manage this Discord server can change these."</p> })}
        <JsonForm method="PUT" url=crate::api::path::settings(&cache::server_now()) class="settings-page" autosave=server_manager on_status>
            <For each=move || [round.get()] key=|round| *round let:_round>
                {settings_rows(shown.get_value(), roles.get_value(), zones.get_value(), server_manager, status)}
            </For>
        </JsonForm>
    }
}

/// A setting's title, with how its last change went: saving, saved, or why
/// the server turned it down.
fn setting_title(title: &'static str, name: &'static str, status: Status) -> impl IntoView {
    let state = move || {
        status.with(|now| match now {
            Some((field, SaveState::Saving)) if field == name => "Saving…",
            Some((field, SaveState::Saved)) if field == name => "Saved",
            _ => "",
        })
    };
    let failed = move || {
        status.with(|now| match now {
            Some((field, SaveState::Failed(message))) if field == name => Some(
                view! { <span class="error setting-error" role="alert">{message.clone()}</span> },
            ),
            _ => None,
        })
    };
    view! {
        <span class="setting-title"><strong>{title}</strong><span class="save-state" aria-live="polite">{state}</span></span>
        {failed}
    }
}

/// Focus on the control of the setting a refused change was to, once the
/// form is drawn again.
#[cfg(feature = "hydrate")]
fn focus_setting(name: String) {
    crate::client::later(0, move || {
        use wasm_bindgen::JsCast;
        let selector = format!(
            ".settings-page [data-setting=\"{name}\"] :is(input:not([type=hidden]):not(:disabled), select, button)"
        );
        if let Ok(Some(control)) = document().query_selector(&selector)
            && let Ok(control) = control.dyn_into::<leptos::web_sys::HtmlElement>()
        {
            let _ = control.focus();
        }
    });
}

#[cfg(not(feature = "hydrate"))]
fn focus_setting(_: String) {}

fn settings_rows(
    settings: marian_types::Settings,
    roles: Vec<marian_types::Role>,
    zones: Vec<marian_types::ZoneGroup>,
    server_manager: bool,
    status: Status,
) -> impl IntoView {
    let on = |name: &str| match name {
        "thread_reminders" => settings.thread_reminders,
        "auto_archive" => settings.auto_archive,
        "event_threads" => settings.event_threads,
        "auto_join_threads" => settings.auto_join_threads,
        "show_attendees" => settings.show_attendees,
        "hour_24" => settings.hour_24,
        "localize_time" => settings.localize_time,
        "purge_channels" => settings.purge_channels,
        "temp_roles" => settings.temp_roles,
        "waitlist" => settings.waitlist,
        "notify_everyone" => settings.notify_everyone,
        _ => settings.calendar_auto_sync,
    };
    let switch = |(name, title, note): &Toggle| {
        view! {
            <label class="setting-row" data-setting=*name>
                <span class="setting-text">{setting_title(title, name, status)}<span class="muted">{*note}</span></span>
                <input type="checkbox" class="switch-input" role="switch" name=*name checked=on(name) disabled=!server_manager/>
            </label>
        }
    };
    let switches = |list: &[Toggle]| list.iter().map(switch).collect_view();
    let number = |name: &'static str,
                  title: &'static str,
                  note: &'static str,
                  value: i64,
                  unit: &'static str| {
        view! {
            <label class="setting-row" data-setting=name>
                <span class="setting-text">{setting_title(title, name, status)}<span class="muted">{note}</span></span>
                <span class="with-unit">
                    <input name=name data-kind="int" type="number" min="1" value=value inputmode="numeric" disabled=!server_manager/>
                    <span class="muted">{unit}</span>
                </span>
            </label>
        }
    };
    let role_rows = ROLE_PICKS
        .iter()
        .map(|(name, title, note, empty)| {
            let current = match *name {
                "role_event" => settings.role_event.clone(),
                "role_edit" => settings.role_edit.clone(),
                "role_delete" => settings.role_delete.clone(),
                "role_channel" => settings.role_channel.clone(),
                _ => settings.role_signup.clone(),
            };
            view! {
                <div class="setting-row" data-setting=*name>
                    <span class="setting-text">{setting_title(title, name, status)}<span class="muted">{*note}</span></span>
                    <span class="combo-slot">
                        <RolePicker name=*name label=*title roles=roles.clone() chosen=current enabled=server_manager empty=*empty/>
                    </span>
                </div>
            }
        })
        .collect_view();
    let notify_everyone = switch(&(
        "notify_everyone",
        "New events mention @everyone",
        "A new event's post mentions @everyone, unless its channel has a setting of its own on the Channels page. The event's creator can change it.",
    ));
    let notify_roles = view! {
        <div class="setting-row" data-setting="notify_role_ids">
            <span class="setting-text">
                {setting_title("New events mention roles", "notify_role_ids", status)}
                <span class="muted">"Roles a new event's post mentions, unless its channel has a setting of its own on the Channels page."</span>
            </span>
            <span class="combo-slot">
                <RolePicker name="notify_role_ids" label="New events mention roles" roles=roles.clone() chosen=settings.notify_role_ids.clone() enabled=server_manager empty="No roles"/>
            </span>
        </div>
    };
    let time_switches = switches(TIME_TOGGLES);
    let post_switches = switches(POST_TOGGLES);
    let thread_switches = switches(THREAD_TOGGLES);
    let archive_switch = switch(&(
        "auto_archive",
        "Archive automatically",
        "Marian archives a posted event once it has ended and the time below has passed.",
    ));
    let waitlist_switch = switch(&(
        "waitlist",
        "Waitlist",
        "When an option is full, people can still sign up and wait. As spots open, the first in line takes them.",
    ));
    let role_switch = switch(&(
        "temp_roles",
        "Temporary attendee role",
        "Marian creates a role for each event and gives it to people on an attending option.",
    ));
    let lead = number(
        "post_lead_hours",
        "Post series dates early",
        "How long before a recurring date Marian posts it.",
        settings.post_lead_hours,
        "hours before",
    );
    let reminder = default_reminder(settings.default_reminder_minutes, server_manager, status);
    let archive_after = number(
        "auto_archive_hours",
        "Archive after",
        "Counted from the end of the event.",
        settings.auto_archive_hours,
        "hours",
    );
    let digest = settings.digest.clone();
    let digests = [("off", "Off"), ("daily", "Daily"), ("weekly", "Weekly")]
        .into_iter()
        .map(|(id, label)| view! { <option value=id selected=digest == id>{label}</option> })
        .collect_view();
    // Each language by its own name.
    let language = settings.language.clone();
    let languages = [("en", "English"), ("pl", "Polski")]
        .into_iter()
        .map(|(id, label)| view! { <option value=id selected=language == id>{label}</option> })
        .collect_view();
    view! {
            <section class="panel settings-group">
                <h2>"Time"</h2>
                <div class="setting-row zone-row" data-setting="timezone">
                    <span class="setting-text">
                        {setting_title("Time zone", "timezone", status)}
                        <span class="muted">"Event times on this site and in Discord are set in this zone."</span>
                    </span>
                    <span class="combo-slot"><ZonePicker groups=zones chosen=settings.timezone.clone() enabled=server_manager/></span>
                </div>
                {time_switches}
            </section>
            <section class="panel settings-group">
                <h2>"Discord posts"</h2>
                <label class="setting-row" data-setting="language">
                    <span class="setting-text">{setting_title("Language", "language", status)}<span class="muted">"Marian writes in this language on Discord: posts, reminders, replies, and command descriptions. This site stays in English."</span></span>
                    <select name="language" disabled=!server_manager>{languages}</select>
                </label>
                {notify_everyone}
                {notify_roles}
                {post_switches}
                {lead}
                <label class="setting-row" data-setting="digest">
                    <span class="setting-text">{setting_title("Digest", "digest", status)}<span class="muted">"A summary of upcoming events."</span></span>
                    <select name="digest" disabled=!server_manager>{digests}</select>
                </label>
            </section>
            <section class="panel settings-group">
                <h2>"Reminders & threads"</h2>
                {reminder}
                {thread_switches}
            </section>
            <section class="panel settings-group">
                <h2>"Signups"</h2>
                {waitlist_switch}
                {role_switch}
            </section>
            <section class="panel settings-group">
                <h2>"Archive"</h2>
                {archive_switch}
                {archive_after}
            </section>
            <section class="panel settings-group">
                <h2>"Permissions"</h2>
                <p class="settings-note">"People who can manage this Discord server can always do all of these."</p>
                {role_rows}
            </section>
    }
}

/// The server's default reminder: a switch and a number of minutes, saved as one
/// field where zero means off. The hidden field is set before the form reads it.
fn default_reminder(minutes: i64, enabled: bool, status: Status) -> impl IntoView {
    let on = RwSignal::new(minutes > 0);
    let amount = RwSignal::new(if minutes > 0 { minutes } else { 15 });
    let field = NodeRef::<leptos::html::Input>::new();
    let store = move || {
        if let Some(input) = field.get_untracked() {
            let value = if on.get_untracked() {
                amount.get_untracked()
            } else {
                0
            };
            input.set_value(&value.to_string());
        }
    };
    view! {
        <div class="setting-row" data-setting="default_reminder_minutes">
            <span class="setting-text">
                {setting_title("Default reminder", "default_reminder_minutes", status)}
                <span class="muted">"An event that goes out with no reminders of its own gets this one."</span>
            </span>
            <span class="with-unit">
                <input type="number" min="1" max="10080" inputmode="numeric" aria-label="Minutes before"
                    value=amount.get_untracked() disabled=move || !enabled || !on.get()
                    on:change=move |ev| {
                        if let Ok(value) = event_target_value(&ev).trim().parse::<i64>() {
                            amount.set(value);
                        }
                        store();
                    }/>
                <span class="muted">"minutes before"</span>
                <input type="checkbox" class="switch-input" role="switch" aria-label="Default reminder"
                    checked={minutes > 0} disabled=!enabled
                    on:change=move |ev| {
                        on.set(event_target_checked(&ev));
                        store();
                    }/>
            </span>
            <input type="hidden" name="default_reminder_minutes" data-kind="int" node_ref=field value=minutes/>
        </div>
    }
}

#[component]
pub fn Google() -> impl IntoView {
    ask(cache::membership_scope(), cache::server_key, |membership| {
        let line = if membership.google.ready {
            "Connect, disconnect, and automatic updates are with your account in the sidebar."
        } else {
            "Google Calendar connects once this server has a Google client id and secret."
        };
        view! {
            {head("Google Calendar")}
            <section class="panel google-cal"><p>{line}</p></section>
        }
    })
}

/// The AI assistants the signed-in person let act for them over MCP, each
/// with a way to sign it out.
#[component]
pub fn ConnectedApps() -> impl IntoView {
    let server = cache::listen(cache::server_scope(), cache::server_key);
    let apps = cache::listen(cache::apps_scope(), || ());
    cache::when(
        move || (server.get(), apps.get()).ready(),
        move |(server, apps)| {
            let clock = format::clock_of(&server);
            let list = if apps.is_empty() {
                view! {
                    <p class="settings-note">
                        "No apps are connected. To connect one, add this site's address with "
                        <code>"/mcp"</code>" on the end to an assistant such as Claude."
                    </p>
                }
                .into_any()
            } else {
                apps.into_iter()
                    .map(|app| {
                        let allowed = format::format_long_date(clock.day(app.allowed_at));
                        let until = format::format_long_date(clock.day(app.until));
                        view! {
                            <article class="card">
                                <div class="card-top"><h3>{app.name.clone()}</h3></div>
                                <p class="meta">{format!("Allowed {allowed}. It has to ask again after {until}.")}</p>
                                <div class="actions">
                                    <ActionButton method="DELETE" url=path::app(&app.id) label="Disconnect" class="danger"
                                        done="Disconnected."
                                        confirm=format!("Disconnect {}? It stops working until you allow it again.", app.name)/>
                                </div>
                            </article>
                        }
                    })
                    .collect_view()
                    .into_any()
            };
            view! {
                {head("Connected apps")}
                <section class="stack">
                    <p class="settings-note">
                        "AI assistants you let act for you over MCP. Each one can do what you can do here, in your servers with Marian."
                    </p>
                    {list}
                </section>
            }
        },
    )
}
