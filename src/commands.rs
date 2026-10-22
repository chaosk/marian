use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use diesel::prelude::*;
use diesel::upsert::excluded;

use crate::clock;
use crate::error::OpError;
use crate::logic::{
    self, EventInput, OptionInput, ReminderInput, RepeatSpec, SaveAction,
};
use crate::parse::{self, tokenize};
use crate::schema;
use crate::store::{self, Connection, Event};

pub const HELP: &str = "\
/event starts an event. Marian asks in a DM where to post it, the title, the description, when it starts, when it ends, and how often it repeats.

The other commands are explained at /help.
";

pub fn execute(conn: &mut Connection, guild_id: i64, membership_id: i64, line: &str) -> String {
    let line = line.trim();
    let reply = if line.is_empty() {
        "Type /help to see the command desk.".to_string()
    } else if !line.starts_with('/') {
        match desk_answer(conn, membership_id, line) {
            Ok(Some(text)) => text,
            Ok(None) => match dispatch(conn, guild_id, membership_id, line) {
                Ok(text) => text,
                Err(err) => err.to_string(),
            },
            Err(err) => err.to_string(),
        }
    } else {
        match dispatch(conn, guild_id, membership_id, line) {
            Ok(text) => text,
            Err(err) => err.to_string(),
        }
    };
    let _ = logic::record_command(conn, guild_id, membership_id, line, &reply);
    reply
}

fn event_path(guild: &store::Guild, public_id: impl std::fmt::Display) -> String {
    match guild.discord_id.as_deref().filter(|id| !id.is_empty()) {
        Some(server) => format!("/servers/{server}/events/{public_id}"),
        None => format!("/events/{public_id}"),
    }
}

fn find_event(conn: &mut Connection, guild_id: i64, token: &str) -> Result<Event, OpError> {
    if crate::ids::parse_event_id(token).is_some() {
        if let Some(event) = store::event_by_public(conn, token)? {
            if event.guild_id == guild_id {
                return Ok(event);
            }
        }
    }
    Err(OpError::message("That event isn't in this server."))
}

/// Discord URL of the posted event message, once Discord has assigned an id.
pub fn posted_message_url(conn: &mut Connection, event_id: i64) -> Result<Option<String>, OpError> {
    let Some(event) = store::event(conn, event_id)? else {
        return Ok(None);
    };
    let Some(message_id) = event.discord_message_id.filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    let Some(channel_id) = store::channel(conn, event.channel_id)?
        .and_then(|channel| channel.discord_id)
        .filter(|id| !id.is_empty())
    else {
        return Ok(None);
    };
    let Some(guild_id) = store::guild(conn, event.guild_id)?
        .and_then(|guild| guild.discord_id)
        .filter(|id| !id.is_empty())
    else {
        return Ok(None);
    };
    Ok(Some(logic::discord_message_link(
        &guild_id,
        &channel_id,
        &message_id,
    )))
}

pub fn link_created_event(replies: &mut [DmReply], message_url: &str) {
    let body = format!("[Click here to view the event]({message_url})");
    for reply in replies {
        if reply.title.as_deref() == Some("Event has been created!") {
            reply.body = body.clone();
        }
    }
}

pub fn channel_posts_waiting(
    conn: &mut Connection,
    discord_user_id: &str,
) -> Result<Vec<i64>, OpError> {
    let raw: Option<serde_json::Value> = schema::event_prompts::table
        .filter(schema::event_prompts::discord_user_id.eq(discord_user_id))
        .filter(schema::event_prompts::step.eq("next"))
        .select(schema::event_prompts::draft.assume_not_null())
        .first(conn)
        .optional()?;
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let draft = serde_json::from_value::<EventDraft>(raw).unwrap_or_default();
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
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
    line: &str,
) -> Result<String, OpError> {
    let (cmd, rest) = split_cmd(line);
    let (pos, map) = fields(rest);
    match cmd {
        "help" | "" => Ok(HELP.to_string()),
        "event" => cmd_event(conn, guild_id, membership_id, rest),
        "edit" => cmd_edit(conn, guild_id, membership_id, &pos, &map),
        "reminders" => cmd_reminders(conn, membership_id, &pos, &map),
        "response" => cmd_response(conn, membership_id, &pos, &map),
        "settings" => cmd_settings(conn, guild_id, membership_id, &pos, &map),
        "signup_options" => cmd_signup_options(conn, guild_id, membership_id, &pos, &map),
        "signup_presets" => cmd_presets(conn, guild_id, membership_id, &pos, &map),
        "channel" => Err(OpError::message(
            "Create the channel from /channel in Discord or the channels page.",
        )),
        "google" => cmd_google(conn, guild_id, membership_id, &pos, &map),
        "sort" => Ok("Run /sort inside an event channel. It reverses the order of the posts there.".into()),
        other => Err(OpError::message(format!("Unknown command /{other}. Try /help."))),
    }
}

#[derive(Insertable)]
#[diesel(table_name = schema::event_prompts)]
#[diesel(treat_none_as_default_value = false)]
struct NewEventPrompt<'a> {
    discord_user_id: &'a str,
    guild_id: i64,
    membership_id: i64,
    step: &'a str,
    title: Option<&'a str>,
    start_raw: Option<&'a str>,
    channel_name: Option<&'a str>,
    source_channel: Option<&'a str>,
    source_discord: Option<&'a str>,
    draft: Option<serde_json::Value>,
}

