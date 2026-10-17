//! How times and counts read on the page, in the server's zone and clock style.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use marian_types as dto;
pub use marian_types::labels::{cadence_label, color_name, spots, status_label, weekday_name};
pub use marian_types::time::{format_day, format_long_date, format_rel, human_duration, human_offset, Clock};

pub fn clock_of(server: &dto::Server) -> Clock {
    Clock::new(&server.timezone, server.hour_24)
}

pub fn now() -> DateTime<Utc> {
    Utc::now()
}

/// "7:00 PM – 9:00 PM (2 hours)".
pub fn time_line(clock: &Clock, start: DateTime<Utc>, minutes: i64) -> String {
    format!(
        "{} – {} ({})",
        clock.clock(start),
        clock.end(start, minutes),
        human_duration(minutes)
    )
}

/// Monday to Sunday around `now`, in the server's zone.
pub fn week_of(clock: &Clock, now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    use chrono::Datelike;
    let today = clock.day(now);
    let monday = today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
    (midnight(clock, monday), midnight(clock, monday + Duration::days(7)))
}

fn midnight(clock: &Clock, date: NaiveDate) -> DateTime<Utc> {
    date.and_hms_opt(0, 0, 0)
        .and_then(|naive| naive.and_local_timezone(clock.tz).earliest())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|| date.and_hms_opt(0, 0, 0).unwrap().and_utc())
}

/// "3/5 +2 waiting" for an event card.
pub fn spot_count(spot: &dto::Spot) -> String {
    let base = spots(spot.confirmed, spot.capacity);
    if spot.waiting > 0 {
        format!("{base} +{} waiting", spot.waiting)
    } else {
        base
    }
}

/// "1–25 of 40", or nothing for an empty list.
pub fn window<T>(page: &dto::Paged<T>) -> String {
    if page.total == 0 {
        return String::new();
    }
    let start = (page.page - 1) * page.per_page + 1;
    let end = (start - 1 + page.items.len() as u32).max(start);
    format!("{start}–{end} of {}", page.total)
}

pub fn reminder_offset(minutes: i64) -> String {
    format!("{} before", human_offset(minutes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn a_week_runs_monday_to_monday_in_the_server_zone() {
        let clock = Clock::new("Europe/Warsaw", true);
        let now = Utc.with_ymd_and_hms(2026, 10, 17, 12, 0, 0).unwrap();
        let (from, to) = week_of(&clock, now);
        assert_eq!(from, Utc.with_ymd_and_hms(2026, 10, 11, 22, 0, 0).unwrap());
        assert_eq!(to, Utc.with_ymd_and_hms(2026, 10, 18, 22, 0, 0).unwrap());
    }

    #[test]
    fn counts_read_as_spots() {
        let spot = dto::Spot {
            emoji: "✅".into(),
            name: "Accepted".into(),
            capacity: Some(5),
            confirmed: 5,
            waiting: 2,
        };
        assert_eq!(spot_count(&spot), "5/5 +2 waiting");
        let page = dto::Paged {
            items: vec![(); 15],
            total: 40,
            page: 2,
            pages: 2,
            per_page: 25,
        };
        assert_eq!(window(&page), "26–40 of 40");
        assert_eq!(time_line(&clock_utc(), Utc.with_ymd_and_hms(2026, 1, 1, 19, 0, 0).unwrap(), 120), "19:00 – 21:00 (2 hours)");
    }

    fn clock_utc() -> Clock {
        Clock::new("UTC", true)
    }
}
