use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use chrono_tz::Tz;
use minijinja::{Environment, Value};
use serde::Serialize;

use crate::clock::{self, zone_label};
use crate::error::OpError;
use crate::ics;
use crate::logic::{self, EventInput, OptionInput, ReminderInput, RepeatSpec, SaveAction};
use crate::store::{self, Connection, Event, Settings, User};

pub struct Page<'a> {
    pub conn: &'a mut Connection,
    pub guild_id: i64,
    pub user_id: i64,
    pub notice: Option<&'a str>,
    pub invite_url: &'a str,
    pub nav: &'a str,
    pub title: &'a str,
    pub allowed_guilds: &'a [i64],
}

pub fn landing(invite_url: &str, next: &str, notice: &str, ready: bool) -> Result<String, OpError> {
    let mut ctx = Bag::new();
    ctx.insert("invite_url", Value::from(invite_url));
    ctx.insert("next", Value::from(next));
    ctx.insert("notice", Value::from(notice));
    ctx.insert("ready", context_bool(ready));
    asset_context(&mut ctx);
    render("landing.html", ctx)
}

pub fn home(page: &mut Page<'_>) -> Result<String, OpError> {
    if page.guild_id == 0 {
        let mut ctx = shell(page)?;
        ctx.insert("empty_server", context_bool(true));
        ctx.insert("hero", Value::from_serialize(&()));
        ctx.insert("days", Value::from_serialize(&Vec::<DayBucket>::new()));
        ctx.insert(
            "wrapped",
            Value::from_serialize(&Vec::<serde_json::Value>::new()),
        );
        ctx.insert(
            "waiting",
            Value::from_serialize(&Vec::<serde_json::Value>::new()),
        );
        ctx.insert(
            "reminders",
            Value::from_serialize(&Vec::<serde_json::Value>::new()),
        );
        ctx.insert(
            "activity",
            Value::from_serialize(&Vec::<serde_json::Value>::new()),
        );
        return render("schedule.html", ctx);
    }
    let now = Utc::now();
    let (settings, tz) = zone_of(page)?;
    let (week_start, week_end) = week_window(now, tz);
    let events = store::list_events(page.conn, page.guild_id)?;
    let mut days: Vec<DayBucket> = Vec::new();
    let mut wrapped = Vec::new();
    let mut waiting = Vec::new();
    let mut reminders = Vec::new();
    let mut hero: Option<serde_json::Value> = None;
    let mut hero_start: Option<DateTime<Utc>> = None;
    let mut shown = 0usize;
    for event in &events {
        let start = clock::parse_stamp(&event.starts_at)?;
        let end = start + Duration::minutes(event.duration_minutes);
        if event.status == "scheduled" || event.status == "failed" {
            waiting.push(serde_json::json!({
                "id": event.public_id,
                "title": event.title,
                "post_when": event.post_at.as_deref().and_then(|v| clock::parse_stamp(v).ok()).map(|dt| clock::format_when(dt, tz, settings.hour_24)).unwrap_or_else(|| "when you publish it".into()),
            }));
        }
        if event.status == "posted" && end < now {
            wrapped.push(serde_json::json!({
                "id": event.public_id,
                "title": event.title,
                "when": clock::format_when(start, tz, settings.hour_24),
            }));
            continue;
        }
        if matches!(event.status.as_str(), "posted" | "scheduled") {
            let local_day = start.with_timezone(&tz).date_naive();
            let in_week = local_day >= week_start && local_day < week_end;
            if in_week && shown < 5 {
                shown += 1;
                if event.status == "posted"
                    && end >= now
                    && hero_start.map(|current| start < current).unwrap_or(true)
                {
                    hero_start = Some(start);
                    hero = Some(card_value(page.conn, event, &settings, tz, now, true)?);
                }
                let label = clock::format_day(start.with_timezone(&tz).date_naive());
                let today =
                    start.with_timezone(&tz).date_naive() == now.with_timezone(&tz).date_naive();
                if days.last().map(|day| day.label != label).unwrap_or(true) {
                    days.push(DayBucket {
                        label,
                        today,
                        events: Vec::new(),
                    });
                }
                days.last_mut()
                    .unwrap()
                    .events
                    .push(card_value(page.conn, event, &settings, tz, now, false)?);
            }
        }
        if event.status == "posted" {
            for rem in store::reminders_for(page.conn, event.id)? {
                if rem.sent_at.is_none() {
                    reminders.push(serde_json::json!({
                        "id": event.public_id,
                        "title": event.title,
                        "detail": format!("{} before", clock::human_offset(rem.offset_minutes)),
                    }));
                }
            }
        }
    }
    let activity = activity_rows(page, 8)?;
    let mut ctx = shell(page)?;
    ctx["empty_server"] = context_bool(false);
    ctx["hero"] = hero
        .map(|value| Value::from_serialize(&value))
        .unwrap_or_else(|| Value::from_serialize(&()));
    ctx["days"] = Value::from_serialize(&days);
    ctx["wrapped"] = Value::from_serialize(&wrapped);
    ctx["waiting"] = Value::from_serialize(&waiting.into_iter().take(4).collect::<Vec<_>>());
    ctx["reminders"] = Value::from_serialize(&reminders.into_iter().take(6).collect::<Vec<_>>());
    ctx["activity"] = Value::from_serialize(&activity);
    render("schedule.html", ctx)
}

#[derive(Serialize)]
struct DayBucket {
    label: String,
    today: bool,
    events: Vec<serde_json::Value>,
}

fn week_window(now: DateTime<Utc>, tz: Tz) -> (NaiveDate, NaiveDate) {
    let today = now.with_timezone(&tz).date_naive();
    let start = today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
    (start, start + Duration::days(7))
}

pub fn event_page(page: &mut Page<'_>, event_id: i64) -> Result<String, OpError> {
    let event = store::event(page.conn, event_id)?.ok_or(OpError::NotFound)?;
    if event.guild_id != page.guild_id {
        return Err(OpError::NotFound);
    }
    let (settings, tz) = zone_of(page)?;
    let user = must_user(page)?;
    let roles = store::user_role_ids(page.conn, user.id)?;
    let can_edit = logic::can_edit(&user, &event, &settings, &roles);
    let options = store::options_for(page.conn, event.id)?;
    let signups = store::signups_for(page.conn, event.id)?;
    let users = store::list_users(page.conn, page.guild_id)?;
    let role_rows = store::list_roles(page.conn, page.guild_id)?;
    let restrict_ids = store::event_role_ids(page.conn, event.id, "restrict")?;
    let now = Utc::now();
    let start = clock::parse_stamp(&event.starts_at)?;
    let closed = event.status != "posted" || event.signups_closed || cutoff_passed(&event, now);
    let mut option_views = Vec::new();
    let mut buttons = Vec::new();
    for opt in &options {
        let confirmed: Vec<_> = signups
            .iter()
            .filter(|s| s.option_id == opt.id && s.state == "confirmed")
            .collect();
        let waiting: Vec<_> = signups
            .iter()
            .filter(|s| s.option_id == opt.id && s.state == "waitlisted")
            .collect();
        let spots = match opt.capacity {
            Some(cap) => format!("{}/{}", confirmed.len(), cap),
            None => confirmed.len().to_string(),
        };
        let mut field_lines = Vec::new();
        if settings.show_attendees {
            field_lines.extend(confirmed.iter().filter_map(|s| {
                users
                    .iter()
                    .find(|u| u.id == s.user_id)
                    .map(|u| u.name.clone())
            }));
        }
        if !waiting.is_empty() {
            field_lines.push(format!("{} waiting", waiting.len()));
        }
        let names = field_lines.join(", ");
        let pct = opt
            .capacity
            .map(|cap| ((confirmed.len() as i64) * 100 / cap.max(1)).min(100))
            .unwrap_or(0);
        let role_name = opt
            .role_id
            .and_then(|id| {
                role_rows
                    .iter()
                    .find(|role| role.id == id)
                    .map(|role| role.name.clone())
            })
            .unwrap_or_default();
        option_views.push(serde_json::json!({
            "id": opt.id,
            "emoji": opt.emoji,
            "name": opt.name,
            "spots": spots,
            "names": names,
            "field_lines": field_lines,
            "role": role_name,
            "reminders": opt.reminders_enabled,
            "pct": pct,
            "confirmed": people(&users, &confirmed.iter().map(|s| s.user_id).collect::<Vec<_>>()),
            "waiting": people(&users, &waiting.iter().map(|s| s.user_id).collect::<Vec<_>>()),
        }));
        let yours = signups
            .iter()
            .any(|s| s.option_id == opt.id && s.user_id == user.id);
        let (disabled, reason, wait, label) = button_state(
            &event,
            opt,
            &user,
            &roles,
            yours,
            confirmed.len(),
            closed,
            &restrict_ids,
        );
        buttons.push(serde_json::json!({
            "id": opt.id,
            "label": label,
            "emoji": opt.emoji,
            "disabled": disabled,
            "reason": reason,
            "wait": wait,
            "yours": yours,
        }));
    }
    let channel = store::channel(page.conn, event.channel_id)?;
    let channel_name = channel
        .as_ref()
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "deleted".into());
    let reminders = store::reminders_for(page.conn, event.id)?;
    let thread = store::thread_messages(page.conn, event.id)?;
    let members = store::thread_member_ids(page.conn, event.id)?;
    let series = event
        .series_id
        .and_then(|id| store::series(page.conn, id).ok().flatten());
    let once = series
        .as_ref()
        .is_some_and(|series| series.cadence == "once");
    let series_note = if series.is_some() && !once {
        "This is one date in a series. Moving this start time leaves the other dates where they are."
    } else {
        ""
    };
    let google = ics::google_link(&event, &format!("#{channel_name}")).unwrap_or_default();
    let plus = ics::calendar_template(&event).unwrap_or_default();
    let creator = users
        .iter()
        .find(|person| person.id == event.created_by)
        .map(|person| person.name.clone())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "Marian".into());
    let image_url = event.image_url.as_deref().and_then(|url| {
        let url = url.trim();
        if url.starts_with("https://") || url.starts_with("http://") {
            Some(url.to_string())
        } else {
            None
        }
    });
    let google_synced = user
        .discord_id
        .as_deref()
        .map(|discord_id| crate::google::on_calendar(page.conn, discord_id, event.id))
        .transpose()?
        .unwrap_or(false);
    let web_url = format!(
        "/servers/{}/events/{}",
        store::discord_server_id(page.conn, event.guild_id).unwrap_or_default(),
        event.public_id
    );
    let mut ctx = shell(page)?;
    ctx["series_id"] = Value::from(
        series
            .as_ref()
            .map(|item| item.public_id.clone())
            .unwrap_or_default(),
    );
    ctx["can_edit"] = context_bool(can_edit);
    ctx["can_delete"] = context_bool(logic::can_delete(&user, &event, &settings, &roles));
    ctx["can_archive"] = context_bool(can_edit && event.status == "posted");
    ctx["can_skip"] = context_bool(
        can_edit
            && event.series_id.is_some()
            && matches!(event.status.as_str(), "scheduled" | "draft"),
    );
    ctx["event"] = Value::from_serialize(&serde_json::json!({
        "id": event.public_id,
        "title": event.title,
        "description": event.description,
        "when": clock::format_when(start, tz, settings.hour_24),
        "end": clock::format_end(start, event.duration_minutes, tz, settings.hour_24),
        "rel": clock::format_rel(start, now),
        "channel": channel_name,
        "status": event.status,
        "status_label": logic::status_label(&event.status),
        "color": event.color,
        "description": event.description,
        "show_names": settings.show_attendees,
        "options": option_views,
        "buttons": buttons,
        "localize_note": if settings.localize_time { "Members see this time in their own zone on Discord." } else { "Times stay in the server zone." },
        "google_url": google,
        "plus_url": plus,
        "web_url": web_url,
        "creator": creator,
        "image_url": image_url,
        "calendar_on": event.calendar_synced_at.is_some(),
        "google_synced": google_synced,
        "duration": clock::human_duration(event.duration_minutes),
        "cutoff": event.cutoff_at.as_deref().and_then(|v| clock::parse_stamp(v).ok()).map(|dt| clock::format_when(dt, tz, settings.hour_24)).unwrap_or_default(),
        "attendee_role": event.attendee_role_id.and_then(|id| role_rows.iter().find(|r| r.id == id).map(|r| r.name.clone())).unwrap_or_default(),
        "series_note": series_note,
        "reminders": reminders.iter().map(|rem| serde_json::json!({
            "offset": format!("{} before", clock::human_offset(rem.offset_minutes)),
            "message": rem.message,
            "state": if rem.sent_at.is_some() { "Sent" } else { "Waiting" },
        })).collect::<Vec<_>>(),
        "thread": thread.iter().map(|note| serde_json::json!({
            "body": note.body,
            "place": note.place,
            "when": clock::parse_stamp(&note.created_at).map(|dt| clock::format_rel(dt, now)).unwrap_or_default(),
        })).collect::<Vec<_>>(),
        "members": members.iter().filter_map(|id| users.iter().find(|u| u.id == *id).map(|u| u.name.clone())).collect::<Vec<_>>(),
    }));
    render("event.html", ctx)
}

