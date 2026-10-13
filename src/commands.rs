use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::clock;
use crate::error::OpError;
use crate::logic::{
    self, EventInput, OptionInput, PresetBody, ReminderInput, RepeatSpec, SaveAction,
};
use crate::parse::{self, tokenize};
use crate::store::{self, Event};
use rusqlite::OptionalExtension;

pub const HELP: &str = "\
/event title:\"Thursday vault\" start:\"next thursday at 7pm\" channel:raids
/edit 12 title:\"New name\"
/events
/reminders off
/response add event:12 user:june option:Healer
/response remove event:12 user:june option:Healer
/response move event:12 user:june option:DPS
In Discord, /response add, /response remove, and /response move each ask for the event, the person, and the signup option, and list the choices.
/settings reminder_interval 30
/settings show_attendees off
/settings role delete Officer
/signup_options add emoji:🛡️ name:Tank
/signup_options remove Tank
/signup_presets
/channel name:briefing kind:event
/google authorize
/google auto_sync on
/google sync 12
/sort
/help

Times can look like \"tomorrow at 7pm\", \"friday 9am\", \"oct 20 at 8:00pm\", or \"in 2 hours\".
The website is where presets, waitlists, and per-option roles are easier to set up.
";

pub fn execute(conn: &mut rusqlite::Connection, guild_id: i64, user_id: i64, line: &str) -> String {
    let line = line.trim();
    let reply = if line.is_empty() {
        "Type /help to see the command desk.".to_string()
    } else {
        match dispatch(conn, guild_id, user_id, line) {
            Ok(text) => text,
            Err(err) => err.to_string(),
        }
    };
    let _ = logic::record_command(conn, guild_id, user_id, line, &reply);
    reply
}

fn event_path(guild: &store::Guild, public_id: &str) -> String {
    match guild.discord_id.as_deref().filter(|id| !id.is_empty()) {
        Some(server) => format!("/servers/{server}/events/{public_id}"),
        None => format!("/events/{public_id}"),
    }
}

fn find_event(conn: &rusqlite::Connection, guild_id: i64, token: &str) -> Result<Event, OpError> {
    if let Some(public_id) = crate::ids::parse_event_id(token) {
        if let Some(event) = store::event_by_public(conn, public_id)? {
            if event.guild_id == guild_id {
                return Ok(event);
            }
        }
    }
    if let Ok(id) = token.parse::<i64>() {
        if let Some(event) = store::event(conn, id)? {
            if event.guild_id == guild_id {
                return Ok(event);
            }
        }
    }
    Err(OpError::message("That event isn't in this server."))
}

pub fn channel_posts_waiting(
    conn: &rusqlite::Connection,
    discord_user_id: &str,
) -> Result<Vec<i64>, OpError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT draft FROM event_prompts WHERE discord_user_id = ? AND step = 'next'",
            [discord_user_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let draft = serde_json::from_str::<EventDraft>(&raw).unwrap_or_default();
    let Some(id) = draft.event_id else {
        return Ok(Vec::new());
    };
    let Some(event) = store::event(conn, id)? else {
        return Ok(Vec::new());
    };
    if event.status == "posted" {
        Ok(vec![id])
    } else {
        Ok(Vec::new())
    }
}

fn dispatch(
    conn: &mut rusqlite::Connection,
    guild_id: i64,
    user_id: i64,
    line: &str,
) -> Result<String, OpError> {
    let (cmd, rest) = split_cmd(line);
    let (pos, map) = fields(rest);
    match cmd {
        "help" | "" => Ok(HELP.to_string()),
        "event" => cmd_event(conn, guild_id, user_id, &pos, &map),
        "edit" => cmd_edit(conn, guild_id, user_id, &pos, &map),
        "events" => cmd_events(conn, guild_id),
        "reminders" => cmd_reminders(conn, user_id, &pos, &map),
        "response" => cmd_response(conn, user_id, &pos, &map),
        "settings" => cmd_settings(conn, guild_id, user_id, &pos, &map),
        "signup_options" => cmd_signup_options(conn, guild_id, user_id, &pos, &map),
        "signup_presets" => cmd_presets(conn, guild_id, user_id, &pos, &map),
        "channel" => cmd_channel(conn, guild_id, user_id, &pos, &map),
        "google" => cmd_google(conn, guild_id, user_id, &pos, &map),
        "sort" => Ok("Events are already in chronological order. Discord posts are refreshed in that same order when Marian syncs them.".into()),
        other => Err(OpError::message(format!("Unknown command /{other}. Try /help."))),
    }
}

fn cmd_event(
    conn: &mut rusqlite::Connection,
    guild_id: i64,
    user_id: i64,
    pos: &[String],
    map: &HashMap<String, String>,
) -> Result<String, OpError> {
    let title = map
        .get("title")
        .cloned()
        .or_else(|| pos.first().cloned())
        .ok_or_else(|| OpError::message("Give the event a title."))?;
    let start_raw = map
        .get("start")
        .cloned()
        .or_else(|| pos.get(1).cloned())
        .ok_or_else(|| OpError::message("Give a start time, like tomorrow at 7pm."))?;
    let guild = store::guild(conn, guild_id)?.ok_or(OpError::NotFound)?;
    let tz = clock::zone(&guild.timezone)?;
    let starts_at = parse::parse_when(&start_raw, Utc::now(), tz).ok_or_else(|| {
        OpError::message(format!("Marian couldn't read the time \"{start_raw}\"."))
    })?;
    let channels = store::list_channels(conn, guild_id)?;
    let channel = if let Some(name) = map.get("channel").or_else(|| pos.get(2)) {
        store::channel_by_name(conn, guild_id, name)?
            .ok_or_else(|| OpError::message(format!("No channel named {name}.")))?
    } else {
        channels
            .first()
            .cloned()
            .ok_or_else(|| OpError::message("Create a channel before posting an event."))?
    };
    let duration = map
        .get("duration")
        .and_then(|value| value.parse().ok())
        .unwrap_or(60);
    let description = map.get("description").cloned().unwrap_or_default();
    let mut options = yes_no_maybe();
    let mut multiple = false;
    if let Some(preset_name) = map.get("preset") {
        if let Some(preset) = store::list_presets(conn, guild_id)?
            .into_iter()
            .find(|preset| preset.name.eq_ignore_ascii_case(preset_name))
        {
            if let Ok(body) = serde_json::from_str::<PresetBody>(&preset.body) {
                options = body.options;
                multiple = body.multiple_signups;
            }
        }
    }
    let mut repeat = None;
    if let Some(cadence) = map.get("repeat") {
        let cadence = match cadence.to_lowercase().as_str() {
            "daily" | "day" => "daily",
            "weekly" | "week" => "weekly",
            "biweekly" => "biweekly",
            "monthly" | "month" => "monthly",
            other => {
                return Err(OpError::message(format!(
                    "Repeat can be daily, weekly, biweekly, or monthly. Not {other}."
                )))
            }
        };
        repeat = Some(RepeatSpec {
            cadence: cadence.into(),
            end_on: None,
            max_occurrences: map.get("count").and_then(|value| value.parse().ok()),
        });
    }
    let draft = matches!(
        map.get("draft").map(String::as_str),
        Some("yes" | "true" | "on")
    );
    let post_at = if let Some(raw) = map.get("post") {
        Some(parse::parse_when(raw, Utc::now(), tz).ok_or_else(|| {
            OpError::message(format!("Marian couldn't read the post time \"{raw}\"."))
        })?)
    } else {
        None
    };
    let action = if draft {
        SaveAction::Draft
    } else if post_at.is_some() {
        SaveAction::Schedule
    } else {
        SaveAction::Publish
    };
    let id = logic::save_event(
        conn,
        EventInput {
            id: None,
            guild_id,
            actor_id: user_id,
            channel_id: channel.id,
            title,
            description,
            starts_at,
            duration_minutes: duration,
            color: "#1e3a6e".into(),
            image_url: None,
            cutoff_at: None,
            attendee_role_id: None,
            thread_enabled: store::settings(conn, guild_id)?.event_threads,
            multiple_signups: multiple,
            mention_everyone: false,
            mention_role_ids: vec![],
            restrict_role_ids: vec![],
            options,
            reminders: vec![],
            repeat,
            action,
            post_at,
            apply_series: false,
            vote: None,
        },
    )?;
    let event = store::event(conn, id)?.ok_or(OpError::NotFound)?;
    Ok(format!(
        "{} \"{}\" for {}. Open {} to set options, reminders, and roles.",
        logic::status_label(&event.status),
        event.title,
        clock::format_when(starts_at, tz, store::settings(conn, guild_id)?.hour_24),
        event_path(&guild, &event.public_id)
    ))
}

#[derive(Debug)]
pub struct DmReply {
    pub title: Option<String>,
    pub body: String,
}

impl DmReply {
    fn plain(body: impl Into<String>) -> Self {
        Self {
            title: None,
            body: body.into(),
        }
    }

    fn card(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: Some(title.into()),
            body: body.into(),
        }
    }
}

impl std::fmt::Display for DmReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.title {
            Some(title) => write!(f, "{title}\n{}", self.body),
            None => write!(f, "{}", self.body),
        }
    }
}

