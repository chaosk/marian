use std::collections::HashMap;

use chrono::{DateTime, Datelike, Duration, Timelike, Utc, Weekday};
use chrono_tz::Tz;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::access::can_edit;
use super::db::{log, must_event, must_guild, must_series, must_user};
use super::model::{SeriesTemplate, VoteQuestion, VoteSpec};
use super::series::{insert_occurrence, mark_posted};
use crate::clock;
use crate::error::OpError;
use crate::jobs::DiscordJob;
use crate::store::{self, Series};

const DAY_NAMES: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

pub fn weekday_name(day: u8) -> &'static str {
    DAY_NAMES.get(day as usize).copied().unwrap_or("Monday")
}

pub struct VoteEffect {
    pub notice: String,
    pub jobs: Vec<DiscordJob>,
}

#[derive(Debug, Clone)]
pub struct PollButton {
    pub custom_id: String,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct PollPost {
    pub channel_discord_id: String,
    pub message_id: Option<String>,
    pub questions_message_id: Option<String>,
    pub thread_discord_id: Option<String>,
    pub thread_name: String,
    pub title: String,
    pub body: String,
    pub questions_title: String,
    pub questions_body: String,
    pub has_questions: bool,
    pub color: u32,
    pub buttons: Vec<PollButton>,
    pub question_buttons: Vec<PollButton>,
    pub close_id: Option<String>,
    pub closed: bool,
}

#[derive(Serialize)]
struct DayCheck {
    n: String,
    name: String,
    on: bool,
}

#[derive(Serialize)]
struct ChoiceView {
    index: usize,
    label: String,
    votes: i64,
    yours: bool,
}

#[derive(Serialize)]
struct PollView {
    key: String,
    prompt: String,
    note: String,
    choices: Vec<ChoiceView>,
}

#[derive(Serialize)]
struct RoundView {
    closes: String,
    polls: Vec<PollView>,
}

#[derive(Serialize)]
struct WeekForm {
    default_weekday: String,
    days: Vec<DayCheck>,
    question_rows: Vec<QuestionFields>,
}

#[derive(Serialize)]
struct VoteEditor {
    enabled: bool,
    weekly: bool,
    can_edit: bool,
    close_weekday: String,
    close_time: String,
    default_weekday: String,
    days: Vec<DayCheck>,
    question_rows: Vec<QuestionFields>,
    round: Option<RoundView>,
    week: Option<WeekForm>,
}

#[derive(Serialize)]
pub struct ChoiceFields {
    pub n: usize,
    pub value: String,
    pub visible: bool,
}

#[derive(Serialize)]
pub struct QuestionFields {
    pub n: usize,
    pub prompt: String,
    pub visible: bool,
    pub choices: Vec<ChoiceFields>,
}

#[derive(Serialize, Deserialize)]
struct WeekChoices {
    #[serde(default)]
    default_weekday: u8,
    #[serde(default)]
    days: Vec<u8>,
    #[serde(default)]
    questions: Vec<VoteQuestion>,
}

struct Round {
    id: i64,
    series_id: i64,
    closes_at: String,
    status: String,
    discord_message_id: Option<String>,
    pins: String,
    spec: Option<String>,
    questions_message_id: Option<String>,
    thread_discord_id: Option<String>,
}

enum How {
    Count,
    Tie(Vec<String>),
    Empty,
    Pin,
}

struct Settled {
    label: String,
    how: How,
}

struct PollSlice {
    key: String,
    prompt: String,
    labels: Vec<String>,
    outcome: Settled,
    counts: Vec<i64>,
    yours: Vec<String>,
}

pub(super) fn check_vote(spec: &VoteSpec) -> Result<(), OpError> {
    if spec.close_weekday > 6 {
        return Err(OpError::message("Pick a weekday for the cutoff."));
    }
    if spec.close_minutes >= 24 * 60 {
        return Err(OpError::message(
            "The cutoff time has to be during the day.",
        ));
    }
    if spec.days.is_empty() || spec.days.iter().any(|day| *day > 6) {
        return Err(OpError::message("Pick the days people can vote for."));
    }
    if !unique(&spec.days) {
        return Err(OpError::message("List each day once."));
    }
    if !spec.days.contains(&spec.default_weekday) {
        return Err(OpError::message(
            "The default day has to be one of the days people can vote for.",
        ));
    }
    if spec.questions.len() > 4 {
        return Err(OpError::message("A week can have up to 4 extra questions."));
    }
    for question in &spec.questions {
        let prompt = question.prompt.trim();
        if prompt.is_empty() || prompt.chars().count() > 80 {
            return Err(OpError::message(
                "Give each question a prompt, up to 80 characters.",
            ));
        }
        if question.choices.len() < 2 {
            return Err(OpError::message(
                "Each question needs at least two choices.",
            ));
        }
        if question.choices.len() > 8 {
            return Err(OpError::message("Each question can have up to 8 choices."));
        }
        if !unique(&question.choices) {
            return Err(OpError::message("List each choice once."));
        }
        for choice in &question.choices {
            let choice = choice.trim();
            if choice.is_empty() || choice.chars().count() > 40 {
                return Err(OpError::message("Keep each choice under 40 characters."));
            }
        }
    }
    Ok(())
}

pub fn spec_from_map(map: &HashMap<String, String>) -> Result<VoteSpec, OpError> {
    let mut days: Vec<u8> = (0..7)
        .filter(|day| map.contains_key(&format!("vote_day_{day}")))
        .collect();
    if days.is_empty() {
        days = (0..7).collect();
    }
    let spec = VoteSpec {
        enabled: map.contains_key("vote"),
        close_weekday: map
            .get("vote_close_weekday")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        close_minutes: parse_minutes(
            map.get("vote_close_time")
                .map(String::as_str)
                .unwrap_or("18:00"),
        )?,
        default_weekday: map
            .get("vote_default_weekday")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        days,
        questions: questions_from_map(map, "vote")?,
    };
    if spec.enabled {
        check_vote(&spec)?;
    }
    Ok(spec)
}

pub fn open_round_id(conn: &Connection, series_id: i64) -> Result<Option<i64>, OpError> {
    Ok(open_round(conn, series_id)?.map(|round| round.id))
}

pub fn closes_label(
    conn: &Connection,
    series_id: i64,
    tz: Tz,
    hour24: bool,
) -> Result<Option<String>, OpError> {
    let Some(round) = open_round(conn, series_id)? else {
        return Ok(None);
    };
    let closes = clock::parse_stamp(&round.closes_at)?;
    Ok(Some(format!(
        "Voting until {}",
        clock::format_when(closes, tz, hour24)
    )))
}

pub fn vote_panel(
    conn: &Connection,
    series_id: i64,
    user_id: i64,
    tz: Tz,
    hour24: bool,
) -> Result<serde_json::Value, OpError> {
    let Some(series) = store::series(conn, series_id)? else {
        return Ok(serde_json::Value::Null);
    };
    let template = template_of(&series)?;
    if series.cadence != "weekly" && !template.vote.enabled {
        return Ok(serde_json::Value::Null);
    }
    let round = open_round(conn, series.id)?;
    let week_spec = round.as_ref().map(|round| spec_for(&template.vote, round));
    let round_view = match (&round, &week_spec) {
        (Some(round), Some(spec)) => {
            let slices = slices(conn, spec, round, Some(user_id))?;
            let closes = clock::parse_stamp(&round.closes_at)?;
            Some(RoundView {
                closes: clock::format_when(closes, tz, hour24),
                polls: slices
                    .iter()
                    .map(|slice| PollView {
                        key: slice.key.clone(),
                        prompt: slice.prompt.clone(),
                        note: slice_note(slice, false).unwrap_or_default(),
                        choices: slice
                            .labels
                            .iter()
                            .enumerate()
                            .map(|(index, label)| ChoiceView {
                                index,
                                label: display_label(&slice.key, label),
                                votes: slice.counts.get(index).copied().unwrap_or(0),
                                yours: slice.yours.iter().any(|choice| choice == label),
                            })
                            .collect(),
                    })
                    .collect(),
            })
        }
        _ => None,
    };
    let week = if series.cadence == "weekly" {
        week_spec.as_ref().map(|spec| WeekForm {
            default_weekday: spec.default_weekday.to_string(),
            days: day_checks(spec),
            question_rows: question_fields(&spec.questions),
        })
    } else {
        None
    };
    let editor = VoteEditor {
        enabled: template.vote.enabled,
        weekly: series.cadence == "weekly",
        can_edit: may_manage(conn, &series, user_id).is_ok(),
        close_weekday: template.vote.close_weekday.to_string(),
        close_time: format!(
            "{:02}:{:02}",
            template.vote.close_minutes / 60,
            template.vote.close_minutes % 60
        ),
        default_weekday: template.vote.default_weekday.to_string(),
        days: day_checks(&template.vote),
        question_rows: question_fields(&template.vote.questions),
        round: round_view,
        week,
    };
    serde_json::to_value(editor).map_err(|err| OpError::message(err.to_string()))
}

pub fn cast_round(
    conn: &mut Connection,
    round_id: i64,
    user_id: i64,
    poll: &str,
    choice: usize,
) -> Result<String, OpError> {
    store::with_tx(conn, |tx| cast_in(tx, round_id, user_id, poll, choice))
}

pub fn cast_series(
    conn: &mut Connection,
    series_id: i64,
    user_id: i64,
    poll: &str,
    choice: usize,
) -> Result<VoteEffect, OpError> {
    store::with_tx(conn, |tx| {
        let round_id =
            open_round_id(tx, series_id)?.ok_or_else(|| OpError::message("Voting is closed."))?;
        let notice = cast_in(tx, round_id, user_id, poll, choice)?;
        Ok(VoteEffect {
            notice,
            jobs: vec![DiscordJob::poll(round_id)],
        })
    })
}

pub fn close_series(
    conn: &mut Connection,
    series_id: i64,
    actor_id: i64,
) -> Result<VoteEffect, OpError> {
    store::with_tx(conn, |tx| close_in(tx, series_id, actor_id, Utc::now()))
}

fn close_in(
    conn: &Connection,
    series_id: i64,
    actor_id: i64,
    now: DateTime<Utc>,
) -> Result<VoteEffect, OpError> {
    let series = must_series(conn, series_id)?;
    may_manage(conn, &series, actor_id)?;
    let Some(round) = open_round(conn, series_id)? else {
        return Err(OpError::message("No vote is open."));
    };
    if round.status != "open" {
        return Err(OpError::message("Voting is closed."));
    }
    let template = template_of(&series)?;
    let event_id = decide(conn, &series, &template, &round, now)?;
    Ok(VoteEffect {
        notice: "Voting is closed. This result is posted.".into(),
        jobs: posted_jobs(event_id, round.id),
    })
}

pub fn pin_series(
    conn: &mut Connection,
    series_id: i64,
    user_id: i64,
    poll: &str,
    choice: usize,
) -> Result<VoteEffect, OpError> {
    store::with_tx(conn, |tx| {
        let series = must_series(tx, series_id)?;
        may_manage(tx, &series, user_id)?;
        let round_id =
            open_round_id(tx, series_id)?.ok_or_else(|| OpError::message("Voting is closed."))?;
        let notice = pin_in(tx, round_id, poll, choice)?;
        Ok(VoteEffect {
            notice,
            jobs: vec![DiscordJob::poll(round_id)],
        })
    })
}

pub fn save_vote_settings(
    conn: &mut Connection,
    series_id: i64,
    actor_id: i64,
    spec: VoteSpec,
) -> Result<VoteEffect, OpError> {
    store::with_tx(conn, |tx| save_in(tx, series_id, actor_id, spec))
}

pub fn save_week(
    conn: &mut Connection,
    series_id: i64,
    actor_id: i64,
    map: &HashMap<String, String>,
) -> Result<VoteEffect, OpError> {
    store::with_tx(conn, |tx| {
        let series = must_series(tx, series_id)?;
        may_manage(tx, &series, actor_id)?;
        if series.cadence != "weekly" {
            return Err(OpError::message(
                "Changing one week is for a weekly series.",
            ));
        }
        let Some(round) = open_round(tx, series_id)? else {
            return Err(OpError::message("No vote is open."));
        };
        ensure_open(&round)?;
        let template = template_of(&series)?;
        let spec = week_spec(&template.vote, map)?;
        let old = spec_for(&template.vote, &round);
        remap_votes(tx, round.id, &old, &spec)?;
        remap_pins(tx, &round, &old, &spec)?;
        store_week(tx, round.id, &spec)?;
        log(
            tx,
            series.guild_id,
            None,
            &format!("Choices for this week of {} are updated.", template.title),
        )?;
        Ok(VoteEffect {
            notice: "This week's choices are saved.".into(),
            jobs: vec![DiscordJob::poll(round.id)],
        })
    })
}

pub(super) fn maintain(
    conn: &Connection,
    guild_id: i64,
    now: DateTime<Utc>,
) -> Result<Vec<DiscordJob>, OpError> {
    let mut jobs = Vec::new();
    for series in store::list_series(conn, guild_id)? {
        let Ok(template) = template_of(&series) else {
            continue;
        };
        if !template.vote.enabled {
            continue;
        }
        if let Some(round) = open_round(conn, series.id)? {
            let closes = clock::parse_stamp(&round.closes_at)?;
            if closes <= now {
                let event_id = decide(conn, &series, &template, &round, now)?;
                jobs.extend(posted_jobs(event_id, round.id));
            }
        }
        if series.active {
            if let Some((round_id, created)) = open_next(conn, series.id, now)? {
                if created {
                    jobs.push(DiscordJob::poll(round_id));
                }
            }
        }
    }
    Ok(jobs)
}

pub(super) fn open_next(
    conn: &Connection,
    series_id: i64,
    now: DateTime<Utc>,
) -> Result<Option<(i64, bool)>, OpError> {
    if let Some(round) = open_round(conn, series_id)? {
        return Ok(Some((round.id, false)));
    }
    let series = must_series(conn, series_id)?;
    if !series.active || !matches!(series.cadence.as_str(), "weekly" | "once") {
        return Ok(None);
    }
    let template = template_of(&series)?;
    if !template.vote.enabled {
        return Ok(None);
    }
    let events = store::events_in_series(conn, series_id)?;
    if let Some(max) = series.max_occurrences {
        let done = events
            .iter()
            .filter(|event| matches!(event.status.as_str(), "posted" | "archived" | "scheduled"))
            .count();
        if done as i64 >= max {
            return Ok(None);
        }
    }
    if let Some(end) = &series.end_on {
        if now > clock::parse_stamp(end)? {
            return Ok(None);
        }
    }
    let guild = must_guild(conn, series.guild_id)?;
    let tz = clock::zone(&guild.timezone)?;
    let closes = next_close(
        now,
        tz,
        template.vote.close_weekday,
        template.vote.close_minutes,
    );
    let spec_json = week_json(&template.vote)?;
    conn.execute(
        "INSERT INTO rounds (series_id, closes_at, status, pins, spec_json, created_at) VALUES (?, ?, 'open', '{}', ?, ?)",
        params![
            series_id,
            clock::stamp(closes),
            spec_json,
            clock::stamp(now)
        ],
    )?;
    let id = conn.last_insert_rowid();
    log(
        conn,
        series.guild_id,
        None,
        &format!("Voting is open for {}.", template.title),
    )?;
    Ok(Some((id, true)))
}

pub fn poll_post(conn: &Connection, round_id: i64) -> Result<Option<PollPost>, OpError> {
    let round = must_round(conn, round_id)?;
    let series = must_series(conn, round.series_id)?;
    let template = template_of(&series)?;
    let Some(channel) = store::channel(conn, template.channel_id)? else {
        return Ok(None);
    };
    let Some(discord_id) = channel.discord_id.filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    let guild = must_guild(conn, series.guild_id)?;
    let tz = clock::zone(&guild.timezone)?;
    let settings = store::settings(conn, series.guild_id)?;
    let closed = round.status != "open";
    let spec = spec_for(&template.vote, &round);
    let poll_slices = slices(conn, &spec, &round, None)?;
    let when = clock::format_when(clock::parse_stamp(&round.closes_at)?, tz, settings.hour_24);
    let day: Vec<&PollSlice> = poll_slices
        .iter()
        .filter(|slice| slice.key == "d")
        .collect();
    let questions: Vec<&PollSlice> = poll_slices
        .iter()
        .filter(|slice| slice.key != "d")
        .collect();
    let mut body = render_slices(&day, closed);
    let mut questions_body = render_slices(&questions, closed);
    let mut buttons = Vec::new();
    let mut question_buttons = Vec::new();
    if !closed {
        buttons = slice_buttons(&day, round.id, 20);
        if buttons.len() == 20 {
            body.push_str("\n\nThe rest of the choices are on the website.");
        }
        body.push_str(&format!("\n\nCloses {when}."));
        body.push_str("\nPick every day you can make. Click a day again to take it off.");
        if !questions.is_empty() {
            question_buttons = slice_buttons(&questions, round.id, 20);
            if question_buttons.len() == 20 {
                questions_body.push_str("\n\nThe rest of the choices are on the website.");
            }
            questions_body.push_str("\n\nOne vote on each question. You can move it until then.");
        }
    } else {
        body.push_str("\n\nVoting is closed.");
        if !questions.is_empty() {
            questions_body.push_str("\n\nVoting is closed.");
        }
    }
    let title = if closed {
        format!("Vote closed · {}", template.title)
    } else {
        format!("Vote · {}", template.title)
    };
    Ok(Some(PollPost {
        channel_discord_id: discord_id,
        message_id: round.discord_message_id,
        questions_message_id: round.questions_message_id,
        thread_discord_id: round.thread_discord_id,
        thread_name: template.title.clone(),
        title,
        body,
        questions_title: "Questions".into(),
        questions_body,
        has_questions: !questions.is_empty(),
        color: color_u32(&template.color),
        buttons,
        question_buttons,
        close_id: (!closed).then(|| format!("marian:v:{}:close", round.id)),
        closed,
    }))
}

fn slice_buttons(slices: &[&PollSlice], round_id: i64, limit: usize) -> Vec<PollButton> {
    let mut buttons = Vec::new();
    for slice in slices {
        for (index, label) in slice.labels.iter().enumerate() {
            if buttons.len() == limit {
                return buttons;
            }
            buttons.push(PollButton {
                custom_id: format!("marian:v:{round_id}:{}:{index}", slice.key),
                label: clip_chars(&display_label(&slice.key, label), 80),
            });
        }
    }
    buttons
}

pub fn remember_poll_message(
    conn: &Connection,
    round_id: i64,
    message_id: &str,
) -> Result<(), OpError> {
    conn.execute(
        "UPDATE rounds SET discord_message_id = ? WHERE id = ?",
        params![message_id, round_id],
    )?;
    Ok(())
}

pub fn remember_questions_message(
    conn: &Connection,
    round_id: i64,
    message_id: Option<&str>,
) -> Result<(), OpError> {
    conn.execute(
        "UPDATE rounds SET questions_message_id = ? WHERE id = ?",
        params![message_id, round_id],
    )?;
    Ok(())
}

pub fn remember_poll_thread(
    conn: &Connection,
    round_id: i64,
    thread_id: &str,
) -> Result<(), OpError> {
    conn.execute(
        "UPDATE rounds SET thread_discord_id = ? WHERE id = ?",
        params![thread_id, round_id],
    )?;
    let event_id: Option<i64> = conn.query_row(
        "SELECT event_id FROM rounds WHERE id = ?",
        [round_id],
        |row| row.get(0),
    )?;
    let Some(event_id) = event_id else {
        return Ok(());
    };
    let event = must_event(conn, event_id)?;
    conn.execute(
        "UPDATE events SET thread_enabled = 1 WHERE id = ?",
        [event_id],
    )?;
    conn.execute(
        "INSERT INTO threads (event_id, name, discord_id) VALUES (?, ?, ?)
         ON CONFLICT(event_id) DO UPDATE SET discord_id = excluded.discord_id",
        params![event_id, event.title, thread_id],
    )?;
    Ok(())
}

fn save_in(
    conn: &Connection,
    series_id: i64,
    actor_id: i64,
    spec: VoteSpec,
) -> Result<VoteEffect, OpError> {
    let series = must_series(conn, series_id)?;
    may_manage(conn, &series, actor_id)?;
    if spec.enabled && !matches!(series.cadence.as_str(), "weekly" | "once") {
        return Err(OpError::message(
            "Voting for the day is for a single event or a weekly series.",
        ));
    }
    if spec.enabled {
        check_vote(&spec)?;
    }
    let mut jobs = Vec::new();
    let now = Utc::now();
    let notice = if spec.enabled {
        let mut template = template_of(&series)?;
        let previous = template.vote.clone();
        template.vote = spec.clone();
        store_template(conn, series_id, &template)?;
        if let Some(round) = open_round(conn, series_id)? {
            if series.cadence == "once" {
                let old = spec_for(&previous, &round);
                remap_votes(conn, round.id, &old, &spec)?;
                remap_pins(conn, &round, &old, &spec)?;
                store_week(conn, round.id, &spec)?;
            } else if round.spec.is_none() {
                store_week(conn, round.id, &previous)?;
            }
            let closes_at = clock::parse_stamp(&round.closes_at)?;
            if closes_at > now {
                let guild = must_guild(conn, series.guild_id)?;
                let tz = clock::zone(&guild.timezone)?;
                let closes = next_close(now, tz, spec.close_weekday, spec.close_minutes);
                conn.execute(
                    "UPDATE rounds SET closes_at = ? WHERE id = ?",
                    params![clock::stamp(closes), round.id],
                )?;
            }
            jobs.push(DiscordJob::poll(round.id));
        } else if let Some((round_id, _)) = open_next(conn, series_id, now)? {
            jobs.push(DiscordJob::poll(round_id));
        }
        "Voting settings saved.".to_string()
    } else {
        let current = template_of(&series)?;
        if current.vote.enabled {
            if let Some(round) = open_round(conn, series_id)? {
                let event_id = decide(conn, &series, &current, &round, now)?;
                jobs.extend(posted_jobs(event_id, round.id));
            }
        }
        let mut template = template_of(&series)?;
        template.vote = spec;
        store_template(conn, series_id, &template)?;
        schedule_ballots(conn, series_id, now)?;
        if jobs.is_empty() {
            "Voting is off.".to_string()
        } else {
            "Voting is off. This week was posted from the votes so far.".to_string()
        }
    };
    Ok(VoteEffect { notice, jobs })
}

fn schedule_ballots(conn: &Connection, series_id: i64, now: DateTime<Utc>) -> Result<(), OpError> {
    let mut stmt =
        conn.prepare("SELECT id, starts_at FROM events WHERE series_id = ? AND status = 'ballot'")?;
    let rows = stmt.query_map([series_id], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let ballots = rows.collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    let stamp = clock::stamp(now);
    for (id, starts_at) in ballots {
        let start = clock::parse_stamp(&starts_at).unwrap_or(now);
        let post_at = if start <= now { now } else { start };
        conn.execute(
            "UPDATE events SET status = 'scheduled', post_at = ?, updated_at = ? WHERE id = ?",
            params![clock::stamp(post_at), stamp, id],
        )?;
    }
    Ok(())
}

fn cast_in(
    conn: &Connection,
    round_id: i64,
    user_id: i64,
    poll: &str,
    choice: usize,
) -> Result<String, OpError> {
    let round = must_round(conn, round_id)?;
    ensure_open(&round)?;
    let series = must_series(conn, round.series_id)?;
    let user = must_user(conn, user_id)?;
    if user.guild_id != series.guild_id {
        return Err(OpError::NotFound);
    }
    let template = template_of(&series)?;
    let spec = spec_for(&template.vote, &round);
    let labels = labels_for(&spec, poll)?;
    let Some(label) = labels.get(choice).cloned() else {
        return Err(OpError::message("That choice isn't on this vote."));
    };
    let stamp = clock::stamp(Utc::now());
    if poll == "d" {
        let removed = conn.execute(
            "DELETE FROM round_votes
             WHERE round_id = ? AND poll_key = ? AND user_id = ? AND choice = ?",
            params![round_id, poll, user_id, label],
        )?;
        if removed == 0 {
            conn.execute(
                "INSERT INTO round_votes (round_id, poll_key, choice, user_id, created_at)
                 VALUES (?, ?, ?, ?, ?)",
                params![round_id, poll, label, user_id, stamp],
            )?;
        }
        let votes = votes_for(conn, round_id, poll)?;
        let picks: Vec<String> = votes
            .into_iter()
            .filter(|(id, _)| *id == user_id)
            .map(|(_, choice)| choice)
            .collect();
        return Ok(day_saved(&labels, &picks));
    }
    conn.execute(
        "DELETE FROM round_votes WHERE round_id = ? AND poll_key = ? AND user_id = ?",
        params![round_id, poll, user_id],
    )?;
    conn.execute(
        "INSERT INTO round_votes (round_id, poll_key, choice, user_id, created_at)
         VALUES (?, ?, ?, ?, ?)",
        params![round_id, poll, label, user_id, stamp],
    )?;
    Ok("Vote saved.".into())
}

fn day_saved(labels: &[String], picks: &[String]) -> String {
    let names: Vec<String> = labels
        .iter()
        .filter(|label| picks.iter().any(|pick| pick == *label))
        .map(|label| display_label("d", label))
        .collect();
    if names.is_empty() {
        "Day vote cleared.".into()
    } else {
        format!("You can make {}.", join_and(&names))
    }
}

fn pin_in(conn: &Connection, round_id: i64, poll: &str, choice: usize) -> Result<String, OpError> {
    let round = must_round(conn, round_id)?;
    ensure_open(&round)?;
    let series = must_series(conn, round.series_id)?;
    let template = template_of(&series)?;
    let spec = spec_for(&template.vote, &round);
    let labels = labels_for(&spec, poll)?;
    let Some(label) = labels.get(choice) else {
        return Err(OpError::message("That choice isn't on this vote."));
    };
    let mut pins = pin_map(&round.pins);
    pins.insert(poll.to_string(), label.clone());
    let json = serde_json::to_string(&pins)
        .map_err(|err| OpError::message(format!("Could not store the pin: {err}")))?;
    conn.execute(
        "UPDATE rounds SET pins = ? WHERE id = ?",
        params![json, round_id],
    )?;
    Ok("Result pinned.".into())
}

fn decide(
    conn: &Connection,
    series: &Series,
    template: &SeriesTemplate,
    round: &Round,
    now: DateTime<Utc>,
) -> Result<i64, OpError> {
    let spec = spec_for(&template.vote, round);
    let poll_slices = slices(conn, &spec, round, None)?;
    let day = poll_slices
        .iter()
        .find(|slice| slice.key == "d")
        .ok_or_else(|| OpError::message("This vote has no day."))?;
    let weekday = day
        .outcome
        .label
        .parse::<u8>()
        .unwrap_or(spec.default_weekday);
    let guild = must_guild(conn, series.guild_id)?;
    let tz = clock::zone(&guild.timezone)?;
    let anchor = clock::parse_stamp(&series.anchor_start)?;
    let closes = clock::parse_stamp(&round.closes_at)?;
    let floor = if now < closes { now } else { closes };
    let start = winning_start(floor, weekday, anchor, tz);
    let description = voted_description(template, &poll_slices);
    let event_id = write_event(conn, series, template, start, &description, now, tz)?;
    adopt_vote_thread(conn, event_id, round)?;
    conn.execute(
        "UPDATE rounds SET status = 'decided', event_id = ? WHERE id = ?",
        params![event_id, round.id],
    )?;
    log(
        conn,
        series.guild_id,
        Some(event_id),
        &format!(
            "Voting closed for {}. It starts {}.",
            template.title,
            weekday_name(weekday)
        ),
    )?;
    Ok(event_id)
}

fn write_event(
    conn: &Connection,
    series: &Series,
    template: &SeriesTemplate,
    start: DateTime<Utc>,
    description: &str,
    now: DateTime<Utc>,
    tz: Tz,
) -> Result<i64, OpError> {
    let events = store::events_in_series(conn, series.id)?;
    let cutoff = template
        .cutoff_offset_minutes
        .map(|mins| start - Duration::minutes(mins));
    if let Some(ballot) = events.iter().find(|event| event.status == "ballot") {
        conn.execute(
            "UPDATE events SET starts_at = ?, description = ?, cutoff_at = ?, updated_at = ? WHERE id = ?",
            params![
                clock::stamp(start),
                description,
                cutoff.map(|dt| clock::stamp(dt)),
                clock::stamp(now),
                ballot.id,
            ],
        )?;
        mark_posted(conn, ballot.id, now)?;
        return Ok(ballot.id);
    }
    let creator = events
        .first()
        .map(|event| event.created_by)
        .ok_or_else(|| OpError::message("This series has no event to post from."))?;
    let index = events
        .iter()
        .filter_map(|event| event.occurrence_index)
        .max()
        .unwrap_or(0)
        + 1;
    let mut posted = template.clone();
    posted.description = description.to_string();
    let id = insert_occurrence(conn, series, &posted, creator, index, start, tz)?;
    mark_posted(conn, id, now)?;
    Ok(id)
}

fn slices(
    conn: &Connection,
    spec: &VoteSpec,
    round: &Round,
    user_id: Option<i64>,
) -> Result<Vec<PollSlice>, OpError> {
    let mut out = Vec::new();
    out.push(one_slice(
        conn,
        spec,
        round,
        user_id,
        "d",
        "Which day?",
        &spec.default_weekday.to_string(),
    )?);
    for (index, question) in spec.questions.iter().enumerate() {
        let fallback = question.choices.first().cloned().unwrap_or_default();
        out.push(one_slice(
            conn,
            spec,
            round,
            user_id,
            &index.to_string(),
            &question.prompt,
            &fallback,
        )?);
    }
    Ok(out)
}

fn one_slice(
    conn: &Connection,
    spec: &VoteSpec,
    round: &Round,
    user_id: Option<i64>,
    key: &str,
    prompt: &str,
    fallback: &str,
) -> Result<PollSlice, OpError> {
    let labels = labels_for(spec, key)?;
    let votes = votes_for(conn, round.id, key)?;
    let choices: Vec<String> = votes.iter().map(|(_, choice)| choice.clone()).collect();
    let pins = pin_map(&round.pins);
    let outcome = settle(
        &labels,
        &choices,
        pins.get(key).map(String::as_str),
        fallback,
    );
    let counts = labels
        .iter()
        .map(|label| choices.iter().filter(|choice| *choice == label).count() as i64)
        .collect();
    let yours = votes
        .iter()
        .filter(|(id, _)| user_id == Some(*id))
        .map(|(_, choice)| choice.clone())
        .collect();
    Ok(PollSlice {
        key: key.to_string(),
        prompt: prompt.to_string(),
        labels,
        outcome,
        counts,
        yours,
    })
}

fn settle(labels: &[String], votes: &[String], pin: Option<&str>, fallback: &str) -> Settled {
    if let Some(pin) = pin {
        if labels.iter().any(|label| label == pin) {
            return Settled {
                label: pin.to_string(),
                how: How::Pin,
            };
        }
    }
    let counts: Vec<i64> = labels
        .iter()
        .map(|label| votes.iter().filter(|vote| *vote == label).count() as i64)
        .collect();
    let best = counts.iter().copied().max().unwrap_or(0);
    if best == 0 {
        let label = if labels.iter().any(|label| label == fallback) {
            fallback.to_string()
        } else {
            labels.first().cloned().unwrap_or_default()
        };
        return Settled {
            label,
            how: How::Empty,
        };
    }
    let tied: Vec<String> = labels
        .iter()
        .enumerate()
        .filter(|(index, _)| counts[*index] == best)
        .map(|(_, label)| label.clone())
        .collect();
    if tied.len() == 1 {
        Settled {
            label: tied[0].clone(),
            how: How::Count,
        }
    } else {
        let label = tied[0].clone();
        Settled {
            label,
            how: How::Tie(tied),
        }
    }
}

fn slice_note(slice: &PollSlice, closed: bool) -> Option<String> {
    let shown = display_label(&slice.key, &slice.outcome.label);
    let day = slice.key == "d";
    match &slice.outcome.how {
        How::Count => None,
        How::Pin => Some(if day {
            format!("An organizer set the day to {shown}.")
        } else {
            format!("An organizer chose {shown}.")
        }),
        How::Empty => Some(if day {
            if closed {
                format!("Nobody voted on the day. Marian used {shown}.")
            } else {
                format!("Nobody has voted on the day. Marian will use {shown}.")
            }
        } else if closed {
            format!("Nobody voted. Marian used {shown}.")
        } else {
            format!("Nobody has voted. Marian will use {shown}.")
        }),
        How::Tie(labels) => {
            let names: Vec<String> = labels
                .iter()
                .map(|label| display_label(&slice.key, label))
                .collect();
            let list = join_and(&names);
            Some(if closed {
                format!("{list} tied. Marian used {shown}, the earlier choice.")
            } else {
                format!("{list} are tied. Marian will use {shown}, the earlier choice.")
            })
        }
    }
}

fn voted_description(template: &SeriesTemplate, slices: &[PollSlice]) -> String {
    let mut out = template.description.trim().to_string();
    for slice in slices {
        if slice.key == "d" {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&format!(
            "**{}**\n{}",
            slice.prompt,
            display_label(&slice.key, &slice.outcome.label)
        ));
    }
    let notes: Vec<String> = slices
        .iter()
        .filter_map(|slice| slice_note(slice, true))
        .collect();
    if !notes.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&notes.join("\n"));
    }
    super::db::trim_chars(&out, 2000)
}