fn cmd_event(
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
    rest: &str,
) -> Result<String, OpError> {
    if !rest.trim().is_empty() {
        return Err(OpError::message(
            "/event doesn't take options. Run /event and answer the questions.",
        ));
    }
    if store::list_channels(conn, guild_id)?.is_empty() {
        return Err(OpError::message(
            "Create a channel before posting an event.",
        ));
    }
    let discord_user_id = prompt_key(conn, membership_id)?;
    diesel::insert_into(schema::event_prompts::table)
        .values(&NewEventPrompt {
            discord_user_id: &discord_user_id,
            guild_id,
            membership_id,
            step: "channel",
            title: None,
            start_raw: None,
            channel_name: None,
            source_channel: None,
            source_discord: None,
            draft: None,
        })
        .on_conflict(schema::event_prompts::discord_user_id)
        .do_update()
        .set((
            schema::event_prompts::guild_id.eq(excluded(schema::event_prompts::guild_id)),
            schema::event_prompts::membership_id.eq(excluded(schema::event_prompts::membership_id)),
            schema::event_prompts::step.eq("channel"),
            schema::event_prompts::title.eq(None::<&str>),
            schema::event_prompts::start_raw.eq(None::<&str>),
            schema::event_prompts::channel_name.eq(None::<&str>),
            schema::event_prompts::source_channel.eq(None::<&str>),
            schema::event_prompts::source_discord.eq(None::<&str>),
            schema::event_prompts::draft.eq(None::<serde_json::Value>),
        ))
        .execute(conn)?;
    Ok(where_to_post(conn, guild_id, &None, &None)?.to_string())
}

fn prompt_key(conn: &mut Connection, membership_id: i64) -> Result<String, OpError> {
    store::discord_id_for_membership(conn, membership_id)
}

