use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, SecondsFormat, TimeZone, Utc, Weekday};
use chrono_tz::Tz;

use crate::error::OpError;

pub const ZONES: &[&str] = &[
    "America/Los_Angeles",
    "America/Denver",
    "America/Chicago",
    "America/New_York",
    "America/Sao_Paulo",
    "Europe/London",
    "Europe/Paris",
    "Europe/Berlin",
    "Europe/Warsaw",
    "Africa/Lagos",
    "Asia/Kolkata",
    "Asia/Singapore",
    "Asia/Tokyo",
    "Australia/Sydney",
    "Pacific/Auckland",
    "UTC",
];

pub use marian_types::labels::{cadence_label, weekday_name, weekday_short};
pub use marian_types::time::{
    date_local, datetime_local, format_clock, format_day, format_end, format_long_date,
    format_rel, format_when, human_duration, human_offset, zone_groups, ZoneGroup, ZoneRow,
};

pub fn zone(name: &str) -> Result<Tz, OpError> {
    name.parse()
        .map_err(|_| OpError::from(crate::msg!("err-pick-zone")))
}

pub fn zone_label(tz: Tz) -> String {
    marian_types::time::zone_label(tz, Utc::now())
}

pub fn stamp(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub fn parse_stamp(value: &str) -> Result<DateTime<Utc>, OpError> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| OpError::from(crate::msg!("err-stored-time")))
}

pub fn shift_days(date: NaiveDate, days: i64) -> NaiveDate {
    if days >= 0 {
        date + chrono::Days::new(days as u64)
    } else {
        date - chrono::Days::new((-days) as u64)
    }
}

pub fn zoned(now: DateTime<Utc>, tz: Tz, day_offset: i64, hour: u32, minute: u32) -> DateTime<Utc> {
    let date = shift_days(now.with_timezone(&tz).date_naive(), day_offset);
    at_local(date, hour, minute, tz)
}

pub fn next_weekday_at(
    now: DateTime<Utc>,
    tz: Tz,
    weekday: Weekday,
    hour: u32,
    minute: u32,
    min_days: i64,
) -> DateTime<Utc> {
    let mut date = shift_days(now.with_timezone(&tz).date_naive(), min_days);
    for _ in 0..14 {
        if date.weekday() == weekday {
            break;
        }
        date = date.succ_opt().unwrap_or(date);
    }
    at_local(date, hour, minute, tz)
}

pub fn at_local(date: NaiveDate, hour: u32, minute: u32, tz: Tz) -> DateTime<Utc> {
    let time = NaiveTime::from_hms_opt(hour, minute, 0).unwrap_or_else(|| NaiveTime::MIN);
    date.and_time(time)
        .and_local_timezone(tz)
        .earliest()
        .unwrap_or_else(|| tz.from_utc_datetime(&date.and_time(time)))
        .with_timezone(&Utc)
}

pub fn anchor_at_clock(raw: &str, tz: Tz) -> Result<DateTime<Utc>, OpError> {
    let time = NaiveTime::parse_from_str(raw.trim(), "%H:%M")
        .map_err(|_| OpError::from(crate::msg!("err-clock-time")))?;
    let date = Utc::now().with_timezone(&tz).date_naive();
    date.and_time(time)
        .and_local_timezone(tz)
        .earliest()
        .map(|dt| dt.with_timezone(&Utc))
        .ok_or_else(|| OpError::from(crate::msg!("err-clock-missing")))
}

pub fn parse_local(value: &str, tz: Tz) -> Result<DateTime<Utc>, OpError> {
    let value = value.trim();
    let naive = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S"))
        .map_err(|_| OpError::from(crate::msg!("err-date-and-time")))?;
    naive
        .and_local_timezone(tz)
        .earliest()
        .map(|dt| dt.with_timezone(&Utc))
        .ok_or_else(|| OpError::from(crate::msg!("err-local-missing")))
}

