use leptos::prelude::*;

use crate::widgets::{JsonForm, RolePicker, ZonePicker};
use crate::cache::{self, ask, Ready};
use crate::head;

/// A switch: its field name, title, and what it does.
type Toggle = (&'static str, &'static str, &'static str);

const TIME_TOGGLES: &[Toggle] = &[
    ("hour_24", "24-hour time", "This site and the digest use a 24-hour clock."),
    ("localize_time", "Members see their own zone", "Discord shows the start time in each member's own zone."),
];

const POST_TOGGLES: &[Toggle] = &[
    ("show_attendees", "Show names on the post", "The Discord post lists who accepted each option."),
    ("purge_channels", "Purge event channels", "Posting or deleting an event clears other unpinned messages from its channel."),
    ("calendar_auto_sync", "Add new posts to calendar feeds", "Each new post appears on members' personal calendar feeds."),
];

const THREAD_TOGGLES: &[Toggle] = &[
    ("event_threads", "Threads on new events", "A new event opens a Discord thread."),
    ("thread_reminders", "Reminders in the thread", "Reminders post in the event thread instead of the channel."),
    ("auto_join_threads", "Pull attendees into the thread", "Off, reminders mention attendees in the thread, which brings them in. On, Marian also adds people on an attending option right away and removes them when they leave it."),
];

/// A permission: its field, title, note, and what choosing no role means.
const ROLE_PICKS: &[(&str, &str, &str, &str)] = &[
    ("role_event", "Create events", "Choose roles to limit who can.", "Everyone"),
    ("role_edit", "Edit any event", "An event's creator can always edit it.", "Nobody extra"),
    ("role_delete", "Delete any event", "An event's creator can always delete it.", "Nobody extra"),
    ("role_channel", "Create channels", "Choose roles to limit who can.", "Same as creating events"),
    ("role_signup", "Change signup options and presets", "Choose roles to limit who can.", "Same as creating events"),
];

#[component]
pub fn Settings() -> impl IntoView {
    let membership = cache::listen(cache::membership_scope(), cache::server_key);
    let settings = cache::listen(cache::settings_scope(), cache::server_key);
    let roles = cache::listen(cache::roles_scope(), cache::server_key);
    let zones = cache::listen(cache::timezones_scope(), {
        let settings = settings;
        move || {
            settings
                .get()
                .and_then(|result| result.ok())
                .map(|settings| settings.timezone)
                .unwrap_or_default()
        }
    });
    // The first full answer stays, so a save that refetches settings doesn't
    // remount the form under the person using it.
    cache::when_once(
        move || (membership.get(), settings.get(), roles.get(), zones.get()).ready(),
        |(membership, settings, roles, zones)| settings_form(membership, settings, roles, zones),
    )
}

fn settings_form(
    membership: marian_types::Membership,
    settings: marian_types::Settings,
    roles: Vec<marian_types::Role>,
    zones: Vec<marian_types::ZoneGroup>,
) -> impl IntoView {
    let server_manager = membership.server_manager;
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
        _ => settings.calendar_auto_sync,
    };
    let switch = |(name, title, note): &Toggle| {
        view! {
            <label class="setting-row">
                <span class="setting-text"><strong>{*title}</strong><span class="muted">{*note}</span></span>
                <input type="checkbox" class="switch-input" role="switch" name=*name checked=on(name) disabled=!server_manager/>
            </label>
        }
    };
    let switches = |list: &[Toggle]| list.iter().map(switch).collect_view();
    let number = |name: &'static str, title: &'static str, note: &'static str, value: i64, unit: &'static str| {
        view! {
            <label class="setting-row">
                <span class="setting-text"><strong>{title}</strong><span class="muted">{note}</span></span>
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
                <div class="setting-row">
                    <span class="setting-text"><strong>{*title}</strong><span class="muted">{*note}</span></span>
                    <span class="combo-slot">
                        <RolePicker name=*name label=*title roles=roles.clone() chosen=current enabled=server_manager empty=*empty/>
                    </span>
                </div>
            }
        })
        .collect_view();
    let time_switches = switches(TIME_TOGGLES);
    let post_switches = switches(POST_TOGGLES);
    let thread_switches = switches(THREAD_TOGGLES);
    let archive_switch = switch(&("auto_archive", "Archive automatically", "Marian archives a posted event once it has ended and the time below has passed."));
    let waitlist_switch = switch(&("waitlist", "Waitlist", "When an option is full, people can still sign up and wait. As spots open, the first in line takes them."));
    let role_switch = switch(&("temp_roles", "Temporary attendee role", "Marian creates a role for each event and gives it to people on an attending option."));
    let lead = number("post_lead_hours", "Post series dates early", "How long before a recurring date Marian posts it.", settings.post_lead_hours, "hours before");
    let reminder = default_reminder(settings.default_reminder_minutes, server_manager);
    let archive_after = number("auto_archive_hours", "Archive after", "Counted from the end of the event.", settings.auto_archive_hours, "hours");
    let digest = settings.digest.clone();
    let digests = [("off", "Off"), ("daily", "Daily"), ("weekly", "Weekly")]
        .into_iter()
        .map(|(id, label)| view! { <option value=id selected=digest == id>{label}</option> })
        .collect_view();
    view! {
        {head("Settings")}
        {(!server_manager).then(|| view! { <p class="notice info">"Only people who can manage this Discord server can change these."</p> })}
        <JsonForm method="PUT" url=crate::api::path::settings(&cache::server_now()) class="settings-page" autosave=server_manager>
            <section class="panel settings-group">
                <h2>"Time"</h2>
                <div class="setting-row zone-row">
                    <span class="setting-text">
                        <strong>"Time zone"</strong>
                        <span class="muted">"Event times on this site and in Discord are set in this zone."</span>
                    </span>
                    <span class="combo-slot"><ZonePicker groups=zones enabled=server_manager/></span>
                </div>
                {time_switches}
            </section>
            <section class="panel settings-group">
                <h2>"Discord posts"</h2>
                {post_switches}
                {lead}
                <label class="setting-row">
                    <span class="setting-text"><strong>"Digest"</strong><span class="muted">"A summary of upcoming events."</span></span>
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
        </JsonForm>
    }
}

/// The server's default reminder: a switch and a number of minutes, saved as one
/// field where zero means off. The hidden field is set before the form reads it.
fn default_reminder(minutes: i64, enabled: bool) -> impl IntoView {
    let on = RwSignal::new(minutes > 0);
    let amount = RwSignal::new(if minutes > 0 { minutes } else { 15 });
    let field = NodeRef::<leptos::html::Input>::new();
    let store = move || {
        if let Some(input) = field.get_untracked() {
            let value = if on.get_untracked() { amount.get_untracked() } else { 0 };
            input.set_value(&value.to_string());
        }
    };
    view! {
        <div class="setting-row">
            <span class="setting-text">
                <strong>"Default reminder"</strong>
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