pub fn start_event_dm(
    conn: &mut rusqlite::Connection,
    guild_id: i64,
    user_id: i64,
    discord_user_id: &str,
    source_channel: &str,
    source_discord: Option<&str>,
) -> Result<DmReply, OpError> {
    let channels = store::list_channels(conn, guild_id)?;
    if channels.is_empty() {
        return Err(OpError::message(
            "Create a channel before posting an event.",
        ));
    }
    if store::channel_by_name(conn, guild_id, source_channel)?.is_none() {
        return Err(OpError::message(
            "Create a channel before posting an event.",
        ));
    }
    conn.execute(
        "INSERT INTO event_prompts (
            discord_user_id, guild_id, user_id, step, title, start_raw, channel_name, source_channel, source_discord, draft
         ) VALUES (?, ?, ?, 'channel', NULL, NULL, NULL, ?, ?, NULL)
         ON CONFLICT(discord_user_id) DO UPDATE SET
           guild_id = excluded.guild_id,
           user_id = excluded.user_id,
           step = 'channel',
           title = NULL,
           start_raw = NULL,
           channel_name = NULL,
           source_channel = excluded.source_channel,
           source_discord = excluded.source_discord,
           draft = NULL",
        rusqlite::params![
            discord_user_id,
            guild_id,
            user_id,
            source_channel,
            source_discord
        ],
    )?;
    let source = Some(source_channel.to_string());
    let discord = source_discord.map(str::to_string);
    where_to_post(conn, guild_id, &source, &discord)
}

pub fn cancel_event_dm(conn: &rusqlite::Connection, discord_user_id: &str) -> Result<(), OpError> {
    conn.execute(
        "DELETE FROM event_prompts WHERE discord_user_id = ?",
        [discord_user_id],
    )?;
    Ok(())
}

pub fn answer_event_dm(
    conn: &mut rusqlite::Connection,
    discord_user_id: &str,
    text: &str,
    public_url: &str,
) -> Result<Option<Vec<DmReply>>, OpError> {
    let row = conn
        .query_row(
            "SELECT guild_id, user_id, step, title, start_raw, channel_name, source_channel, source_discord, draft
             FROM event_prompts WHERE discord_user_id = ?",
            [discord_user_id],
            |row| {
                Ok(EventPrompt {
                    guild_id: row.get(0)?,
                    user_id: row.get(1)?,
                    step: row.get(2)?,
                    title: row.get(3)?,
                    start_raw: row.get(4)?,
                    channel_name: row.get(5)?,
                    source_channel: row.get(6)?,
                    source_discord: row.get(7)?,
                    draft: row.get(8)?,
                })
            },
        )
        .optional()?;
    let Some(prompt) = row else {
        return Ok(None);
    };
    let EventPrompt {
        guild_id,
        user_id,
        step,
        title,
        start_raw,
        channel_name,
        source_channel,
        source_discord,
        draft,
    } = prompt;
    let mut draft = draft
        .as_deref()
        .and_then(|raw| serde_json::from_str::<EventDraft>(raw).ok())
        .unwrap_or_default();
    let text = text.trim();
    if text.eq_ignore_ascii_case("cancel") {
        cancel_event_dm(conn, discord_user_id)?;
        return just(DmReply::plain(
            "Cancelled. Run /event in the server when you want to try again.",
        ));
    }
    match step.as_str() {
        "channel" => answer_channel(
            conn,
            discord_user_id,
            guild_id,
            text,
            source_channel.as_deref(),
            source_discord.as_deref(),
        ),
        "channel_other" => {
            let others = other_channels(conn, guild_id, source_channel.as_deref())?;
            let choice = text.parse::<usize>().ok().filter(|n| *n >= 1);
            let Some(channel) = choice.and_then(|n| others.get(n - 1)) else {
                return just(other_channel_prompt(&others));
            };
            just(advance_to_title(conn, discord_user_id, &channel.name)?)
        }
        "title" => {
            if text.is_empty() || text.chars().count() > 120 {
                return just(note(
                    &title_card(),
                    "Give the event a title, up to 120 characters.",
                ));
            }
            conn.execute(
                "UPDATE event_prompts SET title = ?, step = 'description' WHERE discord_user_id = ?",
                rusqlite::params![text, discord_user_id],
            )?;
            just(description_card())
        }
        "description" => {
            if text.chars().count() > 2000 {
                return just(note(
                    &description_card(),
                    "Keep the description under 2000 characters.",
                ));
            }
            draft.description = if text.eq_ignore_ascii_case("none") {
                String::new()
            } else {
                text.to_string()
            };
            save_draft(conn, discord_user_id, "start", &draft)?;
            just(start_card(conn, guild_id)?)
        }
        "timezone" => {
            let choice = text.parse::<usize>().ok().filter(|n| *n >= 1);
            let Some(name) = choice.and_then(|n| clock::ZONES.get(n - 1)) else {
                return just(timezone_card());
            };
            conn.execute(
                "UPDATE guilds SET timezone = ? WHERE id = ?",
                rusqlite::params![name, guild_id],
            )?;
            save_draft(conn, discord_user_id, "start", &draft)?;
            just(start_card(conn, guild_id)?)
        }
        "start" => {
            if text.eq_ignore_ascii_case("timezone") {
                save_draft(conn, discord_user_id, "timezone", &draft)?;
                return just(timezone_card());
            }
            let guild = store::guild(conn, guild_id)?.ok_or(OpError::NotFound)?;
            let tz = clock::zone(&guild.timezone)?;
            let stored = if text.eq_ignore_ascii_case("now") {
                Some(clock::stamp(Utc::now()))
            } else if parse::parse_when(text, Utc::now(), tz).is_some() {
                Some(text.to_string())
            } else {
                None
            };
            let Some(stored) = stored else {
                return just(note(
                    &start_card(conn, guild_id)?,
                    "Marian couldn't read that time.",
                ));
            };
            conn.execute(
                "UPDATE event_prompts SET start_raw = ?, step = 'end' WHERE discord_user_id = ?",
                rusqlite::params![stored, discord_user_id],
            )?;
            just(end_card())
        }
        "end" => {
            let guild = store::guild(conn, guild_id)?.ok_or(OpError::NotFound)?;
            let tz = clock::zone(&guild.timezone)?;
            let start = parse::parse_when(start_raw.as_deref().unwrap_or(""), Utc::now(), tz)
                .ok_or_else(|| OpError::message("Marian couldn't read the start time."))?;
            match span_minutes(text, start, tz) {
                Ok(minutes) => {
                    draft.duration_minutes = minutes;
                    save_draft(conn, discord_user_id, "repeat", &draft)?;
                    just(repeat_card())
                }
                Err(err) => just(note(&end_card(), &err)),
            }
        }
        "repeat" => {
            let cadence = match text {
                "1" => None,
                "2" => Some("daily"),
                "3" => Some("weekly"),
                "4" => Some("biweekly"),
                "5" => Some("monthly"),
                _ => return just(repeat_card()),
            };
            draft.repeat = cadence.map(str::to_string);
            save_draft(conn, discord_user_id, "finish", &draft)?;
            just(finish_card(&draft))
        }
        "finish" => answer_finish(
            conn,
            discord_user_id,
            guild_id,
            user_id,
            text,
            title.as_deref(),
            start_raw.as_deref(),
            channel_name.as_deref(),
            &mut draft,
            public_url,
        ),
        "signup" => answer_signup(conn, discord_user_id, guild_id, text, &mut draft),
        "signup_custom" => {
            let names: Vec<&str> = text
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .collect();
            if names.is_empty() || names.iter().any(|name| name.chars().count() > 80) {
                return just(ask(
                    "Configure signup options",
                    "Send the option names, separated by commas.",
                ));
            }
            draft.options = names
                .into_iter()
                .enumerate()
                .map(|(index, name)| OptionInput {
                    id: None,
                    name: name.to_string(),
                    emoji: ["✅", "❌", "❔", "•"]
                        .get(index)
                        .copied()
                        .unwrap_or("•")
                        .into(),
                    capacity: None,
                    waitlist: false,
                    role_id: None,
                    reminders_enabled: index == 0,
                })
                .collect();
            save_draft(conn, discord_user_id, "finish", &draft)?;
            just(finish_card(&draft))
        }
        "signup_preset" => {
            let presets = store::list_presets(conn, guild_id)?;
            let choice = text.parse::<usize>().ok().filter(|n| *n >= 1);
            let Some(preset) = choice.and_then(|n| presets.get(n - 1)) else {
                return just(preset_card(&presets));
            };
            if let Ok(body) = serde_json::from_str::<PresetBody>(&preset.body) {
                draft.multiple = body.multiple_signups;
                draft.options = body.options;
            }
            save_draft(conn, discord_user_id, "finish", &draft)?;
            just(finish_card(&draft))
        }
        "mention" => answer_mention(conn, discord_user_id, guild_id, text, &mut draft),
        "image" => {
            if text.eq_ignore_ascii_case("none") {
                draft.image_url = None;
            } else if (text.starts_with("https://") || text.starts_with("http://"))
                && text.len() <= 500
            {
                draft.image_url = Some(text.to_string());
            } else {
                return just(ask(
                    "Add an image",
                    "Send an http:// or https:// link, or `none` to clear it.",
                ));
            }
            save_draft(conn, discord_user_id, "finish", &draft)?;
            just(finish_card(&draft))
        }
        "color" => {
            let choice = text.parse::<usize>().ok().filter(|n| *n >= 1);
            let Some((color, _)) = choice.and_then(|n| EVENT_COLORS.get(n - 1)) else {
                return just(color_card());
            };
            draft.color = (*color).to_string();
            save_draft(conn, discord_user_id, "finish", &draft)?;
            just(finish_card(&draft))
        }
        "restrict" => answer_restrict(conn, discord_user_id, guild_id, text, &mut draft),
        "role" => answer_attendee_role(conn, discord_user_id, guild_id, text, &mut draft),
        "cutoff" => answer_clock_field(
            conn,
            discord_user_id,
            guild_id,
            text,
            &mut draft,
            true,
            start_raw.as_deref(),
        ),
        "post" => answer_clock_field(
            conn,
            discord_user_id,
            guild_id,
            text,
            &mut draft,
            false,
            start_raw.as_deref(),
        ),
        "reminder" => {
            let minutes = text
                .parse::<i64>()
                .ok()
                .filter(|n| (1..=10_080).contains(n));
            let Some(minutes) = minutes else {
                return just(ask(
                    "Configure reminders",
                    "How many minutes before the start should Marian remind people? From 1 to 10080.",
                ));
            };
            draft.reminders = vec![ReminderInput {
                offset_minutes: minutes,
                message: String::new(),
            }];
            save_draft(conn, discord_user_id, "finish", &draft)?;
            just(finish_card(&draft))
        }
        "next" => answer_next(conn, discord_user_id, user_id, text, &draft, public_url),
        "respond" => answer_respond(conn, discord_user_id, user_id, text, &draft),
        _ => {
            cancel_event_dm(conn, discord_user_id)?;
            Ok(None)
        }
    }
}

