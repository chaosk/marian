use chrono::{DateTime, Duration, Utc};

use crate::clock;
use crate::error::OpError;
use crate::store::{self, Event};

pub fn user_feed(conn: &rusqlite::Connection, discord_user_id: &str) -> Result<String, OpError> {
    let mut body = String::from(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Marian//Discord Scheduler//EN\r\nCALSCALE:GREGORIAN\r\nX-WR-CALNAME:Marian\r\n",
    );
    for event in events_for_person(conn, discord_user_id)? {
        body.push_str(&event_body(conn, &event)?);
    }
    body.push_str("END:VCALENDAR\r\n");
    Ok(body)
}

fn events_for_person(
    conn: &rusqlite::Connection,
    discord_user_id: &str,
) -> Result<Vec<Event>, OpError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT e.id
         FROM events e
         JOIN users u ON u.guild_id = e.guild_id AND u.discord_id = ?
         LEFT JOIN signups s ON s.event_id = e.id AND s.user_id = u.id AND s.state = 'confirmed'
         LEFT JOIN signup_options o ON o.id = s.option_id AND o.reminders_enabled = 1
         WHERE e.status IN ('posted', 'scheduled', 'archived')
           AND (e.created_by = u.id OR o.id IS NOT NULL)
         ORDER BY e.starts_at, e.id",
    )?;
    let ids = stmt.query_map([discord_user_id], |row| row.get::<_, i64>(0))?;
    let mut events = Vec::new();
    for id in ids {
        if let Some(event) = store::event(conn, id?)? {
            events.push(event);
        }
    }
    Ok(events)
}

pub fn one_event(conn: &rusqlite::Connection, event_id: i64) -> Result<String, OpError> {
    let event = store::event(conn, event_id)?.ok_or(OpError::NotFound)?;
    let mut body = String::from(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Marian//Discord Scheduler//EN\r\nCALSCALE:GREGORIAN\r\n",
    );
    body.push_str(&event_body(conn, &event)?);
    body.push_str("END:VCALENDAR\r\n");
    Ok(body)
}

fn event_body(conn: &rusqlite::Connection, event: &Event) -> Result<String, OpError> {
    let start = clock::parse_stamp(&event.starts_at)?;
    let end = start + Duration::minutes(event.duration_minutes);
    let channel = store::channel(conn, event.channel_id)?
        .map(|ch| format!("#{}", ch.name))
        .unwrap_or_default();
    let description = format!("{}\n{}", event.description.trim(), channel);
    Ok(format!(
        "BEGIN:VEVENT\r\nUID:marian-{}@marian\r\nDTSTAMP:{}\r\nDTSTART:{}\r\nDTEND:{}\r\nSUMMARY:{}\r\nDESCRIPTION:{}\r\nLOCATION:{}\r\nEND:VEVENT\r\n",
        event.public_id,
        ics_time(Utc::now()),
        ics_time(start),
        ics_time(end),
        fold_escape(&event.title),
        fold_escape(description.trim()),
        fold_escape(&channel),
    ))
}

pub fn calendar_template(event: &Event) -> Result<String, OpError> {
    let start = clock::parse_stamp(&event.starts_at)?;
    let end = start + Duration::minutes(event.duration_minutes);
    let dates = format!("{}/{}", google_time(start), google_time(end));
    Ok(format!(
        "http://www.google.com/calendar/event?action=TEMPLATE&text={}&details=&location=&dates={dates}",
        plus_encode(&event.title),
    ))
}

pub fn google_link(event: &Event, channel: &str) -> Result<String, OpError> {
    let start = clock::parse_stamp(&event.starts_at)?;
    let end = start + Duration::minutes(event.duration_minutes);
    let dates = format!("{}/{}", google_time(start), google_time(end));
    Ok(format!(
        "https://calendar.google.com/calendar/render?action=TEMPLATE&text={}&dates={}&details={}&location={}",
        pct(&event.title),
        pct(&dates),
        pct(event.description.trim()),
        pct(channel),
    ))
}