fn button_state(
    event: &Event,
    opt: &store::SignupOption,
    user: &User,
    roles: &[i64],
    yours: bool,
    confirmed: usize,
    closed: bool,
    restrict_ids: &[i64],
) -> (bool, String, bool, String) {
    let base = format!(
        "{} {} ({})",
        opt.emoji,
        opt.name,
        match opt.capacity {
            Some(cap) => format!("{confirmed}/{cap}"),
            None => confirmed.to_string(),
        }
    );
    if yours {
        return (
            event.status != "posted",
            String::new(),
            false,
            format!("Leave {}", opt.name),
        );
    }
    if event.status != "posted" {
        return (
            true,
            "Signups open once the event is posted.".into(),
            false,
            base,
        );
    }
    if closed {
        return (true, "Signups are closed.".into(), false, base);
    }
    if !restrict_ids.is_empty()
        && !user.organizer
        && !restrict_ids.iter().any(|id| roles.contains(id))
    {
        return (
            true,
            "This event is limited to another role.".into(),
            false,
            base,
        );
    }
    if let Some(role_id) = opt.role_id {
        if !user.organizer && !roles.contains(&role_id) {
            return (
                true,
                "Your roles don't include this option.".into(),
                false,
                base,
            );
        }
    }
    if let Some(cap) = opt.capacity {
        if confirmed as i64 >= cap {
            if opt.waitlist {
                return (false, String::new(), true, format!("Waitlist {}", opt.name));
            }
            return (true, format!("{} is full.", opt.name), false, base);
        }
    }
    (false, String::new(), false, base)
}

#[derive(Clone, Debug, Default)]
pub struct ListQuery {
    pub q: String,
    pub page: u32,
}

impl ListQuery {
    pub fn new(q: impl Into<String>, page: u32) -> Self {
        Self {
            q: q.into().trim().chars().take(200).collect(),
            page,
        }
    }
}

const LIST_PAGE_SIZE: usize = 25;

pub fn list_page(page: &mut Page<'_>, kind: &str, query: ListQuery) -> Result<String, OpError> {
    let (settings, tz) = zone_of(page)?;
    let server = store::discord_server_id(page.conn, page.guild_id)?;
    let events = store::list_events(page.conn, page.guild_id)?;
    let (heading, empty_title, empty_body, rows, columns) = match kind {
        "drafts" => (
            "Drafts",
            "No drafts.",
            "Start an event and choose Save draft.",
            rows_for(&server, &events, &settings, tz, |e| e.status == "draft"),
            Some(("Title", "When", "Status")),
        ),
        "scheduled" => (
            "Scheduled posts",
            "Nothing is queued.",
            "Choose Schedule post on an event to put it here.",
            rows_for(&server, &events, &settings, tz, |e| {
                e.status == "scheduled" || e.status == "failed"
            }),
            Some(("Title", "When", "Status")),
        ),
        "archive" => (
            "Archive",
            "The archive is empty.",
            "Posted events land here when they are archived.",
            rows_for(&server, &events, &settings, tz, |e| {
                matches!(e.status.as_str(), "archived" | "cancelled" | "skipped")
            }),
            Some(("Title", "When", "Status")),
        ),
        "activity" => {
            let rows = activity_rows(page, 80)?;
            let mapped = rows
                .iter()
                .map(|row| {
                    serde_json::json!({
                        "href": row.get("href").cloned().unwrap_or(serde_json::json!("/activity")),
                        "badge": "Log",
                        "title": row.get("body").cloned().unwrap_or(serde_json::json!("")),
                        "meta": row.get("when").cloned().unwrap_or(serde_json::json!("")),
                    })
                })
                .collect::<Vec<_>>();
            (
                "Activity",
                "The log is empty.",
                "Signups and posts will show up here.",
                mapped,
                None,
            )
        }
        _ => ("Schedule", "", "", vec![], None),
    };
    render_rows(page, heading, empty_title, empty_body, rows, columns, query)
}

fn render_rows(
    page: &mut Page<'_>,
    heading: &str,
    empty_title: &str,
    empty_body: &str,
    rows: Vec<serde_json::Value>,
    columns: Option<(&str, &str, &str)>,
    query: ListQuery,
) -> Result<String, OpError> {
    let table = columns.is_some();
    let searchable = table && (!rows.is_empty() || !query.q.is_empty());
    let matched: Vec<serde_json::Value> = if table {
        rows.into_iter()
            .filter(|row| row_hits(&query.q, row))
            .collect()
    } else {
        rows
    };
    let (shown, window, prev, next) = if table {
        let (page_n, start, end) = page_window(matched.len(), query.page, LIST_PAGE_SIZE);
        let prev = if page_n > 1 {
            list_link(&query.q, page_n - 1)
        } else {
            String::new()
        };
        let next = if end < matched.len() {
            list_link(&query.q, page_n + 1)
        } else {
            String::new()
        };
        let window = if matched.is_empty() {
            String::new()
        } else {
            format!("{}–{} of {}", start + 1, end, matched.len())
        };
        (matched[start..end].to_vec(), window, prev, next)
    } else {
        (matched, String::new(), String::new(), String::new())
    };
    let (col_title, col_when, col_status) = columns.unwrap_or(("", "", ""));
    let mut ctx = shell(page)?;
    ctx["heading"] = Value::from(heading);
    ctx["empty_title"] = Value::from(empty_title);
    ctx["empty_body"] = Value::from(empty_body);
    ctx["rows"] = Value::from_serialize(&shown);
    ctx["extra"] = Value::from("");
    ctx["dense"] = context_bool(!table);
    ctx["searchable"] = context_bool(searchable);
    ctx["q"] = Value::from(query.q);
    ctx["window"] = Value::from(window);
    ctx["prev"] = Value::from(prev);
    ctx["next"] = Value::from(next);
    ctx["col_title"] = Value::from(col_title);
    ctx["col_when"] = Value::from(col_when);
    ctx["col_status"] = Value::from(col_status);
    render("list.html", ctx)
}

fn row_hits(q: &str, row: &serde_json::Value) -> bool {
    let q = q.trim();
    if q.is_empty() {
        return true;
    }
    let q = q.to_lowercase();
    ["title", "meta", "badge"].iter().any(|key| {
        row.get(*key)
            .and_then(|value| value.as_str())
            .is_some_and(|text| text.to_lowercase().contains(&q))
    })
}

fn page_window(len: usize, page: u32, size: usize) -> (u32, usize, usize) {
    if len == 0 || size == 0 {
        return (1, 0, 0);
    }
    let pages = len.div_ceil(size);
    let page = (page.max(1) as usize).min(pages);
    let start = (page - 1) * size;
    let end = (start + size).min(len);
    (page as u32, start, end)
}

fn list_link(q: &str, page: u32) -> String {
    let mut parts = Vec::new();
    if !q.is_empty() {
        parts.push(format!("q={}", query_escape(q)));
    }
    if page > 1 {
        parts.push(format!("page={page}"));
    }
    if parts.is_empty() {
        "?".into()
    } else {
        format!("?{}", parts.join("&"))
    }
}