fn render_slices(slices: &[&PollSlice], closed: bool) -> String {
    let mut lines = Vec::new();
    for slice in slices {
        lines.push(format!("**{}**", slice.prompt));
        for (index, label) in slice.labels.iter().enumerate() {
            let name = display_label(&slice.key, label);
            let count = slice.counts.get(index).copied().unwrap_or(0);
            if label == &slice.outcome.label {
                lines.push(format!("**{name}** — {count}"));
            } else {
                lines.push(format!("{name} — {count}"));
            }
        }
        if let Some(note) = slice_note(slice, closed) {
            lines.push(note);
        }
        lines.push(String::new());
    }
    lines.join("\n").trim_end().to_string()
}

fn adopt_vote_thread(conn: &Connection, event_id: i64, round: &Round) -> Result<(), OpError> {
    let event = must_event(conn, event_id)?;
    conn.execute(
        "UPDATE events SET thread_enabled = 1, discord_message_id = COALESCE(?, discord_message_id) WHERE id = ?",
        params![round.discord_message_id, event_id],
    )?;
    if let Some(thread_id) = &round.thread_discord_id {
        conn.execute(
            "INSERT INTO threads (event_id, name, discord_id) VALUES (?, ?, ?)
             ON CONFLICT(event_id) DO UPDATE SET discord_id = excluded.discord_id",
            params![event_id, event.title, thread_id],
        )?;
    }
    Ok(())
}