fn desk_answer(conn: &mut Connection, membership_id: i64, line: &str) -> Result<Option<String>, OpError> {
    let key = prompt_key(conn, membership_id)?;
    let Some(replies) = answer_event_dm(conn, &key, line, "")? else {
        return Ok(None);
    };
    Ok(Some(
        replies
            .into_iter()
            .map(|reply| reply.to_string())
            .collect::<Vec<_>>()
            .join("\n\n"),
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
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
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
    diesel::insert_into(schema::event_prompts::table)
        .values(&NewEventPrompt {
            discord_user_id,
            guild_id,
            membership_id,
            step: "channel",
            title: None,
            start_raw: None,
            channel_name: None,
            source_channel: Some(source_channel),
            source_discord,
            draft: None,
        })
        .on_conflict(schema::event_prompts::discord_user_id)
        .do_update()
        .set((
            schema::event_prompts::guild_id.eq(excluded(schema::event_prompts::guild_id)),
            schema::event_prompts::membership_id.eq(excluded(schema::event_prompts::membership_id)),
            schema::event_prompts::step.eq("channel"),
            schema::event_prompts::title.eq(None::<&str>),
            schema::event_prompts::start_raw.eq(None::<&str>),
            schema::event_prompts::channel_name.eq(None::<&str>),
            schema::event_prompts::source_channel
                .eq(excluded(schema::event_prompts::source_channel)),
            schema::event_prompts::source_discord
                .eq(excluded(schema::event_prompts::source_discord)),
            schema::event_prompts::draft.eq(None::<serde_json::Value>),
        ))
        .execute(conn)?;
    let source = Some(source_channel.to_string());
    let discord = source_discord.map(str::to_string);
    where_to_post(conn, guild_id, &source, &discord)
}

pub fn cancel_event_dm(conn: &mut Connection, discord_user_id: &str) -> Result<(), OpError> {
    diesel::delete(
        schema::event_prompts::table
            .filter(schema::event_prompts::discord_user_id.eq(discord_user_id)),
    )
    .execute(conn)?;
    Ok(())
}

pub fn answer_event_dm(
    conn: &mut Connection,
    discord_user_id: &str,
    text: &str,
    public_url: &str,
) -> Result<Option<Vec<DmReply>>, OpError> {
    let row = schema::event_prompts::table
        .filter(schema::event_prompts::discord_user_id.eq(discord_user_id))
        .select((
            schema::event_prompts::guild_id,
            schema::event_prompts::membership_id,
            schema::event_prompts::step,
            schema::event_prompts::title,
            schema::event_prompts::start_raw,
            schema::event_prompts::channel_name,
            schema::event_prompts::source_channel,
            schema::event_prompts::source_discord,
            schema::event_prompts::draft,
        ))
        .first(conn)
        .optional()?
        .map(
            |(
                guild_id,
                membership_id,
                step,
                title,
                start_raw,
                channel_name,
                source_channel,
                source_discord,
                draft,
            )| EventPrompt {
                guild_id,
                membership_id,
                step,
                title,
                start_raw,
                channel_name,
                source_channel,
                source_discord,
                draft,
            },
        );
    let Some(prompt) = row else {
        return Ok(None);
    };
    let EventPrompt {
        guild_id,
        membership_id,
        step,
        title,
        start_raw,
        channel_name,
        source_channel,
        source_discord,
        draft,
    } = prompt;
    let mut draft = draft
        .and_then(|raw| serde_json::from_value::<EventDraft>(raw).ok())
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
            diesel::update(
                schema::event_prompts::table
                    .filter(schema::event_prompts::discord_user_id.eq(discord_user_id)),
            )
            .set((
                schema::event_prompts::title.eq(text),
                schema::event_prompts::step.eq("description"),
            ))
            .execute(conn)?;
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
            diesel::update(schema::guilds::table.filter(schema::guilds::id.eq(guild_id)))
                .set(schema::guilds::timezone.eq(*name))
                .execute(conn)?;
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
            diesel::update(
                schema::event_prompts::table
                    .filter(schema::event_prompts::discord_user_id.eq(discord_user_id)),
            )
            .set((
                schema::event_prompts::start_raw.eq(&stored),
                schema::event_prompts::step.eq("end"),
            ))
            .execute(conn)?;
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
            membership_id,
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
            // Each name is a type from the library, added there if it is new.
            let mut types = Vec::new();
            for (index, name) in names.into_iter().enumerate() {
                let emoji = ["✅", "❌", "❔"].get(index).copied().unwrap_or("•");
                let type_id = logic::ensure_type(conn, guild_id, name, emoji, index == 0)?;
                types.push(draft_type(conn, type_id)?);
            }
            draft.types = types;
            save_draft(conn, discord_user_id, "finish", &draft)?;
            just(finish_card(&draft))
        }
        "signup_preset" => {
            let presets = store::list_presets(conn, guild_id)?;
            let choice = text.parse::<usize>().ok().filter(|n| *n >= 1);
            let Some(preset) = choice.and_then(|n| presets.get(n - 1)) else {
                return just(preset_card(&presets));
            };
            draft.multiple = preset.multiple_signups;
            draft.types = logic::preset_types(conn, preset.id)?
                .into_iter()
                .map(|opt| draft_type(conn, opt.type_id))
                .collect::<Result<_, _>>()?;
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
        "next" => answer_next(conn, discord_user_id, membership_id, text, &draft, public_url),
        "respond" => answer_respond(conn, discord_user_id, membership_id, text, &draft),
        _ => {
            cancel_event_dm(conn, discord_user_id)?;
            Ok(None)
        }
    }
}

struct EventPrompt {
    guild_id: i64,
    membership_id: i64,
    step: String,
    title: Option<String>,
    start_raw: Option<String>,
    channel_name: Option<String>,
    source_channel: Option<String>,
    source_discord: Option<String>,
    draft: Option<serde_json::Value>,
}

#[derive(Clone, Serialize, Deserialize)]
struct EventDraft {
    #[serde(default)]
    description: String,
    /// The attendance types picked so far; empty means the server's default preset.
    #[serde(default)]
    types: Vec<DraftType>,
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
            types: Vec::new(),
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

/// An attendance type in a DM draft, with its name for the summary.
#[derive(Clone, Serialize, Deserialize)]
struct DraftType {
    type_id: i64,
    name: String,
}

fn draft_type(conn: &mut Connection, type_id: i64) -> Result<DraftType, OpError> {
    let kind = store::library_type(conn, type_id)?.ok_or(OpError::NotFound)?;
    Ok(DraftType { type_id, name: kind.name })
}

fn finish_card(draft: &EventDraft) -> DmReply {
    let mut notes = Vec::new();
    if !draft.types.is_empty() {
        let names = draft
            .types
            .iter()
            .map(|kind| kind.name.as_str())
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

fn start_card(conn: &mut Connection, guild_id: i64) -> Result<DmReply, OpError> {
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
    conn: &mut Connection,
    discord_user_id: &str,
    step: &str,
    draft: &EventDraft,
) -> Result<(), OpError> {
    let json = serde_json::to_value(draft)
        .map_err(|err| OpError::message(format!("Could not keep the draft: {err}")))?;
    diesel::update(
        schema::event_prompts::table
            .filter(schema::event_prompts::discord_user_id.eq(discord_user_id)),
    )
    .set((
        schema::event_prompts::step.eq(step),
        schema::event_prompts::draft.eq(json),
    ))
    .execute(conn)?;
    Ok(())
}

fn advance_to_title(
    conn: &mut Connection,
    discord_user_id: &str,
    channel: &str,
) -> Result<DmReply, OpError> {
    diesel::update(
        schema::event_prompts::table
            .filter(schema::event_prompts::discord_user_id.eq(discord_user_id)),
    )
    .set((
        schema::event_prompts::channel_name.eq(channel),
        schema::event_prompts::step.eq("title"),
    ))
    .execute(conn)?;
    Ok(title_card())
}

fn answer_channel(
    conn: &mut Connection,
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
            diesel::update(
                schema::event_prompts::table
                    .filter(schema::event_prompts::discord_user_id.eq(discord_user_id)),
            )
            .set(schema::event_prompts::step.eq("channel_other"))
            .execute(conn)?;
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
    conn: &mut Connection,
    discord_user_id: &str,
    guild_id: i64,
    membership_id: i64,
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
                membership_id,
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
    conn: &mut Connection,
    discord_user_id: &str,
    guild_id: i64,
    text: &str,
    draft: &mut EventDraft,
) -> Result<Option<Vec<DmReply>>, OpError> {
    match text {
        "1" => {
            draft.types.clear();
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
    conn: &mut Connection,
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
    conn: &mut Connection,
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
    conn: &mut Connection,
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
    conn: &mut Connection,
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
    conn: &mut Connection,
    discord_user_id: &str,
    guild_id: i64,
    membership_id: i64,
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
    let (options, multiple) = if draft.types.is_empty() {
        logic::starting_options(conn, guild_id)?
    } else {
        (draft.types.iter().map(|kind| OptionInput::of_type(kind.type_id)).collect(), draft.multiple)
    };
    let duration = if draft.duration_minutes == 0 {
        60
    } else {
        draft.duration_minutes
    };
    let thread_enabled = store::settings(conn, guild_id)?.event_threads;
    let saved = logic::save_event(
        conn,
        EventInput {
            id: None,
            guild_id,
            actor_id: membership_id,
            channel_id: channel.id,
            title: title.unwrap_or("Event").to_string(),
            description: draft.description.clone(),
            location: String::new(),
            starts_at,
            duration_minutes: duration,
            color: draft.color.clone(),
            image_url: draft.image_url.clone(),
            cutoff_at,
            attendee_role_id: draft.attendee_role_id,
            thread_enabled,
            multiple_signups: multiple,
            mention_everyone: draft.mention_everyone,
            mention_role_ids: draft.mention_role_ids.clone(),
            restrict_role_ids: draft.restrict_role_ids.clone(),
            options,
            reminders: draft.reminders.clone(),
            repeat,
            action,
            post_at,
            apply_series: false,
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
    conn: &mut Connection,
    discord_user_id: &str,
    membership_id: i64,
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
            diesel::delete(schema::signups::table.filter(schema::signups::event_id.eq(event_id)))
                .execute(conn)?;
            just(next_card(Some("Removed the responses.")))
        }
        "3" => {
            save_draft(conn, discord_user_id, "respond", draft)?;
            let event = store::event(conn, event_id)?.ok_or(OpError::NotFound)?;
            let options = store::options_for(conn, &event)?;
            let lines = options
                .iter()
                .map(|opt| format!("{} {}", opt.emoji, opt.name))
                .collect::<Vec<_>>();
            just(numbered_prompt("Add a response", &lines))
        }
        "4" => {
            let id = logic::duplicate_event(conn, event_id, membership_id)?;
            just(next_card(Some(&format!(
                "Duplicated it. The copy is event {id}, one week later."
            ))))
        }
        _ => just(next_card(None)),
    }
}

fn answer_respond(
    conn: &mut Connection,
    discord_user_id: &str,
    membership_id: i64,
    text: &str,
    draft: &EventDraft,
) -> Result<Option<Vec<DmReply>>, OpError> {
    let Some(event_id) = draft.event_id else {
        cancel_event_dm(conn, discord_user_id)?;
        return Ok(None);
    };
    let event = store::event(conn, event_id)?.ok_or(OpError::NotFound)?;
    let options = store::options_for(conn, &event)?;
    let choice = text.parse::<usize>().ok().filter(|n| *n >= 1);
    let Some(option) = choice.and_then(|n| options.get(n - 1)) else {
        let lines = options
            .iter()
            .map(|opt| format!("{} {}", opt.emoji, opt.name))
            .collect::<Vec<_>>();
        return just(numbered_prompt("Add a response", &lines));
    };
    let reply = logic::signup(conn, event_id, option.id, membership_id)?;
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
    conn: &mut Connection,
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
    conn: &mut Connection,
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

pub struct EditChange {
    pub event: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub start: Option<String>,
    pub duration: Option<i64>,
    pub channel: Option<String>,
}

pub fn edit_event(
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
    change: EditChange,
) -> Result<String, OpError> {
    if change.title.is_none()
        && change.description.is_none()
        && change.start.is_none()
        && change.duration.is_none()
        && change.channel.is_none()
    {
        return Err(OpError::message(
            "Name a change: title, description, start, length, or channel.",
        ));
    }
    let event = find_event(conn, guild_id, &change.event)?;
    let guild = store::guild(conn, event.guild_id)?.ok_or(OpError::NotFound)?;
    let tz = clock::zone(&guild.timezone)?;
    let mut input = input_from(conn, &event, membership_id)?;
    if let Some(title) = change.title {
        input.title = title;
    }
    if let Some(description) = change.description {
        input.description = description;
    }
    if let Some(start) = change.start {
        input.starts_at = parse::parse_when(&start, Utc::now(), tz).ok_or_else(|| {
            OpError::message(format!("Marian couldn't read the time \"{start}\"."))
        })?;
    }
    if let Some(duration) = change.duration {
        input.duration_minutes = duration;
    }
    if let Some(name) = change.channel {
        input.channel_id = store::channel_by_name(conn, event.guild_id, &name)?
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

fn cmd_edit(
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
    pos: &[String],
    map: &HashMap<String, String>,
) -> Result<String, OpError> {
    let event = map
        .get("id")
        .or_else(|| pos.first())
        .cloned()
        .ok_or_else(|| OpError::message("Say which event to edit."))?;
    edit_event(
        conn,
        guild_id,
        membership_id,
        EditChange {
            event,
            title: map.get("title").cloned(),
            description: map.get("description").cloned(),
            start: map.get("start").cloned(),
            duration: map.get("duration").and_then(|value| value.parse().ok()),
            channel: map.get("channel").cloned(),
        },
    )
}

pub fn set_reminders(
    conn: &mut Connection,
    membership_id: i64,
    enabled: Option<bool>,
) -> Result<String, OpError> {
    match enabled {
        Some(value) => logic::set_user_reminders(conn, membership_id, value),
        None => {
            let user = store::membership(conn, membership_id)?.ok_or(OpError::NotFound)?;
            Ok(if user.reminders_enabled {
                format!("{} gets reminders.", user.name)
    } else {
                format!("{} is opted out of reminders.", user.name)
            })
        }
    }
}

fn cmd_reminders(
    conn: &mut Connection,
    membership_id: i64,
    pos: &[String],
    map: &HashMap<String, String>,
) -> Result<String, OpError> {
    let raw = map
        .get("state")
        .cloned()
        .or_else(|| pos.first().cloned())
        .unwrap_or_else(|| "status".into());
    let enabled = match raw.to_lowercase().as_str() {
        "off" | "no" | "false" => Some(false),
        "on" | "yes" | "true" => Some(true),
        _ => None,
    };
    set_reminders(conn, membership_id, enabled)
}

fn cmd_response(
    conn: &mut Connection,
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
    let actor = store::membership(conn, actor_id)?.ok_or(OpError::NotFound)?;
    let handle = map
        .get("user")
        .cloned()
        .or_else(|| pos.get(2).cloned())
        .ok_or_else(|| OpError::message("Name the person by handle."))?;
    let user = store::membership_by_handle(conn, actor.guild_id, &handle)?
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
    conn: &mut Connection,
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
        let start = event.starts_at;
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
            && event.public_id.to_string() != typed
        {
            continue;
        }
        choices.push(Choice {
            label,
            value: event.public_id.to_string(),
        });
        if choices.len() == 25 {
            break;
        }
    }
    Ok(choices)
}

pub fn response_options(
    conn: &mut Connection,
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
    for opt in store::options_for(conn, &event)? {
        let label = option_menu_label(&opt.emoji, &opt.name);
        if !needle.is_empty()
            && !opt.name.to_lowercase().contains(&needle)
            && !label.to_lowercase().contains(&needle)
            && opt.public_id.to_string() != partial.trim()
        {
            continue;
        }
        choices.push(Choice {
            label,
            value: opt.public_id.to_string(),
        });
        if choices.len() == 25 {
            break;
        }
    }
    Ok(choices)
}

pub fn roster_action(
    conn: &mut Connection,
    actor_id: i64,
    event_token: &str,
    target_id: i64,
    option_token: &str,
    action: &str,
) -> Result<(String, i64), OpError> {
    let actor = store::membership(conn, actor_id)?.ok_or(OpError::NotFound)?;
    let event = find_event(conn, actor.guild_id, event_token)?;
    let option = resolve_option(conn, &event, option_token)?;
    let notice = logic::editor_response(conn, event.id, actor_id, target_id, option.id, action)?;
    Ok((notice, event.id))
}

fn resolve_option(
    conn: &mut Connection,
    event: &Event,
    token: &str,
) -> Result<store::SignupOption, OpError> {
    let options = store::options_for(conn, event)?;
    // A pick from the autocomplete list is the option's public id; a typed answer is its name.
    if let Some(opt) = options
        .iter()
        .find(|opt| opt.public_id.to_string() == token.trim())
        .or_else(|| options.iter().find(|opt| opt.name.eq_ignore_ascii_case(token)))
    {
        return Ok(opt.clone());
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

pub fn channel_choices(
    conn: &mut Connection,
    guild_id: i64,
    partial: &str,
) -> Result<Vec<Choice>, OpError> {
    let needle = partial.trim().to_lowercase();
    let mut choices = Vec::new();
    for channel in store::list_channels(conn, guild_id)? {
        if !needle.is_empty() && !channel.name.to_lowercase().contains(&needle) {
            continue;
        }
        let kind = if channel.kind == "event" {
            "event"
        } else {
            "text"
        };
        let label = format!("#{} · {kind}", channel.name);
        choices.push(Choice {
            label: label.chars().take(100).collect(),
            value: channel.name,
        });
        if choices.len() == 25 {
            break;
        }
    }
    Ok(choices)
}

pub enum Setting {
    ReminderInterval(i64),
    Hour24(bool),
    AutoArchive(bool),
    AutoArchiveHours(i64),
    AutoJoinThreads(bool),
    EventThreads(bool),
    LocalizeTime(bool),
    PurgeChannels(bool),
    ThreadReminders(bool),
    ShowAttendees(bool),
    TempRoles(bool),
    Waitlist(bool),
    Digest(String),
    PostLeadHours(i64),
    Role { which: String, name: String },
}

pub fn apply_setting(
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
    setting: Setting,
) -> Result<String, OpError> {
    require_server_manager(conn, membership_id)?;
    let mut settings = store::settings(conn, guild_id)?;
    let key = match setting {
        Setting::ReminderInterval(minutes) => {
            settings.default_reminder_minutes = minutes;
            "reminder_interval"
        }
        Setting::Hour24(enabled) => {
            settings.hour_24 = enabled;
            "24_hour_time"
        }
        Setting::AutoArchive(enabled) => {
            settings.auto_archive = enabled;
            "auto_archive"
        }
        Setting::AutoArchiveHours(hours) => {
            settings.auto_archive_hours = hours;
            "auto_archive_interval"
        }
        Setting::AutoJoinThreads(enabled) => {
            settings.auto_join_threads = enabled;
            "auto_join_threads"
        }
        Setting::EventThreads(enabled) => {
            settings.event_threads = enabled;
            "event_threads"
        }
        Setting::LocalizeTime(enabled) => {
            settings.localize_time = enabled;
            "localize_time"
        }
        Setting::PurgeChannels(enabled) => {
            settings.purge_channels = enabled;
            "purge"
        }
        Setting::ThreadReminders(enabled) => {
            settings.thread_reminders = enabled;
            "thread_reminders"
        }
        Setting::ShowAttendees(enabled) => {
            settings.show_attendees = enabled;
            "show_attendees"
        }
        Setting::TempRoles(enabled) => {
            settings.temp_roles = enabled;
            "temp_roles"
        }
        Setting::Waitlist(enabled) => {
            settings.waitlist = enabled;
            "waitlist"
        }
        Setting::Digest(value) => {
            if !matches!(value.as_str(), "off" | "daily" | "weekly") {
                return Err(OpError::message("Digest can be off, daily, or weekly."));
            }
            settings.digest = value;
            "digest"
        }
        Setting::PostLeadHours(hours) => {
            settings.post_lead_hours = hours;
            "post_lead"
        }
        Setting::Role { which, name } => {
            // One role replaces the list; `clear` empties it. The web settings take several.
            let role_id: Vec<i64> = if name.eq_ignore_ascii_case("clear") || name.is_empty() {
                Vec::new()
            } else {
                vec![
                    store::role_by_name(conn, guild_id, &name)?
                        .ok_or_else(|| OpError::message(format!("No role named {name}.")))?
                        .id,
                ]
            };
            match which.as_str() {
                "event" => settings.role_event = role_id,
                "edit" => settings.role_edit = role_id,
                "delete" => settings.role_delete = role_id,
                "channel" => settings.role_channel = role_id,
                "signup_options" | "signup" => settings.role_signup = role_id,
                other => return Err(OpError::message(format!("Unknown role setting {other}."))),
            }
            "role"
        }
    };
    store::update_settings(conn, &settings)?;
    Ok(format!("Updated {key}."))
}

fn cmd_settings(
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
    pos: &[String],
    map: &HashMap<String, String>,
) -> Result<String, OpError> {
    require_server_manager(conn, membership_id)?;
    let key = map
        .keys()
        .next()
                .cloned()
        .or_else(|| pos.first().cloned())
        .ok_or_else(|| OpError::message("Name a setting."))?;
    let value = map
        .get(&key)
                .cloned()
                .or_else(|| pos.get(1).cloned())
                .unwrap_or_default();
    let extra = pos.get(2).cloned().unwrap_or_default();
    let setting = match key.as_str() {
        "reminder_interval" => Setting::ReminderInterval(
            value
                .parse()
                .map_err(|_| OpError::message("Use a number of minutes."))?,
        ),
        "24_hour_time" => Setting::Hour24(on(&value)?),
        "auto_archive" => Setting::AutoArchive(on(&value)?),
        "auto_archive_interval" => Setting::AutoArchiveHours(
            value
                .parse()
                .map_err(|_| OpError::message("Use a number of hours."))?,
        ),
        "auto_join_threads" => Setting::AutoJoinThreads(on(&value)?),
        "event_threads" => Setting::EventThreads(on(&value)?),
        "localize_time" => Setting::LocalizeTime(on(&value)?),
        "purge" => Setting::PurgeChannels(on(&value)?),
        "thread_reminders" => Setting::ThreadReminders(on(&value)?),
        "show_attendees" => Setting::ShowAttendees(on(&value)?),
        "temp_roles" => Setting::TempRoles(on(&value)?),
        "waitlist" => Setting::Waitlist(on(&value)?),
        "digest" => Setting::Digest(value),
        "post_lead" => Setting::PostLeadHours(
            value
                .parse()
                .map_err(|_| OpError::message("Use a number of hours."))?,
        ),
        "role" => Setting::Role {
            which: value,
            name: extra,
        },
        other => return Err(OpError::message(format!("Unknown setting {other}."))),
    };
    apply_setting(conn, guild_id, membership_id, setting)
}

pub fn library_text(conn: &mut Connection, guild_id: i64) -> Result<String, OpError> {
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

/// Adds a type to the library, or gives an existing one a new emoji.
pub fn add_library_option(
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
    name: &str,
    emoji: &str,
    attending: bool,
) -> Result<String, OpError> {
    let emoji = if emoji.trim().is_empty() { "•" } else { emoji.trim() };
    match store::library_type_named(conn, guild_id, name)? {
        Some(existing) => {
            logic::update_type(
                conn,
                existing.id,
                membership_id,
                &logic::TypeInput {
                    name: existing.name.clone(),
                    emoji: emoji.to_string(),
                    attending: existing.attending,
                },
            )?;
            Ok(format!("{emoji} {} is in the library.", existing.name))
        }
        None => {
            let made = logic::add_type(
                conn,
                guild_id,
                membership_id,
                &logic::TypeInput {
                    name: name.to_string(),
                    emoji: emoji.to_string(),
                    attending,
                },
            )?;
            Ok(format!("Saved {} {} in the library.", made.emoji, made.name))
        }
    }
}

/// Takes a type out of the library, unless something still uses it.
pub fn remove_library_option(
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
    name: &str,
) -> Result<String, OpError> {
    let kind = store::library_type_named(conn, guild_id, name)?
        .ok_or_else(|| OpError::message(format!("The library has no {}.", name.trim())))?;
    logic::delete_type(conn, kind.id, membership_id)?;
    Ok(format!("Removed {} from the library.", kind.name))
}

fn cmd_signup_options(
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
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
            // A new type counts as attending unless told otherwise.
            let attending = !matches!(map.get("attending").map(|value| value.trim().to_lowercase()).as_deref(), Some("no" | "false"));
            add_library_option(conn, guild_id, membership_id, &name, &emoji, attending)
        }
        "remove" => {
            let name = map
                .get("name")
                .cloned()
                .or_else(|| pos.get(1).cloned())
                .unwrap_or_default();
            remove_library_option(conn, guild_id, membership_id, &name)
        }
        _ => library_text(conn, guild_id),
    }
}

pub fn preset_text(conn: &mut Connection, guild_id: i64) -> Result<String, OpError> {
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

/// A preset from type names, comma separated. A name the library doesn't have
/// yet is added to it.
pub fn add_preset(
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
    name: &str,
    options: &str,
) -> Result<String, OpError> {
    let mut types = Vec::new();
    for part in options.split(',').map(str::trim).filter(|part| !part.is_empty()) {
        types.push(OptionInput::of_type(logic::ensure_type(conn, guild_id, part, "•", true)?));
    }
    logic::save_preset(
        conn,
        guild_id,
        membership_id,
        None,
        &logic::PresetInput {
            name: name.to_string(),
            multiple_signups: false,
            options: types,
        },
    )?;
    Ok(format!("Saved the {} preset.", name.trim()))
}

pub fn remove_preset(
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
    name: &str,
) -> Result<String, OpError> {
            if let Some(preset) = store::list_presets(conn, guild_id)?
                .into_iter()
        .find(|preset| preset.name.eq_ignore_ascii_case(name))
            {
        logic::delete_preset(conn, preset.id, membership_id)?;
            }
            Ok(format!("Removed {name}."))
}

fn cmd_presets(
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
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
            add_preset(conn, guild_id, membership_id, &name, &raw)
        }
        "remove" => {
            let name = map
                .get("name")
                .cloned()
                .or_else(|| pos.get(1).cloned())
                .unwrap_or_default();
            remove_preset(conn, guild_id, membership_id, &name)
        }
        _ => preset_text(conn, guild_id),
    }
}

pub fn channel_request(line: &str) -> Option<String> {
    let (cmd, rest) = split_cmd(line);
    if cmd != "channel" {
        return None;
    }
    let (pos, map) = fields(rest);
    Some(
        map.get("name")
        .cloned()
        .or_else(|| pos.first().cloned())
            .unwrap_or_default(),
    )
}

fn public_base() -> String {
    std::env::var("MARIAN_PUBLIC_URL")
        .unwrap_or_default()
        .trim()
        .trim_end_matches('/')
        .to_string()
}

fn person_user_id(conn: &mut Connection, membership_id: i64) -> Result<i64, OpError> {
    store::membership(conn, membership_id)?
        .map(|membership| membership.user_id)
        .ok_or_else(|| {
            OpError::message(
                "Marian needs your Discord account before it can connect Google Calendar.",
            )
        })
}

pub fn google_sign_in_url(conn: &mut Connection, membership_id: i64) -> Result<String, OpError> {
    if !crate::google::configured_from_env() {
        return Err(OpError::message("Google Calendar is not configured yet."));
    }
    let base = public_base();
    if base.is_empty() {
        return Err(OpError::message(
            "Set MARIAN_PUBLIC_URL so Marian can send the Google link.",
        ));
    }
    let user_id = person_user_id(conn, membership_id)?;
    let state = crate::google::begin_link(conn, user_id)?;
    let Some(url) = crate::google::connect_url(&base, &state) else {
        return Err(OpError::message(
            "Set MARIAN_PUBLIC_URL so Marian can send the Google link.",
        ));
    };
    if url.len() > 512 {
        let _ = crate::google::finish_oauth(conn, &state);
        return Err(OpError::message(
            "The Google sign-in link is too long for Discord. Connect from Settings.",
        ));
    }
    Ok(url)
}

pub fn google_authorize(conn: &mut Connection, membership_id: i64) -> Result<String, OpError> {
    let url = google_sign_in_url(conn, membership_id)?;
    Ok(format!(
        "Click the button below to connect Google Calendar.\n{url}"
    ))
}

pub fn google_auto_sync(
    conn: &mut Connection,
    _guild_id: i64,
    membership_id: i64,
    enabled: bool,
) -> Result<String, OpError> {
    let user_id = person_user_id(conn, membership_id)?;
    crate::google::set_auto_sync(conn, user_id, enabled)?;
    Ok(if enabled {
        "Marian will keep your Google Calendar updated.".into()
            } else {
        "Automatic Google Calendar updates are paused. /google sync still adds one event.".into()
    })
}

pub fn google_revoke(
    conn: &mut Connection,
    _guild_id: i64,
    membership_id: i64,
) -> Result<String, OpError> {
    let user_id = person_user_id(conn, membership_id)?;
    if !crate::google::queue_revoke(conn, user_id)? {
        return Ok("Google Calendar is already disconnected.".into());
    }
    Ok("Google Calendar will be disconnected, and Marian will remove the events it added.".into())
}

pub fn google_sync(
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
    event: &str,
) -> Result<String, OpError> {
    let event = find_event(conn, guild_id, event)?;
    if !matches!(event.status.as_str(), "posted" | "scheduled" | "archived") {
        return Err(OpError::message(
            "Post or schedule the event before syncing it.",
        ));
    }
    let user_id = person_user_id(conn, membership_id)?;
    crate::google::queue_event(conn, user_id, event.id)?;
    Ok(format!(
        "Marian will add {} to your Google Calendar.",
        event.title
    ))
}

fn cmd_google(
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
    pos: &[String],
    map: &HashMap<String, String>,
) -> Result<String, OpError> {
    let action = pos.first().map(String::as_str).unwrap_or("authorize");
    match action {
        "authorize" => google_authorize(conn, membership_id),
        "auto_sync" => {
            let value = map
                .get("state")
                .cloned()
                .or_else(|| pos.get(1).cloned())
                .unwrap_or_else(|| "on".into());
            google_auto_sync(conn, guild_id, membership_id, on(&value)?)
        }
        "revoke" => google_revoke(conn, guild_id, membership_id),
        "sync" => {
            let token = map
                .get("event")
                .map(String::as_str)
                .or_else(|| pos.get(1).map(String::as_str))
                .ok_or_else(|| OpError::message("Say which event to sync."))?;
            google_sync(conn, guild_id, membership_id, token)
        }
        other => Err(OpError::message(format!("Unknown google command {other}."))),
    }
}

fn input_from(conn: &mut Connection, event: &Event, actor_id: i64) -> Result<EventInput, OpError> {
    let options = store::options_for(conn, event)?;
    let reminders = store::reminders_for(conn, event)?;
    Ok(EventInput {
        id: Some(event.id),
        guild_id: event.guild_id,
        actor_id,
        channel_id: event.channel_id,
        title: event.title.clone(),
        description: event.description.clone(),
        location: event.location.clone(),
        starts_at: event.starts_at,
        duration_minutes: event.duration_minutes,
        color: event.color.clone(),
        image_url: event.image_url.clone(),
        cutoff_at: event.cutoff_at,
        attendee_role_id: event.attendee_role_id,
        thread_enabled: event.thread_enabled,
        multiple_signups: event.multiple_signups,
        mention_everyone: event.mention_everyone,
        mention_role_ids: store::event_role_ids(conn, event, "mention")?,
        restrict_role_ids: store::event_role_ids(conn, event, "restrict")?,
        options: options
            .into_iter()
            .map(|opt| OptionInput {
                id: Some(opt.id),
                type_id: opt.type_id,
                capacity: opt.capacity,
                role_id: opt.role_id,
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
        post_at: event.post_at,
        apply_series: false,
    })
}

fn require_server_manager(conn: &mut Connection, membership_id: i64) -> Result<(), OpError> {
    let user = store::membership(conn, membership_id)?.ok_or(OpError::NotFound)?;
    if user.server_manager {
        Ok(())
    } else {
        Err(OpError::message("Only a server manager can change that."))
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
    use diesel::connection::SimpleConnection as _;

    use super::*;

    fn only(result: Result<Option<Vec<DmReply>>, OpError>) -> DmReply {
        let mut replies = result.unwrap().expect("a reply");
        assert_eq!(replies.len(), 1);
        replies.remove(0)
    }

    #[test]
    fn a_response_picked_from_the_list_names_the_option_by_public_id() {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Guild', 'UTC', '4242');
            INSERT INTO channels (id, guild_id, name, kind, position) VALUES (1, 1, 'social', 'text', 0);
            INSERT INTO users (discord_id) VALUES ('nell'), ('june');
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color, reminders_enabled, server_manager)
            SELECT 1, id, 1, 'Nell', 'nell', '#112233', true, true FROM users WHERE discord_id = 'nell'
            UNION ALL
            SELECT 2, id, 1, 'June', 'june', '#223344', true, false FROM users WHERE discord_id = 'june';
            ",
        )
        .unwrap();
        store::insert_default_settings(&mut conn, 1).unwrap();
        let starts_at = Utc::now() + chrono::Duration::days(2);
        let kinds: Vec<_> = ["Tank", "Healer"]
            .into_iter()
            .map(|name| logic::test_option(&mut conn, 1, name, "", true))
            .collect();
        let id = logic::save_event(
            &mut conn,
            logic::EventInput {
                id: None,
                guild_id: 1,
                actor_id: 1,
                channel_id: 1,
                title: "Raid".into(),
                description: String::new(),
                location: String::new(),
                starts_at,
                duration_minutes: 60,
                color: "#1e3a6e".into(),
                image_url: None,
                cutoff_at: None,
                attendee_role_id: None,
                thread_enabled: false,
                multiple_signups: false,
                mention_everyone: false,
                mention_role_ids: vec![],
                restrict_role_ids: vec![],
                options: kinds,
                reminders: vec![],
                repeat: None,
                action: logic::SaveAction::Publish,
                post_at: None,
                apply_series: false,
            },
        )
        .unwrap();
        let event = store::event(&mut conn, id).unwrap().unwrap();
        let token = event.public_id.to_string();
        let choices = response_options(&mut conn, 1, &token, "heal").unwrap();
        assert_eq!(choices.len(), 1);
        let picked = &choices[0].value;
        assert_eq!(uuid::Uuid::parse_str(picked).unwrap().get_version_num(), 7, "not a row number");
        roster_action(&mut conn, 1, &token, 2, picked, "add").unwrap();
        let healer = store::options_for(&mut conn, &event).unwrap().into_iter().find(|opt| opt.name == "Healer").unwrap();
        assert!(store::signups_for(&mut conn, &event).unwrap().iter().any(|s| s.option_id == healer.id && s.membership_id == 2));
        // A typed name still works.
        roster_action(&mut conn, 1, &token, 2, "tank", "add").unwrap();
    }

    #[test]
    fn help_points_at_the_guide() {
        assert!(HELP.contains("/event starts an event"));
        assert!(HELP.contains("/help"));
        assert!(!HELP.contains("Healer"));
        assert!(!HELP.contains("event:12"));
        assert!(!HELP.contains("title:"));
    }

    #[test]
    fn event_on_the_desk_asks_the_same_questions() {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone) VALUES (1, 'Guild', 'UTC');
            INSERT INTO channels (id, guild_id, name, kind, position) VALUES (1, 1, 'social', 'text', 0);
            INSERT INTO users (discord_id) VALUES ('nell');
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color, reminders_enabled, server_manager)
            SELECT 1, id, 1, 'Nell', 'nell', '#112233', true, true FROM users WHERE discord_id = 'nell';
            ",
        )
        .unwrap();
        store::insert_default_settings(&mut conn, 1).unwrap();
        let rejected = execute(
            &mut conn,
            1,
            1,
            r#"/event title:"Shore cleanup" start:"tomorrow at 4pm" channel:social"#,
        );
        assert!(rejected.contains("doesn't take options"), "{rejected}");
        let first = execute(&mut conn, 1, 1, "/event");
        assert!(
            first.contains("Where would you like to post this event?"),
            "{first}"
        );
        assert!(first.contains("#social"), "{first}");
        let title = execute(&mut conn, 1, 1, "1");
        assert!(title.contains("Enter the event title"), "{title}");
        let missing = execute(&mut conn, 1, 1, "/events");
        assert!(missing.contains("Unknown command /events"), "{missing}");
    }

    #[test]
    fn event_dm_asks_then_posts() {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Guild', 'UTC', '4242');
            INSERT INTO channels (id, guild_id, name, kind, position, discord_id)
              VALUES (1, 1, 'social', 'text', 0, '99'), (2, 1, 'briefing', 'text', 1, '100');
            INSERT INTO users (discord_id) VALUES ('nell');
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color, reminders_enabled, server_manager)
            SELECT 1, id, 1, 'Nell', 'nell', '#112233', true, true FROM users WHERE discord_id = 'nell';
            ",
        )
        .unwrap();
        store::insert_default_settings(&mut conn, 1).unwrap();
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
        let posts = channel_posts_waiting(&mut conn, "u1").unwrap();
        assert_eq!(posts.len(), 1, "{posts:?}");
        diesel::update(schema::events::table.filter(schema::events::id.eq(posts[0])))
            .set(schema::events::discord_message_id.eq("555"))
            .execute(&mut conn)
            .unwrap();
        let url = posted_message_url(&mut conn, posts[0]).unwrap().unwrap();
        assert_eq!(url, "https://discord.com/channels/4242/100/555");
        let event = store::event(&mut conn, posts[0]).unwrap().unwrap();
        assert!(
            posted[0].body.contains(&event.public_id.to_string()),
            "{}",
            posted[0]
        );
        assert_eq!(event.channel_id, 2, "the agreed channel");
        assert_eq!(event.status, "posted");
        let view = logic::discord_view(&mut conn, event.id, "https://marian.example.com")
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
        let mut sent = posted;
        link_created_event(&mut sent, &url);
        assert_eq!(
            sent[0].body,
            "[Click here to view the event](https://discord.com/channels/4242/100/555)"
        );
        assert_eq!(sent[1].title.as_deref(), Some("What would you like to do?"));
    }
}
