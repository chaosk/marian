use leptos::prelude::*;
use marian_types as dto;

use crate::format::clock_of;
use crate::islands::EventForm;
use crate::{api, load, param, query};

#[component]
pub fn NewEvent() -> impl IntoView {
    let server_id = param("server");
    let preset = query("preset");
    let data = async move {
        let channels = api::channels(&server_id).await?;
        let options = if preset.is_empty() {
            dto::default_signup_options()
        } else {
            api::preset(&server_id, &preset).await?.options
        };
        Ok((channels, options))
    };
    load(data, |(channels, options)| {
        let seed = dto::EventSeed {
            title: String::new(),
            description: String::new(),
            location: String::new(),
            channel_id: channels.first().map(|c| c.id).unwrap_or_default(),
            color: "#1e3a6e".into(),
            starts_local: String::new(),
            duration_minutes: 60,
            cutoff_local: String::new(),
            post_local: String::new(),
            attendee_role_id: None,
            thread_enabled: true,
            multiple_signups: false,
            mention_everyone: false,
            mention_role_ids: Vec::new(),
            restrict_role_ids: Vec::new(),
            options,
            reminders: vec![dto::Reminder {
                offset_minutes: 15,
                message: String::new(),
                sent: false,
            }],
            repeat: None,
        };
        view! {
            <EventForm
                server=param("server")
                event_id=String::new()
                heading="New event".into()
                series_note=String::new()
                status="draft".into()
                series_locked=false
                channels
                seed
            />
        }
    })
}

#[component]
pub fn EditEvent() -> impl IntoView {
    let server_id = param("server");
    let id = param("id");
    let data = async move {
        let server = api::server(&server_id).await?;
        let event = api::event(&server_id, &id).await?;
        let options = api::event_options(&server_id, &id).await?;
        let reminders = api::event_reminders(&server_id, &id).await?;
        let channels = api::channels(&server_id).await?;
        Ok((server, event, options, reminders, channels))
    };
    load(data, |(server, event, options, reminders, channels)| {
        let clock = clock_of(&server);
        let once = event.series.as_ref().is_some_and(|series| series.cadence == "once");
        let series_note = if event.series.is_some() && !once {
            "This date keeps its own start time. Cadence, date count, and end date are on the series."
                .to_string()
        } else {
            String::new()
        };
        let seed = dto::EventSeed {
            title: event.title.clone(),
            description: event.description,
            location: event.location,
            channel_id: event.channel_id,
            color: event.color,
            starts_local: clock.input(event.starts_at),
            duration_minutes: event.duration_minutes,
            cutoff_local: event.cutoff_at.map(|dt| clock.input(dt)).unwrap_or_default(),
            post_local: event.post_at.map(|dt| clock.input(dt)).unwrap_or_default(),
            attendee_role_id: event.attendee_role_id,
            thread_enabled: event.thread_enabled,
            multiple_signups: event.multiple_signups,
            mention_everyone: event.mention_everyone,
            mention_role_ids: event.mention_role_ids,
            restrict_role_ids: event.restrict_role_ids,
            options,
            reminders,
            repeat: None,
        };
        view! {
            <EventForm
                server=param("server")
                event_id=event.id
                heading=format!("Edit {}", event.title)
                series_note
                status=event.status
                series_locked=event.series.is_some()
                channels
                seed
            />
        }
    })
}