fn query_escape(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub fn recurring(page: &mut Page<'_>, query: ListQuery) -> Result<String, OpError> {
    let (settings, tz) = zone_of(page)?;
    let mut rows = Vec::new();
    for series in store::list_series(page.conn, page.guild_id)? {
        if series.cadence == "once" {
            continue;
        }
        let template = logic::series_template(page.conn, &series)?;
        let events = store::events_in_series(page.conn, series.id)?;
        let next = events
            .iter()
            .find(|event| matches!(event.status.as_str(), "posted" | "scheduled" | "draft"));
        let when = next
            .and_then(|event| clock::parse_stamp(&event.starts_at).ok())
            .map(|dt| clock::format_when(dt, tz, settings.hour_24))
            .unwrap_or_else(|| "no upcoming date".into());
        rows.push(serde_json::json!({
            "href": format!(
                "/servers/{}/recurring/{}",
                store::discord_server_id(page.conn, page.guild_id)?,
                series.public_id
            ),
            "badge": if series.active { clock::cadence_label(&series.cadence) } else { "Ended" },
            "title": template.title,
            "meta": format!(
                "{when} · {}",
                if series.active && series.max_occurrences.is_none() && series.end_on.is_none() {
                    "keeps going".to_string()
                } else {
                    format!("{} dates", events.len())
                }
            ),
        }));
    }
    render_rows(
        page,
        "Recurring",
        "No series yet.",
        "Set a repeat when you create an event.",
        rows,
        Some(("Series", "Next", "Repeat")),
        query,
    )
}

pub fn series_page(
    page: &mut Page<'_>,
    public_id: &str,
    query: ListQuery,
) -> Result<String, OpError> {
    let Some(public_id) = crate::ids::parse_event_id(public_id) else {
        return Err(OpError::NotFound);
    };
    let series = store::series_by_public(page.conn, public_id)?.ok_or(OpError::NotFound)?;
    if series.guild_id != page.guild_id {
        return Err(OpError::NotFound);
    }
    let (settings, tz) = zone_of(page)?;
    let template = logic::series_template(page.conn, &series)?;
    let title = template.title;
    let events = store::events_in_series(page.conn, series.id)?;
    let rows = events.iter().filter_map(|event| {
        let start = clock::parse_stamp(&event.starts_at).ok()?;
        let post = event.post_at.as_deref().and_then(|v| clock::parse_stamp(v).ok()).map(|dt| clock::format_when(dt, tz, settings.hour_24)).unwrap_or_default();
        Some(serde_json::json!({
            "id": event.public_id,
            "title": event.title,
            "badge": logic::status_label(&event.status),
            "meta": format!("{} · posts {}", clock::format_when(start, tz, settings.hour_24), if post.is_empty() { "with the event".into() } else { post }),
        }))
    }).collect::<Vec<_>>();
    let had_dates = !rows.is_empty();
    let matched: Vec<serde_json::Value> = rows
        .into_iter()
        .filter(|row| row_hits(&query.q, row))
        .collect();
    let (page_n, start, end) = page_window(matched.len(), query.page, LIST_PAGE_SIZE);
    let prev = if page_n > 1 {
        list_link(&query.q, page_n - 1)
    } else {
        String::new()
    };
    let next = if end < matched.len() {
        list_link(&query.q, page_n + 1)
    } else {
        String::new()
    };
    let window = if matched.is_empty() {
        String::new()
    } else {
        format!("{}–{} of {}", start + 1, end, matched.len())
    };
    let shown = matched[start..end].to_vec();
    let repeat_until = series
        .end_on
        .as_deref()
        .and_then(|value| clock::parse_stamp(value).ok())
        .map(|dt| clock::date_local(dt, tz))
        .unwrap_or_default();
    let mut ctx = shell(page)?;
    ctx["heading"] = Value::from(title);
    ctx["cadence"] = Value::from(clock::cadence_label(&series.cadence));
    ctx["active"] = context_bool(series.active);
    ctx["series_id"] = Value::from(series.public_id.clone());
    ctx["repeat"] = Value::from(series.cadence);
    ctx["repeat_count"] = Value::from(
        series
            .max_occurrences
            .map(|count| count.to_string())
            .unwrap_or_default(),
    );
    ctx["repeat_until"] = Value::from(repeat_until);
    ctx["rows"] = Value::from_serialize(&shown);
    ctx["searchable"] = context_bool(had_dates || !query.q.is_empty());
    ctx["q"] = Value::from(query.q);
    ctx["window"] = Value::from(window);
    ctx["prev"] = Value::from(prev);
    ctx["next"] = Value::from(next);
    render("series.html", ctx)
}

pub fn presets(page: &mut Page<'_>) -> Result<String, OpError> {
    let mut items = Vec::new();
    for preset in store::list_presets(page.conn, page.guild_id)? {
        let summary = serde_json::from_str::<logic::PresetBody>(&preset.body)
            .map(|body| {
                body.options
                    .iter()
                    .map(|opt| opt.name.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_else(|_| "Unreadable preset".into());
        items.push(
            serde_json::json!({ "id": preset.public_id, "name": preset.name, "summary": summary }),
        );
    }
    let library = store::list_library(page.conn, page.guild_id)?;
    let mut ctx = shell(page)?;
    ctx["presets"] = Value::from_serialize(&items);
    ctx["library"] = Value::from_serialize(
        &library
            .iter()
            .map(|item| serde_json::json!({"emoji": item.emoji, "name": item.name}))
            .collect::<Vec<_>>(),
    );
    render("presets.html", ctx)
}

pub fn channels(page: &mut Page<'_>) -> Result<String, OpError> {
    let (settings, tz) = zone_of(page)?;
    let events = store::list_events(page.conn, page.guild_id)?;
    let mut rows = Vec::new();
    for channel in store::list_channels(page.conn, page.guild_id)? {
        let count = events
            .iter()
            .filter(|event| event.channel_id == channel.id)
            .count();
        rows.push(serde_json::json!({
            "name": channel.name,
            "kind": if channel.kind == "event" { "Event channel" } else { "Text" },
            "count": count,
            "purged": channel.purged_at.as_deref().and_then(|v| clock::parse_stamp(v).ok()).map(|dt| clock::format_when(dt, tz, settings.hour_24)).unwrap_or_default(),
        }));
    }
    let mut ctx = shell(page)?;
    ctx["channels"] = Value::from_serialize(&rows);
    render("channels.html", ctx)
}

pub fn help_page() -> Result<String, OpError> {
    let mut ctx = Bag::new();
    asset_context(&mut ctx);
    render("help.html", ctx)
}

pub fn commands_page(page: &mut Page<'_>) -> Result<String, OpError> {
    let rows = store::list_commands(page.conn, page.guild_id, 30)?;
    let users = store::list_users(page.conn, page.guild_id)?;
    let mut log = Vec::new();
    for row in rows.into_iter().rev() {
        let name = users
            .iter()
            .find(|user| user.id == row.user_id)
            .map(|user| user.name.clone())
            .unwrap_or_else(|| "Marian".into());
        log.push(serde_json::json!({"user": name, "line": row.line, "reply": row.reply}));
    }
    let mut ctx = shell(page)?;
    ctx["log"] = Value::from_serialize(&log);
    render("commands.html", ctx)
}

pub fn routines_page(page: &mut Page<'_>) -> Result<String, OpError> {
    let routines = logic::list_routines(page.conn, page.guild_id)?;
    let mut ctx = shell(page)?;
    ctx.insert("creating", context_bool(false));
    ctx.insert("routines", Value::from_serialize(&routines));
    render("routines.html", ctx)
}

pub fn routine_form(page: &mut Page<'_>) -> Result<String, OpError> {
    let channels = store::list_channels(page.conn, page.guild_id)?;
    let mut ctx = shell(page)?;
    ctx.insert("creating", context_bool(true));
    ctx.insert(
        "channels",
        Value::from_serialize(
            &channels
                .iter()
                .map(|channel| serde_json::json!({"id": channel.id, "name": channel.name}))
                .collect::<Vec<_>>(),
        ),
    );
    ctx.insert("weekdays", Value::from_serialize(&weekday_options()));
    render("routines.html", ctx)
}

pub fn routine_detail(page: &mut Page<'_>, public_id: &str) -> Result<String, OpError> {
    let id = logic::routine_in_guild(page.conn, page.guild_id, public_id)?;
    let routine = logic::routine_page_data(page.conn, id)?.ok_or(OpError::NotFound)?;
    let channels = store::list_channels(page.conn, page.guild_id)?;
    let mut ctx = shell(page)?;
    ctx.insert("creating", context_bool(false));
    ctx.insert("routine", Value::from_serialize(&routine));
    ctx.insert(
        "channels",
        Value::from_serialize(
            &channels
                .iter()
                .map(|channel| {
                    serde_json::json!({
                        "id": channel.id,
                        "name": channel.name,
                        "selected": channel.id == routine.channel_id,
                    })
                })
                .collect::<Vec<_>>(),
        ),
    );
    ctx.insert("weekdays", Value::from_serialize(&weekday_options()));
    render("routines.html", ctx)
}

fn weekday_options() -> Vec<serde_json::Value> {
    (0..7)
        .map(|n| {
            serde_json::json!({
                "n": n.to_string(),
                "name": clock::weekday_name(n),
            })
        })
        .collect()
}

pub fn settings_page(page: &mut Page<'_>) -> Result<String, OpError> {
    if page.guild_id == 0 {
        let mut ctx = shell(page)?;
        ctx.insert("empty_server", context_bool(true));
        return render("settings.html", ctx);
    }
    let settings = store::settings(page.conn, page.guild_id)?;
    let guild = store::guild(page.conn, page.guild_id)?.ok_or(OpError::NotFound)?;
    let user = store::user(page.conn, page.user_id)?;
    let organizer = user.as_ref().map(|user| user.organizer).unwrap_or(false);
    let roles = store::list_roles(page.conn, page.guild_id)?;
    let mut ctx = shell(page)?;
    ctx["empty_server"] = context_bool(false);
    ctx["empty_people"] = context_bool(user.is_none());
    ctx["organizer"] = context_bool(organizer);
    ctx["default_reminder_minutes"] = Value::from(settings.default_reminder_minutes);
    ctx["auto_archive_hours"] = Value::from(settings.auto_archive_hours);
    ctx["post_lead_hours"] = Value::from(settings.post_lead_hours);
    ctx["thread_reminders"] = context_bool(settings.thread_reminders);
    ctx["auto_archive"] = context_bool(settings.auto_archive);
    ctx["event_threads"] = context_bool(settings.event_threads);
    ctx["auto_join_threads"] = context_bool(settings.auto_join_threads);
    ctx["show_attendees"] = context_bool(settings.show_attendees);
    ctx["hour_24"] = context_bool(settings.hour_24);
    ctx["localize_time"] = context_bool(settings.localize_time);
    ctx["purge_channels"] = context_bool(settings.purge_channels);
    ctx["temp_roles"] = context_bool(settings.temp_roles);
    ctx["calendar_auto_sync"] = context_bool(settings.calendar_auto_sync);
    ctx["park_clock"] = context_bool(settings.park_clock);
    ctx["zone_groups"] = Value::from_serialize(&clock::zone_groups(Utc::now(), &guild.timezone));
    ctx["digests"] = Value::from_serialize(
        &["off", "daily", "weekly"]
            .iter()
            .map(|id| {
                serde_json::json!({
                    "id": id,
                    "label": id,
                    "selected": settings.digest == *id,
                })
            })
            .collect::<Vec<_>>(),
    );
    ctx["role_event"] = role_choices(&roles, settings.role_event);
    ctx["role_edit"] = role_choices(&roles, settings.role_edit);
    ctx["role_delete"] = role_choices(&roles, settings.role_delete);
    ctx["role_channel"] = role_choices(&roles, settings.role_channel);
    ctx["role_signup"] = role_choices(&roles, settings.role_signup);
    render("settings.html", ctx)
}

pub fn google_page(page: &mut Page<'_>) -> Result<String, OpError> {
    if page.guild_id == 0 {
        let mut ctx = shell(page)?;
        ctx.insert("empty_server", context_bool(true));
        return render("google.html", ctx);
    }
    let user = store::user(page.conn, page.user_id)?;
    let google = user
        .as_ref()
        .and_then(|person| person.discord_id.as_deref())
        .map(|discord_id| crate::google::status(page.conn, discord_id))
        .transpose()?
        .flatten();
    let mut ctx = shell(page)?;
    ctx["empty_server"] = context_bool(false);
    ctx["google_ready"] = context_bool(crate::google::configured_from_env());
    ctx["google_connected"] = context_bool(google.as_ref().is_some_and(|status| status.connected));
    ctx["google_reconnect"] = context_bool(google.as_ref().is_some_and(|status| status.reconnect));
    ctx["google_auto_sync"] = context_bool(google.as_ref().is_some_and(|status| status.auto_sync));
    ctx["google_email"] = Value::from(
        google
            .as_ref()
            .map(|status| status.email.clone())
            .unwrap_or_default(),
    );
    render("google.html", ctx)
}

pub fn form_page(page: &mut Page<'_>, draft: &FormDraft) -> Result<String, OpError> {
    let (channels, roles) = form_refs(page, draft)?;
    let mut ctx = shell(page)?;
    ctx["heading"] = Value::from(draft.heading.clone());
    ctx["error"] = Value::from(draft.error.clone());
    ctx["series_note"] = Value::from(draft.series_note.clone());
    ctx["action_url"] = Value::from(draft.action_url.clone());
    ctx["event_title"] = Value::from(draft.event_title.clone());
    ctx["description"] = Value::from(draft.description.clone());
    ctx["channels"] = channels;
    ctx["colors"] = colors(&draft.color);
    ctx["start"] = Value::from(draft.start.clone());
    ctx["duration"] = Value::from(draft.duration.clone());
    ctx["cutoff"] = Value::from(draft.cutoff.clone());
    ctx["post_at"] = Value::from(draft.post_at.clone());
    ctx["options"] = Value::from_serialize(&draft.options);
    ctx["reminders"] = Value::from_serialize(&draft.reminders);
    ctx["roles"] = roles;
    ctx["lock_options"] = context_bool(draft.lock_options);
    ctx["thread_enabled"] = context_bool(draft.thread_enabled);
    ctx["multiple_signups"] = context_bool(draft.multiple_signups);
    ctx["mention_everyone"] = context_bool(draft.mention_everyone);
    ctx["attendee_role_id"] = Value::from(draft.attendee_role_id.clone());
    ctx["series_locked"] = context_bool(draft.series_locked);
    ctx["repeat"] = Value::from(draft.repeat.clone());
    ctx["repeat_count"] = Value::from(draft.repeat_count.clone());
    ctx["repeat_until"] = Value::from(draft.repeat_until.clone());
    ctx["show_apply"] = context_bool(draft.show_apply);
    ctx["apply_series"] = context_bool(draft.apply_series);
    ctx["show_draft"] = context_bool(draft.show_draft);
    ctx["show_schedule"] = context_bool(draft.show_schedule);
    ctx["show_publish"] = context_bool(draft.show_publish);
    ctx["show_save"] = context_bool(draft.show_save);
    render("form.html", ctx)
}

pub fn not_found(page: &mut Page<'_>) -> Result<String, OpError> {
    render("notfound.html", shell(page)?)
}

#[derive(Clone, Serialize)]
pub struct Slot {
    pub n: usize,
    pub id: String,
    pub name: String,
    pub emoji: String,
    pub capacity: String,
    pub waitlist: bool,
    pub role_id: String,
    pub reminders: bool,
}

#[derive(Clone, Serialize)]
pub struct RemSlot {
    pub n: usize,
    pub offset: String,
    pub message: String,
}

#[derive(Clone)]
pub struct FormDraft {
    pub heading: String,
    pub error: String,
    pub series_note: String,
    pub action_url: String,
    pub event_title: String,
    pub description: String,
    pub channel_id: String,
    pub color: String,
    pub start: String,
    pub duration: String,
    pub cutoff: String,
    pub post_at: String,
    pub options: Vec<Slot>,
    pub reminders: Vec<RemSlot>,
    pub thread_enabled: bool,
    pub multiple_signups: bool,
    pub mention_everyone: bool,
    pub attendee_role_id: String,
    pub mention_role_ids: Vec<i64>,
    pub restrict_role_ids: Vec<i64>,
    pub series_locked: bool,
    pub repeat: String,
    pub repeat_count: String,
    pub repeat_until: String,
    pub show_apply: bool,
    pub apply_series: bool,
    pub lock_options: bool,
    pub show_draft: bool,
    pub show_schedule: bool,
    pub show_publish: bool,
    pub show_save: bool,
    pub existing_id: Option<i64>,
}

pub fn blank_form(server: &str, preset: Option<logic::PresetBody>) -> FormDraft {
    let mut draft = FormDraft {
        heading: "New event".into(),
        error: String::new(),
        series_note: String::new(),
        action_url: format!("/servers/{server}/events/new"),
        event_title: String::new(),
        description: String::new(),
        channel_id: String::new(),
        color: "#1e3a6e".into(),
        start: String::new(),
        duration: "60".into(),
        cutoff: String::new(),
        post_at: String::new(),
        options: vec![],
        reminders: vec![RemSlot {
            n: 0,
            offset: "15".into(),
            message: String::new(),
        }],
        thread_enabled: true,
        multiple_signups: false,
        mention_everyone: false,
        attendee_role_id: String::new(),
        mention_role_ids: vec![],
        restrict_role_ids: vec![],
        series_locked: false,
        repeat: "none".into(),
        repeat_count: String::new(),
        repeat_until: String::new(),
        show_apply: false,
        apply_series: false,
        lock_options: false,
        show_draft: true,
        show_schedule: true,
        show_publish: true,
        show_save: false,
        existing_id: None,
    };
    let options = preset
        .map(|body| {
            draft.multiple_signups = body.multiple_signups;
            body.options
        })
        .unwrap_or_else(default_options);
    draft.options = finish_options(options.into_iter().map(slot_from_option).collect());
    draft.reminders = finish_reminders(draft.reminders);
    draft
}

pub fn form_from_event(conn: &mut Connection, event_id: i64, tz: Tz) -> Result<FormDraft, OpError> {
    let event = store::event(conn, event_id)?.ok_or(OpError::NotFound)?;
    let options = store::options_for(conn, event_id)?;
    let reminders = store::reminders_for(conn, event_id)?;
    let server = store::discord_server_id(conn, event.guild_id)?;
    let mut draft = blank_form(&server, None);
    draft.heading = format!("Edit {}", event.title);
    draft.action_url = format!("/servers/{server}/events/{}/edit", event.public_id);
    draft.existing_id = Some(event.id);
    draft.event_title = event.title.clone();
    draft.description = event.description.clone();
    draft.channel_id = event.channel_id.to_string();
    draft.color = event.color.clone();
    draft.start = clock::datetime_local(clock::parse_stamp(&event.starts_at)?, tz);
    draft.duration = event.duration_minutes.to_string();
    draft.cutoff = event
        .cutoff_at
        .as_deref()
        .and_then(|v| clock::parse_stamp(v).ok())
        .map(|dt| clock::datetime_local(dt, tz))
        .unwrap_or_default();
    draft.post_at = event
        .post_at
        .as_deref()
        .and_then(|v| clock::parse_stamp(v).ok())
        .map(|dt| clock::datetime_local(dt, tz))
        .unwrap_or_default();
    draft.options = finish_options(
        options
            .into_iter()
            .map(|opt| Slot {
                n: 0,
                id: opt.id.to_string(),
                name: opt.name,
                emoji: opt.emoji,
                capacity: opt.capacity.map(|n| n.to_string()).unwrap_or_default(),
                waitlist: opt.waitlist,
                role_id: opt.role_id.map(|id| id.to_string()).unwrap_or_default(),
                reminders: opt.reminders_enabled,
            })
            .collect(),
    );
    draft.reminders = finish_reminders(
        reminders
            .into_iter()
            .map(|rem| RemSlot {
                n: 0,
                offset: rem.offset_minutes.to_string(),
                message: rem.message,
            })
            .collect(),
    );
    draft.thread_enabled = event.thread_enabled;
    draft.multiple_signups = event.multiple_signups;
    draft.mention_everyone = event.mention_everyone;
    draft.attendee_role_id = event
        .attendee_role_id
        .map(|id| id.to_string())
        .unwrap_or_default();
    draft.mention_role_ids = store::event_role_ids(conn, event.id, "mention")?;
    draft.restrict_role_ids = store::event_role_ids(conn, event.id, "restrict")?;
    draft.series_locked = event.series_id.is_some();
    let once = event.series_id.is_some_and(|id| {
        store::series(conn, id)
            .ok()
            .flatten()
            .is_some_and(|series| series.cadence == "once")
    });
    draft.series_note = if event.series_id.is_some() && !once {
        "This date keeps its own start time. Cadence, date count, and end date are on the series."
            .into()
    } else {
        String::new()
    };
    apply_flags(&mut draft, &event.status, event.series_id.is_some());
    Ok(draft)
}

pub fn form_from_map(
    map: &HashMap<String, String>,
    server: &str,
    existing: Option<&Event>,
    error: String,
    tz: Tz,
) -> FormDraft {
    let mut draft = blank_form(server, None);
    draft.error = error;
    draft.event_title = map.get("title").cloned().unwrap_or_default();
    draft.description = map.get("description").cloned().unwrap_or_default();
    draft.channel_id = map.get("channel_id").cloned().unwrap_or_default();
    draft.color = map
        .get("color")
        .cloned()
        .unwrap_or_else(|| "#1e3a6e".into());
    draft.start = map.get("start").cloned().unwrap_or_default();
    draft.duration = map.get("duration").cloned().unwrap_or_else(|| "60".into());
    draft.cutoff = map.get("cutoff").cloned().unwrap_or_default();
    draft.post_at = map.get("post_at").cloned().unwrap_or_default();
    draft.thread_enabled = map.contains_key("thread_enabled");
    draft.multiple_signups = map.contains_key("multiple_signups");
    draft.mention_everyone = map.contains_key("mention_everyone");
    draft.attendee_role_id = map.get("attendee_role_id").cloned().unwrap_or_default();
    draft.repeat = map.get("repeat").cloned().unwrap_or_else(|| "none".into());
    draft.repeat_count = map.get("repeat_count").cloned().unwrap_or_default();
    draft.repeat_until = map.get("repeat_until").cloned().unwrap_or_default();
    draft.apply_series = map.contains_key("apply_series");
    for key in map.keys() {
        if let Some(id) = key.strip_prefix("mention_") {
            if let Ok(id) = id.parse() {
                draft.mention_role_ids.push(id);
            }
        }
        if let Some(id) = key.strip_prefix("restrict_") {
            if let Ok(id) = id.parse() {
                draft.restrict_role_ids.push(id);
            }
        }
    }
    draft.options = finish_options(
        option_indexes(map)
            .into_iter()
            .map(|n| Slot {
                n,
                id: map.get(&format!("opt{n}_id")).cloned().unwrap_or_default(),
                name: map
                    .get(&format!("opt{n}_name"))
                    .cloned()
                    .unwrap_or_default(),
                emoji: map
                    .get(&format!("opt{n}_emoji"))
                    .cloned()
                    .unwrap_or_default(),
                capacity: map
                    .get(&format!("opt{n}_capacity"))
                    .cloned()
                    .unwrap_or_default(),
                waitlist: map.contains_key(&format!("opt{n}_waitlist")),
                role_id: map
                    .get(&format!("opt{n}_role"))
                    .cloned()
                    .unwrap_or_default(),
                reminders: map.contains_key(&format!("opt{n}_reminders")),
            })
            .collect(),
    );
    draft.reminders = finish_reminders(
        reminder_indexes(map)
            .into_iter()
            .map(|n| RemSlot {
                n,
                offset: map
                    .get(&format!("rem{n}_offset"))
                    .cloned()
                    .unwrap_or_default(),
                message: map
                    .get(&format!("rem{n}_message"))
                    .cloned()
                    .unwrap_or_default(),
            })
            .collect(),
    );
    if let Some(event) = existing {
        draft.existing_id = Some(event.id);
        draft.action_url = format!("/servers/{server}/events/{}/edit", event.public_id);
        draft.heading = format!("Edit {}", event.title);
        draft.series_locked = event.series_id.is_some();
        apply_flags(&mut draft, &event.status, event.series_id.is_some());
    }
    let _ = tz;
    draft
}

pub fn input_from_form(
    map: &HashMap<String, String>,
    guild_id: i64,
    actor_id: i64,
    tz: Tz,
    existing: Option<&Event>,
) -> Result<EventInput, OpError> {
    let draft = form_from_map(map, "", existing, String::new(), tz);
    let start = clock::parse_local(&draft.start, tz)?;
    let duration = draft
        .duration
        .parse::<i64>()
        .map_err(|_| OpError::message("Duration needs to be a number of minutes."))?;
    let cutoff = if draft.cutoff.trim().is_empty() {
        None
    } else {
        Some(clock::parse_local(&draft.cutoff, tz)?)
    };
    let post_at = if draft.post_at.trim().is_empty() {
        None
    } else {
        Some(clock::parse_local(&draft.post_at, tz)?)
    };
    let repeat_until = if draft.repeat_until.trim().is_empty() {
        None
    } else {
        Some(clock::parse_date(&draft.repeat_until, tz)?)
    };
    let repeat = if draft.series_locked || draft.repeat == "none" || draft.repeat.is_empty() {
        None
    } else {
        Some(RepeatSpec {
            cadence: draft.repeat.clone(),
            end_on: repeat_until,
            max_occurrences: if draft.repeat_count.trim().is_empty() {
                None
            } else {
                Some(
                    draft
                        .repeat_count
                        .parse()
                        .map_err(|_| OpError::message("The date count needs to be a number."))?,
                )
            },
        })
    };
    let action = match map.get("action").map(String::as_str).unwrap_or("publish") {
        "draft" => SaveAction::Draft,
        "schedule" => SaveAction::Schedule,
        "save" => SaveAction::Save,
        _ => SaveAction::Publish,
    };
    let mut mention_role_ids = Vec::new();
    let mut restrict_role_ids = Vec::new();
    for (key, _) in map {
        if let Some(id) = key.strip_prefix("mention_") {
            if let Ok(id) = id.parse() {
                mention_role_ids.push(id);
            }
        }
        if let Some(id) = key.strip_prefix("restrict_") {
            if let Ok(id) = id.parse() {
                restrict_role_ids.push(id);
            }
        }
    }
    let options = draft
        .options
        .iter()
        .filter(|slot| !slot.name.trim().is_empty())
        .map(|slot| OptionInput {
            id: slot.id.parse().ok(),
            name: slot.name.clone(),
            emoji: slot.emoji.clone(),
            capacity: if slot.capacity.trim().is_empty() {
                None
            } else {
                slot.capacity.parse().ok()
            },
            waitlist: slot.waitlist,
            role_id: slot.role_id.parse().ok(),
            reminders_enabled: slot.reminders,
        })
        .collect();
    let reminders = draft
        .reminders
        .iter()
        .filter(|slot| !slot.offset.trim().is_empty())
        .map(|slot| ReminderInput {
            offset_minutes: slot.offset.parse().unwrap_or(0),
            message: slot.message.clone(),
        })
        .collect();
    Ok(EventInput {
        id: existing.map(|event| event.id),
        guild_id,
        actor_id,
        channel_id: draft.channel_id.parse().unwrap_or(0),
        title: draft.event_title,
        description: draft.description,
        starts_at: start,
        duration_minutes: duration,
        color: draft.color,
        image_url: None,
        cutoff_at: cutoff,
        attendee_role_id: draft.attendee_role_id.parse().ok(),
        thread_enabled: draft.thread_enabled,
        multiple_signups: draft.multiple_signups,
        mention_everyone: draft.mention_everyone,
        mention_role_ids,
        restrict_role_ids,
        options,
        reminders,
        repeat,
        action,
        post_at,
        apply_series: draft.apply_series,
    })
}

fn apply_flags(draft: &mut FormDraft, status: &str, series: bool) {
    draft.show_apply = series;
    draft.lock_options = status == "posted";
    match status {
        "posted" => {
            draft.show_draft = false;
            draft.show_schedule = false;
            draft.show_publish = false;
            draft.show_save = true;
        }
        "scheduled" | "failed" => {
            draft.show_draft = true;
            draft.show_schedule = true;
            draft.show_publish = true;
            draft.show_save = true;
        }
        _ => {
            draft.show_draft = true;
            draft.show_schedule = true;
            draft.show_publish = true;
            draft.show_save = false;
        }
    }
}

fn option_indexes(map: &HashMap<String, String>) -> Vec<usize> {
    let mut indexes = Vec::new();
    for key in map.keys() {
        let Some(rest) = key.strip_prefix("opt") else {
            continue;
        };
        let Some((n, field)) = rest.split_once('_') else {
            continue;
        };
        if field == "name" {
            if let Ok(n) = n.parse() {
                indexes.push(n);
            }
        }
    }
    indexes.sort_unstable();
    indexes.dedup();
    indexes.truncate(8);
    indexes
}

fn finish_options(mut slots: Vec<Slot>) -> Vec<Slot> {
    slots.truncate(8);
    for (n, slot) in slots.iter_mut().enumerate() {
        slot.n = n;
    }
    slots
}

fn reminder_indexes(map: &HashMap<String, String>) -> Vec<usize> {
    let mut indexes = Vec::new();
    for key in map.keys() {
        let Some(rest) = key.strip_prefix("rem") else {
            continue;
        };
        let Some((n, field)) = rest.split_once('_') else {
            continue;
        };
        if field == "offset" {
            if let Ok(n) = n.parse() {
                indexes.push(n);
            }
        }
    }
    indexes.sort_unstable();
    indexes.dedup();
    indexes.truncate(5);
    indexes
}

fn finish_reminders(mut slots: Vec<RemSlot>) -> Vec<RemSlot> {
    slots.truncate(5);
    for (n, slot) in slots.iter_mut().enumerate() {
        slot.n = n;
    }
    slots
}

fn slot_from_option(opt: OptionInput) -> Slot {
    Slot {
        n: 0,
        id: String::new(),
        name: opt.name,
        emoji: opt.emoji,
        capacity: opt.capacity.map(|n| n.to_string()).unwrap_or_default(),
        waitlist: opt.waitlist,
        role_id: opt.role_id.map(|id| id.to_string()).unwrap_or_default(),
        reminders: opt.reminders_enabled,
    }
}

fn default_options() -> Vec<OptionInput> {
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

fn form_refs(page: &mut Page<'_>, draft: &FormDraft) -> Result<(Value, Value), OpError> {
    let channels = store::list_channels(page.conn, page.guild_id)?;
    let roles = store::list_roles(page.conn, page.guild_id)?;
    let channel_values = channels.iter().map(|channel| serde_json::json!({
        "id": channel.id,
        "name": channel.name,
        "selected": draft.channel_id == channel.id.to_string() || (draft.channel_id.is_empty() && channel.id == channels.first().map(|c| c.id).unwrap_or(0)),
    })).collect::<Vec<_>>();
    let mention: HashSet<i64> = draft.mention_role_ids.iter().copied().collect();
    let restrict: HashSet<i64> = draft.restrict_role_ids.iter().copied().collect();
    let role_values = roles
        .iter()
        .map(|role| {
            serde_json::json!({
                "id": role.id,
                "id_s": role.id.to_string(),
                "name": role.name,
                "mentioned": mention.contains(&role.id),
                "restricted": restrict.contains(&role.id),
            })
        })
        .collect::<Vec<_>>();
    Ok((
        Value::from_serialize(&channel_values),
        Value::from_serialize(&role_values),
    ))
}

fn colors(current: &str) -> Value {
    let palette = [
        ("#1e3a6e", "Marian blue"),
        ("#2f6b4f", "Tide green"),
        ("#8a6232", "Brass"),
        ("#6b3a6b", "Plum"),
        ("#8d3a32", "Seal red"),
        ("#243038", "Ink"),
    ];
    Value::from_serialize(
        &palette
            .iter()
            .map(|(value, label)| {
                serde_json::json!({
                    "value": value,
                    "label": label,
                    "selected": *value == current,
                })
            })
            .collect::<Vec<_>>(),
    )
}

fn role_choices(roles: &[store::Role], selected: Option<i64>) -> Value {
    let mut rows = vec![
        serde_json::json!({"id": "", "name": "No extra role", "selected": selected.is_none()}),
    ];
    for role in roles {
        rows.push(serde_json::json!({
            "id": role.id.to_string(),
            "name": role.name,
            "selected": selected == Some(role.id),
        }));
    }
    Value::from_serialize(&rows)
}

fn rows_for(
    server: &str,
    events: &[Event],
    settings: &Settings,
    tz: Tz,
    keep: impl Fn(&Event) -> bool,
) -> Vec<serde_json::Value> {
    events
        .iter()
        .filter(|event| keep(event))
        .filter_map(|event| {
            let start = clock::parse_stamp(&event.starts_at).ok()?;
            Some(serde_json::json!({
                "href": format!("/servers/{server}/events/{}", event.public_id),
                "badge": logic::status_label(&event.status),
                "title": event.title,
                "meta": clock::format_when(start, tz, settings.hour_24),
            }))
        })
        .collect()
}

fn card_value(
    conn: &mut Connection,
    event: &Event,
    settings: &Settings,
    tz: Tz,
    now: DateTime<Utc>,
    hero: bool,
) -> Result<serde_json::Value, OpError> {
    let start = clock::parse_stamp(&event.starts_at)?;
    let options = store::options_for(conn, event.id)?;
    let signups = store::signups_for(conn, event.id)?;
    let spots: Vec<serde_json::Value> = options
        .iter()
        .map(|opt| {
            let confirmed = signups
                .iter()
                .filter(|s| s.option_id == opt.id && s.state == "confirmed")
                .count();
            let waiting = signups
                .iter()
                .filter(|s| s.option_id == opt.id && s.state == "waitlisted")
                .count();
            let mut count = match opt.capacity {
                Some(cap) => format!("{confirmed}/{cap}"),
                None => confirmed.to_string(),
            };
            if waiting > 0 {
                count = format!("{count} +{waiting} waiting");
            }
            serde_json::json!({"emoji": opt.emoji, "count": count})
        })
        .collect();
    let channel = store::channel(conn, event.channel_id)?
        .map(|c| c.name)
        .unwrap_or_else(|| "deleted".into());
    let mut value = serde_json::json!({
        "id": event.public_id,
        "title": event.title,
        "when": clock::format_when(start, tz, settings.hour_24),
        "end": clock::format_end(start, event.duration_minutes, tz, settings.hour_24),
        "channel": channel,
        "status_label": logic::status_label(&event.status),
        "rel": clock::format_rel(start, now),
        "spots": spots,
    });
    if hero {
        value["description"] =
            serde_json::Value::String(event.description.chars().take(180).collect());
        value["spots"] = serde_json::Value::String(spots_join(&options, &signups));
    }
    Ok(value)
}

fn spots_join(options: &[store::SignupOption], signups: &[store::Signup]) -> String {
    options
        .iter()
        .map(|opt| {
            let confirmed = signups
                .iter()
                .filter(|s| s.option_id == opt.id && s.state == "confirmed")
                .count();
            format!("{} {}", opt.name, confirmed)
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

fn people(users: &[User], ids: &[i64]) -> Vec<serde_json::Value> {
    ids.iter()
        .filter_map(|id| {
            users.iter().find(|user| user.id == *id).map(|user| {
                serde_json::json!({
                    "id": user.id,
                    "name": user.name,
                    "color": user.color,
                })
            })
        })
        .collect()
}

fn activity_rows(page: &mut Page<'_>, limit: i64) -> Result<Vec<serde_json::Value>, OpError> {
    let now = Utc::now();
    let mut rows = Vec::new();
    for item in store::list_activity(page.conn, page.guild_id, limit)? {
        let when = clock::parse_stamp(&item.created_at)
            .map(|dt| clock::format_rel(dt, now))
            .unwrap_or_default();
        let server = store::discord_server_id(page.conn, page.guild_id)?;
        let href = match item.event_id {
            Some(id) => match store::event(page.conn, id)? {
                Some(event) => format!("/servers/{server}/events/{}", event.public_id),
                None => format!("/servers/{server}/activity"),
            },
            None => format!("/servers/{server}/activity"),
        };
        rows.push(serde_json::json!({"body": item.body, "when": when, "href": href}));
    }
    Ok(rows)
}

fn cutoff_passed(event: &Event, now: DateTime<Utc>) -> bool {
    event
        .cutoff_at
        .as_deref()
        .and_then(|v| clock::parse_stamp(v).ok())
        .map(|dt| now >= dt)
        .unwrap_or(false)
}

fn zone_of(page: &mut Page<'_>) -> Result<(Settings, Tz), OpError> {
    let settings = store::settings(page.conn, page.guild_id)?;
    let guild = store::guild(page.conn, page.guild_id)?.ok_or(OpError::NotFound)?;
    Ok((settings, clock::zone(&guild.timezone)?))
}

fn must_user(page: &mut Page<'_>) -> Result<User, OpError> {
    store::user(page.conn, page.user_id)?.ok_or(OpError::NotFound)
}

struct Bag(HashMap<String, Value>);

impl Bag {
    fn new() -> Self {
        Self(HashMap::new())
    }

    fn insert(&mut self, key: &str, value: Value) {
        self.0.insert(key.to_string(), value);
    }
}

impl std::ops::Index<&str> for Bag {
    type Output = Value;

    fn index(&self, key: &str) -> &Value {
        &self.0[key]
    }
}

impl std::ops::IndexMut<&str> for Bag {
    fn index_mut(&mut self, key: &str) -> &mut Value {
        self.0
            .entry(key.to_string())
            .or_insert_with(|| Value::from(""))
    }
}

fn shell(page: &mut Page<'_>) -> Result<Bag, OpError> {
    let mut ctx = Bag::new();
    asset_context(&mut ctx);
    ctx.insert("title", Value::from(page.title));
    ctx.insert("notice", Value::from(page.notice.unwrap_or("")));
    ctx.insert("nav", Value::from(page.nav));
    ctx.insert("invite_url", Value::from(page.invite_url));
    if page.guild_id == 0 {
        ctx.insert("guild_name", Value::from(""));
        ctx.insert("guild_id", Value::from(0));
        ctx.insert("base", Value::from(""));
        ctx.insert("tz_label", Value::from(""));
        ctx.insert("park", context_bool(false));
        ctx.insert("users", Value::from_serialize(&Vec::<Value>::new()));
        ctx.insert("guilds", Value::from_serialize(&Vec::<Value>::new()));
        ctx.insert("user_id", Value::from(0));
        ctx.insert("user_name", Value::from(""));
        ctx.insert("reminders_on", context_bool(false));
        ctx.insert("draft_count", Value::from(0));
        ctx.insert("scheduled_count", Value::from(0));
        ctx.insert("series_count", Value::from(0));
        ctx.insert("routine_count", Value::from(0));
        ctx.insert("calendar_url", Value::from(""));
        return Ok(ctx);
    }
    let guild = store::guild(page.conn, page.guild_id)?.ok_or(OpError::NotFound)?;
    let settings = store::settings(page.conn, page.guild_id)?;
    let tz = clock::zone(&guild.timezone)?;
    let users = store::list_users(page.conn, page.guild_id)?;
    let guilds = store::list_guilds(page.conn)?
        .into_iter()
        .filter(|guild| page.allowed_guilds.contains(&guild.id))
        .collect::<Vec<_>>();
    let user = users.iter().find(|user| user.id == page.user_id);
    let server = guild
        .discord_id
        .clone()
        .filter(|id| !id.is_empty())
        .ok_or(OpError::NotFound)?;
    ctx.insert("guild_name", Value::from(guild.name));
    ctx.insert("guild_id", Value::from(server.clone()));
    ctx.insert("base", Value::from(format!("/servers/{server}")));
    ctx.insert("tz_label", Value::from(zone_label(tz)));
    ctx.insert("park", context_bool(settings.park_clock));
    ctx.insert(
        "users",
        Value::from_serialize(
            &users
                .iter()
                .map(|user| serde_json::json!({"id": user.id, "name": user.name}))
                .collect::<Vec<_>>(),
        ),
    );
    ctx.insert(
        "guilds",
        Value::from_serialize(
            &guilds
                .iter()
                .filter_map(|g| {
                    let id = g.discord_id.clone().filter(|id| !id.is_empty())?;
                    Some(serde_json::json!({"id": id, "name": g.name}))
                })
                .collect::<Vec<_>>(),
        ),
    );
    ctx.insert("user_id", Value::from(page.user_id));
    ctx.insert(
        "user_name",
        Value::from(user.map(|u| u.name.as_str()).unwrap_or("")),
    );
    ctx.insert(
        "reminders_on",
        context_bool(user.map(|u| u.reminders_enabled).unwrap_or(true)),
    );
    ctx.insert(
        "draft_count",
        Value::from(store::count_status(page.conn, page.guild_id, "draft")?),
    );
    ctx.insert(
        "scheduled_count",
        Value::from(
            store::count_status(page.conn, page.guild_id, "scheduled")?
                + store::count_status(page.conn, page.guild_id, "failed")?,
        ),
    );
    ctx.insert(
        "series_count",
        Value::from(store::count_active_series(page.conn, page.guild_id)?),
    );
    ctx.insert(
        "routine_count",
        Value::from(logic::count_active(page.conn, page.guild_id)?),
    );
    let calendar_url = user
        .and_then(|person| person.discord_id.as_deref())
        .map(|discord_id| store::ensure_calendar_token(page.conn, discord_id))
        .transpose()?
        .map(|token| format!("/calendar/{token}.ics"))
        .unwrap_or_default();
    ctx.insert("calendar_url", Value::from(calendar_url));
    Ok(ctx)
}

fn asset_context(ctx: &mut Bag) {
    let url = |path: &str| Value::from_safe_string(path.to_string());
    ctx.insert("asset_css", url(crate::assets::css()));
    ctx.insert("asset_js", url(crate::assets::js()));
    ctx.insert("asset_mark", url(crate::assets::mark()));
    ctx.insert("asset_icon", url(crate::assets::icon()));
}

fn context_bool(value: bool) -> Value {
    Value::from(value)
}

fn markdown_filter(value: String) -> Value {
    Value::from_safe_string(crate::markdown::render(&value))
}

fn render(name: &str, ctx: Bag) -> Result<String, OpError> {
    let env = templates();
    let template = env
        .get_template(name)
        .map_err(|err| OpError::message(err.to_string()))?;
    let operation = format!("template {name}");
    let span = tracing::info_span!(
        "template",
        otel.name = operation.as_str(),
        "template.name" = name,
    );
    span.in_scope(|| {
        template
            .render(ctx.0)
            .map_err(|err| OpError::message(err.to_string()))
    })
}

fn templates() -> &'static Environment<'static> {
    use std::sync::OnceLock;
    static ENV: OnceLock<Environment<'static>> = OnceLock::new();
    ENV.get_or_init(|| {
        let mut env = Environment::new();
        env.set_trim_blocks(true);
        env.set_lstrip_blocks(true);
        env.set_undefined_behavior(minijinja::UndefinedBehavior::Strict);
        env.set_auto_escape_callback(|name| {
            if name.ends_with(".html") {
                minijinja::AutoEscape::Html
            } else {
                minijinja::AutoEscape::None
            }
        });
        env.add_filter("md", markdown_filter);
        env.add_template("base.html", include_str!("../templates/base.html"))
            .expect("base");
        env.add_template("schedule.html", include_str!("../templates/schedule.html"))
            .expect("schedule");
        env.add_template("event.html", include_str!("../templates/event.html"))
            .expect("event");
        env.add_template("form.html", include_str!("../templates/form.html"))
            .expect("form");
        env.add_template(
            "option_row.html",
            include_str!("../templates/option_row.html"),
        )
        .expect("option row");
        env.add_template(
            "reminder_row.html",
            include_str!("../templates/reminder_row.html"),
        )
        .expect("reminder row");
        env.add_template("list.html", include_str!("../templates/list.html"))
            .expect("list");
        env.add_template("series.html", include_str!("../templates/series.html"))
            .expect("series");
        env.add_template("settings.html", include_str!("../templates/settings.html"))
            .expect("settings");
        env.add_template("google.html", include_str!("../templates/google.html"))
            .expect("google");
        env.add_template(
            "role_option.html",
            include_str!("../templates/role_option.html"),
        )
        .expect("role");
        env.add_template("presets.html", include_str!("../templates/presets.html"))
            .expect("presets");
        env.add_template("channels.html", include_str!("../templates/channels.html"))
            .expect("channels");
        env.add_template("commands.html", include_str!("../templates/commands.html"))
            .expect("commands");
        env.add_template("guide.html", include_str!("../templates/guide.html"))
            .expect("guide");
        env.add_template("help.html", include_str!("../templates/help.html"))
            .expect("help");
        env.add_template("routines.html", include_str!("../templates/routines.html"))
            .expect("routines");
        env.add_template("notfound.html", include_str!("../templates/notfound.html"))
            .expect("notfound");
        env.add_template("landing.html", include_str!("../templates/landing.html"))
            .expect("landing");
        env
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;
    use crate::store::Connection;
    use chrono::TimeZone;
    use diesel::connection::SimpleConnection;
    use diesel::prelude::*;

    fn sample() -> Connection {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Guild', 'UTC', '99');
            INSERT INTO channels (id, guild_id, name, kind, position) VALUES (1, 1, 'events', 'text', 0);
            INSERT INTO users (id, guild_id, name, handle, color, reminders_enabled, organizer)
              VALUES (1, 1, 'Nell', 'nell', '#112233', 1, 1);
            ",
        )
        .unwrap();
        store::insert_default_settings(&mut conn, 1).unwrap();
        conn
    }

    #[test]
    fn pages_render() {
        let mut conn = sample();
        logic::save_event(
            &mut conn,
            EventInput {
                id: None,
                guild_id: 1,
                actor_id: 1,
                channel_id: 1,
                title: "Raid".into(),
                description: "Meet at **the hall**. <script>".into(),
                starts_at: Utc::now() + Duration::hours(5),
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
                    name: "Yes".into(),
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
                repeat: None,
                action: SaveAction::Publish,
                post_at: None,
                apply_series: false,
            },
        )
        .unwrap();
        diesel::insert_into(schema::activity::table)
            .values((
                schema::activity::guild_id.eq(1),
                schema::activity::body.eq("Posted Raid."),
                schema::activity::created_at.eq("2026-10-13T00:00:00Z"),
            ))
            .execute(&mut conn)
            .unwrap();
        let event_id = store::list_events(&mut conn, 1)
            .unwrap()
            .into_iter()
            .find(|e| e.status == "posted")
            .unwrap()
            .id;
        let mut page = Page {
            conn: &mut conn,
            guild_id: 1,
            user_id: 1,
            notice: None,
            invite_url: "",
            nav: "schedule",
            title: "This week",
            allowed_guilds: &[1],
        };
        let home = home(&mut page).unwrap();
        assert!(
            home.contains("<title>This week · Guild · Marian</title>"),
            "{home}"
        );
        page.nav = "event";
        page.title = "Event";
        let body = event_page(&mut page, event_id).unwrap();
        assert!(body.contains("d-kicker"), "{body}");
        assert!(body.contains("Time"), "{body}");
        assert!(body.contains("Created by Nell"), "{body}");
        assert!(body.contains("View on web"), "{body}");
        assert!(body.contains("d-btn primary"), "{body}");
        assert!(body.contains("d-btn danger"), "{body}");
        assert!(body.contains("✅"), "{body}");
        assert!(body.contains("<strong>the hall</strong>"), "{body}");
        assert!(body.contains("&lt;script&gt;"), "{body}");
        let form = form_page(&mut page, &blank_form("99", None)).unwrap();
        assert!(form.contains("Does not repeat"), "{form}");
        assert!(form.contains("Post now"), "{form}");
        assert!(form.contains("Add option"), "{form}");
        assert!(form.contains("Add reminder"), "{form}");
        assert_eq!(form.matches("placeholder=\"minutes\"").count(), 2, "{form}");
        assert!(
            form.contains("This event opens a Discord thread."),
            "{form}"
        );
        assert!(
            form.contains("Only people with a checked role can sign up."),
            "{form}"
        );
        let mention = form
            .find("The Discord post mentions the roles you check.")
            .expect("mention heading");
        let everyone = form.find("name=\"mention_everyone\"").expect("everyone");
        let restrict = form
            .find("Only people with a checked role can sign up.")
            .expect("restrict");
        assert!(mention < everyone && everyone < restrict, "{form}");
        assert!(form.contains(">@everyone<"), "{form}");
        assert!(!form.contains("Mention everyone"), "{form}");
        assert!(form.contains("value=\"Accepted\""), "{form}");
        assert!(form.contains("value=\"Declined\""), "{form}");
        assert!(form.contains("value=\"Tentative\""), "{form}");
        assert_eq!(
            form.matches("aria-label=\"Option name\"").count(),
            4,
            "{form}"
        );
        page.nav = "settings";
        page.title = "Settings";
        let settings = settings_page(&mut page).unwrap();
        assert!(
            settings.contains("Reminders post in the event thread."),
            "{settings}"
        );
        assert!(
            settings.contains("Every event stays the same distance from now."),
            "{settings}"
        );
        page.nav = "commands";
        page.title = "Commands";
        let desk = commands_page(&mut page).unwrap();
        assert!(desk.contains("command-line"), "{desk}");
        assert!(desk.contains("Starts an event"), "{desk}");
        let help = help_page().unwrap();
        assert!(help.contains("Starts an event"), "{help}");
        assert!(help.contains("Accepted, Declined, and Tentative"), "{help}");
        assert!(!help.contains("event:12"), "{help}");
        assert!(!help.contains("Healer"), "{help}");
        page.nav = "drafts";
        page.title = "Drafts";
        let drafts = list_page(&mut page, "drafts", ListQuery::default()).unwrap();
        assert!(!drafts.contains("log-list"), "{drafts}");
        assert!(drafts.contains("No drafts."), "{drafts}");
        page.nav = "activity";
        page.title = "Activity";
        let activity = list_page(&mut page, "activity", ListQuery::default()).unwrap();
        assert!(activity.contains("log-list"), "{activity}");
        assert!(activity.contains("Posted Raid."), "{activity}");
        assert!(!activity.contains(">Log<"), "{activity}");
        page.nav = "event";
        page.title = "Event";
        recurring(&mut page, ListQuery::default()).unwrap();
        assert!(!activity.contains("<table"), "{activity}");
    }

    #[test]
    fn home_keeps_this_week_to_five_events() {
        let mut conn = sample();
        let now = Utc::now();
        let (_, week_end) = week_window(now, chrono_tz::UTC);
        let deadline = week_end.and_hms_opt(0, 0, 0).unwrap().and_utc();
        let room = (deadline - now).num_seconds().max(8);
        let step = (room / 8).max(1);
        let names = ["Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot"];
        for (i, name) in names.iter().enumerate() {
            publish(
                &mut conn,
                name,
                now + Duration::seconds(1 + step * i as i64),
                60,
            );
        }
        publish(&mut conn, "Next month", deadline + Duration::days(14), 60);
        let mut page = Page {
            conn: &mut conn,
            guild_id: 1,
            user_id: 1,
            notice: None,
            invite_url: "",
            nav: "schedule",
            title: "This week",
            allowed_guilds: &[1],
        };
        let home = home(&mut page).unwrap();
        for name in ["Alpha", "Bravo", "Charlie", "Delta", "Echo"] {
            assert!(
                home.contains(&format!("<h3>{name}</h3>")),
                "{name} missing from this week\n{home}"
            );
        }
        assert!(!home.contains("<h3>Foxtrot</h3>"), "{home}");
        assert!(!home.contains("<h3>Next month</h3>"), "{home}");
        assert!(home.contains(">Alpha</a></h2>"), "{home}");
        assert!(!home.contains(">Foxtrot</a></h2>"), "{home}");
        assert!(!home.contains(">Next month</a></h2>"), "{home}");
    }

    fn publish(conn: &mut Connection, title: &str, start: DateTime<Utc>, duration: i64) {
        logic::save_event(
            conn,
            EventInput {
                id: None,
                guild_id: 1,
                actor_id: 1,
                channel_id: 1,
                title: title.into(),
                description: String::new(),
                starts_at: start,
                duration_minutes: duration,
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
                    name: "Yes".into(),
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
                repeat: None,
                action: SaveAction::Publish,
                post_at: None,
                apply_series: false,
            },
        )
        .unwrap();
    }

    #[test]
    fn a_scheduled_list_searches_and_pages() {
        assert_eq!(super::list_link("raid night", 2), "?q=raid+night&page=2");
        assert_eq!(super::page_window(26, 9, 25), (2, 25, 26));

        let mut conn = sample();
        for n in 1..=26 {
            logic::save_event(
                &mut conn,
                EventInput {
                    id: None,
                    guild_id: 1,
                    actor_id: 1,
                    channel_id: 1,
                    title: format!("Slot {n:02}"),
                    description: String::new(),
                    starts_at: Utc::now() + Duration::hours(10 + n),
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
                        name: "Yes".into(),
                        emoji: "✅".into(),
                        capacity: None,
                        waitlist: false,
                        role_id: None,
                        reminders_enabled: true,
                    }],
                    reminders: vec![],
                    repeat: None,
                    action: SaveAction::Schedule,
                    post_at: Some(Utc::now() + Duration::hours(1)),
                    apply_series: false,
                },
            )
            .unwrap();
        }
        let mut page = Page {
            conn: &mut conn,
            guild_id: 1,
            user_id: 1,
            notice: None,
            invite_url: "",
            nav: "scheduled",
            title: "Scheduled posts",
            allowed_guilds: &[1],
        };
        let first = list_page(&mut page, "scheduled", ListQuery::default()).unwrap();
        assert!(first.contains("<table"), "{first}");
        assert!(first.contains("Slot 01"), "{first}");
        assert!(first.contains("Slot 25"), "{first}");
        assert!(!first.contains("Slot 26"), "{first}");
        assert!(first.contains("1–25 of 26"), "{first}");
        assert!(first.contains("href=\"?page=2\""), "{first}");
        assert!(!first.contains("Previous"), "{first}");

        let second = list_page(&mut page, "scheduled", ListQuery::new("", 2)).unwrap();
        assert!(second.contains("Slot 26"), "{second}");
        assert!(!second.contains("Slot 01"), "{second}");
        assert!(second.contains("26–26 of 26"), "{second}");
        assert!(second.contains("Previous"), "{second}");

        let found = list_page(&mut page, "scheduled", ListQuery::new("slot 26", 1)).unwrap();
        assert!(found.contains("Slot 26"), "{found}");
        assert!(!found.contains("Slot 01"), "{found}");
        assert!(found.contains("1–1 of 1"), "{found}");

        let none = list_page(&mut page, "scheduled", ListQuery::new("no such", 1)).unwrap();
        assert!(none.contains("No matches."), "{none}");
        assert!(none.contains("value=\"no such\""), "{none}");
    }

    #[test]
    fn a_form_keeps_only_the_options_that_were_sent() {
        let mut map = std::collections::HashMap::new();
        map.insert("opt0_name".into(), "Accepted".into());
        map.insert("opt0_emoji".into(), "✅".into());
        map.insert("opt2_name".into(), "Late".into());
        map.insert("opt2_emoji".into(), "⏰".into());
        map.insert("opt2_reminders".into(), "1".into());
        map.insert("rem0_offset".into(), "15".into());
        map.insert("rem3_offset".into(), "60".into());
        map.insert("rem3_message".into(), "flasks".into());
        let draft = form_from_map(&map, "99", None, String::new(), chrono_tz::UTC);
        assert_eq!(draft.options.len(), 2);
        assert_eq!(draft.options[0].name, "Accepted");
        assert_eq!(draft.options[0].n, 0);
        assert_eq!(draft.options[1].name, "Late");
        assert_eq!(draft.options[1].n, 1);
        assert!(draft.options[1].reminders);
        assert_eq!(draft.reminders.len(), 2);
        assert_eq!(draft.reminders[0].offset, "15");
        assert_eq!(draft.reminders[0].n, 0);
        assert_eq!(draft.reminders[1].offset, "60");
        assert_eq!(draft.reminders[1].message, "flasks");
        assert_eq!(draft.reminders[1].n, 1);
    }

    #[test]
    fn series_page_edits_the_repeat() {
        let mut conn = sample();
        let id = logic::save_event(
            &mut conn,
            EventInput {
                id: None,
                guild_id: 1,
                actor_id: 1,
                channel_id: 1,
                title: "Vault".into(),
                description: String::new(),
                starts_at: Utc.with_ymd_and_hms(2026, 10, 15, 19, 0, 0).unwrap(),
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
                    emoji: String::new(),
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
                    max_occurrences: Some(3),
                }),
                action: SaveAction::Publish,
                post_at: None,
                apply_series: false,
            },
        )
        .unwrap();
        let event = store::event(&mut conn, id).unwrap().unwrap();
        let series = store::series(&mut conn, event.series_id.unwrap())
            .unwrap()
            .unwrap();
        logic::save_preset(
            &mut conn,
            1,
            1,
            "Raid",
            &logic::PresetBody {
                multiple_signups: false,
                options: vec![OptionInput {
                    id: None,
                    name: "Accepted".into(),
                    emoji: String::new(),
                    capacity: None,
                    waitlist: false,
                    role_id: None,
                    reminders_enabled: true,
                }],
            },
        )
        .unwrap();
        let preset = store::list_presets(&mut conn, 1).unwrap().remove(0);
        let mut page = Page {
            conn: &mut conn,
            guild_id: 1,
            user_id: 1,
            notice: None,
            invite_url: "",
            nav: "recurring",
            title: "Series",
            allowed_guilds: &[1],
        };
        assert!(series_page(&mut page, "1", ListQuery::default()).is_err());
        let html = series_page(&mut page, &series.public_id, ListQuery::default()).unwrap();
        assert!(
            html.contains(&format!("/recurring/{}", series.public_id)),
            "{html}"
        );
        assert!(html.contains("Save repeat"), "{html}");
        assert!(html.contains("<table"), "{html}");
        assert!(html.contains("Vault"), "{html}");
        assert!(html.contains("aria-label=\"Search\""), "{html}");
        let missed =
            series_page(&mut page, &series.public_id, ListQuery::new("no such", 1)).unwrap();
        assert!(missed.contains("No matches."), "{missed}");
        assert!(html.contains("value=\"weekly\" selected"), "{html}");
        assert!(html.contains("value=\"3\""), "{html}");
        let shown = event_page(&mut page, event.id).unwrap();
        assert!(
            shown.contains(&format!("/recurring/{}", series.public_id)),
            "{shown}"
        );
        let presets_html = presets(&mut page).unwrap();
        assert!(
            presets_html.contains(&format!("/presets/{}/delete", preset.public_id)),
            "{presets_html}"
        );
        assert!(!presets_html.contains("/presets/1/"), "{presets_html}");
    }

    #[test]
    fn settings_render_for_a_server_with_no_people() {
        let mut conn = store::open_memory().unwrap();
        diesel::insert_into(schema::guilds::table)
            .values((
                schema::guilds::id.eq(1),
                schema::guilds::name.eq("Guild"),
                schema::guilds::timezone.eq("UTC"),
                schema::guilds::discord_id.eq("99"),
            ))
            .execute(&mut conn)
            .unwrap();
        store::insert_default_settings(&mut conn, 1).unwrap();
        let mut page = Page {
            conn: &mut conn,
            guild_id: 1,
            user_id: 0,
            notice: None,
            invite_url: "",
            nav: "settings",
            title: "Settings",
            allowed_guilds: &[],
        };
        let body = settings_page(&mut page).unwrap();
        assert!(
            body.contains("Nobody from this server has used Marian yet"),
            "{body}"
        );
        assert!(body.contains("id=\"zone-list\""));
        assert!(body.contains("Europe&#x2f;Warsaw"));
        assert!(body.contains(">Warsaw</option>"));
        assert!(body.contains("value=\"UTC\" selected"));
    }

    #[test]
    fn google_page_offers_automatic_updates() {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Guild', 'UTC', '99');
            INSERT INTO users (id, guild_id, name, handle, color, discord_id) VALUES
              (1, 1, 'Nell', 'nell', '#112233', 'u1');
            INSERT INTO google_accounts (discord_user_id, refresh_token, email, connected_at, auto_sync)
              VALUES ('u1', 'refresh-token', 'nell@example.com', '2026-10-15T00:00:00Z', 0);
            ",
        )
        .unwrap();
        store::insert_default_settings(&mut conn, 1).unwrap();
        let mut page = Page {
            conn: &mut conn,
            guild_id: 1,
            user_id: 1,
            notice: None,
            invite_url: "",
            nav: "google",
            title: "Google Calendar",
            allowed_guilds: &[1],
        };
        let body = google_page(&mut page).unwrap();
        assert!(body.contains("nell@example.com"), "{body}");
        assert!(body.contains("name=\"auto_sync\""), "{body}");
        assert!(
            !body.contains("name=\"auto_sync\" value=\"1\" checked"),
            "{body}"
        );
        assert!(body.contains("Keep your Google Calendar updated"), "{body}");
    }
}