fn ics_time(dt: DateTime<Utc>) -> String {
    dt.format("%Y%m%dT%H%M%SZ").to_string()
}

fn google_time(dt: DateTime<Utc>) -> String {
    dt.format("%Y%m%dT%H%M%SZ").to_string()
}

fn fold_escape(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace(',', "\\,")
        .replace(';', "\\;");
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_personal_feed_lists_accepted_and_created_events() {
        let conn = store::open_memory().unwrap();
        conn.execute_batch(
            "
            INSERT INTO guilds (id, name, timezone) VALUES (1, 'Guild', 'UTC');
            INSERT INTO channels (id, guild_id, name) VALUES (1, 1, 'social');
            INSERT INTO users (id, guild_id, name, handle, color, discord_id) VALUES
              (1, 1, 'Nell', 'nell', '#112233', 'u1'),
              (2, 1, 'June', 'june', '#112233', 'u2');
            INSERT INTO events (public_id, guild_id, channel_id, title, starts_at, status, created_by, created_at, updated_at) VALUES
              ('01900000-0000-7000-8000-000000000001', 1, 1, 'Nell hosts', '2026-10-14T18:00:00Z', 'posted', 1, '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z'),
              ('01900000-0000-7000-8000-000000000002', 1, 1, 'June hosts accepted', '2026-10-15T18:00:00Z', 'posted', 2, '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z'),
              ('01900000-0000-7000-8000-000000000003', 1, 1, 'June hosts declined', '2026-10-16T18:00:00Z', 'posted', 2, '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z');
            INSERT INTO signup_options (id, event_id, name, emoji, reminders_enabled, position) VALUES
              (1, 2, 'Accepted', '✅', 1, 0),
              (2, 3, 'Declined', '❌', 0, 0);
            INSERT INTO signups (event_id, option_id, user_id, state, created_at) VALUES
              (2, 1, 1, 'confirmed', '2026-10-13T00:00:00Z'),
              (3, 2, 1, 'confirmed', '2026-10-13T00:00:00Z');
            ",
        )
        .unwrap();
        let feed = user_feed(&conn, "u1").unwrap();
        assert!(feed.contains("Nell hosts"), "{feed}");
        assert!(feed.contains("June hosts accepted"), "{feed}");
        assert!(!feed.contains("June hosts declined"), "{feed}");
        let token = store::ensure_calendar_token(&conn, "u1").unwrap();
        assert_eq!(token.len(), 64);
        assert_eq!(store::ensure_calendar_token(&conn, "u1").unwrap(), token);
        assert_eq!(
            store::calendar_discord_id(&conn, &token)
                .unwrap()
                .as_deref(),
            Some("u1")
        );
    }

    #[test]
    fn calendar_template_keeps_the_slash_between_dates() {
        let event = store::event(
            &{
                let conn = store::open_memory().unwrap();
                conn.execute_batch(
                    "
                    INSERT INTO guilds (id, name, timezone) VALUES (1, 'Guild', 'UTC');
                    INSERT INTO channels (id, guild_id, name) VALUES (1, 1, 'social');
                    INSERT INTO users (id, guild_id, name, handle, color) VALUES (1, 1, 'Nell', 'nell', '#112233');
                    INSERT INTO events (public_id, guild_id, channel_id, title, starts_at, duration_minutes, status, created_by, created_at, updated_at)
                    VALUES ('01900000-0000-7000-8000-000000000009', 1, 1, 'BotC #135', '2026-10-14T15:30:00Z', 105, 'posted', 1, '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z');
                    ",
                )
                .unwrap();
                conn
            },
            1,
        )
        .unwrap()
        .unwrap();
        let link = calendar_template(&event).unwrap();
        assert_eq!(
            link,
            "http://www.google.com/calendar/event?action=TEMPLATE&text=BotC+%23135&details=&location=&dates=20261014T153000Z/20261014T171500Z"
        );
    }
}

fn plus_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub fn pct(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
