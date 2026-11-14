use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use diesel::prelude::*;
use diesel::upsert::excluded;

use crate::clock;
use crate::error::OpError;
use crate::i18n::{self, Lang};
use crate::tr;
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

/// An event in the server named by public id, if `sight` takes in its channel.
fn find_event(conn: &mut Connection, guild_id: i64, sight: &logic::Sight, token: &str) -> Result<Event, OpError> {
    if crate::ids::parse_event_id(token).is_some() {
        if let Some(event) = store::event_by_public(conn, token)? {
            if event.guild_id == guild_id && sight.sees_event(&event) {
                return Ok(event);
            }
        }
    }
    Err(OpError::from(crate::msg!("err-event-elsewhere")))
}

fn member_sight(conn: &mut Connection, membership_id: i64) -> Result<logic::Sight, OpError> {
    let member = store::membership(conn, membership_id)?.ok_or(OpError::NotFound)?;
    logic::sight(conn, &member)
}

/// The server's channels the Discord member can see.
fn seen_channels(conn: &mut Connection, guild_id: i64, discord_user_id: &str) -> Result<Vec<store::Channel>, OpError> {
    let sight = logic::sight_for_discord(conn, guild_id, discord_user_id)?;
    Ok(store::list_channels(conn, guild_id)?
        .into_iter()
        .filter(|channel| sight.sees_channel(channel.id))
        .collect())
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
    for reply in replies {
        if let Some(lang) = reply.created {
            reply.body = tr!(lang, "dm-view-event", url = message_url);
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
        "channel" => Err(OpError::from(crate::msg!("err-channel-where"))),
        "google" => cmd_google(conn, guild_id, membership_id, &pos, &map),
        "sort" => Ok(tr!(Lang::En, "reply-sort-where")),
        other => Err(OpError::from(crate::msg!("err-unknown-command", command = other))),
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
        return Err(OpError::from(crate::msg!("err-event-no-options")));
    }
    if store::list_channels(conn, guild_id)?.is_empty() {
        return Err(OpError::from(crate::msg!("err-no-channels")));
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
    Ok(where_to_post(conn, guild_id, &discord_user_id, &None, &None, Lang::En)?.to_string())
}

fn prompt_key(conn: &mut Connection, membership_id: i64) -> Result<String, OpError> {
    store::discord_id_for_membership(conn, membership_id)
}

fn desk_answer(conn: &mut Connection, membership_id: i64, line: &str) -> Result<Option<String>, OpError> {
    let key = prompt_key(conn, membership_id)?;
    let Some(replies) = answer_event_dm_in(conn, &key, line, "", Some(Lang::En))? else {
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
    /// Set on the card that announces a new event, in the language it was
    /// written in, so its link can point at the Discord post once there is one.
    pub created: Option<Lang>,
}

impl DmReply {
    fn plain(body: impl Into<String>) -> Self {
        Self {
            title: None,
            body: body.into(),
            created: None,
        }
    }

    fn card(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: Some(title.into()),
            body: body.into(),
            created: None,
        }
    }
}

/// Words a DM answer may be, in English or Polish, whatever the server's language.
const CANCEL: [&str; 2] = ["cancel", "anuluj"];
const NONE: [&str; 3] = ["none", "brak", "nic"];
const NOW: [&str; 2] = ["now", "teraz"];
const ZONE: [&str; 2] = ["timezone", "strefa"];
const EVERYONE: [&str; 2] = ["everyone", "wszyscy"];

fn said(text: &str, words: &[&str]) -> bool {
    words.iter().any(|word| text.trim().eq_ignore_ascii_case(word))
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
    lang: Lang,
) -> Result<DmReply, OpError> {
    let channels = store::list_channels(conn, guild_id)?;
    if channels.is_empty() {
        return Err(OpError::from(crate::msg!("err-no-channels")));
    }
    if store::channel_by_name(conn, guild_id, source_channel)?.is_none() {
        return Err(OpError::from(crate::msg!("err-no-channels")));
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
    where_to_post(conn, guild_id, discord_user_id, &source, &discord, lang)
}

pub fn cancel_event_dm(conn: &mut Connection, discord_user_id: &str) -> Result<(), OpError> {
    diesel::delete(
        schema::event_prompts::table
            .filter(schema::event_prompts::discord_user_id.eq(discord_user_id)),
    )
    .execute(conn)?;
    Ok(())
}

/// Answers the next question of someone's event DM, in their server's language.
pub fn answer_event_dm(
    conn: &mut Connection,
    discord_user_id: &str,
    text: &str,
    public_url: &str,
) -> Result<Option<Vec<DmReply>>, OpError> {
    answer_event_dm_in(conn, discord_user_id, text, public_url, None)
}

/// As `answer_event_dm`, in `lang` when given.
fn answer_event_dm_in(
    conn: &mut Connection,
    discord_user_id: &str,
    text: &str,
    public_url: &str,
    lang: Option<Lang>,
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
    let lang = lang.unwrap_or_else(|| i18n::lang_of(conn, guild_id));
    let text = text.trim();
    if said(text, &CANCEL) {
        cancel_event_dm(conn, discord_user_id)?;
        return just(DmReply::plain(tr!(lang, "dm-cancelled")));
    }
    match step.as_str() {
        "channel" => answer_channel(
            conn,
            discord_user_id,
            guild_id,
            text,
            source_channel.as_deref(),
            source_discord.as_deref(),
            lang,
        ),
        "channel_other" => {
            let others = other_channels(conn, guild_id, discord_user_id, source_channel.as_deref())?;
            let choice = text.parse::<usize>().ok().filter(|n| *n >= 1);
            let Some(channel) = choice.and_then(|n| others.get(n - 1)) else {
                return just(other_channel_prompt(&others, lang));
            };
            just(advance_to_title(conn, discord_user_id, &channel.name, lang)?)
        }
        "title" => {
            if text.is_empty() || text.chars().count() > 120 {
                return just(note(&title_card(lang), &tr!(lang, "err-title-length")));
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
            just(description_card(lang))
        }
        "description" => {
            if text.chars().count() > 2000 {
                return just(note(&description_card(lang), &tr!(lang, "err-description-length")));
            }
            draft.description = if said(text, &NONE) {
                String::new()
            } else {
                text.to_string()
            };
            save_draft(conn, discord_user_id, "start", &draft)?;
            just(start_card(conn, guild_id, lang)?)
        }
        "timezone" => {
            let choice = text.parse::<usize>().ok().filter(|n| *n >= 1);
            let Some(name) = choice.and_then(|n| clock::ZONES.get(n - 1)) else {
                return just(timezone_card(lang));
            };
            diesel::update(schema::guilds::table.filter(schema::guilds::id.eq(guild_id)))
                .set(schema::guilds::timezone.eq(*name))
                .execute(conn)?;
            save_draft(conn, discord_user_id, "start", &draft)?;
            just(start_card(conn, guild_id, lang)?)
        }
        "start" => {
            if said(text, &ZONE) {
                save_draft(conn, discord_user_id, "timezone", &draft)?;
                return just(timezone_card(lang));
            }
            let guild = store::guild(conn, guild_id)?.ok_or(OpError::NotFound)?;
            let tz = clock::zone(&guild.timezone)?;
            let stored = if said(text, &NOW) {
                Some(clock::stamp(Utc::now()))
            } else if parse::parse_when(text, Utc::now(), tz).is_some() {
                Some(text.to_string())
            } else {
                None
            };
            let Some(stored) = stored else {
                return just(note(
                    &start_card(conn, guild_id, lang)?,
                    &tr!(lang, "dm-time-unreadable"),
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
            just(end_card(lang))
        }
        "end" => {
            let guild = store::guild(conn, guild_id)?.ok_or(OpError::NotFound)?;
            let tz = clock::zone(&guild.timezone)?;
            let start = parse::parse_when(start_raw.as_deref().unwrap_or(""), Utc::now(), tz)
                .ok_or_else(|| OpError::from(crate::msg!("err-start-unreadable")))?;
            match span_minutes(text, start, tz, lang) {
                Ok(minutes) => {
                    draft.duration_minutes = minutes;
                    save_draft(conn, discord_user_id, "repeat", &draft)?;
                    just(repeat_card(lang))
                }
                Err(err) => just(note(&end_card(lang), &err)),
            }
        }
        "repeat" => {
            let cadence = match text {
                "1" => None,
                "2" => Some("daily"),
                "3" => Some("weekly"),
                "4" => Some("biweekly"),
                "5" => Some("monthly"),
                _ => return just(repeat_card(lang)),
            };
            draft.repeat = cadence.map(str::to_string);
            save_draft(conn, discord_user_id, "finish", &draft)?;
            just(finish_card(&draft, lang))
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
            lang,
        ),
        "signup" => answer_signup(conn, discord_user_id, guild_id, text, &mut draft, lang),
        "signup_custom" => {
            let names: Vec<&str> = text
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .collect();
            if names.is_empty() || names.iter().any(|name| name.chars().count() > 80) {
                return just(ask(
                    &tr!(lang, "dm-signup-custom-title"),
                    &tr!(lang, "dm-signup-custom-body"),
                    lang,
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
            just(finish_card(&draft, lang))
        }
        "signup_preset" => {
            let presets = store::list_presets(conn, guild_id)?;
            let choice = text.parse::<usize>().ok().filter(|n| *n >= 1);
            let Some(preset) = choice.and_then(|n| presets.get(n - 1)) else {
                return just(preset_card(&presets, lang));
            };
            draft.multiple = preset.multiple_signups;
            draft.types = logic::preset_types(conn, preset.id)?
                .into_iter()
                .map(|opt| draft_type(conn, opt.type_id))
                .collect::<Result<_, _>>()?;
            save_draft(conn, discord_user_id, "finish", &draft)?;
            just(finish_card(&draft, lang))
        }
        "mention" => answer_mention(conn, discord_user_id, guild_id, text, &mut draft, lang),
        "image" => {
            if said(text, &NONE) {
                draft.image_url = None;
            } else if (text.starts_with("https://") || text.starts_with("http://"))
                && text.len() <= 500
            {
                draft.image_url = Some(text.to_string());
            } else {
                return just(image_card(lang));
            }
            save_draft(conn, discord_user_id, "finish", &draft)?;
            just(finish_card(&draft, lang))
        }
        "color" => {
            let choice = text.parse::<usize>().ok().filter(|n| *n >= 1);
            let Some((color, _)) = choice.and_then(|n| EVENT_COLORS.get(n - 1)) else {
                return just(color_card(lang));
            };
            draft.color = (*color).to_string();
            save_draft(conn, discord_user_id, "finish", &draft)?;
            just(finish_card(&draft, lang))
        }
        "restrict" => answer_restrict(conn, discord_user_id, guild_id, text, &mut draft, lang),
        "role" => answer_attendee_role(conn, discord_user_id, guild_id, text, &mut draft, lang),
        "cutoff" => answer_clock_field(
            conn,
            discord_user_id,
            guild_id,
            text,
            &mut draft,
            true,
            start_raw.as_deref(),
            lang,
        ),
        "post" => answer_clock_field(
            conn,
            discord_user_id,
            guild_id,
            text,
            &mut draft,
            false,
            start_raw.as_deref(),
            lang,
        ),
        "reminder" => {
            let minutes = text
                .parse::<i64>()
                .ok()
                .filter(|n| (1..=10_080).contains(n));
            let Some(minutes) = minutes else {
                return just(reminder_card(lang));
            };
            draft.reminders = vec![ReminderInput {
                offset_minutes: minutes,
                message: String::new(),
            }];
            save_draft(conn, discord_user_id, "finish", &draft)?;
            just(finish_card(&draft, lang))
        }
        "next" => answer_next(conn, discord_user_id, membership_id, text, &draft, public_url, lang),
        "respond" => answer_respond(conn, discord_user_id, membership_id, text, &draft, lang),
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
    ("#1e3a6e", "color-marian-blue"),
    ("#2f6b4f", "color-tide-green"),
    ("#8a6232", "color-brass"),
    ("#6b3a6b", "color-plum"),
    ("#8d3a32", "color-seal-red"),
    ("#243038", "color-ink"),
];

fn just(reply: DmReply) -> Result<Option<Vec<DmReply>>, OpError> {
    Ok(Some(vec![reply]))
}

fn ask(title: &str, body: &str, lang: Lang) -> DmReply {
    DmReply::card(title, format!("{body}\n\n{}", tr!(lang, "dm-exit")))
}

fn note(card: &DmReply, line: &str) -> DmReply {
    DmReply {
        title: card.title.clone(),
        body: format!("{line}\n\n{}", card.body),
        created: None,
    }
}

fn title_card(lang: Lang) -> DmReply {
    ask(&tr!(lang, "dm-title-title"), &tr!(lang, "dm-title-body"), lang)
}

fn description_card(lang: Lang) -> DmReply {
    ask(&tr!(lang, "dm-description-title"), &tr!(lang, "dm-description-body"), lang)
}

fn end_card(lang: Lang) -> DmReply {
    ask(&tr!(lang, "dm-end-title"), &tr!(lang, "dm-end-body"), lang)
}

fn image_card(lang: Lang) -> DmReply {
    ask(&tr!(lang, "dm-image-title"), &tr!(lang, "dm-image-body"), lang)
}

fn reminder_card(lang: Lang) -> DmReply {
    ask(&tr!(lang, "dm-reminder-title"), &tr!(lang, "dm-reminder-body"), lang)
}

fn clock_card(cutoff: bool, lang: Lang) -> DmReply {
    if cutoff {
        ask(&tr!(lang, "dm-cutoff-title"), &tr!(lang, "dm-cutoff-body"), lang)
    } else {
        ask(&tr!(lang, "dm-post-title"), &tr!(lang, "dm-post-body"), lang)
    }
}

fn repeat_card(lang: Lang) -> DmReply {
    numbered_prompt(
        &tr!(lang, "dm-repeat-title"),
        &[
            tr!(lang, "dm-repeat-never"),
            tr!(lang, "dm-repeat-daily"),
            tr!(lang, "dm-repeat-weekly"),
            tr!(lang, "dm-repeat-biweekly"),
            tr!(lang, "dm-repeat-monthly"),
        ],
        lang,
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

fn finish_card(draft: &EventDraft, lang: Lang) -> DmReply {
    let mut notes = Vec::new();
    if !draft.types.is_empty() {
        let names = draft
            .types
            .iter()
            .map(|kind| kind.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        notes.push(tr!(lang, "dm-finish-types", names = names));
    }
    if draft.multiple {
        notes.push(tr!(lang, "dm-finish-multiple"));
    }
    let plain = tr!(lang, "dm-finish-plain");
    let intro = if notes.is_empty() {
        plain
    } else {
        format!("{}\n\n{plain}", notes.join("\n"))
    };
    let changes = [
        tr!(lang, "dm-finish-signup"),
        tr!(lang, "dm-finish-mention"),
        tr!(lang, "dm-finish-image"),
        tr!(lang, "dm-finish-color"),
        tr!(lang, "dm-finish-restrict"),
        tr!(lang, "dm-finish-multiple-toggle"),
        tr!(lang, "dm-finish-role"),
        tr!(lang, "dm-finish-cutoff"),
        tr!(lang, "dm-finish-post"),
        tr!(lang, "dm-finish-reminders"),
    ]
    .iter()
    .enumerate()
    .map(|(index, line)| format!("**{}** {line}", index + 2))
    .collect::<Vec<_>>()
    .join("\n");
    DmReply::card(
        tr!(lang, "dm-finish-title"),
        format!(
            "{intro}\n**1** {}\n\n{}\n{changes}\n\n{}\n{}",
            tr!(lang, "dm-finish-done"),
            tr!(lang, "dm-finish-or"),
            tr!(lang, "dm-pick-number"),
            tr!(lang, "dm-exit"),
        ),
    )
}

fn signup_card(lang: Lang) -> DmReply {
    numbered_prompt(
        &tr!(lang, "dm-signup-title"),
        &[
            tr!(lang, "dm-signup-defaults"),
            tr!(lang, "dm-signup-custom-title"),
            tr!(lang, "dm-signup-preset"),
        ],
        lang,
    )
}

fn next_card(note_line: Option<&str>, lang: Lang) -> DmReply {
    let choices = [
        tr!(lang, "dm-next-modify"),
        tr!(lang, "dm-next-clear"),
        tr!(lang, "dm-next-respond"),
        tr!(lang, "dm-next-duplicate"),
    ]
    .iter()
    .enumerate()
    .map(|(index, line)| format!("**{}** {line}", index + 1))
    .collect::<Vec<_>>()
    .join("\n");
    let body = match note_line {
        Some(line) => format!("{line}\n\n{choices}"),
        None => choices,
    };
    DmReply::card(
        tr!(lang, "dm-next-title"),
        format!("{body}\n\n{}\n{}", tr!(lang, "dm-pick-number"), tr!(lang, "dm-exit")),
    )
}

fn start_card(conn: &mut Connection, guild_id: i64, lang: Lang) -> Result<DmReply, OpError> {
    let guild = store::guild(conn, guild_id)?.ok_or(OpError::NotFound)?;
    let tz = clock::zone(&guild.timezone)?;
    let label = clock::zone_label(tz);
    Ok(ask(
        &tr!(lang, "dm-start-title"),
        &tr!(lang, "dm-start-body", zone = label),
        lang,
    ))
}

fn timezone_card(lang: Lang) -> DmReply {
    let lines = clock::ZONES
        .iter()
        .map(|name| {
            clock::zone(name)
                .map(|tz| clock::zone_label(tz))
                .unwrap_or_else(|_| (*name).to_string())
        })
        .collect::<Vec<_>>();
    numbered_prompt(&tr!(lang, "dm-zone-title"), &lines, lang)
}

fn color_card(lang: Lang) -> DmReply {
    let lines = EVENT_COLORS
        .iter()
        .map(|(_, key)| i18n::text(lang, key, &[]))
        .collect::<Vec<_>>();
    numbered_prompt(&tr!(lang, "dm-color-title"), &lines, lang)
}

fn preset_card(presets: &[store::Preset], lang: Lang) -> DmReply {
    if presets.is_empty() {
        return note(&signup_card(lang), &tr!(lang, "dm-no-presets"));
    }
    let lines = presets
        .iter()
        .map(|preset| preset.name.clone())
        .collect::<Vec<_>>();
    numbered_prompt(&tr!(lang, "dm-signup-preset"), &lines, lang)
}

/// The heading and the hint for picking roles at one step of the DM.
fn role_step(step: &str, lang: Lang) -> (String, String) {
    match step {
        "mention" => (tr!(lang, "dm-finish-mention"), tr!(lang, "dm-mention-hint")),
        "restrict" => (tr!(lang, "dm-finish-restrict"), tr!(lang, "dm-restrict-hint")),
        _ => (tr!(lang, "dm-finish-role"), tr!(lang, "dm-role-hint")),
    }
}

fn role_card(step: &str, roles: &[store::Role], lang: Lang) -> DmReply {
    let (title, extra) = role_step(step, lang);
    if roles.is_empty() {
        return ask(&title, &tr!(lang, "dm-no-roles"), lang);
    }
    let lines = roles
        .iter()
        .enumerate()
        .map(|(index, role)| format!("**{}** {}", index + 1, role.name))
        .collect::<Vec<_>>()
        .join("\n");
    ask(&title, &format!("{extra}\n\n{lines}"), lang)
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
    lang: Lang,
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
    Ok(title_card(lang))
}

fn answer_channel(
    conn: &mut Connection,
    discord_user_id: &str,
    guild_id: i64,
    text: &str,
    source_channel: Option<&str>,
    source_discord: Option<&str>,
    lang: Lang,
) -> Result<Option<Vec<DmReply>>, OpError> {
    let Some(source) = source_channel else {
        let channels = seen_channels(conn, guild_id, discord_user_id)?;
        let choice = text.parse::<usize>().ok().filter(|n| *n >= 1);
        let Some(channel) = choice.and_then(|n| channels.get(n - 1)) else {
            return just(where_to_post(conn, guild_id, discord_user_id, &None, &None, lang)?);
        };
        return just(advance_to_title(conn, discord_user_id, &channel.name, lang)?);
    };
    let source_owned = Some(source.to_string());
    let discord_owned = source_discord.map(str::to_string);
    match text {
        "1" => just(advance_to_title(conn, discord_user_id, source, lang)?),
        "2" => {
            let others = other_channels(conn, guild_id, discord_user_id, Some(source))?;
            if others.is_empty() {
                return just(where_to_post(
                    conn,
                    guild_id,
                    discord_user_id,
                    &source_owned,
                    &discord_owned,
                    lang,
                )?);
            }
            diesel::update(
                schema::event_prompts::table
                    .filter(schema::event_prompts::discord_user_id.eq(discord_user_id)),
            )
            .set(schema::event_prompts::step.eq("channel_other"))
            .execute(conn)?;
            just(other_channel_prompt(&others, lang))
        }
        _ => just(where_to_post(
            conn,
            guild_id,
            discord_user_id,
            &source_owned,
            &discord_owned,
            lang,
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
    lang: Lang,
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
                lang,
            )
        }
        "2" => {
            save_draft(conn, discord_user_id, "signup", draft)?;
            return just(signup_card(lang));
        }
        "3" => "mention",
        "4" => {
            save_draft(conn, discord_user_id, "image", draft)?;
            return just(image_card(lang));
        }
        "5" => {
            save_draft(conn, discord_user_id, "color", draft)?;
            return just(color_card(lang));
        }
        "6" => "restrict",
        "7" => {
            draft.multiple = !draft.multiple;
            save_draft(conn, discord_user_id, "finish", draft)?;
            return just(finish_card(draft, lang));
        }
        "8" => "role",
        "9" => {
            save_draft(conn, discord_user_id, "cutoff", draft)?;
            return just(clock_card(true, lang));
        }
        "10" => {
            save_draft(conn, discord_user_id, "post", draft)?;
            return just(clock_card(false, lang));
        }
        "11" => {
            save_draft(conn, discord_user_id, "reminder", draft)?;
            return just(reminder_card(lang));
        }
        _ => return just(finish_card(draft, lang)),
    };
    let roles = store::list_roles(conn, guild_id)?;
    save_draft(conn, discord_user_id, step, draft)?;
    if roles.is_empty() {
        save_draft(conn, discord_user_id, "finish", draft)?;
        return just(note(&finish_card(draft, lang), &tr!(lang, "dm-no-roles")));
    }
    just(role_card(step, &roles, lang))
}

fn answer_signup(
    conn: &mut Connection,
    discord_user_id: &str,
    guild_id: i64,
    text: &str,
    draft: &mut EventDraft,
    lang: Lang,
) -> Result<Option<Vec<DmReply>>, OpError> {
    match text {
        "1" => {
            draft.types.clear();
            draft.multiple = false;
            save_draft(conn, discord_user_id, "finish", draft)?;
            just(finish_card(draft, lang))
        }
        "2" => {
            save_draft(conn, discord_user_id, "signup_custom", draft)?;
            just(ask(
                &tr!(lang, "dm-signup-custom-title"),
                &tr!(lang, "dm-signup-custom-body"),
                lang,
            ))
        }
        "3" => {
            let presets = store::list_presets(conn, guild_id)?;
            save_draft(conn, discord_user_id, "signup_preset", draft)?;
            just(preset_card(&presets, lang))
        }
        _ => just(signup_card(lang)),
    }
}

fn answer_mention(
    conn: &mut Connection,
    discord_user_id: &str,
    guild_id: i64,
    text: &str,
    draft: &mut EventDraft,
    lang: Lang,
) -> Result<Option<Vec<DmReply>>, OpError> {
    let roles = store::list_roles(conn, guild_id)?;
    if said(text, &NONE) {
        draft.mention_everyone = false;
        draft.mention_role_ids.clear();
    } else if said(text, &EVERYONE) {
        draft.mention_everyone = true;
        draft.mention_role_ids.clear();
    } else {
        match picked_roles(text, &roles, lang) {
            Ok(ids) => {
                draft.mention_everyone = false;
                draft.mention_role_ids = ids;
            }
            Err(err) => {
                return just(note(&role_card("mention", &roles, lang), &err));
            }
        }
    }
    save_draft(conn, discord_user_id, "finish", draft)?;
    just(finish_card(draft, lang))
}

fn answer_restrict(
    conn: &mut Connection,
    discord_user_id: &str,
    guild_id: i64,
    text: &str,
    draft: &mut EventDraft,
    lang: Lang,
) -> Result<Option<Vec<DmReply>>, OpError> {
    let roles = store::list_roles(conn, guild_id)?;
    if said(text, &NONE) {
        draft.restrict_role_ids.clear();
    } else {
        match picked_roles(text, &roles, lang) {
            Ok(ids) => draft.restrict_role_ids = ids,
            Err(err) => {
                return just(note(&role_card("restrict", &roles, lang), &err));
            }
        }
    }
    save_draft(conn, discord_user_id, "finish", draft)?;
    just(finish_card(draft, lang))
}

fn answer_attendee_role(
    conn: &mut Connection,
    discord_user_id: &str,
    guild_id: i64,
    text: &str,
    draft: &mut EventDraft,
    lang: Lang,
) -> Result<Option<Vec<DmReply>>, OpError> {
    let roles = store::list_roles(conn, guild_id)?;
    if said(text, &NONE) {
        draft.attendee_role_id = None;
    } else {
        let choice = text.parse::<usize>().ok().filter(|n| *n >= 1);
        let Some(role) = choice.and_then(|n| roles.get(n - 1)) else {
            return just(note(
                &role_card("role", &roles, lang),
                &tr!(lang, "dm-pick-from-list"),
            ));
        };
        draft.attendee_role_id = Some(role.id);
    }
    save_draft(conn, discord_user_id, "finish", draft)?;
    just(finish_card(draft, lang))
}

#[allow(clippy::too_many_arguments)]
fn answer_clock_field(
    conn: &mut Connection,
    discord_user_id: &str,
    guild_id: i64,
    text: &str,
    draft: &mut EventDraft,
    cutoff: bool,
    start_raw: Option<&str>,
    lang: Lang,
) -> Result<Option<Vec<DmReply>>, OpError> {
    let guild = store::guild(conn, guild_id)?.ok_or(OpError::NotFound)?;
    let tz = clock::zone(&guild.timezone)?;
    if said(text, &NONE) {
        if cutoff {
            draft.cutoff_raw = None;
        } else {
            draft.post_raw = None;
        }
    } else if parse::parse_when(text, Utc::now(), tz).is_none() {
        return just(note(&clock_card(cutoff, lang), &tr!(lang, "dm-time-unreadable")));
    } else if cutoff {
        if let Some(start) = start_raw.and_then(|raw| parse::parse_when(raw, Utc::now(), tz)) {
            if let Some(when) = parse::parse_when(text, Utc::now(), tz) {
                if when >= start {
                    return just(note(
                        &clock_card(cutoff, lang),
                        &tr!(lang, "err-signups-close-late"),
                    ));
                }
            }
        }
        draft.cutoff_raw = Some(text.to_string());
    } else {
        draft.post_raw = Some(text.to_string());
    }
    save_draft(conn, discord_user_id, "finish", draft)?;
    just(finish_card(draft, lang))
}

#[allow(clippy::too_many_arguments)]
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
    lang: Lang,
) -> Result<Option<Vec<DmReply>>, OpError> {
    let guild = store::guild(conn, guild_id)?.ok_or(OpError::NotFound)?;
    let tz = clock::zone(&guild.timezone)?;
    let starts_at = parse::parse_when(start_raw.unwrap_or(""), Utc::now(), tz)
        .ok_or_else(|| OpError::from(crate::msg!("err-start-unreadable")))?;
    let channel = store::channel_by_name(conn, guild_id, channel_name.unwrap_or(""))?
        .ok_or_else(|| OpError::from(crate::msg!("err-channel-gone")))?;
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
            title: title.map(str::to_string).unwrap_or_else(|| tr!(lang, "post-event-fallback")),
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
        Err(err) => return just(note(&finish_card(draft, lang), &err.localized(lang))),
    };
    draft.event_id = Some(id);
    save_draft(conn, discord_user_id, "next", draft)?;
    let event = store::event(conn, id)?.ok_or(OpError::NotFound)?;
    let path = event_path(&guild, &event.public_id);
    let body = if public_url.trim().is_empty() {
        tr!(lang, "reply-open-path", path = path)
    } else {
        let link = format!("{}{path}", public_url.trim().trim_end_matches('/'));
        tr!(lang, "dm-view-event", url = link)
    };
    let mut created = DmReply::card(tr!(lang, "dm-created-title"), body);
    created.created = Some(lang);
    Ok(Some(vec![created, next_card(None, lang)]))
}

fn answer_next(
    conn: &mut Connection,
    discord_user_id: &str,
    membership_id: i64,
    text: &str,
    draft: &EventDraft,
    public_url: &str,
    lang: Lang,
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
                tr!(lang, "reply-open-path", path = path)
            } else {
                tr!(
                    lang,
                    "dm-open-event",
                    url = format!("{}{path}", public_url.trim().trim_end_matches('/'))
                )
            };
            just(next_card(Some(&line), lang))
        }
        "2" => {
            diesel::delete(schema::signups::table.filter(schema::signups::event_id.eq(event_id)))
                .execute(conn)?;
            just(next_card(Some(&tr!(lang, "dm-cleared")), lang))
        }
        "3" => {
            save_draft(conn, discord_user_id, "respond", draft)?;
            let event = store::event(conn, event_id)?.ok_or(OpError::NotFound)?;
            let options = store::options_for(conn, &event)?;
            let lines = options
                .iter()
                .map(|opt| format!("{} {}", opt.emoji, opt.name))
                .collect::<Vec<_>>();
            just(numbered_prompt(&tr!(lang, "dm-next-respond"), &lines, lang))
        }
        "4" => {
            let id = logic::duplicate_event(conn, event_id, membership_id)?;
            // The copy goes by its public id; the row number stays on the server.
            let copy = store::event(conn, id)?.ok_or(OpError::NotFound)?;
            let guild = store::guild(conn, copy.guild_id)?.ok_or(OpError::NotFound)?;
            let path = event_path(&guild, &copy.public_id);
            let line = if public_url.trim().is_empty() {
                tr!(lang, "dm-duplicated", path = path.as_str())
            } else {
                let url = format!("{}{path}", public_url.trim().trim_end_matches('/'));
                tr!(lang, "dm-duplicated-link", url = url.as_str())
            };
            just(next_card(Some(&line), lang))
        }
        _ => just(next_card(None, lang)),
    }
}

fn answer_respond(
    conn: &mut Connection,
    discord_user_id: &str,
    membership_id: i64,
    text: &str,
    draft: &EventDraft,
    lang: Lang,
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
        return just(numbered_prompt(&tr!(lang, "dm-next-respond"), &lines, lang));
    };
    let reply = logic::signup(conn, event_id, option.id, membership_id)?;
    save_draft(conn, discord_user_id, "next", draft)?;
    just(next_card(Some(&reply.render(lang)), lang))
}

fn picked_roles(text: &str, roles: &[store::Role], lang: Lang) -> Result<Vec<i64>, String> {
    let mut ids = Vec::new();
    for part in text.split(|c: char| c == ',' || c.is_whitespace()) {
        if part.is_empty() {
            continue;
        }
        let n: usize = part
            .parse()
            .map_err(|_| tr!(lang, "dm-send-numbers"))?;
        if n == 0 {
            return Err(tr!(lang, "dm-pick-from-list"));
        }
        let role = roles
            .get(n - 1)
            .ok_or_else(|| tr!(lang, "dm-not-in-list"))?;
        ids.push(role.id);
    }
    if ids.is_empty() {
        return Err(tr!(lang, "dm-send-numbers"));
    }
    Ok(ids)
}

fn span_minutes(text: &str, start: DateTime<Utc>, tz: Tz, lang: Lang) -> Result<i64, String> {
    let text = text.trim();
    if said(text, &NONE) {
        return Ok(60);
    }
    if let Some(minutes) = hour_span(text) {
        return fit_span(minutes, lang);
    }
    let Some(mut end) = parse::parse_when(text, Utc::now(), tz) else {
        return Err(tr!(lang, "dm-end-unreadable"));
    };
    if end <= start {
        end += Duration::days(1);
    }
    fit_span((end - start).num_minutes(), lang)
}

/// "2 hours" or "90 minutes", in English or Polish, as minutes.
fn hour_span(text: &str) -> Option<i64> {
    let lower = parse::normalize(text);
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

fn fit_span(minutes: i64, lang: Lang) -> Result<i64, String> {
    if (15..=1440).contains(&minutes) {
        Ok(minutes)
    } else {
        Err(tr!(lang, "err-event-length"))
    }
}

fn where_to_post(
    conn: &mut Connection,
    guild_id: i64,
    discord_user_id: &str,
    source_channel: &Option<String>,
    source_discord: &Option<String>,
    lang: Lang,
) -> Result<DmReply, OpError> {
    let guild = store::guild(conn, guild_id)?.ok_or(OpError::NotFound)?;
    let Some(source) = source_channel else {
        let lines = seen_channels(conn, guild_id, discord_user_id)?
            .into_iter()
            .map(|channel| channel_label(&channel))
            .collect::<Vec<_>>();
        return Ok(numbered_prompt(&tr!(lang, "dm-where-title"), &lines, lang));
    };
    let here = source_discord
        .as_deref()
        .filter(|id| !id.is_empty())
        .map(|id| format!("<#{id}>"))
        .unwrap_or_else(|| format!("{} #{source}", guild.name));
    Ok(numbered_prompt(
        &tr!(lang, "dm-where-title"),
        &[
            tr!(lang, "dm-where-here", channel = here),
            tr!(lang, "dm-where-other"),
        ],
        lang,
    ))
}

fn other_channels(
    conn: &mut Connection,
    guild_id: i64,
    discord_user_id: &str,
    source_channel: Option<&str>,
) -> Result<Vec<store::Channel>, OpError> {
    Ok(seen_channels(conn, guild_id, discord_user_id)?
        .into_iter()
        .filter(|channel| source_channel != Some(channel.name.as_str()))
        .collect())
}

fn other_channel_prompt(channels: &[store::Channel], lang: Lang) -> DmReply {
    let lines = channels.iter().map(channel_label).collect::<Vec<_>>();
    numbered_prompt(&tr!(lang, "dm-where-title"), &lines, lang)
}

fn channel_label(channel: &store::Channel) -> String {
    channel
        .discord_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .map(|id| format!("<#{id}>"))
        .unwrap_or_else(|| format!("#{}", channel.name))
}

fn numbered_prompt(heading: &str, lines: &[String], lang: Lang) -> DmReply {
    let body = lines
        .iter()
        .enumerate()
        .map(|(index, line)| format!("**{}** {line}", index + 1))
        .collect::<Vec<_>>()
        .join("\n");
    DmReply::card(
        heading,
        format!("{body}\n\n{}\n{}", tr!(lang, "dm-pick-number"), tr!(lang, "dm-exit")),
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
    lang: Lang,
) -> Result<String, OpError> {
    if change.title.is_none()
        && change.description.is_none()
        && change.start.is_none()
        && change.duration.is_none()
        && change.channel.is_none()
    {
        return Err(OpError::from(crate::msg!("err-edit-nothing")));
    }
    let sight = member_sight(conn, membership_id)?;
    let event = find_event(conn, guild_id, &sight, &change.event)?;
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
            OpError::from(crate::msg!("err-time-unreadable", text = start.as_str()))
        })?;
    }
    if let Some(duration) = change.duration {
        input.duration_minutes = duration;
    }
    if let Some(name) = change.channel {
        input.channel_id = store::channel_by_name(conn, event.guild_id, &name)?
            .filter(|channel| sight.sees_channel(channel.id))
            .ok_or_else(|| OpError::from(crate::msg!("err-no-channel", name = name.as_str())))?
            .id;
    }
    input.action = SaveAction::Save;
    let id = logic::save_event(conn, input)?;
    let event = store::event(conn, id)?.ok_or(OpError::NotFound)?;
    Ok(tr!(
        lang,
        "reply-event-updated",
        title = &event.title,
        path = event_path(&guild, &event.public_id)
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
        .ok_or_else(|| OpError::from(crate::msg!("err-edit-which")))?;
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
        Lang::En,
    )
}

pub fn set_reminders(
    conn: &mut Connection,
    membership_id: i64,
    enabled: Option<bool>,
    lang: Lang,
) -> Result<String, OpError> {
    match enabled {
        Some(value) => Ok(logic::set_user_reminders(conn, membership_id, value)?.render(lang)),
        None => {
            let user = store::membership(conn, membership_id)?.ok_or(OpError::NotFound)?;
            Ok(if user.reminders_enabled {
                tr!(lang, "reply-reminders-get", name = &user.name)
            } else {
                tr!(lang, "reply-reminders-opted-out", name = &user.name)
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
    set_reminders(conn, membership_id, enabled, Lang::En)
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
        .ok_or_else(|| OpError::from(crate::msg!("err-name-event")))?;
    let actor = store::membership(conn, actor_id)?.ok_or(OpError::NotFound)?;
    let handle = map
        .get("user")
        .cloned()
        .or_else(|| pos.get(2).cloned())
        .ok_or_else(|| OpError::from(crate::msg!("err-name-person")))?;
    let user = store::membership_by_handle(conn, actor.guild_id, &handle)?
        .ok_or_else(|| OpError::from(crate::msg!("err-no-handle", handle = handle.as_str())))?;
    let option_name = map
        .get("option")
        .cloned()
        .or_else(|| pos.get(3).cloned())
        .ok_or_else(|| OpError::from(crate::msg!("err-name-option")))?;
    Ok(roster_action(conn, actor_id, token, user.id, &option_name, &action, None, Lang::En)?.0)
}

pub struct Choice {
    pub label: String,
    pub value: String,
}

pub fn response_events(
    conn: &mut Connection,
    guild_id: i64,
    discord_user_id: &str,
    partial: &str,
    now: DateTime<Utc>,
    lang: Lang,
) -> Result<Vec<Choice>, OpError> {
    let guild = store::guild(conn, guild_id)?.ok_or(OpError::NotFound)?;
    let tz = clock::zone(&guild.timezone).unwrap_or(Tz::UTC);
    let needle = partial.trim().to_lowercase();
    let sight = logic::sight_for_discord(conn, guild_id, discord_user_id)?;
    let mut choices = Vec::new();
    for event in store::list_events(conn, guild_id)? {
        if event.status != "posted" || !sight.sees_event(&event) {
            continue;
        }
        let start = event.starts_at;
        let end = start + Duration::minutes(event.duration_minutes);
        if end < now {
            continue;
        }
        let start_label = menu_date(start, tz, lang);
        let end_label = menu_date(end, tz, lang);
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
    discord_user_id: &str,
    event_token: &str,
    partial: &str,
) -> Result<Vec<Choice>, OpError> {
    let sight = logic::sight_for_discord(conn, guild_id, discord_user_id)?;
    let event = match find_event(conn, guild_id, &sight, event_token) {
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
    guests: Option<i64>,
    lang: Lang,
) -> Result<(String, i64), OpError> {
    let actor = store::membership(conn, actor_id)?.ok_or(OpError::NotFound)?;
    let sight = logic::sight(conn, &actor)?;
    let event = find_event(conn, actor.guild_id, &sight, event_token)?;
    let option = resolve_option(conn, &event, option_token)?;
    let notice = logic::editor_response(conn, event.id, actor_id, target_id, option.id, action, guests)?;
    Ok((notice.render(lang), event.id))
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
    Err(OpError::from(crate::msg!("err-no-option", name = token)))
}

fn menu_date(dt: DateTime<Utc>, tz: Tz, lang: Lang) -> String {
    i18n::format_month_day(lang, dt.with_timezone(&tz).date_naive())
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
    discord_user_id: &str,
    partial: &str,
    lang: Lang,
) -> Result<Vec<Choice>, OpError> {
    let needle = partial.trim().to_lowercase();
    let mut choices = Vec::new();
    for channel in seen_channels(conn, guild_id, discord_user_id)? {
        if !needle.is_empty() && !channel.name.to_lowercase().contains(&needle) {
            continue;
        }
        let kind = if channel.kind == "event" {
            tr!(lang, "reply-channel-kind-event")
        } else {
            tr!(lang, "reply-channel-kind-text")
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
    Language(String),
}

pub fn apply_setting(
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
    setting: Setting,
    lang: Lang,
) -> Result<String, OpError> {
    require_server_manager(conn, membership_id)?;
    let mut settings = store::settings(conn, guild_id)?;
    let mut lang = lang;
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
                return Err(OpError::from(crate::msg!("err-digest-values")));
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
                        .ok_or_else(|| OpError::from(crate::msg!("err-no-role", name = name.as_str())))?
                        .id,
                ]
            };
            match which.as_str() {
                "event" => settings.role_event = role_id,
                "edit" => settings.role_edit = role_id,
                "delete" => settings.role_delete = role_id,
                "channel" => settings.role_channel = role_id,
                "signup_options" | "signup" => settings.role_signup = role_id,
                other => return Err(OpError::from(crate::msg!("err-unknown-role-setting", name = other))),
            }
            "role"
        }
        Setting::Language(code) => {
            let picked = Lang::from_code(&code)
                .ok_or_else(|| OpError::from(crate::msg!("err-unknown-language", code = code.as_str())))?;
            settings.language = picked.code().to_string();
            // The reply is in the language just picked.
            lang = picked;
            "language"
        }
    };
    store::update_settings(conn, &settings)?;
    Ok(tr!(lang, "reply-setting-updated", key = key))
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
        .ok_or_else(|| OpError::from(crate::msg!("err-name-setting")))?;
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
                .map_err(|_| OpError::from(crate::msg!("err-number-minutes")))?,
        ),
        "24_hour_time" => Setting::Hour24(on(&value)?),
        "auto_archive" => Setting::AutoArchive(on(&value)?),
        "auto_archive_interval" => Setting::AutoArchiveHours(
            value
                .parse()
                .map_err(|_| OpError::from(crate::msg!("err-number-hours")))?,
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
                .map_err(|_| OpError::from(crate::msg!("err-number-hours")))?,
        ),
        "role" => Setting::Role {
            which: value,
            name: extra,
        },
        "language" => Setting::Language(value),
        other => return Err(OpError::from(crate::msg!("err-unknown-setting", name = other))),
    };
    apply_setting(conn, guild_id, membership_id, setting, Lang::En)
}

pub fn library_text(conn: &mut Connection, guild_id: i64, lang: Lang) -> Result<String, OpError> {
    let lines: Vec<_> = store::list_library(conn, guild_id)?
        .into_iter()
        .map(|opt| format!("{} {}", opt.emoji, opt.name))
        .collect();
    if lines.is_empty() {
        Ok(tr!(lang, "reply-library-empty"))
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
    lang: Lang,
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
            Ok(tr!(lang, "reply-library-kept", emoji = emoji, name = &existing.name))
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
            Ok(tr!(lang, "reply-library-saved", emoji = &made.emoji, name = &made.name))
        }
    }
}

/// Takes a type out of the library, unless something still uses it.
pub fn remove_library_option(
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
    name: &str,
    lang: Lang,
) -> Result<String, OpError> {
    let kind = store::library_type_named(conn, guild_id, name)?
        .ok_or_else(|| OpError::from(crate::msg!("err-library-missing", name = name.trim())))?;
    logic::delete_type(conn, kind.id, membership_id)?;
    Ok(tr!(lang, "reply-library-removed", name = &kind.name))
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
                .ok_or_else(|| OpError::from(crate::msg!("err-name-library-option")))?;
            let emoji = map.get("emoji").cloned().unwrap_or_else(|| "•".into());
            // A new type counts as attending unless told otherwise.
            let attending = !matches!(map.get("attending").map(|value| value.trim().to_lowercase()).as_deref(), Some("no" | "false"));
            add_library_option(conn, guild_id, membership_id, &name, &emoji, attending, Lang::En)
        }
        "remove" => {
            let name = map
                .get("name")
                .cloned()
                .or_else(|| pos.get(1).cloned())
                .unwrap_or_default();
            remove_library_option(conn, guild_id, membership_id, &name, Lang::En)
        }
        _ => library_text(conn, guild_id, Lang::En),
    }
}

pub fn preset_text(conn: &mut Connection, guild_id: i64, lang: Lang) -> Result<String, OpError> {
    let lines: Vec<_> = store::list_presets(conn, guild_id)?
        .into_iter()
        .map(|preset| preset.name)
        .collect();
    if lines.is_empty() {
        Ok(tr!(lang, "reply-presets-empty"))
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
    lang: Lang,
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
    Ok(tr!(lang, "reply-preset-saved", name = name.trim()))
}

pub fn remove_preset(
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
    name: &str,
    lang: Lang,
) -> Result<String, OpError> {
    if let Some(preset) = store::list_presets(conn, guild_id)?
        .into_iter()
        .find(|preset| preset.name.eq_ignore_ascii_case(name))
    {
        logic::delete_preset(conn, preset.id, membership_id)?;
    }
    Ok(tr!(lang, "reply-preset-removed", name = name))
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
                .ok_or_else(|| OpError::from(crate::msg!("err-name-preset")))?;
            let raw = map.get("options").cloned().unwrap_or_default();
            add_preset(conn, guild_id, membership_id, &name, &raw, Lang::En)
        }
        "remove" => {
            let name = map
                .get("name")
                .cloned()
                .or_else(|| pos.get(1).cloned())
                .unwrap_or_default();
            remove_preset(conn, guild_id, membership_id, &name, Lang::En)
        }
        _ => preset_text(conn, guild_id, Lang::En),
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
            OpError::from(crate::msg!("err-google-needs-account"))
        })
}

pub fn google_sign_in_url(conn: &mut Connection, membership_id: i64) -> Result<String, OpError> {
    if !crate::google::configured_from_env() {
        return Err(OpError::from(crate::msg!("err-google-unconfigured")));
    }
    let base = public_base();
    if base.is_empty() {
        return Err(OpError::from(crate::msg!("err-google-no-url")));
    }
    let user_id = person_user_id(conn, membership_id)?;
    let state = crate::google::begin_link(conn, user_id)?;
    let Some(url) = crate::google::connect_url(&base, &state) else {
        return Err(OpError::from(crate::msg!("err-google-no-url")));
    };
    if url.len() > 512 {
        let _ = crate::google::finish_oauth(conn, &state);
        return Err(OpError::from(crate::msg!("err-google-link-long")));
    }
    Ok(url)
}

pub fn google_authorize(conn: &mut Connection, membership_id: i64, lang: Lang) -> Result<String, OpError> {
    let url = google_sign_in_url(conn, membership_id)?;
    Ok(format!("{}\n{url}", tr!(lang, "reply-google-click")))
}

pub fn google_auto_sync(
    conn: &mut Connection,
    _guild_id: i64,
    membership_id: i64,
    enabled: bool,
    lang: Lang,
) -> Result<String, OpError> {
    let user_id = person_user_id(conn, membership_id)?;
    crate::google::set_auto_sync(conn, user_id, enabled)?;
    Ok(if enabled {
        tr!(lang, "reply-google-auto-on")
    } else {
        tr!(lang, "reply-google-auto-off")
    })
}

pub fn google_revoke(
    conn: &mut Connection,
    _guild_id: i64,
    membership_id: i64,
    lang: Lang,
) -> Result<String, OpError> {
    let user_id = person_user_id(conn, membership_id)?;
    if !crate::google::queue_revoke(conn, user_id)? {
        return Ok(tr!(lang, "reply-google-already-off"));
    }
    Ok(tr!(lang, "reply-google-revoking"))
}

pub fn google_sync(
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
    event: &str,
    lang: Lang,
) -> Result<String, OpError> {
    let sight = member_sight(conn, membership_id)?;
    let event = find_event(conn, guild_id, &sight, event)?;
    if !matches!(event.status.as_str(), "posted" | "scheduled" | "archived") {
        return Err(OpError::from(crate::msg!("err-sync-unposted")));
    }
    let user_id = person_user_id(conn, membership_id)?;
    crate::google::queue_event(conn, user_id, event.id)?;
    Ok(tr!(lang, "reply-google-sync", title = &event.title))
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
        "authorize" => google_authorize(conn, membership_id, Lang::En),
        "auto_sync" => {
            let value = map
                .get("state")
                .cloned()
                .or_else(|| pos.get(1).cloned())
                .unwrap_or_else(|| "on".into());
            google_auto_sync(conn, guild_id, membership_id, on(&value)?, Lang::En)
        }
        "revoke" => google_revoke(conn, guild_id, membership_id, Lang::En),
        "sync" => {
            let token = map
                .get("event")
                .map(String::as_str)
                .or_else(|| pos.get(1).map(String::as_str))
                .ok_or_else(|| OpError::from(crate::msg!("err-sync-which")))?;
            google_sync(conn, guild_id, membership_id, token, Lang::En)
        }
        other => Err(OpError::from(crate::msg!("err-unknown-google", name = other))),
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
                max_guests: opt.max_guests,
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
        Err(OpError::from(crate::msg!("err-managers-only")))
    }
}

fn on(value: &str) -> Result<bool, OpError> {
    match value.to_lowercase().as_str() {
        "on" | "yes" | "true" | "1" => Ok(true),
        "off" | "no" | "false" | "0" => Ok(false),
        _ => Err(OpError::from(crate::msg!("err-on-off"))),
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
        let choices = response_options(&mut conn, 1, "100", &token, "heal").unwrap();
        assert_eq!(choices.len(), 1);
        let picked = &choices[0].value;
        assert_eq!(uuid::Uuid::parse_str(picked).unwrap().get_version_num(), 7, "not a row number");
        roster_action(&mut conn, 1, &token, 2, picked, "add", None, Lang::En).unwrap();
        let healer = store::options_for(&mut conn, &event).unwrap().into_iter().find(|opt| opt.name == "Healer").unwrap();
        assert!(store::signups_for(&mut conn, &event).unwrap().iter().any(|s| s.option_id == healer.id && s.membership_id == 2));
        // A typed name still works.
        roster_action(&mut conn, 1, &token, 2, "tank", "add", None, Lang::En).unwrap();
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
    fn a_polish_server_gets_the_questions_and_the_post_in_polish() {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Guild', 'Europe/Warsaw', '4242');
            INSERT INTO channels (id, guild_id, name, kind, position, discord_id) VALUES (1, 1, 'social', 'text', 0, '99');
            INSERT INTO users (discord_id) VALUES ('nell');
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color, reminders_enabled, server_manager)
            SELECT 1, id, 1, 'Nell', 'nell', '#112233', true, true FROM users WHERE discord_id = 'nell';
            ",
        )
        .unwrap();
        store::insert_default_settings(&mut conn, 1).unwrap();
        let switched = apply_setting(&mut conn, 1, 1, Setting::Language("pl".into()), Lang::En).unwrap();
        assert_eq!(switched, "Zaktualizowano ustawienie language.", "the reply is in the new language");
        assert_eq!(i18n::lang_of(&mut conn, 1), Lang::Pl);

        let first = start_event_dm(&mut conn, 1, 1, "u1", "social", Some("99"), Lang::Pl).unwrap();
        assert_eq!(first.title.as_deref(), Some("Gdzie opublikować to wydarzenie?"));
        assert!(first.body.contains("**1** Na obecnym kanale, <#99>"), "{first}");
        assert!(first.body.ends_with("Aby wyjść, wpisz „anuluj”"), "{first}");
        let title = only(answer_event_dm(&mut conn, "u1", "1", ""));
        assert_eq!(title.title.as_deref(), Some("Podaj tytuł wydarzenia"));
        only(answer_event_dm(&mut conn, "u1", "Sprzątanie plaży", ""));
        let start = only(answer_event_dm(&mut conn, "u1", "brak", ""));
        assert_eq!(start.title.as_deref(), Some("Kiedy wydarzenie ma się zacząć?"));
        let again = only(answer_event_dm(&mut conn, "u1", "kiedyś", ""));
        assert!(again.body.starts_with("Marian nie rozumie tej godziny."), "{again}");
        let end = only(answer_event_dm(&mut conn, "u1", "jutro o 16", ""));
        assert_eq!(end.title.as_deref(), Some("Kiedy wydarzenie się kończy?"));
        let repeat = only(answer_event_dm(&mut conn, "u1", "2 godziny", ""));
        assert_eq!(repeat.title.as_deref(), Some("Jak często wydarzenie ma się powtarzać?"));
        assert!(repeat.body.contains("**3** Co tydzień"), "{repeat}");
        let finish = only(answer_event_dm(&mut conn, "u1", "1", ""));
        assert_eq!(finish.title.as_deref(), Some("Zakończ albo dostosuj"));
        assert!(finish.body.contains("**11** Ustaw przypomnienia"), "{finish}");
        let posted = answer_event_dm(&mut conn, "u1", "1", "https://marian.example.com")
            .unwrap()
            .unwrap();
        assert_eq!(posted[0].title.as_deref(), Some("Wydarzenie zostało utworzone!"));
        assert_eq!(posted[0].created, Some(Lang::Pl));
        assert_eq!(posted[1].title.as_deref(), Some("Co chcesz zrobić?"));

        let id = channel_posts_waiting(&mut conn, "u1").unwrap()[0];
        let event = store::event(&mut conn, id).unwrap().unwrap();
        assert_eq!(event.title, "Sprzątanie plaży");
        assert_eq!(event.duration_minutes, 120);
        let local = event.starts_at.with_timezone(&chrono_tz::Europe::Warsaw);
        assert_eq!(chrono::Timelike::hour(&local), 16, "jutro o 16 is 16:00 in Warsaw");
        let view = logic::discord_view(&mut conn, id, "https://marian.example.com")
            .unwrap()
            .unwrap();
        assert!(view.body.contains("**Czas**"), "{}", view.body);
        assert!(!view.body.contains("marian.example.com"), "the web link is a button: {}", view.body);
        assert_eq!(view.footer, "Utworzone przez Nell");

        let mut sent = posted;
        link_created_event(&mut sent, "https://discord.com/channels/4242/99/555");
        assert_eq!(
            sent[0].body,
            "[Kliknij tutaj, aby zobaczyć wydarzenie](https://discord.com/channels/4242/99/555)"
        );

        let unknown = find_event(&mut conn, 1, &logic::Sight::default(), "nope").unwrap_err();
        assert_eq!(unknown.to_string(), "That event isn't in this server.", "the web reads English");
        assert_eq!(unknown.localized(Lang::Pl), "Tego wydarzenia nie ma na tym serwerze.");
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
        let first = start_event_dm(&mut conn, 1, 1, "u1", "social", Some("99"), Lang::En).unwrap();
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
        assert!(!view.body.contains("marian.example.com"), "the web link is a button: {}", view.body);
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
        let web = format!("https://marian.example.com/servers/4242/events/{}", event.public_id);
        assert_eq!(
            view.buttons.last().map(|button| (&button.role, button.label.as_str())),
            Some((&logic::ButtonRole::Web(web), "↗")),
            "the event's page is the last button"
        );
        assert!(view.buttons[..view.buttons.len() - 1].iter().all(|button| {
            matches!(button.role, logic::ButtonRole::Signup(_) | logic::ButtonRole::Guests(_))
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

        let duplicated = only(answer_event_dm(&mut conn, "u1", "4", "https://marian.example.com"));
        let copy = store::list_events(&mut conn, 1)
            .unwrap()
            .into_iter()
            .find(|other| other.id != event.id)
            .expect("a copy");
        assert!(
            duplicated.body.starts_with(&format!(
                "Duplicated it, one week later. [Open the copy](https://marian.example.com/servers/4242/events/{})",
                copy.public_id
            )),
            "{duplicated}"
        );
        assert!(
            !duplicated.body.contains(&format!("event {}", copy.id)),
            "the row number stays on the server: {duplicated}"
        );
    }
}