fn labels_for(spec: &VoteSpec, key: &str) -> Result<Vec<String>, OpError> {
    if key == "d" {
        if spec.days.is_empty() {
            return Err(OpError::message("Pick the days people can vote for."));
        }
        return Ok(spec.days.iter().map(|day| day.to_string()).collect());
    }
    let index: usize = key
        .parse()
        .map_err(|_| OpError::message("That question isn't on this vote."))?;
    let question = spec
        .questions
        .get(index)
        .ok_or_else(|| OpError::message("That question isn't on this vote."))?;
    Ok(question.choices.clone())
}

fn votes_for(conn: &Connection, round_id: i64, key: &str) -> Result<Vec<(i64, String)>, OpError> {
    let mut stmt = conn
        .prepare("SELECT user_id, choice FROM round_votes WHERE round_id = ? AND poll_key = ?")?;
    let rows = stmt.query_map(params![round_id, key], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(OpError::from)
}

fn next_close(now: DateTime<Utc>, tz: Tz, weekday: u8, minutes: u16) -> DateTime<Utc> {
    let hour = u32::from(minutes / 60);
    let minute = u32::from(minutes % 60);
    let dt = clock::next_weekday_at(now, tz, weekday_chrono(weekday), hour, minute, 0);
    if dt > now {
        dt
    } else {
        dt + Duration::days(7)
    }
}

fn winning_start(
    closes_at: DateTime<Utc>,
    weekday: u8,
    anchor: DateTime<Utc>,
    tz: Tz,
) -> DateTime<Utc> {
    let local = anchor.with_timezone(&tz);
    let hour = local.hour();
    let minute = local.minute();
    let target = weekday_chrono(weekday);
    let mut date = closes_at.with_timezone(&tz).date_naive();
    for _ in 0..21 {
        if date.weekday() == target {
            let start = clock::at_local(date, hour, minute, tz);
            if start > closes_at {
                return start;
            }
        }
        date = date.succ_opt().unwrap_or(date);
    }
    closes_at + Duration::days(7)
}

fn weekday_chrono(day: u8) -> Weekday {
    match day {
        0 => Weekday::Mon,
        1 => Weekday::Tue,
        2 => Weekday::Wed,
        3 => Weekday::Thu,
        4 => Weekday::Fri,
        5 => Weekday::Sat,
        _ => Weekday::Sun,
    }
}

fn display_label(key: &str, label: &str) -> String {
    if key == "d" {
        label
            .parse::<u8>()
            .map(weekday_name)
            .unwrap_or(label)
            .to_string()
    } else {
        label.to_string()
    }
}

fn day_checks(spec: &VoteSpec) -> Vec<DayCheck> {
    (0..7)
        .map(|day| DayCheck {
            n: day.to_string(),
            name: weekday_name(day).to_string(),
            on: spec.days.is_empty() || spec.days.contains(&day),
        })
        .collect()
}

pub fn question_fields(questions: &[VoteQuestion]) -> Vec<QuestionFields> {
    let count = questions.len().min(4);
    (0..4)
        .map(|n| {
            let stored = questions
                .get(n)
                .map(|question| question.choices.clone())
                .unwrap_or_default();
            let visible = n < count || (count == 0 && n == 0);
            let shown = if visible { stored.len().clamp(2, 8) } else { 0 };
            QuestionFields {
                n,
                prompt: questions
                    .get(n)
                    .map(|question| question.prompt.clone())
                    .unwrap_or_default(),
                visible,
                choices: (0..8)
                    .map(|index| ChoiceFields {
                        n: index,
                        value: stored.get(index).cloned().unwrap_or_default(),
                        visible: index < shown,
                    })
                    .collect(),
            }
        })
        .collect()
}

fn ensure_open(round: &Round) -> Result<(), OpError> {
    if round.status != "open" {
        return Err(OpError::message("Voting is closed."));
    }
    let closes = clock::parse_stamp(&round.closes_at)?;
    if closes <= Utc::now() {
        return Err(OpError::message("Voting is closed."));
    }
    Ok(())
}

fn may_manage(conn: &Connection, series: &Series, actor_id: i64) -> Result<(), OpError> {
    let actor = must_user(conn, actor_id)?;
    if actor.guild_id != series.guild_id {
        return Err(OpError::NotFound);
    }
    if actor.organizer {
        return Ok(());
    }
    let settings = store::settings(conn, series.guild_id)?;
    let roles = store::user_role_ids(conn, actor.id)?;
    let events = store::events_in_series(conn, series.id)?;
    if events
        .iter()
        .any(|event| can_edit(&actor, event, &settings, &roles))
    {
        return Ok(());
    }
    Err(OpError::message("You can't change this vote."))
}

fn posted_jobs(event_id: i64, round_id: i64) -> Vec<DiscordJob> {
    vec![
        DiscordJob::sync(event_id),
        DiscordJob::attend(event_id),
        DiscordJob::poll(round_id),
    ]
}

fn template_of(series: &Series) -> Result<SeriesTemplate, OpError> {
    serde_json::from_str(&series.template_json)
        .map_err(|err| OpError::message(format!("The series template is unreadable: {err}")))
}

fn store_template(
    conn: &Connection,
    series_id: i64,
    template: &SeriesTemplate,
) -> Result<(), OpError> {
    let json = serde_json::to_string(template)
        .map_err(|err| OpError::message(format!("Could not store the series: {err}")))?;
    conn.execute(
        "UPDATE series SET template_json = ? WHERE id = ?",
        params![json, series_id],
    )?;
    Ok(())
}

fn open_round(conn: &Connection, series_id: i64) -> Result<Option<Round>, OpError> {
    conn.query_row(
        "SELECT id, series_id, closes_at, status, discord_message_id, pins, spec_json,
                questions_message_id, thread_discord_id
         FROM rounds WHERE series_id = ? AND status = 'open'",
        [series_id],
        map_round,
    )
    .optional()
    .map_err(OpError::from)
}

fn must_round(conn: &Connection, id: i64) -> Result<Round, OpError> {
    conn.query_row(
        "SELECT id, series_id, closes_at, status, discord_message_id, pins, spec_json,
                questions_message_id, thread_discord_id
         FROM rounds WHERE id = ?",
        [id],
        map_round,
    )
    .optional()?
    .ok_or(OpError::NotFound)
}

fn map_round(row: &rusqlite::Row<'_>) -> rusqlite::Result<Round> {
    Ok(Round {
        id: row.get(0)?,
        series_id: row.get(1)?,
        closes_at: row.get(2)?,
        status: row.get(3)?,
        discord_message_id: row.get(4)?,
        pins: row.get(5)?,
        spec: row.get(6)?,
        questions_message_id: row.get(7)?,
        thread_discord_id: row.get(8)?,
    })
}

fn spec_for(base: &VoteSpec, round: &Round) -> VoteSpec {
    let Some(raw) = round.spec.as_deref() else {
        return base.clone();
    };
    let Ok(week) = serde_json::from_str::<WeekChoices>(raw) else {
        return base.clone();
    };
    let mut spec = base.clone();
    spec.default_weekday = week.default_weekday;
    if !week.days.is_empty() {
        spec.days = week.days;
    }
    spec.questions = week.questions;
    spec
}

fn week_json(spec: &VoteSpec) -> Result<String, OpError> {
    serde_json::to_string(&WeekChoices {
        default_weekday: spec.default_weekday,
        days: spec.days.clone(),
        questions: spec.questions.clone(),
    })
    .map_err(|err| OpError::message(format!("Could not store this week's choices: {err}")))
}

fn store_week(conn: &Connection, round_id: i64, spec: &VoteSpec) -> Result<(), OpError> {
    conn.execute(
        "UPDATE rounds SET spec_json = ? WHERE id = ?",
        params![week_json(spec)?, round_id],
    )?;
    Ok(())
}

fn week_spec(base: &VoteSpec, map: &HashMap<String, String>) -> Result<VoteSpec, OpError> {
    let mut days: Vec<u8> = (0..7)
        .filter(|day| map.contains_key(&format!("week_day_{day}")))
        .collect();
    if days.is_empty() {
        days = (0..7).collect();
    }
    let mut spec = base.clone();
    spec.enabled = true;
    spec.default_weekday = map
        .get("week_default_weekday")
        .and_then(|value| value.parse().ok())
        .unwrap_or(base.default_weekday);
    spec.days = days;
    spec.questions = questions_from_map(map, "week")?;
    check_vote(&spec)?;
    Ok(spec)
}

struct StoredVote {
    id: i64,
    poll_key: String,
    choice: String,
    user_id: i64,
}

fn remap_votes(
    conn: &Connection,
    round_id: i64,
    old: &VoteSpec,
    new: &VoteSpec,
) -> Result<(), OpError> {
    let mut stmt =
        conn.prepare("SELECT id, poll_key, choice, user_id FROM round_votes WHERE round_id = ?")?;
    let votes = stmt
        .query_map([round_id], |row| {
            Ok(StoredVote {
                id: row.get(0)?,
                poll_key: row.get(1)?,
                choice: row.get(2)?,
                user_id: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    let mut drop_ids = Vec::new();
    let mut keep: Vec<(i64, String, i64, String)> = Vec::new();
    for vote in votes {
        let Some(key) = mapped_key(old, new, &vote.poll_key, &vote.choice) else {
            drop_ids.push(vote.id);
            continue;
        };
        let duplicate = keep.iter().any(|(_, kept, user_id, kept_choice)| {
            *user_id == vote.user_id && kept == &key && (key != "d" || kept_choice == &vote.choice)
        });
        if duplicate {
            drop_ids.push(vote.id);
        } else {
            keep.push((vote.id, key, vote.user_id, vote.choice));
        }
    }
    for id in drop_ids {
        conn.execute("DELETE FROM round_votes WHERE id = ?", [id])?;
    }
    for (id, _, _, _) in &keep {
        conn.execute(
            "UPDATE round_votes SET poll_key = ? WHERE id = ?",
            params![format!("hold-{id}"), id],
        )?;
    }
    for (id, key, _, _) in &keep {
        conn.execute(
            "UPDATE round_votes SET poll_key = ? WHERE id = ?",
            params![key, id],
        )?;
    }
    Ok(())
}

fn remap_pins(
    conn: &Connection,
    round: &Round,
    old: &VoteSpec,
    new: &VoteSpec,
) -> Result<(), OpError> {
    let pins = pin_map(&round.pins);
    let mut next = HashMap::new();
    for (key, choice) in pins {
        if let Some(new_key) = mapped_key(old, new, &key, &choice) {
            next.insert(new_key, choice);
        }
    }
    let json = serde_json::to_string(&next)
        .map_err(|err| OpError::message(format!("Could not store the pin: {err}")))?;
    conn.execute(
        "UPDATE rounds SET pins = ? WHERE id = ?",
        params![json, round.id],
    )?;
    Ok(())
}

fn mapped_key(old: &VoteSpec, new: &VoteSpec, poll_key: &str, choice: &str) -> Option<String> {
    if poll_key == "d" {
        let day = choice.parse::<u8>().ok()?;
        return new.days.contains(&day).then(|| "d".to_string());
    }
    let index = poll_key.parse::<usize>().ok()?;
    let prompt = old.questions.get(index)?.prompt.as_str();
    let new_index = new
        .questions
        .iter()
        .position(|question| question.prompt == prompt)?;
    let question = &new.questions[new_index];
    question
        .choices
        .iter()
        .any(|item| item == choice)
        .then(|| new_index.to_string())
}

fn pin_map(raw: &str) -> HashMap<String, String> {
    serde_json::from_str(raw).unwrap_or_default()
}

fn parse_minutes(raw: &str) -> Result<u16, OpError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(18 * 60);
    }
    let mut parts = raw.split(':');
    let hour: u16 = parts
        .next()
        .unwrap_or("")
        .parse()
        .map_err(|_| OpError::message("Use a time like 18:00 for the cutoff."))?;
    let minute: u16 = parts
        .next()
        .unwrap_or("0")
        .parse()
        .map_err(|_| OpError::message("Use a time like 18:00 for the cutoff."))?;
    if hour > 23 || minute > 59 {
        return Err(OpError::message("Use a time like 18:00 for the cutoff."));
    }
    Ok(hour * 60 + minute)
}

pub fn questions_from_map(
    map: &HashMap<String, String>,
    prefix: &str,
) -> Result<Vec<VoteQuestion>, OpError> {
    let structured = (0..4).any(|n| {
        map.contains_key(&format!("{prefix}_q{n}_prompt"))
            || (0..8).any(|choice| map.contains_key(&format!("{prefix}_q{n}_c{choice}")))
    });
    if !structured {
        return parse_questions(
            map.get(&format!("{prefix}_questions"))
                .map(String::as_str)
                .unwrap_or(""),
        );
    }
    let mut questions = Vec::new();
    for n in 0..4 {
        let prompt = map
            .get(&format!("{prefix}_q{n}_prompt"))
            .map(|value| value.trim().to_string())
            .unwrap_or_default();
        let choices = (0..8)
            .filter_map(|choice| map.get(&format!("{prefix}_q{n}_c{choice}")))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        if prompt.is_empty() && choices.is_empty() {
            continue;
        }
        questions.push(VoteQuestion { prompt, choices });
    }
    Ok(questions)
}

fn parse_questions(raw: &str) -> Result<Vec<VoteQuestion>, OpError> {
    let mut questions = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((prompt, rest)) = line.split_once('|') else {
            return Err(OpError::message(
                "Each question needs a prompt, a |, and the choices.",
            ));
        };
        let choices = rest
            .split(',')
            .map(|part| part.trim().to_string())
            .filter(|part| !part.is_empty())
            .collect();
        questions.push(VoteQuestion {
            prompt: prompt.trim().to_string(),
            choices,
        });
    }
    Ok(questions)
}

fn unique<T: PartialEq>(items: &[T]) -> bool {
    items
        .iter()
        .enumerate()
        .all(|(index, item)| !items[..index].contains(item))
}

fn join_and(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [first, second] => format!("{first} and {second}"),
        _ => {
            let (last, rest) = names.split_last().unwrap();
            format!("{}, and {last}", rest.join(", "))
        }
    }
}

fn color_u32(hex: &str) -> u32 {
    u32::from_str_radix(hex.trim().trim_start_matches('#'), 16).unwrap_or(0x1e3a6e)
}

fn clip_chars(value: &str, max: usize) -> String {
    let clipped: String = value.chars().take(max).collect();
    if clipped.is_empty() {
        "Choice".into()
    } else {
        clipped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::logic::{
        self, EventInput, OptionInput, ReminderInput, RepeatSpec, SaveAction, SeriesTemplate,
    };
    use crate::present::{self, Page};
    use chrono::TimeZone;

    fn world() -> Connection {
        let conn = store::open_memory().unwrap();
        conn.execute_batch(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Test', 'UTC', '99');
            INSERT INTO settings (guild_id) VALUES (1);
            INSERT INTO channels (id, guild_id, name, kind, position, discord_id)
              VALUES (1, 1, 'events', 'text', 0, '555');
            INSERT INTO users (id, guild_id, name, handle, color, reminders_enabled, organizer) VALUES
              (1, 1, 'Nell', 'nell', '#112233', 1, 1),
              (2, 1, 'June', 'june', '#223344', 1, 0),
              (3, 1, 'Samir', 'samir', '#334455', 1, 0);
            ",
        )
        .unwrap();
        conn
    }

    fn week_fields(days: &[u8], default_day: u8, questions: &str) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for day in days {
            map.insert(format!("week_day_{day}"), "1".into());
        }
        map.insert("week_default_weekday".into(), default_day.to_string());
        map.insert("week_questions".into(), questions.into());
        map
    }

    fn spec() -> VoteSpec {
        VoteSpec {
            enabled: true,
            close_weekday: 0,
            close_minutes: 18 * 60,
            default_weekday: 1,
            days: vec![1, 4],
            questions: vec![VoteQuestion {
                prompt: "What are we doing?".into(),
                choices: vec!["Raid".into(), "PvP".into()],
            }],
        }
    }

    fn voting(start: DateTime<Utc>) -> EventInput {
        EventInput {
            id: None,
            guild_id: 1,
            actor_id: 1,
            channel_id: 1,
            title: "Raid".into(),
            description: "Bring a snack.".into(),
            starts_at: start,
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
            options: vec![OptionInput {
                id: None,
                name: "Accepted".into(),
                emoji: "✅".into(),
                capacity: None,
                waitlist: false,
                role_id: None,
                reminders_enabled: true,
            }],
            reminders: vec![ReminderInput {
                offset_minutes: 15,
                message: String::new(),
            }],
            repeat: Some(RepeatSpec {
                cadence: "weekly".into(),
                end_on: None,
                max_occurrences: None,
            }),
            action: SaveAction::Publish,
            post_at: None,
            apply_series: false,
            vote: Some(spec()),
        }
    }

    fn anchor() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 15, 19, 0, 0).unwrap()
    }

    fn round_of(conn: &Connection) -> i64 {
        conn.query_row("SELECT id FROM rounds WHERE status = 'open'", [], |row| {
            row.get(0)
        })
        .unwrap()
    }

    fn backdate(conn: &Connection, id: i64, closes: &str) {
        conn.execute(
            "UPDATE rounds SET closes_at = ? WHERE id = ?",
            params![closes, id],
        )
        .unwrap();
    }

    fn posted_start(conn: &Connection) -> String {
        conn.query_row(
            "SELECT starts_at FROM events WHERE status = 'posted' ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn old_series_json_still_loads() {
        let raw = r##"{"title":"A","description":"","duration_minutes":60,"channel_id":1,"color":"#1e3a6e","image_url":null,"thread_enabled":false,"multiple_signups":false,"mention_everyone":false,"attendee_role_id":null,"mention_role_ids":[],"restrict_role_ids":[],"cutoff_offset_minutes":null,"options":[],"reminders":[]}"##;
        let template: SeriesTemplate = serde_json::from_str(raw).unwrap();
        assert!(!template.vote.enabled);
    }

    #[test]
    fn weekly_vote_does_not_schedule_ahead() {
        let mut conn = world();
        let id = logic::save_event(&mut conn, voting(anchor())).unwrap();
        let event = store::event(&conn, id).unwrap().unwrap();
        assert_eq!(event.status, "ballot");
        logic::tick(&mut conn, Utc::now()).unwrap();
        let total: i64 = conn
            .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
            .unwrap();
        let scheduled: i64 = conn
            .query_row(
                "SELECT count(*) FROM events WHERE status = 'scheduled'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(scheduled, 0);
        let page = Page {
            conn: &conn,
            guild_id: 1,
            user_id: 1,
            notice: None,
            invite_url: "",
            nav: "recurring",
            title: "Series",
            allowed_guilds: &[1],
        };
        let body = present::series_page(&page, event.series_id.unwrap()).unwrap();
        assert!(body.contains("Which day"), "{body}");
        assert!(body.contains("What are we doing?"), "{body}");
        assert!(body.contains("Pin this result"), "{body}");
        let post = poll_post(&conn, round_of(&conn)).unwrap().unwrap();
        assert!(post.title.starts_with("Vote ·"));
        assert!(post
            .buttons
            .iter()
            .any(|button| button.custom_id.contains("marian:v:")));
        assert!(post.body.contains("Which day"));
        assert!(post.body.contains("Pick every day you can make"));
        assert!(body.contains("Pick every day you can make"), "{body}");
        assert!(post.close_id.as_deref().unwrap_or("").ends_with(":close"));
        assert!(body.contains("Close vote and use this result"), "{body}");
        assert!(body.contains("Save this week"), "{body}");
        assert!(body.contains("Save for later weeks"), "{body}");
        assert!(body.contains("week_q0_prompt"), "{body}");
        assert!(body.contains("vote_q0_prompt"), "{body}");
        assert!(body.contains("Add a question"), "{body}");
    }

    #[test]
    fn one_week_can_use_different_choices() {
        let mut conn = world();
        logic::save_event(&mut conn, voting(anchor())).unwrap();
        let series_id = conn
            .query_row("SELECT id FROM series", [], |row| row.get(0))
            .unwrap();
        let round_id = round_of(&conn);
        cast_round(&mut conn, round_id, 2, "d", 0).unwrap();
        cast_round(&mut conn, round_id, 2, "0", 0).unwrap();
        let effect = save_week(
            &mut conn,
            series_id,
            1,
            &week_fields(&[2], 2, "Where? | Hall, Park"),
        )
        .unwrap();
        assert_eq!(effect.notice, "This week's choices are saved.");
        let kept: i64 = conn
            .query_row(
                "SELECT count(*) FROM round_votes WHERE round_id = ?",
                [round_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(kept, 0);
        let mut later = spec();
        later.days = vec![5];
        later.default_weekday = 5;
        later.questions = vec![VoteQuestion {
            prompt: "Food?".into(),
            choices: vec!["Pizza".into(), "Tacos".into()],
        }];
        logic::save_vote_settings(&mut conn, series_id, 1, later).unwrap();
        let post = poll_post(&conn, round_id).unwrap().unwrap();
        assert!(post.questions_body.contains("Where?"), "{post:?}");
        assert!(post.body.contains("Wednesday"), "{post:?}");
        assert!(!post.questions_body.contains("Food?"), "{post:?}");
        assert!(!post.body.contains("Saturday"), "{post:?}");
        assert!(
            !post.questions_body.contains("What are we doing?"),
            "{post:?}"
        );
        let page = Page {
            conn: &conn,
            guild_id: 1,
            user_id: 1,
            notice: None,
            invite_url: "",
            nav: "recurring",
            title: "Series",
            allowed_guilds: &[1],
        };
        let body = present::series_page(&page, series_id).unwrap();
        assert!(body.contains("Choices this week"), "{body}");
        assert!(body.contains("Where?"), "{body}");
        assert!(body.contains("What are we doing?"), "{body}");
        cast_round(&mut conn, round_id, 2, "d", 0).unwrap();
        cast_round(&mut conn, round_id, 2, "0", 0).unwrap();
        backdate(&conn, round_id, "2026-10-12T00:00:00Z");
        conn.execute(
            "UPDATE rounds SET discord_message_id = 'm-vote', thread_discord_id = 't-vote' WHERE id = ?",
            [round_id],
        )
        .unwrap();
        maintain(
            &conn,
            1,
            Utc.with_ymd_and_hms(2026, 10, 13, 12, 0, 0).unwrap(),
        )
        .unwrap();
        assert_eq!(posted_start(&conn), "2026-10-14T19:00:00Z");
        let description: String = conn
            .query_row(
                "SELECT description FROM events WHERE status = 'posted'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(description.contains("**Where?**"), "{description}");
        assert!(description.contains("Hall"), "{description}");
        let message: String = conn
            .query_row(
                "SELECT discord_message_id FROM events WHERE status = 'posted'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let thread: String = conn
            .query_row("SELECT discord_id FROM threads", [], |row| row.get(0))
            .unwrap();
        assert_eq!(message, "m-vote");
        assert_eq!(thread, "t-vote");
        let second = round_of(&conn);
        let next = poll_post(&conn, second).unwrap().unwrap();
        assert!(next.questions_body.contains("Food?"), "{next:?}");
        assert!(next.body.contains("Saturday"), "{next:?}");
        assert!(!next.questions_body.contains("Where?"), "{next:?}");
    }

    #[test]
    fn reordering_questions_keeps_each_vote() {
        let mut conn = world();
        logic::save_event(&mut conn, voting(anchor())).unwrap();
        let series_id = conn
            .query_row("SELECT id FROM series", [], |row| row.get(0))
            .unwrap();
        save_week(
            &mut conn,
            series_id,
            1,
            &week_fields(
                &[1, 4],
                1,
                "What are we doing? | Raid, PvP\nWhere? | Hall, Park",
            ),
        )
        .unwrap();
        let round_id = round_of(&conn);
        cast_round(&mut conn, round_id, 2, "1", 0).unwrap();
        save_week(
            &mut conn,
            series_id,
            1,
            &week_fields(
                &[1, 4],
                1,
                "Where? | Hall, Park\nWhat are we doing? | Raid, PvP",
            ),
        )
        .unwrap();
        let (key, choice): (String, String) = conn
            .query_row(
                "SELECT poll_key, choice FROM round_votes WHERE round_id = ?",
                [round_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(key, "0");
        assert_eq!(choice, "Hall");
    }

    #[test]
    fn question_fields_save_without_the_line_format() {
        let mut conn = world();
        logic::save_event(&mut conn, voting(anchor())).unwrap();
        let series_id = conn
            .query_row("SELECT id FROM series", [], |row| row.get(0))
            .unwrap();
        let mut map = HashMap::new();
        map.insert("week_day_2".into(), "1".into());
        map.insert("week_default_weekday".into(), "2".into());
        map.insert("week_q0_prompt".into(), "Where?".into());
        map.insert("week_q0_c0".into(), "Hall".into());
        map.insert("week_q0_c1".into(), "Park".into());
        map.insert("week_q0_c2".into(), String::new());
        map.insert("week_q1_prompt".into(), String::new());
        save_week(&mut conn, series_id, 1, &map).unwrap();
        let post = poll_post(&conn, round_of(&conn)).unwrap().unwrap();
        assert!(post.questions_body.contains("Where?"), "{post:?}");
        assert!(post.questions_body.contains("Hall"), "{post:?}");
        assert!(post.questions_body.contains("Park"), "{post:?}");
        assert!(
            !post.questions_body.contains("What are we doing?"),
            "{post:?}"
        );
    }

    #[test]
    fn a_single_event_takes_one_vote() {
        let mut conn = world();
        let mut input = voting(anchor());
        input.repeat = None;
        let id = logic::save_event(&mut conn, input).unwrap();
        let event = store::event(&conn, id).unwrap().unwrap();
        assert_eq!(event.status, "ballot");
        let series = store::series(&conn, event.series_id.unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(series.cadence, "once");
        assert_eq!(series.max_occurrences, Some(1));
        let page = Page {
            conn: &conn,
            guild_id: 1,
            user_id: 1,
            notice: None,
            invite_url: "",
            nav: "event",
            title: "Event",
            allowed_guilds: &[1],
        };
        let body = present::event_page(&page, event.id).unwrap();
        assert!(body.contains("Which day"), "{body}");
        assert!(body.contains("clock time"), "{body}");
        assert!(body.contains("Save vote"), "{body}");
        assert!(!body.contains("Save this week"), "{body}");
        let list = present::recurring(&Page {
            nav: "recurring",
            title: "Recurring",
            ..page
        })
        .unwrap();
        assert!(list.contains("No series yet."), "{list}");
        let round_id = round_of(&conn);
        cast_round(&mut conn, round_id, 2, "d", 0).unwrap();
        backdate(&conn, round_id, "2026-10-12T00:00:00Z");
        maintain(
            &conn,
            1,
            Utc.with_ymd_and_hms(2026, 10, 13, 12, 0, 0).unwrap(),
        )
        .unwrap();
        assert_eq!(posted_start(&conn), "2026-10-13T19:00:00Z");
        let open: i64 = conn
            .query_row(
                "SELECT count(*) FROM rounds WHERE status = 'open'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let posted: i64 = conn
            .query_row(
                "SELECT count(*) FROM events WHERE status = 'posted'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(open, 0);
        assert_eq!(posted, 1);
    }

    #[test]
    fn a_series_without_a_vote_still_renders() {
        let mut conn = world();
        let mut input = voting(anchor());
        input.vote = None;
        input.repeat = Some(RepeatSpec {
            cadence: "daily".into(),
            end_on: None,
            max_occurrences: Some(2),
        });
        let id = logic::save_event(&mut conn, input).unwrap();
        let event = store::event(&conn, id).unwrap().unwrap();
        let page = Page {
            conn: &conn,
            guild_id: 1,
            user_id: 1,
            notice: None,
            invite_url: "",
            nav: "recurring",
            title: "Series",
            allowed_guilds: &[1],
        };
        let body = present::series_page(&page, event.series_id.unwrap()).unwrap();
        assert!(!body.contains("Vote settings"), "{body}");
        assert!(body.contains("Raid"), "{body}");
    }

    #[test]
    fn majority_day_and_question_win() {
        let mut conn = world();
        logic::save_event(&mut conn, voting(anchor())).unwrap();
        let round_id = round_of(&conn);
        cast_round(&mut conn, round_id, 2, "d", 0).unwrap();
        cast_round(&mut conn, round_id, 3, "d", 0).unwrap();
        cast_round(&mut conn, round_id, 1, "d", 1).unwrap();
        cast_round(&mut conn, round_id, 2, "0", 0).unwrap();
        backdate(&conn, round_id, "2026-10-12T00:00:00Z");
        let jobs = maintain(
            &conn,
            1,
            Utc.with_ymd_and_hms(2026, 10, 13, 12, 0, 0).unwrap(),
        )
        .unwrap();
        assert!(jobs
            .iter()
            .any(|job| matches!(job, DiscordJob::Sync { .. })));
        assert_eq!(posted_start(&conn), "2026-10-13T19:00:00Z");
        let description: String = conn
            .query_row(
                "SELECT description FROM events WHERE status = 'posted'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            description.contains("**What are we doing?**"),
            "{description}"
        );
        assert!(description.contains("Raid"), "{description}");
        let closed = poll_post(&conn, round_id).unwrap().unwrap();
        assert!(closed.title.starts_with("Vote closed"));
        assert!(closed.buttons.is_empty());
        assert!(closed.body.contains("Voting is closed."));

        let second = round_of(&conn);
        assert_ne!(second, round_id);
        backdate(&conn, second, "2099-01-01T00:00:00Z");
        cast_round(&mut conn, second, 2, "d", 0).unwrap();
        cast_round(&mut conn, second, 2, "0", 0).unwrap();
        backdate(&conn, second, "2026-10-19T18:00:00Z");
        maintain(
            &conn,
            1,
            Utc.with_ymd_and_hms(2026, 10, 19, 19, 0, 0).unwrap(),
        )
        .unwrap();
        let starts: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT starts_at FROM events WHERE status = 'posted' ORDER BY starts_at")
                .unwrap();
            let rows = stmt
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<Vec<String>, _>>()
                .unwrap();
            rows
        };
        assert_eq!(
            starts,
            vec![
                "2026-10-13T19:00:00Z".to_string(),
                "2026-10-20T19:00:00Z".to_string(),
            ]
        );
        let scheduled: i64 = conn
            .query_row(
                "SELECT count(*) FROM events WHERE status = 'scheduled'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(scheduled, 0);
    }

    #[test]
    fn tie_uses_the_earlier_day() {
        let mut conn = world();
        logic::save_event(&mut conn, voting(anchor())).unwrap();
        let round_id = round_of(&conn);
        cast_round(&mut conn, round_id, 2, "d", 0).unwrap();
        cast_round(&mut conn, round_id, 3, "d", 1).unwrap();
        backdate(&conn, round_id, "2026-10-12T00:00:00Z");
        maintain(
            &conn,
            1,
            Utc.with_ymd_and_hms(2026, 10, 13, 12, 0, 0).unwrap(),
        )
        .unwrap();
        assert_eq!(posted_start(&conn), "2026-10-13T19:00:00Z");
        let description: String = conn
            .query_row(
                "SELECT description FROM events WHERE status = 'posted' ORDER BY id LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            description
                .contains("Tuesday and Friday tied. Marian used Tuesday, the earlier choice."),
            "{description}"
        );
    }

    #[test]
    fn nobody_voting_uses_the_default_day() {
        let mut conn = world();
        logic::save_event(&mut conn, voting(anchor())).unwrap();
        let round_id = round_of(&conn);
        backdate(&conn, round_id, "2026-10-12T00:00:00Z");
        maintain(
            &conn,
            1,
            Utc.with_ymd_and_hms(2026, 10, 13, 12, 0, 0).unwrap(),
        )
        .unwrap();
        assert_eq!(posted_start(&conn), "2026-10-13T19:00:00Z");
        let description: String = conn
            .query_row(
                "SELECT description FROM events WHERE status = 'posted'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            description.contains("Nobody voted on the day. Marian used Tuesday."),
            "{description}"
        );
    }

    #[test]
    fn the_creator_can_close_the_vote() {
        let mut conn = world();
        logic::save_event(&mut conn, voting(anchor())).unwrap();
        let series_id = conn
            .query_row("SELECT id FROM series", [], |row| row.get(0))
            .unwrap();
        let round_id = round_of(&conn);
        cast_round(&mut conn, round_id, 2, "d", 0).unwrap();
        cast_round(&mut conn, round_id, 2, "0", 0).unwrap();
        conn.execute(
            "UPDATE rounds SET closes_at = '2099-01-01T00:00:00Z' WHERE id = ?",
            [round_id],
        )
        .unwrap();
        let now = Utc.with_ymd_and_hms(2026, 10, 13, 12, 0, 0).unwrap();
        let denied = match close_in(&conn, series_id, 2, now) {
            Err(err) => err,
            Ok(_) => panic!("someone else closed the vote"),
        };
        assert!(
            denied.to_string().contains("can't change this vote"),
            "{denied}"
        );
        let effect = close_in(&conn, series_id, 1, now).unwrap();
        assert_eq!(effect.notice, "Voting is closed. This result is posted.");
        assert!(effect
            .jobs
            .iter()
            .any(|job| matches!(job, DiscordJob::Sync { .. })));
        assert_eq!(posted_start(&conn), "2026-10-13T19:00:00Z");
        let description: String = conn
            .query_row(
                "SELECT description FROM events WHERE status = 'posted'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(description.contains("Raid"), "{description}");
        let open: i64 = conn
            .query_row(
                "SELECT count(*) FROM rounds WHERE status = 'open'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(open, 0);
    }

    #[test]
    fn a_day_vote_keeps_every_day_you_pick() {
        let mut conn = world();
        logic::save_event(&mut conn, voting(anchor())).unwrap();
        let round_id = round_of(&conn);
        let first = cast_round(&mut conn, round_id, 2, "d", 0).unwrap();
        assert_eq!(first, "You can make Tuesday.");
        let both = cast_round(&mut conn, round_id, 2, "d", 1).unwrap();
        assert_eq!(both, "You can make Tuesday and Friday.");
        let mut choices = day_choices(&conn, 2);
        choices.sort();
        assert_eq!(choices, vec!["1".to_string(), "4".to_string()]);
        let cleared = cast_round(&mut conn, round_id, 2, "d", 0).unwrap();
        assert_eq!(cleared, "You can make Friday.");
        assert_eq!(day_choices(&conn, 2), vec!["4".to_string()]);
        assert_eq!(
            cast_round(&mut conn, round_id, 2, "d", 1).unwrap(),
            "Day vote cleared."
        );
        cast_round(&mut conn, round_id, 2, "0", 0).unwrap();
        cast_round(&mut conn, round_id, 2, "0", 1).unwrap();
        let question: String = conn
            .query_row(
                "SELECT choice FROM round_votes WHERE user_id = 2 AND poll_key = '0'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(question, "PvP");
    }

    fn day_choices(conn: &Connection, user_id: i64) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT choice FROM round_votes WHERE user_id = ? AND poll_key = 'd'")
            .unwrap();
        stmt.query_map([user_id], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    #[test]
    fn several_days_from_one_person_each_get_a_vote() {
        let mut conn = world();
        logic::save_event(&mut conn, voting(anchor())).unwrap();
        let round_id = round_of(&conn);
        cast_round(&mut conn, round_id, 2, "d", 0).unwrap();
        cast_round(&mut conn, round_id, 2, "d", 1).unwrap();
        cast_round(&mut conn, round_id, 3, "d", 0).unwrap();
        backdate(&conn, round_id, "2026-10-12T00:00:00Z");
        maintain(
            &conn,
            1,
            Utc.with_ymd_and_hms(2026, 10, 13, 12, 0, 0).unwrap(),
        )
        .unwrap();
        assert_eq!(posted_start(&conn), "2026-10-13T19:00:00Z");
    }

    #[test]
    fn organizer_pin_overrides_the_count() {
        let mut conn = world();
        logic::save_event(&mut conn, voting(anchor())).unwrap();
        let series_id: i64 = conn
            .query_row("SELECT id FROM series", [], |row| row.get(0))
            .unwrap();
        cast_series(&mut conn, series_id, 2, "d", 0).unwrap();
        pin_series(&mut conn, series_id, 1, "d", 1).unwrap();
        let round_id = round_of(&conn);
        backdate(&conn, round_id, "2026-10-12T00:00:00Z");
        maintain(
            &conn,
            1,
            Utc.with_ymd_and_hms(2026, 10, 13, 12, 0, 0).unwrap(),
        )
        .unwrap();
        assert_eq!(posted_start(&conn), "2026-10-16T19:00:00Z");
        let description: String = conn
            .query_row(
                "SELECT description FROM events WHERE status = 'posted'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            description.contains("An organizer set the day to Friday."),
            "{description}"
        );
    }
}