struct EventPrompt {
    guild_id: i64,
    user_id: i64,
    step: String,
    title: Option<String>,
    start_raw: Option<String>,
    channel_name: Option<String>,
    source_channel: Option<String>,
    source_discord: Option<String>,
    draft: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct EventDraft {
    #[serde(default)]
    description: String,
    #[serde(default)]
    options: Vec<OptionInput>,
    #[serde(default)]
    multiple: bool,
    #[serde(default)]
    duration_minutes: i64,
    #[serde(default)]
    repeat: Option<String>,
    #[serde(default = "marian_blue")]
    color: String,
    #[serde(default)]
    image_url: Option<String>,
    #[serde(default)]
    mention_role_ids: Vec<i64>,
    #[serde(default)]
    restrict_role_ids: Vec<i64>,
    #[serde(default)]
    attendee_role_id: Option<i64>,
    #[serde(default)]
    cutoff_raw: Option<String>,
    #[serde(default)]
    post_raw: Option<String>,
    #[serde(default)]
    reminders: Vec<ReminderInput>,
    #[serde(default)]
    mention_everyone: bool,
    #[serde(default)]
    event_id: Option<i64>,
}

impl Default for EventDraft {
    fn default() -> Self {
        Self {
            description: String::new(),
            options: Vec::new(),
            multiple: false,
            duration_minutes: 0,
            repeat: None,
            color: marian_blue(),
            image_url: None,
            mention_role_ids: Vec::new(),
            restrict_role_ids: Vec::new(),
            attendee_role_id: None,
            cutoff_raw: None,
            post_raw: None,
            reminders: Vec::new(),
            mention_everyone: false,
            event_id: None,
        }
    }
}

fn marian_blue() -> String {
    "#1e3a6e".into()
}

const EVENT_COLORS: &[(&str, &str)] = &[
    ("#1e3a6e", "Marian blue"),
    ("#2f6b4f", "Tide green"),
    ("#8a6232", "Brass"),
    ("#6b3a6b", "Plum"),
    ("#8d3a32", "Seal red"),
    ("#243038", "Ink"),
];

fn just(reply: DmReply) -> Result<Option<Vec<DmReply>>, OpError> {
    Ok(Some(vec![reply]))
}

fn ask(title: &str, body: &str) -> DmReply {
    DmReply::card(title, format!("{body}\n\nTo exit, type 'cancel'"))
}

fn note(card: &DmReply, line: &str) -> DmReply {
    DmReply {
        title: card.title.clone(),
        body: format!("{line}\n\n{}", card.body),
    }
}

fn title_card() -> DmReply {
    ask(
        "Enter the event title",
        "Up to 120 characters are permitted",
    )
}

fn description_card() -> DmReply {
    ask(
        "Enter the event description",
        "Type `none` for no description. Up to 2000 characters are permitted.",
    )
}

fn end_card() -> DmReply {
    ask(
        "When does the event end?",
        "Type `none` for a 1 hour event.\n\n> 2 hours\n> 9pm\n> Friday at 6am",
    )
}

fn repeat_card() -> DmReply {
    numbered_prompt(
        "How often should this event repeat?",
        &[
            "Never".into(),
            "Daily".into(),
            "Weekly".into(),
            "Every two weeks".into(),
            "Monthly".into(),
        ],
    )
}

fn finish_card(draft: &EventDraft) -> DmReply {
    let mut notes = Vec::new();
    if !draft.options.is_empty() {
        let names = draft
            .options
            .iter()
            .map(|opt| opt.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        notes.push(format!("Signup options: {names}."));
    }
    if draft.multiple {
        notes.push("People can pick more than one option.".into());
    }
    let intro = if notes.is_empty() {
        "Don't need anything fancy?".to_string()
    } else {
        format!("{}\n\nDon't need anything fancy?", notes.join("\n"))
    };
    DmReply::card(
        "Finish or customize",
        format!(
            "{intro}\n**1** Finish event creation\n\nOr change something first:\n**2** Signup options\n**3** Mention roles when the event is posted\n**4** Add an image\n**5** Change the event color\n**6** Restrict signup to certain roles\n**7** Allow multiple responses\n**8** Assign a role to attendees\n**9** Set a time when signups close\n**10** Schedule when the event is posted\n**11** Configure reminders\n\nEnter a number to select an option\nTo exit, type 'cancel'"
        ),
    )
}

fn signup_card() -> DmReply {
    numbered_prompt(
        "Signup options",
        &[
            "Keep the defaults (Accepted, Declined, Tentative)".into(),
            "Configure signup options".into(),
            "Use a signup option preset".into(),
        ],
    )
}

fn next_card(note_line: Option<&str>) -> DmReply {
    let choices = "**1** Modify the event\n**2** Remove responses\n**3** Add a response\n**4** Duplicate the event";
    let body = match note_line {
        Some(line) => format!("{line}\n\n{choices}"),
        None => choices.to_string(),
    };
    DmReply::card(
        "What would you like to do?",
        format!("{body}\n\nEnter a number to select an option\nTo exit, type 'cancel'"),
    )
}

fn start_card(conn: &rusqlite::Connection, guild_id: i64) -> Result<DmReply, OpError> {
    let guild = store::guild(conn, guild_id)?.ok_or(OpError::NotFound)?;
    let tz = clock::zone(&guild.timezone)?;
    let label = clock::zone_label(tz);
    Ok(ask(
        "When should the event start?",
        &format!(
            "Your time zone is: **{label}**\nType `timezone` to change it.\n\n> Friday at 9pm\n> Tomorrow at 18:00\n> Now\n> In 1 hour\n> 2026-10-14 19:00"
        ),
    ))
}

fn timezone_card() -> DmReply {
    let lines = clock::ZONES
        .iter()
        .map(|name| {
            clock::zone(name)
                .map(|tz| clock::zone_label(tz))
                .unwrap_or_else(|_| (*name).to_string())
        })
        .collect::<Vec<_>>();
    numbered_prompt("Which time zone?", &lines)
}

fn color_card() -> DmReply {
    let lines = EVENT_COLORS
        .iter()
        .map(|(_, label)| (*label).to_string())
        .collect::<Vec<_>>();
    numbered_prompt("Change the event color", &lines)
}

fn preset_card(presets: &[store::Preset]) -> DmReply {
    if presets.is_empty() {
        return note(&signup_card(), "There are no signup presets yet.");
    }
    let lines = presets
        .iter()
        .map(|preset| preset.name.clone())
        .collect::<Vec<_>>();
    numbered_prompt("Use a signup option preset", &lines)
}

fn role_card(title: &str, roles: &[store::Role], extra: &str) -> DmReply {
    if roles.is_empty() {
        return ask(title, "This server has no roles yet.");
    }
    let lines = roles
        .iter()
        .enumerate()
        .map(|(index, role)| format!("**{}** {}", index + 1, role.name))
        .collect::<Vec<_>>()
        .join("\n");
    ask(title, &format!("{extra}\n\n{lines}"))
}

fn save_draft(
    conn: &rusqlite::Connection,
    discord_user_id: &str,
    step: &str,
    draft: &EventDraft,
) -> Result<(), OpError> {
    let json = serde_json::to_string(draft)
        .map_err(|err| OpError::message(format!("Could not keep the draft: {err}")))?;
    conn.execute(
        "UPDATE event_prompts SET step = ?, draft = ? WHERE discord_user_id = ?",
        rusqlite::params![step, json, discord_user_id],
    )?;
    Ok(())
}

fn advance_to_title(
    conn: &rusqlite::Connection,
    discord_user_id: &str,
    channel: &str,
) -> Result<DmReply, OpError> {
    conn.execute(
        "UPDATE event_prompts SET channel_name = ?, step = 'title' WHERE discord_user_id = ?",
        rusqlite::params![channel, discord_user_id],
    )?;
    Ok(title_card())
}

fn answer_channel(
    conn: &rusqlite::Connection,
    discord_user_id: &str,
    guild_id: i64,
    text: &str,
    source_channel: Option<&str>,
    source_discord: Option<&str>,
) -> Result<Option<Vec<DmReply>>, OpError> {
    let Some(source) = source_channel else {
        let channels = store::list_channels(conn, guild_id)?;
        let choice = text.parse::<usize>().ok().filter(|n| *n >= 1);
        let Some(channel) = choice.and_then(|n| channels.get(n - 1)) else {
            return just(where_to_post(conn, guild_id, &None, &None)?);
        };
        return just(advance_to_title(conn, discord_user_id, &channel.name)?);
    };
    let source_owned = Some(source.to_string());
    let discord_owned = source_discord.map(str::to_string);
    match text {
        "1" => just(advance_to_title(conn, discord_user_id, source)?),
        "2" => {
            let others = other_channels(conn, guild_id, Some(source))?;
            if others.is_empty() {
                return just(where_to_post(
                    conn,
                    guild_id,
                    &source_owned,
                    &discord_owned,
                )?);
            }
            conn.execute(
                "UPDATE event_prompts SET step = 'channel_other' WHERE discord_user_id = ?",
                [discord_user_id],
            )?;
            just(other_channel_prompt(&others))
        }
        _ => just(where_to_post(
            conn,
            guild_id,
            &source_owned,
            &discord_owned,
        )?),
    }
}

fn answer_finish(
    conn: &mut rusqlite::Connection,
    discord_user_id: &str,
    guild_id: i64,
    user_id: i64,
    text: &str,
    title: Option<&str>,
    start_raw: Option<&str>,
    channel_name: Option<&str>,
    draft: &mut EventDraft,
    public_url: &str,
) -> Result<Option<Vec<DmReply>>, OpError> {
    let step = match text {
        "1" => {
            return finish_event(
                conn,
                discord_user_id,
                guild_id,
                user_id,
                title,
                start_raw,
                channel_name,
                draft,
                public_url,
            )
        }
        "2" => {
            save_draft(conn, discord_user_id, "signup", draft)?;
            return just(signup_card());
        }
        "3" => "mention",
        "4" => {
            save_draft(conn, discord_user_id, "image", draft)?;
            return just(ask(
                "Add an image",
                "Send an http:// or https:// link, or `none` to clear it.",
            ));
        }
        "5" => {
            save_draft(conn, discord_user_id, "color", draft)?;
            return just(color_card());
        }
        "6" => "restrict",
        "7" => {
            draft.multiple = !draft.multiple;
            save_draft(conn, discord_user_id, "finish", draft)?;
            return just(finish_card(draft));
        }
        "8" => "role",
        "9" => {
            save_draft(conn, discord_user_id, "cutoff", draft)?;
            return just(ask(
                "Set a time when signups close",
                "For example, \"tomorrow at 3pm\", or `none` to leave signups open.",
            ));
        }
        "10" => {
            save_draft(conn, discord_user_id, "post", draft)?;
            return just(ask(
                "Schedule when the event is posted",
                "For example, \"tomorrow at 9am\", or `none` to post it now.",
            ));
        }
        "11" => {
            save_draft(conn, discord_user_id, "reminder", draft)?;
            return just(ask(
                "Configure reminders",
                "How many minutes before the start should Marian remind people? From 1 to 10080.",
            ));
        }
        _ => return just(finish_card(draft)),
    };
    let roles = store::list_roles(conn, guild_id)?;
    let (heading, extra) = match step {
        "mention" => (
            "Mention roles when the event is posted",
            "Send the numbers, separated by commas. `everyone` mentions @everyone. `none` clears mentions.",
        ),
        "restrict" => (
            "Restrict signup to certain roles",
            "Send the numbers, separated by commas, or `none` to clear the limit.",
        ),
        _ => (
            "Assign a role to attendees",
            "Send a number, or `none` for no role.",
        ),
    };
    save_draft(conn, discord_user_id, step, draft)?;
    if roles.is_empty() {
        save_draft(conn, discord_user_id, "finish", draft)?;
        return just(note(&finish_card(draft), "This server has no roles yet."));
    }
    just(role_card(heading, &roles, extra))
}

fn answer_signup(
    conn: &rusqlite::Connection,
    discord_user_id: &str,
    guild_id: i64,
    text: &str,
    draft: &mut EventDraft,
) -> Result<Option<Vec<DmReply>>, OpError> {
    match text {
        "1" => {
            draft.options.clear();
            draft.multiple = false;
            save_draft(conn, discord_user_id, "finish", draft)?;
            just(finish_card(draft))
        }
        "2" => {
            save_draft(conn, discord_user_id, "signup_custom", draft)?;
            just(ask(
                "Configure signup options",
                "Send the option names, separated by commas.",
            ))
        }
        "3" => {
            let presets = store::list_presets(conn, guild_id)?;
            save_draft(conn, discord_user_id, "signup_preset", draft)?;
            just(preset_card(&presets))
        }
        _ => just(signup_card()),
    }
}

fn answer_mention(
    conn: &rusqlite::Connection,
    discord_user_id: &str,
    guild_id: i64,
    text: &str,
    draft: &mut EventDraft,
) -> Result<Option<Vec<DmReply>>, OpError> {
    let roles = store::list_roles(conn, guild_id)?;
    if text.eq_ignore_ascii_case("none") {
        draft.mention_everyone = false;
        draft.mention_role_ids.clear();
    } else if text.eq_ignore_ascii_case("everyone") {
        draft.mention_everyone = true;
        draft.mention_role_ids.clear();
    } else {
        match picked_roles(text, &roles) {
            Ok(ids) => {
                draft.mention_everyone = false;
                draft.mention_role_ids = ids;
            }
            Err(err) => {
                return just(note(
                    &role_card(
                        "Mention roles when the event is posted",
                        &roles,
                        "Send the numbers, separated by commas. `everyone` mentions @everyone. `none` clears mentions.",
                    ),
                    &err,
                ));
            }
        }
    }
    save_draft(conn, discord_user_id, "finish", draft)?;
    just(finish_card(draft))
}

fn answer_restrict(
    conn: &rusqlite::Connection,
    discord_user_id: &str,
    guild_id: i64,
    text: &str,
    draft: &mut EventDraft,
) -> Result<Option<Vec<DmReply>>, OpError> {
    let roles = store::list_roles(conn, guild_id)?;
    if text.eq_ignore_ascii_case("none") {
        draft.restrict_role_ids.clear();
    } else {
        match picked_roles(text, &roles) {
            Ok(ids) => draft.restrict_role_ids = ids,
            Err(err) => {
                return just(note(
                    &role_card(
                        "Restrict signup to certain roles",
                        &roles,
                        "Send the numbers, separated by commas, or `none` to clear the limit.",
                    ),
                    &err,
                ));
            }
        }
    }
    save_draft(conn, discord_user_id, "finish", draft)?;
    just(finish_card(draft))
}

fn answer_attendee_role(
    conn: &rusqlite::Connection,
    discord_user_id: &str,
    guild_id: i64,
    text: &str,
    draft: &mut EventDraft,
) -> Result<Option<Vec<DmReply>>, OpError> {
    let roles = store::list_roles(conn, guild_id)?;
    if text.eq_ignore_ascii_case("none") {
        draft.attendee_role_id = None;
    } else {
        let choice = text.parse::<usize>().ok().filter(|n| *n >= 1);
        let Some(role) = choice.and_then(|n| roles.get(n - 1)) else {
            return just(note(
                &role_card(
                    "Assign a role to attendees",
                    &roles,
                    "Send a number, or `none` for no role.",
                ),
                "Send a number from the list.",
            ));
        };
        draft.attendee_role_id = Some(role.id);
    }
    save_draft(conn, discord_user_id, "finish", draft)?;
    just(finish_card(draft))
}

fn answer_clock_field(
    conn: &rusqlite::Connection,
    discord_user_id: &str,
    guild_id: i64,
    text: &str,
    draft: &mut EventDraft,
    cutoff: bool,
    start_raw: Option<&str>,
) -> Result<Option<Vec<DmReply>>, OpError> {
    let guild = store::guild(conn, guild_id)?.ok_or(OpError::NotFound)?;
    let tz = clock::zone(&guild.timezone)?;
    let heading = if cutoff {
        "Set a time when signups close"
    } else {
        "Schedule when the event is posted"
    };
    let hint = if cutoff {
        "For example, \"tomorrow at 3pm\", or `none` to leave signups open."
    } else {
        "For example, \"tomorrow at 9am\", or `none` to post it now."
    };
    if text.eq_ignore_ascii_case("none") {
        if cutoff {
            draft.cutoff_raw = None;
        } else {
            draft.post_raw = None;
        }
    } else if parse::parse_when(text, Utc::now(), tz).is_none() {
        return just(note(&ask(heading, hint), "Marian couldn't read that time."));
    } else if cutoff {
        if let Some(start) = start_raw.and_then(|raw| parse::parse_when(raw, Utc::now(), tz)) {
            if let Some(when) = parse::parse_when(text, Utc::now(), tz) {
                if when >= start {
                    return just(note(
                        &ask(heading, hint),
                        "Signups need to close before the event starts.",
                    ));
                }
            }
        }
        draft.cutoff_raw = Some(text.to_string());
    } else {
        draft.post_raw = Some(text.to_string());
    }
    save_draft(conn, discord_user_id, "finish", draft)?;
    just(finish_card(draft))
}

fn finish_event(
    conn: &mut rusqlite::Connection,
    discord_user_id: &str,
    guild_id: i64,
    user_id: i64,
    title: Option<&str>,
    start_raw: Option<&str>,
    channel_name: Option<&str>,
    draft: &mut EventDraft,
    public_url: &str,
) -> Result<Option<Vec<DmReply>>, OpError> {
    let guild = store::guild(conn, guild_id)?.ok_or(OpError::NotFound)?;
    let tz = clock::zone(&guild.timezone)?;
    let starts_at = parse::parse_when(start_raw.unwrap_or(""), Utc::now(), tz)
        .ok_or_else(|| OpError::message("Marian couldn't read the start time."))?;
    let channel = store::channel_by_name(conn, guild_id, channel_name.unwrap_or(""))?
        .ok_or_else(|| OpError::message("That channel is no longer saved."))?;
    let cutoff_at = draft
        .cutoff_raw
        .as_deref()
        .and_then(|raw| parse::parse_when(raw, Utc::now(), tz));
    let post_at = draft
        .post_raw
        .as_deref()
        .and_then(|raw| parse::parse_when(raw, Utc::now(), tz));
    let action = if post_at.is_some() {
        SaveAction::Schedule
    } else {
        SaveAction::Publish
    };
    let repeat = draft.repeat.clone().map(|cadence| RepeatSpec {
        cadence,
        end_on: None,
        max_occurrences: None,
    });
    let options = if draft.options.is_empty() {
        yes_no_maybe()
    } else {
        draft.options.clone()
    };
    let duration = if draft.duration_minutes == 0 {
        60
    } else {
        draft.duration_minutes
    };
    let saved = logic::save_event(
        conn,
        EventInput {
            id: None,
            guild_id,
            actor_id: user_id,
            channel_id: channel.id,
            title: title.unwrap_or("Event").to_string(),
            description: draft.description.clone(),
            starts_at,
            duration_minutes: duration,
            color: draft.color.clone(),
            image_url: draft.image_url.clone(),
            cutoff_at,
            attendee_role_id: draft.attendee_role_id,
            thread_enabled: store::settings(conn, guild_id)?.event_threads,
            multiple_signups: draft.multiple,
            mention_everyone: draft.mention_everyone,
            mention_role_ids: draft.mention_role_ids.clone(),
            restrict_role_ids: draft.restrict_role_ids.clone(),
            options,
            reminders: draft.reminders.clone(),
            repeat,
            action,
            post_at,
            apply_series: false,
            vote: None,
        },
    );
    let id = match saved {
        Ok(id) => id,
        Err(err) => return just(note(&finish_card(draft), &err.to_string())),
    };
    draft.event_id = Some(id);
    save_draft(conn, discord_user_id, "next", draft)?;
    let event = store::event(conn, id)?.ok_or(OpError::NotFound)?;
    let path = event_path(&guild, &event.public_id);
    let created = if public_url.trim().is_empty() {
        DmReply::card("Event has been created!", format!("Open {path}."))
    } else {
        let link = format!("{}{path}", public_url.trim().trim_end_matches('/'));
        DmReply::card(
            "Event has been created!",
            format!("[Click here to view the event]({link})"),
        )
    };
    Ok(Some(vec![created, next_card(None)]))
}

fn answer_next(
    conn: &mut rusqlite::Connection,
    discord_user_id: &str,
    user_id: i64,
    text: &str,
    draft: &EventDraft,
    public_url: &str,
) -> Result<Option<Vec<DmReply>>, OpError> {
    let Some(event_id) = draft.event_id else {
        cancel_event_dm(conn, discord_user_id)?;
        return Ok(None);
    };
    match text {
        "1" => {
            let event = store::event(conn, event_id)?.ok_or(OpError::NotFound)?;
            let guild = store::guild(conn, event.guild_id)?.ok_or(OpError::NotFound)?;
            let path = event_path(&guild, &event.public_id);
            let line = if public_url.trim().is_empty() {
                format!("Open {path}.")
            } else {
                format!(
                    "[Open the event]({}{path})",
                    public_url.trim().trim_end_matches('/')
                )
            };
            just(next_card(Some(&line)))
        }
        "2" => {
            conn.execute("DELETE FROM signups WHERE event_id = ?", [event_id])?;
            just(next_card(Some("Removed the responses.")))
        }
        "3" => {
            save_draft(conn, discord_user_id, "respond", draft)?;
            let options = store::options_for(conn, event_id)?;
            let lines = options
                .iter()
                .map(|opt| format!("{} {}", opt.emoji, opt.name))
                .collect::<Vec<_>>();
            just(numbered_prompt("Add a response", &lines))
        }
        "4" => {
            let id = logic::duplicate_event(conn, event_id, user_id)?;
            just(next_card(Some(&format!(
                "Duplicated it. The copy is event {id}, one week later."
            ))))
        }
        _ => just(next_card(None)),
    }
}

fn answer_respond(
    conn: &mut rusqlite::Connection,
    discord_user_id: &str,
    user_id: i64,
    text: &str,
    draft: &EventDraft,
) -> Result<Option<Vec<DmReply>>, OpError> {
    let Some(event_id) = draft.event_id else {
        cancel_event_dm(conn, discord_user_id)?;
        return Ok(None);
    };
    let options = store::options_for(conn, event_id)?;
    let choice = text.parse::<usize>().ok().filter(|n| *n >= 1);
    let Some(option) = choice.and_then(|n| options.get(n - 1)) else {
        let lines = options
            .iter()
            .map(|opt| format!("{} {}", opt.emoji, opt.name))
            .collect::<Vec<_>>();
        return just(numbered_prompt("Add a response", &lines));
    };
    let reply = logic::signup(conn, event_id, option.id, user_id)?;
    save_draft(conn, discord_user_id, "next", draft)?;
    just(next_card(Some(&reply)))
}

fn picked_roles(text: &str, roles: &[store::Role]) -> Result<Vec<i64>, String> {
    let mut ids = Vec::new();
    for part in text.split(|c: char| c == ',' || c.is_whitespace()) {
        if part.is_empty() {
            continue;
        }
        let n: usize = part
            .parse()
            .map_err(|_| "Send the numbers, separated by commas.".to_string())?;
        if n == 0 {
            return Err("Send a number from the list.".into());
        }
        let role = roles
            .get(n - 1)
            .ok_or_else(|| "That number isn't in the list.".to_string())?;
        ids.push(role.id);
    }
    if ids.is_empty() {
        return Err("Send the numbers, separated by commas.".into());
    }
    Ok(ids)
}

fn span_minutes(text: &str, start: DateTime<Utc>, tz: Tz) -> Result<i64, String> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("none") {
        return Ok(60);
    }
    if let Some(minutes) = hour_span(text) {
        return fit_span(minutes);
    }
    let Some(mut end) = parse::parse_when(text, Utc::now(), tz) else {
        return Err("Try \"2 hours\", \"9pm\", \"Friday at 6am\", or `none` for 1 hour.".into());
    };
    if end <= start {
        end += Duration::days(1);
    }
    fit_span((end - start).num_minutes())
}

fn hour_span(text: &str) -> Option<i64> {
    let lower = text.to_lowercase();
    let mut parts = lower.split_whitespace();
    let count: i64 = parts.next()?.parse().ok()?;
    let unit = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if unit.starts_with("hour") {
        Some(count * 60)
    } else if unit.starts_with("min") {
        Some(count)
    } else {
        None
    }
}

fn fit_span(minutes: i64) -> Result<i64, String> {
    if (15..=1440).contains(&minutes) {
        Ok(minutes)
    } else {
        Err("The event needs to last between 15 minutes and 24 hours.".into())
    }
}

fn where_to_post(
    conn: &rusqlite::Connection,
    guild_id: i64,
    source_channel: &Option<String>,
    source_discord: &Option<String>,
) -> Result<DmReply, OpError> {
    let guild = store::guild(conn, guild_id)?.ok_or(OpError::NotFound)?;
    let Some(source) = source_channel else {
        let lines = store::list_channels(conn, guild_id)?
            .into_iter()
            .map(|channel| channel_label(&channel))
            .collect::<Vec<_>>();
        return Ok(numbered_prompt(
            "Where would you like to post this event?",
            &lines,
        ));
    };
    let here = source_discord
        .as_deref()
        .filter(|id| !id.is_empty())
        .map(|id| format!("<#{id}>"))
        .unwrap_or_else(|| format!("{} #{source}", guild.name));
    Ok(numbered_prompt(
        "Where would you like to post this event?",
        &[
            format!("In the current channel, {here}"),
            "In another channel".into(),
        ],
    ))
}

fn other_channels(
    conn: &rusqlite::Connection,
    guild_id: i64,
    source_channel: Option<&str>,
) -> Result<Vec<store::Channel>, OpError> {
    Ok(store::list_channels(conn, guild_id)?
        .into_iter()
        .filter(|channel| source_channel != Some(channel.name.as_str()))
        .collect())
}

fn other_channel_prompt(channels: &[store::Channel]) -> DmReply {
    let lines = channels.iter().map(channel_label).collect::<Vec<_>>();
    numbered_prompt("Where would you like to post this event?", &lines)
}

fn channel_label(channel: &store::Channel) -> String {
    channel
        .discord_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .map(|id| format!("<#{id}>"))
        .unwrap_or_else(|| format!("#{}", channel.name))
}

fn numbered_prompt(heading: &str, lines: &[String]) -> DmReply {
    let body = lines
        .iter()
        .enumerate()
        .map(|(index, line)| format!("**{}** {line}", index + 1))
        .collect::<Vec<_>>()
        .join("\n");
    DmReply::card(
        heading,
        format!("{body}\n\nEnter a number to select an option\nTo exit, type 'cancel'"),
    )
}

fn cmd_edit(
    conn: &mut rusqlite::Connection,
    guild_id: i64,
    user_id: i64,
    pos: &[String],
    map: &HashMap<String, String>,
) -> Result<String, OpError> {
    let token = map
        .get("id")
        .or_else(|| pos.first())
        .map(String::as_str)
        .ok_or_else(|| OpError::message("Say which event, like /edit 12 title:\"New name\"."))?;
    let event = find_event(conn, guild_id, token)?;
    let guild = store::guild(conn, event.guild_id)?.ok_or(OpError::NotFound)?;
    let tz = clock::zone(&guild.timezone)?;
    let mut input = input_from(conn, &event, user_id)?;
    if let Some(title) = map.get("title") {
        input.title = title.clone();
    }
    if let Some(description) = map.get("description") {
        input.description = description.clone();
    }
    if let Some(start) = map.get("start") {
        input.starts_at = parse::parse_when(start, Utc::now(), tz).ok_or_else(|| {
            OpError::message(format!("Marian couldn't read the time \"{start}\"."))
        })?;
    }
    if let Some(duration) = map.get("duration").and_then(|value| value.parse().ok()) {
        input.duration_minutes = duration;
    }
    if let Some(name) = map.get("channel") {
        input.channel_id = store::channel_by_name(conn, event.guild_id, name)?
            .ok_or_else(|| OpError::message(format!("No channel named {name}.")))?
            .id;
    }
    input.action = SaveAction::Save;
    let id = logic::save_event(conn, input)?;
    let event = store::event(conn, id)?.ok_or(OpError::NotFound)?;
    Ok(format!(
        "Updated {}. Open {}.",
        event.title,
        event_path(&guild, &event.public_id)
    ))
}

fn cmd_events(conn: &rusqlite::Connection, guild_id: i64) -> Result<String, OpError> {
    let guild = store::guild(conn, guild_id)?.ok_or(OpError::NotFound)?;
    let tz = clock::zone(&guild.timezone)?;
    let settings = store::settings(conn, guild_id)?;
    let mut lines = Vec::new();
    for event in store::list_events(conn, guild_id)? {
        if !matches!(event.status.as_str(), "posted" | "scheduled" | "draft") {
            continue;
        }
        let start = clock::parse_stamp(&event.starts_at)?;
        lines.push(format!(
            "{}  {}  {}  {}",
            event.public_id,
            logic::status_label(&event.status),
            clock::format_when(start, tz, settings.hour_24),
            event.title
        ));
    }
    if lines.is_empty() {
        Ok("Nothing upcoming. /event title:\"Game night\" start:\"friday at 7pm\"".into())
    } else {
        Ok(lines.join("\n"))
    }
}

fn cmd_reminders(
    conn: &mut rusqlite::Connection,
    user_id: i64,
    pos: &[String],
    map: &HashMap<String, String>,
) -> Result<String, OpError> {
    let raw = map
        .get("state")
        .cloned()
        .or_else(|| pos.first().cloned())
        .unwrap_or_else(|| "status".into());
    match raw.to_lowercase().as_str() {
        "off" | "no" | "false" => logic::set_user_reminders(conn, user_id, false),
        "on" | "yes" | "true" => logic::set_user_reminders(conn, user_id, true),
        _ => {
            let user = store::user(conn, user_id)?.ok_or(OpError::NotFound)?;
            Ok(if user.reminders_enabled {
                format!("{} gets reminders. /reminders off to opt out.", user.name)
            } else {
                format!(
                    "{} is opted out. /reminders on to hear them again.",
                    user.name
                )
            })
        }
    }
}

fn cmd_response(
    conn: &mut rusqlite::Connection,
    actor_id: i64,
    pos: &[String],
    map: &HashMap<String, String>,
) -> Result<String, OpError> {
    let action = map
        .get("action")
        .cloned()
        .or_else(|| pos.first().cloned())
        .unwrap_or_else(|| "add".into());
    let token = map
        .get("event")
        .or_else(|| pos.get(1))
        .map(String::as_str)
        .ok_or_else(|| OpError::message("Name the event id."))?;
    let actor = store::user(conn, actor_id)?.ok_or(OpError::NotFound)?;
    let handle = map
        .get("user")
        .cloned()
        .or_else(|| pos.get(2).cloned())
        .ok_or_else(|| OpError::message("Name the person by handle."))?;
    let user = store::user_by_handle(conn, actor.guild_id, &handle)?
        .ok_or_else(|| OpError::message(format!("Nobody here goes by {handle}.")))?;
    let option_name = map
        .get("option")
        .cloned()
        .or_else(|| pos.get(3).cloned())
        .ok_or_else(|| OpError::message("Name the signup option."))?;
    Ok(roster_action(conn, actor_id, token, user.id, &option_name, &action)?.0)
}

pub struct Choice {
    pub label: String,
    pub value: String,
}

pub fn response_events(
    conn: &rusqlite::Connection,
    guild_id: i64,
    partial: &str,
    now: DateTime<Utc>,
) -> Result<Vec<Choice>, OpError> {
    let guild = store::guild(conn, guild_id)?.ok_or(OpError::NotFound)?;
    let tz = clock::zone(&guild.timezone).unwrap_or(Tz::UTC);
    let needle = partial.trim().to_lowercase();
    let mut choices = Vec::new();
    for event in store::list_events(conn, guild_id)? {
        if event.status != "posted" {
            continue;
        }
        let Ok(start) = clock::parse_stamp(&event.starts_at) else {
            continue;
        };
        let end = start + Duration::minutes(event.duration_minutes);
        if end < now {
            continue;
        }
        let start_label = menu_date(start, tz);
        let end_label = menu_date(end, tz);
        let label = event_menu_label(&event.title, &start_label, &end_label);
        let typed = partial.trim();
        if !needle.is_empty()
            && !event.title.to_lowercase().contains(&needle)
            && !label.to_lowercase().contains(&needle)
            && event.public_id != typed
        {
            continue;
        }
        choices.push(Choice {
            label,
            value: event.public_id,
        });
        if choices.len() == 25 {
            break;
        }
    }
    Ok(choices)
}

pub fn response_options(
    conn: &rusqlite::Connection,
    guild_id: i64,
    event_token: &str,
    partial: &str,
) -> Result<Vec<Choice>, OpError> {
    let event = match find_event(conn, guild_id, event_token) {
        Ok(event) => event,
        Err(_) => return Ok(Vec::new()),
    };
    let needle = partial.trim().to_lowercase();
    let mut choices = Vec::new();
    for opt in store::options_for(conn, event.id)? {
        let label = option_menu_label(&opt.emoji, &opt.name);
        if !needle.is_empty()
            && !opt.name.to_lowercase().contains(&needle)
            && !label.to_lowercase().contains(&needle)
            && opt.id.to_string() != partial.trim()
        {
            continue;
        }
        choices.push(Choice {
            label,
            value: opt.id.to_string(),
        });
        if choices.len() == 25 {
            break;
        }
    }
    Ok(choices)
}

pub fn roster_action(
    conn: &mut rusqlite::Connection,
    actor_id: i64,
    event_token: &str,
    target_id: i64,
    option_token: &str,
    action: &str,
) -> Result<(String, i64), OpError> {
    let actor = store::user(conn, actor_id)?.ok_or(OpError::NotFound)?;
    let event = find_event(conn, actor.guild_id, event_token)?;
    let option = resolve_option(conn, event.id, option_token)?;
    let notice = logic::organizer_response(conn, event.id, actor_id, target_id, option.id, action)?;
    Ok((notice, event.id))
}

fn resolve_option(
    conn: &rusqlite::Connection,
    event_id: i64,
    token: &str,
) -> Result<store::SignupOption, OpError> {
    let options = store::options_for(conn, event_id)?;
    if let Some(opt) = options
        .iter()
        .find(|opt| opt.name.eq_ignore_ascii_case(token))
    {
        return Ok(opt.clone());
    }
    if let Ok(id) = token.parse::<i64>() {
        if let Some(opt) = options.into_iter().find(|opt| opt.id == id) {
            return Ok(opt);
        }
    }
    Err(OpError::message(format!("No option named {token}.")))
}

fn menu_date(dt: DateTime<Utc>, tz: Tz) -> String {
    let local = dt.with_timezone(&tz).date_naive();
    let day = local.format("%d").to_string();
    let day = day.trim_start_matches('0');
    format!("{} {day}", local.format("%b"))
}

fn event_menu_label(title: &str, start: &str, end: &str) -> String {
    let dates = if start == end {
        start.to_string()
    } else {
        format!("{start} - {end}")
    };
    let suffix = format!(" -- {dates}");
    let room = 100usize.saturating_sub(suffix.chars().count());
    let title: String = title.chars().take(room).collect();
    format!("{title}{suffix}").chars().take(100).collect()
}

fn option_menu_label(emoji: &str, name: &str) -> String {
    let label = if emoji.trim().is_empty() {
        name.to_string()
    } else {
        format!("{} {name}", emoji.trim())
    };
    label.chars().take(100).collect()
}

fn cmd_settings(
    conn: &mut rusqlite::Connection,
    guild_id: i64,
    user_id: i64,
    pos: &[String],
    map: &HashMap<String, String>,
) -> Result<String, OpError> {
    require_organizer(conn, user_id)?;
    let key = map
        .keys()
        .next()
        .cloned()
        .or_else(|| pos.first().cloned())
        .ok_or_else(|| OpError::message("Name a setting. Try /settings reminder_interval 30."))?;
    let value = map
        .get(&key)
        .cloned()
        .or_else(|| pos.get(1).cloned())
        .unwrap_or_default();
    let extra = pos.get(2).cloned().unwrap_or_default();
    let mut settings = store::settings(conn, guild_id)?;
    match key.as_str() {
        "reminder_interval" => {
            settings.default_reminder_minutes = value
                .parse()
                .map_err(|_| OpError::message("Use a number of minutes."))?
        }
        "24_hour_time" => settings.hour_24 = on(&value)?,
        "auto_archive" => settings.auto_archive = on(&value)?,
        "auto_archive_interval" => {
            settings.auto_archive_hours = value
                .parse()
                .map_err(|_| OpError::message("Use a number of hours."))?
        }
        "auto_join_threads" => settings.auto_join_threads = on(&value)?,
        "event_threads" => settings.event_threads = on(&value)?,
        "localize_time" => settings.localize_time = on(&value)?,
        "purge" => settings.purge_channels = on(&value)?,
        "thread_reminders" => settings.thread_reminders = on(&value)?,
        "show_attendees" => settings.show_attendees = on(&value)?,
        "temp_roles" => settings.temp_roles = on(&value)?,
        "park_clock" => settings.park_clock = on(&value)?,
        "digest" => {
            if !matches!(value.as_str(), "off" | "daily" | "weekly") {
                return Err(OpError::message("Digest can be off, daily, or weekly."));
            }
            settings.digest = value.clone();
        }
        "post_lead" => {
            settings.post_lead_hours = value
                .parse()
                .map_err(|_| OpError::message("Use a number of hours."))?
        }
        "role" => {
            let which = value;
            let role_name = extra;
            let role_id = if role_name.eq_ignore_ascii_case("clear") || role_name.is_empty() {
                None
            } else {
                Some(
                    store::role_by_name(conn, guild_id, &role_name)?
                        .ok_or_else(|| OpError::message(format!("No role named {role_name}.")))?
                        .id,
                )
            };
            match which.as_str() {
                "event" => settings.role_event = role_id,
                "edit" => settings.role_edit = role_id,
                "delete" => settings.role_delete = role_id,
                "channel" => settings.role_channel = role_id,
                "signup_options" | "signup" => settings.role_signup = role_id,
                other => return Err(OpError::message(format!("Unknown role setting {other}."))),
            }
        }
        other => return Err(OpError::message(format!("Unknown setting {other}."))),
    }
    store::update_settings(conn, &settings)?;
    Ok(format!("Updated {key}."))
}

fn cmd_signup_options(
    conn: &mut rusqlite::Connection,
    guild_id: i64,
    user_id: i64,
    pos: &[String],
    map: &HashMap<String, String>,
) -> Result<String, OpError> {
    let action = pos.first().map(String::as_str).unwrap_or("list");
    match action {
        "add" => {
            let name = map
                .get("name")
                .cloned()
                .or_else(|| pos.get(2).cloned())
                .or_else(|| pos.get(1).cloned())
                .ok_or_else(|| OpError::message("Name the option."))?;
            let emoji = map.get("emoji").cloned().unwrap_or_else(|| "•".into());
            logic::library_add(conn, guild_id, user_id, &name, &emoji)?;
            Ok(format!("Saved {emoji} {name} in the option library."))
        }
        "remove" => {
            let name = map
                .get("name")
                .cloned()
                .or_else(|| pos.get(1).cloned())
                .unwrap_or_default();
            logic::library_remove(conn, guild_id, user_id, &name)?;
            Ok(format!("Removed {name} from the library."))
        }
        _ => {
            let lines: Vec<_> = store::list_library(conn, guild_id)?
                .into_iter()
                .map(|opt| format!("{} {}", opt.emoji, opt.name))
                .collect();
            if lines.is_empty() {
                Ok("The option library is empty.".into())
            } else {
                Ok(lines.join("\n"))
            }
        }
    }
}

fn cmd_presets(
    conn: &mut rusqlite::Connection,
    guild_id: i64,
    user_id: i64,
    pos: &[String],
    map: &HashMap<String, String>,
) -> Result<String, OpError> {
    let action = pos.first().map(String::as_str).unwrap_or("list");
    match action {
        "add" => {
            let name = map
                .get("name")
                .cloned()
                .or_else(|| pos.get(1).cloned())
                .ok_or_else(|| OpError::message("Name the preset."))?;
            let raw = map.get("options").cloned().unwrap_or_default();
            let options = raw
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(|part| OptionInput {
                    id: None,
                    name: part.to_string(),
                    emoji: "•".into(),
                    capacity: None,
                    waitlist: false,
                    role_id: None,
                    reminders_enabled: true,
                })
                .collect();
            logic::save_preset(
                conn,
                guild_id,
                user_id,
                &name,
                &PresetBody {
                    multiple_signups: false,
                    options,
                },
            )?;
            Ok(format!("Saved the {name} preset."))
        }
        "remove" => {
            let name = map
                .get("name")
                .cloned()
                .or_else(|| pos.get(1).cloned())
                .unwrap_or_default();
            if let Some(preset) = store::list_presets(conn, guild_id)?
                .into_iter()
                .find(|preset| preset.name.eq_ignore_ascii_case(&name))
            {
                logic::delete_preset(conn, preset.id, user_id)?;
            }
            Ok(format!("Removed {name}."))
        }
        _ => {
            let lines: Vec<_> = store::list_presets(conn, guild_id)?
                .into_iter()
                .map(|preset| preset.name)
                .collect();
            if lines.is_empty() {
                Ok("No presets yet.".into())
            } else {
                Ok(lines.join("\n"))
            }
        }
    }
}

fn cmd_channel(
    conn: &mut rusqlite::Connection,
    guild_id: i64,
    user_id: i64,
    pos: &[String],
    map: &HashMap<String, String>,
) -> Result<String, OpError> {
    let name = map
        .get("name")
        .cloned()
        .or_else(|| pos.first().cloned())
        .ok_or_else(|| OpError::message("Name the channel."))?;
    let kind = map.get("kind").cloned().unwrap_or_else(|| "text".into());
    let id = logic::create_channel(conn, guild_id, user_id, &name, &kind)?;
    let channel = store::channel(conn, id)?.ok_or(OpError::NotFound)?;
    Ok(format!("Created #{}.", channel.name))
}

fn cmd_google(
    conn: &mut rusqlite::Connection,
    guild_id: i64,
    user_id: i64,
    pos: &[String],
    map: &HashMap<String, String>,
) -> Result<String, OpError> {
    let action = pos.first().map(String::as_str).unwrap_or("authorize");
    match action {
        "authorize" => {
            let user = store::user(conn, user_id)?.ok_or(OpError::NotFound)?;
            let discord_id = user.discord_id.as_deref().ok_or_else(|| {
                OpError::message(
                    "Marian needs your Discord account before it can make a calendar link.",
                )
            })?;
            let token = store::ensure_calendar_token(conn, discord_id)?;
            Ok(format!(
                "Subscribe your calendar app to /calendar/{token}.ics. It lists events you accepted and events you created. The link is only yours. Marian does not ask Google for access. Add to Google Calendar on an event still opens one date."
            ))
        }
        "auto_sync" => {
            require_organizer(conn, user_id)?;
            let mut settings = store::settings(conn, guild_id)?;
            let value = map
                .get("state")
                .cloned()
                .or_else(|| pos.get(1).cloned())
                .unwrap_or_else(|| "on".into());
            settings.calendar_auto_sync = on(&value)?;
            store::update_settings(conn, &settings)?;
            Ok(if settings.calendar_auto_sync {
                "New posts are marked on the calendar feed.".into()
            } else {
                "Automatic calendar marking is off. The feed still lists posted events.".into()
            })
        }
        "revoke" => {
            require_organizer(conn, user_id)?;
            let mut settings = store::settings(conn, guild_id)?;
            settings.calendar_auto_sync = false;
            store::update_settings(conn, &settings)?;
            Ok("Turned automatic calendar marking off.".into())
        }
        "sync" => {
            let id: i64 = pos
                .get(1)
                .and_then(|value| value.parse().ok())
                .or_else(|| map.get("event").and_then(|value| value.parse().ok()))
                .ok_or_else(|| OpError::message("Say which event to sync."))?;
            logic::manual_calendar_sync(conn, id, user_id)
        }
        other => Err(OpError::message(format!("Unknown google command {other}."))),
    }
}

fn input_from(
    conn: &rusqlite::Connection,
    event: &Event,
    actor_id: i64,
) -> Result<EventInput, OpError> {
    let options = store::options_for(conn, event.id)?;
    let reminders = store::reminders_for(conn, event.id)?;
    Ok(EventInput {
        id: Some(event.id),
        guild_id: event.guild_id,
        actor_id,
        channel_id: event.channel_id,
        title: event.title.clone(),
        description: event.description.clone(),
        starts_at: clock::parse_stamp(&event.starts_at)?,
        duration_minutes: event.duration_minutes,
        color: event.color.clone(),
        image_url: event.image_url.clone(),
        cutoff_at: event
            .cutoff_at
            .as_deref()
            .map(clock::parse_stamp)
            .transpose()?,
        attendee_role_id: event.attendee_role_id,
        thread_enabled: event.thread_enabled,
        multiple_signups: event.multiple_signups,
        mention_everyone: event.mention_everyone,
        mention_role_ids: store::event_role_ids(conn, event.id, "mention")?,
        restrict_role_ids: store::event_role_ids(conn, event.id, "restrict")?,
        options: options
            .into_iter()
            .map(|opt| OptionInput {
                id: Some(opt.id),
                name: opt.name,
                emoji: opt.emoji,
                capacity: opt.capacity,
                waitlist: opt.waitlist,
                role_id: opt.role_id,
                reminders_enabled: opt.reminders_enabled,
            })
            .collect(),
        reminders: reminders
            .into_iter()
            .map(|rem| ReminderInput {
                offset_minutes: rem.offset_minutes,
                message: rem.message,
            })
            .collect(),
        repeat: None,
        action: SaveAction::Save,
        post_at: event
            .post_at
            .as_deref()
            .map(clock::parse_stamp)
            .transpose()?,
        apply_series: false,
        vote: None,
    })
}

fn yes_no_maybe() -> Vec<OptionInput> {
    vec![
        OptionInput {
            id: None,
            name: "Accepted".into(),
            emoji: "✅".into(),
            capacity: None,
            waitlist: false,
            role_id: None,
            reminders_enabled: true,
        },
        OptionInput {
            id: None,
            name: "Declined".into(),
            emoji: "❌".into(),
            capacity: None,
            waitlist: false,
            role_id: None,
            reminders_enabled: false,
        },
        OptionInput {
            id: None,
            name: "Tentative".into(),
            emoji: "❔".into(),
            capacity: None,
            waitlist: false,
            role_id: None,
            reminders_enabled: false,
        },
    ]
}

fn require_organizer(conn: &rusqlite::Connection, user_id: i64) -> Result<(), OpError> {
    let user = store::user(conn, user_id)?.ok_or(OpError::NotFound)?;
    if user.organizer {
        Ok(())
    } else {
        Err(OpError::message("Only a server organizer can change that."))
    }
}

fn on(value: &str) -> Result<bool, OpError> {
    match value.to_lowercase().as_str() {
        "on" | "yes" | "true" | "1" => Ok(true),
        "off" | "no" | "false" | "0" => Ok(false),
        _ => Err(OpError::message("Use on or off.")),
    }
}

fn split_cmd(line: &str) -> (&str, &str) {
    let line = line.trim().trim_start_matches('/');
    let mut parts = line.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("").trim();
    let rest = parts.next().unwrap_or("").trim();
    (cmd, rest)
}

fn fields(rest: &str) -> (Vec<String>, HashMap<String, String>) {
    let mut pos = Vec::new();
    let mut map = HashMap::new();
    for (key, value) in tokenize(rest) {
        if let Some(key) = key {
            map.insert(key, value);
        } else {
            pos.push(value);
        }
    }
    (pos, map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn only(result: Result<Option<Vec<DmReply>>, OpError>) -> DmReply {
        let mut replies = result.unwrap().expect("a reply");
        assert_eq!(replies.len(), 1);
        replies.remove(0)
    }

    #[test]
    fn event_command_creates_a_posted_event() {
        let mut conn = store::open_memory().unwrap();
        conn.execute_batch(
            "
            INSERT INTO guilds (id, name, timezone) VALUES (1, 'Guild', 'UTC');
            INSERT INTO channels (id, guild_id, name, kind, position) VALUES (1, 1, 'social', 'text', 0);
            INSERT INTO users (id, guild_id, name, handle, color, reminders_enabled, organizer)
              VALUES (1, 1, 'Nell', 'nell', '#112233', 1, 1);
            ",
        )
        .unwrap();
        store::insert_default_settings(&conn, 1).unwrap();
        let reply = execute(
            &mut conn,
            1,
            1,
            r#"/event title:"Shore cleanup" start:"tomorrow at 4pm" channel:social duration:90"#,
        );
        assert!(reply.contains("Shore cleanup"), "{reply}");
    }

    #[test]
    fn event_dm_asks_then_posts() {
        let mut conn = store::open_memory().unwrap();
        conn.execute_batch(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Guild', 'UTC', '4242');
            INSERT INTO channels (id, guild_id, name, kind, position, discord_id)
              VALUES (1, 1, 'social', 'text', 0, '99'), (2, 1, 'briefing', 'text', 1, '100');
            INSERT INTO users (id, guild_id, name, handle, color, reminders_enabled, organizer)
              VALUES (1, 1, 'Nell', 'nell', '#112233', 1, 1);
            ",
        )
        .unwrap();
        store::insert_default_settings(&conn, 1).unwrap();
        let first = start_event_dm(&mut conn, 1, 1, "u1", "social", Some("99")).unwrap();
        assert_eq!(
            first.title.as_deref(),
            Some("Where would you like to post this event?")
        );
        assert!(
            first.body.contains("**1** In the current channel, <#99>"),
            "{first}"
        );
        assert!(first.body.contains("**2** In another channel"), "{first}");
        assert!(
            first.body.contains("Enter a number to select an option"),
            "{first}"
        );
        assert!(first.body.contains("To exit, type 'cancel'"), "{first}");
        let still = only(answer_event_dm(&mut conn, "u1", "9", ""));
        assert!(still.title.is_some(), "{still}");
        assert!(still.body.contains("In the current channel"), "{still}");
        let other = only(answer_event_dm(&mut conn, "u1", "2", ""));
        assert!(other.body.contains("**1** <#100>"), "{other}");
        let title = only(answer_event_dm(&mut conn, "u1", "1", ""));
        assert_eq!(title.title.as_deref(), Some("Enter the event title"));
        let description = only(answer_event_dm(&mut conn, "u1", "Shore cleanup", ""));
        assert_eq!(
            description.title.as_deref(),
            Some("Enter the event description")
        );
        let start = only(answer_event_dm(&mut conn, "u1", "none", ""));
        assert_eq!(start.title.as_deref(), Some("When should the event start?"));
        assert!(!start.body.contains("Signup"), "{start}");
        let again = only(answer_event_dm(&mut conn, "u1", "not a time", ""));
        assert!(again.body.contains("couldn't read"), "{again}");
        let end = only(answer_event_dm(&mut conn, "u1", "tomorrow at 4pm", ""));
        assert_eq!(end.title.as_deref(), Some("When does the event end?"));
        let repeat = only(answer_event_dm(&mut conn, "u1", "2 hours", ""));
        assert_eq!(
            repeat.title.as_deref(),
            Some("How often should this event repeat?")
        );
        let finish = only(answer_event_dm(&mut conn, "u1", "1", ""));
        assert_eq!(finish.title.as_deref(), Some("Finish or customize"));
        assert!(finish.body.contains("Signup options"), "{finish}");
        let signup = only(answer_event_dm(&mut conn, "u1", "2", ""));
        assert_eq!(signup.title.as_deref(), Some("Signup options"));
        let back = only(answer_event_dm(&mut conn, "u1", "1", ""));
        assert_eq!(back.title.as_deref(), Some("Finish or customize"));
        let posted = answer_event_dm(&mut conn, "u1", "1", "https://marian.example.com")
            .unwrap()
            .unwrap();
        assert_eq!(posted.len(), 2, "{posted:?}");
        assert_eq!(posted[0].title.as_deref(), Some("Event has been created!"));
        assert!(
            posted[0]
                .body
                .contains("https://marian.example.com/servers/4242/events/"),
            "{}",
            posted[0]
        );
        let posts = channel_posts_waiting(&conn, "u1").unwrap();
        assert_eq!(posts.len(), 1, "{posts:?}");
        let event = store::event(&conn, posts[0]).unwrap().unwrap();
        assert!(posted[0].body.contains(&event.public_id), "{}", posted[0]);
        assert_eq!(event.channel_id, 2, "the agreed channel");
        assert_eq!(event.status, "posted");
        let view = logic::discord_view(&conn, event.id, "https://marian.example.com")
            .unwrap()
            .unwrap();
        assert_eq!(view.channel_id, "100");
        assert!(view.body.contains("**Time**"), "{}", view.body);
        assert!(
            view.body.contains(&format!(
                "[View on web](https://marian.example.com/servers/4242/events/{})",
                event.public_id
            )),
            "{}",
            view.body
        );
        assert!(view
            .body
            .contains("[+](http://www.google.com/calendar/event"),);
        assert_eq!(view.footer, "Created by Nell");
        assert!(
            view.fields
                .iter()
                .any(|field| field.name.contains("**Accepted**") && field.value == "-"),
            "{:?}",
            view.fields
        );
        assert!(view.buttons.iter().any(|button| {
            matches!(button.role, logic::ButtonRole::Signup(_))
                && button.label.is_empty()
                && button.emoji == "✅"
                && button.tone == logic::ButtonTone::Secondary
        }));
        assert!(view.buttons.iter().any(|button| {
            button.role == logic::ButtonRole::Edit && button.tone == logic::ButtonTone::Primary
        }));
        assert!(view.buttons.iter().any(|button| {
            button.role == logic::ButtonRole::Delete && button.tone == logic::ButtonTone::Danger
        }));
        assert_eq!(
            posted[1].title.as_deref(),
            Some("What would you like to do?")
        );
    }
}