pub fn parse_date(value: &str, tz: Tz) -> Result<DateTime<Utc>, OpError> {
    let date = chrono::NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
        .map_err(|_| OpError::from(crate::msg!("err-calendar-date")))?;
    at_local_result(date, tz)
}

fn at_local_result(date: NaiveDate, tz: Tz) -> Result<DateTime<Utc>, OpError> {
    let time = NaiveTime::from_hms_opt(23, 59, 0).unwrap();
    date.and_time(time)
        .and_local_timezone(tz)
        .earliest()
        .map(|dt| dt.with_timezone(&Utc))
        .ok_or_else(|| OpError::from(crate::msg!("err-date-missing")))
}

pub fn occurrence_start(anchor: DateTime<Utc>, index: i64, cadence: &str, tz: Tz) -> DateTime<Utc> {
    let delta = (index - 1).max(0);
    let local = anchor.with_timezone(&tz);
    let time = local.time();
    let date = match cadence {
        "daily" => shift_days(local.date_naive(), delta),
        "weekly" => shift_days(local.date_naive(), delta * 7),
        "biweekly" => shift_days(local.date_naive(), delta * 14),
        "monthly" => local
            .date_naive()
            .checked_add_months(chrono::Months::new(delta as u32))
            .unwrap_or(local.date_naive()),
        _ => local.date_naive(),
    };
    date.and_time(time)
        .and_local_timezone(tz)
        .earliest()
        .unwrap_or_else(|| tz.from_utc_datetime(&date.and_time(time)))
        .with_timezone(&Utc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike};

    #[test]
    fn weekly_step_keeps_local_hour_across_dst() {
        let tz: Tz = "America/Los_Angeles".parse().unwrap();
        let anchor = zoned(
            Utc.with_ymd_and_hms(2026, 3, 5, 12, 0, 0).unwrap(),
            tz,
            0,
            19,
            0,
        );
        let next = occurrence_start(anchor, 2, "weekly", tz);
        let local = next.with_timezone(&tz);
        assert_eq!(local.date_naive().to_string(), "2026-03-12");
        assert_eq!(local.hour(), 19);
        assert_eq!(local.minute(), 0);
    }

    #[test]
    fn zone_menu_names_the_city_and_groups_by_offset() {
        let warsaw: Tz = "Europe/Warsaw".parse().unwrap();
        let label = zone_label(warsaw);
        assert!(label.starts_with("Warsaw"), "{label}");
        assert!(label.contains("UTC"), "{label}");
        let groups = zone_groups(
            Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap(),
            "Europe/Warsaw",
        );
        let chosen = groups
            .iter()
            .find_map(|group| group.zones.iter().find(|zone| zone.selected))
            .unwrap();
        assert_eq!(chosen.id, "Europe/Warsaw");
        assert_eq!(chosen.label, "Warsaw");
        assert!(groups.iter().any(|group| group.offset == "UTC+1"));
        assert!(groups
            .iter()
            .any(|group| group.zones.iter().any(|zone| zone.id == "Asia/Tokyo")));
        let backwards = zone_groups(
            Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap(),
            "Etc/GMT+11",
        );
        assert!(backwards.iter().all(|group| {
            group
                .zones
                .iter()
                .all(|zone| zone.label != "GMT+11" && zone.label != "GMT-11")
        }));
        let chosen = backwards
            .iter()
            .find_map(|group| {
                group
                    .zones
                    .iter()
                    .find(|zone| zone.selected)
                    .map(|zone| (group.offset.as_str(), zone))
            })
            .unwrap();
        assert_eq!(chosen.0, "UTC-11");
        assert_eq!(chosen.1.label, "UTC-11");
        assert_eq!(chosen.1.id, "Etc/GMT+11");
    }

    #[test]
    fn month_end_clamps() {
        let tz = Tz::UTC;
        let anchor = Utc.with_ymd_and_hms(2026, 1, 31, 18, 0, 0).unwrap();
        let next = occurrence_start(anchor, 2, "monthly", tz);
        assert_eq!(
            next.with_timezone(&tz).date_naive().to_string(),
            "2026-02-28"
        );
    }
}
