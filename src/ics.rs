use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;

use crate::error::OpError;
use crate::schema::{events, signup_options, signups, memberships};
use crate::store::{self, Connection, Event};

pub fn user_feed(conn: &mut Connection, user_id: i64) -> Result<String, OpError> {
    let mut body = String::from(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Marian//Discord Scheduler//EN\r\nCALSCALE:GREGORIAN\r\nX-WR-CALNAME:Marian\r\n",
    );
    for event in events_for_person(conn, user_id)? {
        body.push_str(&event_body(conn, &event)?);
    }
    body.push_str("END:VCALENDAR\r\n");
    Ok(body)
}

pub(crate) fn events_for_person(
    conn: &mut Connection,
    user_id: i64,
) -> Result<Vec<Event>, OpError> {
    Ok(events::table
        .inner_join(memberships::table.on(memberships::guild_id.eq(events::guild_id)))
        .left_join(
            signups::table.on(signups::event_id
                .eq(events::id)
                .and(signups::membership_id.eq(memberships::id))
                .and(signups::state.eq("confirmed"))),
        )
        .left_join(
            signup_options::table.on(signup_options::id
                .eq(signups::option_id)
                .and(signup_options::reminders_enabled.eq(true))),
        )
        .filter(memberships::user_id.eq(user_id))
        .filter(events::status.eq_any(["posted", "scheduled", "archived"]))
        .filter(
            events::created_by
                .eq(memberships::id.nullable())
                .or(signup_options::id.is_not_null()),
        )
        .select(Event::as_select())
        .distinct()
        .order((events::starts_at.asc(), events::id.asc()))
        .load(conn)?)
}

fn event_body(conn: &mut Connection, event: &Event) -> Result<String, OpError> {
    let start = event.starts_at;
    let end = start + Duration::minutes(event.duration_minutes);
    let channel = store::channel(conn, event.channel_id)?
        .map(|ch| format!("#{}", ch.name))
        .unwrap_or_default();
    Ok(format!(
        "BEGIN:VEVENT\r\nUID:marian-{}@marian\r\nDTSTAMP:{}\r\nDTSTART:{}\r\nDTEND:{}\r\nSUMMARY:{}\r\nDESCRIPTION:{}\r\nLOCATION:{}\r\nEND:VEVENT\r\n",
        event.public_id,
        ics_time(Utc::now()),
        ics_time(start),
        ics_time(end),
        fold_escape(&event.title),
        fold_escape(event.description.trim()),
        fold_escape(&place(event, &channel)),
    ))
}

pub fn calendar_template(event: &Event) -> Result<String, OpError> {
    let start = event.starts_at;
    let end = start + Duration::minutes(event.duration_minutes);
    let dates = format!("{}/{}", google_time(start), google_time(end));
    Ok(format!(
        "http://www.google.com/calendar/event?action=TEMPLATE&text={}&details=&location={}&dates={dates}",
        plus_encode(&event.title),
        plus_encode(event.location.trim()),
    ))
}

pub fn google_link(event: &Event, channel: &str) -> Result<String, OpError> {
    let start = event.starts_at;
    let end = start + Duration::minutes(event.duration_minutes);
    let dates = format!("{}/{}", google_time(start), google_time(end));
    Ok(format!(
        "https://calendar.google.com/calendar/render?action=TEMPLATE&text={}&dates={}&details={}&location={}",
        pct(&event.title),
        pct(&dates),
        pct(event.description.trim()),
        pct(&place(event, channel)),
    ))
}

fn place(event: &Event, channel: &str) -> String {
    let location = event.location.trim();
    if location.is_empty() {
        channel.to_string()
    } else {
        location.to_string()
    }
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
    use crate::schema;
    use diesel::connection::SimpleConnection;

    #[test]
    fn a_personal_feed_lists_accepted_and_created_events() {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone) VALUES (1, 'Guild', 'UTC');
            INSERT INTO channels (id, guild_id, name) VALUES (1, 1, 'social');
            INSERT INTO users (discord_id) VALUES ('u1'), ('u2');
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color)
            SELECT 1, id, 1, 'Nell', 'nell', '#112233' FROM users WHERE discord_id = 'u1'
            UNION ALL
            SELECT 2, id, 1, 'June', 'june', '#112233' FROM users WHERE discord_id = 'u2';
            INSERT INTO events (public_id, guild_id, channel_id, title, starts_at, status, created_by, created_at, updated_at) VALUES
              ('01900000-0000-7000-8000-000000000001', 1, 1, 'Nell hosts', '2026-10-14T18:00:00Z', 'posted', 1, '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z'),
              ('01900000-0000-7000-8000-000000000002', 1, 1, 'June hosts accepted', '2026-10-15T18:00:00Z', 'posted', 2, '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z'),
              ('01900000-0000-7000-8000-000000000003', 1, 1, 'June hosts declined', '2026-10-16T18:00:00Z', 'posted', 2, '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z');
            INSERT INTO signup_options (id, event_id, name, emoji, reminders_enabled, position) VALUES
              (1, 2, 'Accepted', '✅', true, 0),
              (2, 3, 'Declined', '❌', false, 0);
            INSERT INTO signups (event_id, option_id, membership_id, state, created_at) VALUES
              (2, 1, 1, 'confirmed', '2026-10-13T00:00:00Z'),
              (3, 2, 1, 'confirmed', '2026-10-13T00:00:00Z');
            ",
        )
        .unwrap();
        let person: i64 = schema::users::table
            .filter(schema::users::discord_id.eq("u1"))
            .select(schema::users::id)
            .first(&mut conn)
            .unwrap();
        let feed = user_feed(&mut conn, person).unwrap();
        assert!(feed.contains("Nell hosts"), "{feed}");
        assert!(feed.contains("June hosts accepted"), "{feed}");
        assert!(!feed.contains("June hosts declined"), "{feed}");
        let token = store::ensure_calendar_token(&mut conn, person).unwrap();
        assert_eq!(token.len(), 64);
        assert_eq!(store::ensure_calendar_token(&mut conn, person).unwrap(), token);
        assert_eq!(
            store::calendar_user_id(&mut conn, &token).unwrap(),
            Some(person)
        );
    }

    #[test]
    fn calendar_template_keeps_the_slash_between_dates() {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone) VALUES (1, 'Guild', 'UTC');
            INSERT INTO channels (id, guild_id, name) VALUES (1, 1, 'social');
            INSERT INTO users (discord_id) VALUES ('nell');
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color)
            SELECT 1, id, 1, 'Nell', 'nell', '#112233' FROM users WHERE discord_id = 'nell';
            INSERT INTO events (public_id, guild_id, channel_id, title, starts_at, duration_minutes, status, created_by, created_at, updated_at)
            VALUES ('01900000-0000-7000-8000-000000000009', 1, 1, 'BotC #135', '2026-10-14T15:30:00Z', 105, 'posted', 1, '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z');
            ",
        )
        .unwrap();
        let event = store::event(&mut conn, 1).unwrap().unwrap();
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
