use leptos::prelude::*;

use crate::islands::{JsonForm, ZonePicker};
use crate::{api, base, head, load, param};

const TOGGLES: &[(&str, &str, &str)] = &[
    ("thread_reminders", "Reminders in the thread", "Reminders post in the event thread."),
    ("auto_archive", "Auto archive", "Marian archives a posted event after it ends, once the hours above have passed."),
    ("event_threads", "Threads on new events", "A new event opens a Discord thread."),
    ("auto_join_threads", "Pull attendees into the thread", "People who accept on a reminding option join the thread. Marian removes them when they leave that option."),
    ("show_attendees", "Show names on the Discord post", "The post lists who accepted each option."),
    ("hour_24", "24-hour time", "This site and the digest use a 24-hour clock."),
    ("localize_time", "Members see their own zone", "Discord shows the start time in each member's own zone."),
    ("purge_channels", "Purge event channels", "Posting or deleting an event removes other messages, and pinned messages stay."),
    ("temp_roles", "Make a temporary attendee role", "Marian creates an attendee role for the event and gives it to people on a reminding option."),
    ("calendar_auto_sync", "Mark new posts on the calendar feed", "Each new post is marked on the personal calendar feed."),
];

const ROLE_PICKS: &[(&str, &str)] = &[
    ("role_event", "Who may create events"),
    ("role_edit", "Who else may edit"),
    ("role_delete", "Who else may delete"),
    ("role_channel", "Who may create channels"),
    ("role_signup", "Who may edit signup options"),
];

#[component]
pub fn Settings() -> impl IntoView {
    let server = param("server");
    let data = async move {
        let membership = api::membership(&server).await?;
        let settings = api::settings(&server).await?;
        let roles = api::roles(&server).await?;
        let zones = api::timezones(&settings.timezone).await?;
        Ok((membership, settings, roles, zones))
    };
    load(data, |(membership, settings, roles, zones)| {
        let organizer = membership.organizer;
        let warning = if !organizer {
            Some("Only a server organizer can change these.")
        } else {
            None
        };
        let picked = |current: Option<i64>| {
            let mut rows = vec![view! {
                <option value="" selected=current.is_none()>"No extra role"</option>
            }
            .into_any()];
            rows.extend(roles.iter().map(|role| {
                view! { <option value=role.id.to_string() selected=current == Some(role.id)>{role.name.clone()}</option> }
                    .into_any()
            }));
            rows.collect_view()
        };
        let role_fields = ROLE_PICKS
            .iter()
            .map(|(name, label)| {
                let current = match *name {
                    "role_event" => settings.role_event,
                    "role_edit" => settings.role_edit,
                    "role_delete" => settings.role_delete,
                    "role_channel" => settings.role_channel,
                    _ => settings.role_signup,
                };
                view! {
                    <label class="field">
                        {*label}
                        <select name=*name data-kind="int" disabled=!organizer>{picked(current)}</select>
                    </label>
                }
            })
            .collect_view();
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
            _ => settings.calendar_auto_sync,
        };
        let toggles = TOGGLES
            .iter()
            .map(|(name, title, note)| {
                view! {
                    <label class="setting">
                        <input type="checkbox" name=*name checked=on(name) disabled=!organizer/>
                        <span><strong>{*title}</strong><span class="muted">{*note}</span></span>
                    </label>
                }
            })
            .collect_view();
        let digest = settings.digest.clone();
        let digests = ["off", "daily", "weekly"]
            .into_iter()
            .map(|id| view! { <option value=id selected=digest == id>{id}</option> })
            .collect_view();
        view! {
            {head("Settings")}
            {warning.map(|text| view! { <p class="error">{text}</p> })}
            <JsonForm method="PUT".into() url=format!("/api{}/settings", base()) class="grid".into() autosave=organizer>
                <label class="field span-2">"Time zone"<ZonePicker groups=zones enabled=organizer/></label>
                <label class="field">
                    "Default reminder, minutes"
                    <input name="default_reminder_minutes" data-kind="int" value=settings.default_reminder_minutes inputmode="numeric" disabled=!organizer/>
                </label>
                <label class="field">
                    "Archive posted events after, hours"
                    <input name="auto_archive_hours" data-kind="int" value=settings.auto_archive_hours inputmode="numeric" disabled=!organizer/>
                </label>
                <label class="field">
                    "How early series dates post, hours"
                    <input name="post_lead_hours" data-kind="int" value=settings.post_lead_hours inputmode="numeric" disabled=!organizer/>
                </label>
                <label class="field">"Digest"<select name="digest" disabled=!organizer>{digests}</select></label>
                {role_fields}
                <div class="settings span-2">{toggles}</div>
                {organizer.then(|| view! { <p class="muted span-2">"Each change saves as you make it."</p> })}
            </JsonForm>
        }
    })
}

#[component]
pub fn Google() -> impl IntoView {
    let server = param("server");
    let data = async move { api::membership(&server).await };
    load(data, |membership| {
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
