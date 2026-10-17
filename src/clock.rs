use std::collections::HashMap;

use chrono::{
    DateTime, Datelike, Duration, NaiveDate, NaiveTime, Offset, SecondsFormat, TimeZone, Utc,
    Weekday,
};
use chrono_tz::{Tz, TZ_VARIANTS};
use serde::Serialize;

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

const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

pub fn weekday_name(day: u8) -> &'static str {
    WEEKDAYS.get(day as usize).copied().unwrap_or("Monday")
}

pub fn zone(name: &str) -> Result<Tz, OpError> {
    name.parse()
        .map_err(|_| OpError::message("Pick a time zone from the list."))
}

pub fn zone_label(tz: Tz) -> String {
    let offset = offset_text(offset_seconds(tz, Utc::now()));
    if tz.name() == "UTC" {
        return "UTC".into();
    }
    if fixed_gmt_alias(tz.name()) {
        return offset;
    }
    format!("{} · {offset}", place_name(tz.name()))
}

#[derive(Serialize)]
pub struct ZoneGroup {
    pub offset: String,
    pub zones: Vec<ZoneRow>,
}

#[derive(Serialize)]
pub struct ZoneRow {
    pub id: String,
    pub label: String,
    pub selected: bool,
}

pub fn zone_groups(now: DateTime<Utc>, selected: &str) -> Vec<ZoneGroup> {
    let mut rows = TZ_VARIANTS
        .iter()
        .filter(|tz| tz.name() == selected || !fixed_gmt_alias(tz.name()))
        .map(|tz| {
            let id = tz.name().to_string();
            let seconds = offset_seconds(*tz, now);
            (seconds, id)
        })
        .collect::<Vec<_>>();
    if !rows.iter().any(|(_, id)| id == selected) && !selected.is_empty() {
        rows.push((0, selected.to_string()));
    }
    rows.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| place_name(&a.1).cmp(&place_name(&b.1)))
    });
    let mut groups: Vec<ZoneGroup> = Vec::new();
    for (seconds, id) in rows {
        let offset = if id == selected && !TZ_VARIANTS.iter().any(|tz| tz.name() == id) {
            "Current".into()
        } else {
            offset_text(seconds)
        };
        if groups
            .last()
            .map(|group| group.offset != offset)
            .unwrap_or(true)
        {
            groups.push(ZoneGroup {
                offset,
                zones: Vec::new(),
            });
        }
        let label = if fixed_gmt_alias(&id) {
            offset_text(seconds)
        } else {
            place_name(&id)
        };
        groups.last_mut().unwrap().zones.push(ZoneRow {
            label,
            selected: id == selected,
            id,
        });
    }
    for group in &mut groups {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for zone in &group.zones {
            *counts.entry(zone.label.clone()).or_insert(0) += 1;
        }
        for zone in &mut group.zones {
            if counts.get(&zone.label).copied().unwrap_or(0) > 1 {
                zone.label = detailed_place(&zone.id);
            }
        }
    }
    groups
}

fn offset_seconds(tz: Tz, now: DateTime<Utc>) -> i32 {
    tz.offset_from_utc_datetime(&now.naive_utc())
        .fix()
        .local_minus_utc()
}

fn offset_text(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let seconds = seconds.abs();
    let hours = seconds / 3600;
    let mins = (seconds % 3600) / 60;
    if mins == 0 {
        format!("UTC{sign}{hours}")
    } else {
        format!("UTC{sign}{hours}:{mins:02}")
    }
}

fn fixed_gmt_alias(id: &str) -> bool {
    let name = id.rsplit('/').next().unwrap_or(id);
    name == "GMT" || name == "GMT0" || name.starts_with("GMT+") || name.starts_with("GMT-")
}

fn place_name(id: &str) -> String {
    id.rsplit('/').next().unwrap_or(id).replace('_', " ")
}

fn detailed_place(id: &str) -> String {
    let mut parts: Vec<&str> = id.split('/').collect();
    if parts.len() < 2 {
        return id.replace('_', " ");
    }
    let city = parts.pop().unwrap().replace('_', " ");
    let parent = parts.pop().unwrap().replace('_', " ");
    format!("{city}, {parent}")
}

pub fn stamp(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub fn parse_stamp(value: &str) -> Result<DateTime<Utc>, OpError> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| OpError::message("A stored time was unreadable."))
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

pub fn datetime_local(dt: DateTime<Utc>, tz: Tz) -> String {
    dt.with_timezone(&tz).format("%Y-%m-%dT%H:%M").to_string()
}

pub fn date_local(dt: DateTime<Utc>, tz: Tz) -> String {
    dt.with_timezone(&tz).format("%Y-%m-%d").to_string()
}

pub fn anchor_at_clock(raw: &str, tz: Tz) -> Result<DateTime<Utc>, OpError> {
    let time = NaiveTime::parse_from_str(raw.trim(), "%H:%M")
        .map_err(|_| OpError::message("Use a clock time like 19:00."))?;
    let date = Utc::now().with_timezone(&tz).date_naive();
    date.and_time(time)
        .and_local_timezone(tz)
        .earliest()
        .map(|dt| dt.with_timezone(&Utc))
        .ok_or_else(|| OpError::message("That clock time doesn't exist in this zone."))
}

pub fn parse_local(value: &str, tz: Tz) -> Result<DateTime<Utc>, OpError> {
    let value = value.trim();
    let naive = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S"))
        .map_err(|_| OpError::message("Use a date and a time."))?;
    naive
        .and_local_timezone(tz)
        .earliest()
        .map(|dt| dt.with_timezone(&Utc))
        .ok_or_else(|| OpError::message("That local time doesn't exist in this zone."))
}

pub fn parse_date(value: &str, tz: Tz) -> Result<DateTime<Utc>, OpError> {
    let date = chrono::NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
        .map_err(|_| OpError::message("Use a calendar date."))?;
    at_local_result(date, tz)
}

fn at_local_result(date: NaiveDate, tz: Tz) -> Result<DateTime<Utc>, OpError> {
    let time = NaiveTime::from_hms_opt(23, 59, 0).unwrap();
    date.and_time(time)
        .and_local_timezone(tz)
        .earliest()
        .map(|dt| dt.with_timezone(&Utc))
        .ok_or_else(|| OpError::message("That date doesn't land in this zone."))
}

pub fn format_clock<T: TimeZone>(local: DateTime<T>, hour24: bool) -> String
where
    T::Offset: std::fmt::Display,
{
    if hour24 {
        local.format("%H:%M").to_string()
    } else {
        let hour = local.format("%I").to_string();
        let hour = hour.trim_start_matches('0');
        let hour = if hour.is_empty() { "0" } else { hour };
        format!("{hour}:{} {}", local.format("%M"), local.format("%p"))
    }
}

pub fn format_long_date(date: NaiveDate) -> String {
    let day = date.format("%d").to_string();
    let day = day.trim_start_matches('0');
    format!("{} {day}, {}", date.format("%A, %B"), date.format("%Y"))
}

pub fn format_day(date: NaiveDate) -> String {
    let day = date.format("%d").to_string();
    let day = day.trim_start_matches('0');
    format!("{} {day}", date.format("%a, %b"))
}

pub fn format_when(dt: DateTime<Utc>, tz: Tz, hour24: bool) -> String {
    let local = dt.with_timezone(&tz);
    format!(
        "{} · {}",
        format_day(local.date_naive()),
        format_clock(local, hour24)
    )
}

pub fn format_end(start: DateTime<Utc>, duration_minutes: i64, tz: Tz, hour24: bool) -> String {
    let end = start + Duration::minutes(duration_minutes);
    let local = end.with_timezone(&tz);
    let start_local = start.with_timezone(&tz);
    if local.date_naive() == start_local.date_naive() {
        format_clock(local, hour24)
    } else {
        format!(
            "{} · {}",
            format_day(local.date_naive()),
            format_clock(local, hour24)
        )
    }
}

pub fn format_rel(target: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (target - now).num_seconds();
    if secs.abs() < 45 {
        return "now".into();
    }
    let future = secs > 0;
    let mins = secs.abs() / 60;
    let text = if mins < 60 {
        format!("{mins} min")
    } else if mins < 60 * 36 {
        let hours = mins / 60;
        if hours == 1 {
            "1 hr".into()
        } else {
            format!("{hours} hr")
        }
    } else {
        let days = mins / (60 * 24);
        if days == 1 {
            "1 day".into()
        } else {
            format!("{days} days")
        }
    };
    if future {
        format!("in {text}")
    } else {
        format!("{text} ago")
    }
}

pub fn human_offset(mins: i64) -> String {
    if mins % 1440 == 0 && mins >= 1440 {
        let days = mins / 1440;
        if days == 1 {
            "1 day".into()
        } else {
            format!("{days} days")
        }
    } else if mins % 60 == 0 && mins >= 60 {
        let hours = mins / 60;
        if hours == 1 {
            "1 hour".into()
        } else {
            format!("{hours} hours")
        }
    } else if mins == 1 {
        "1 minute".into()
    } else {
        format!("{mins} minutes")
    }
}

pub fn human_duration(mins: i64) -> String {
    if mins % 60 == 0 && mins >= 60 {
        let hours = mins / 60;
        let extra = mins % 60;
        if extra == 0 {
            if hours == 1 {
                "1 hour".into()
            } else {
                format!("{hours} hours")
            }
        } else {
            format!("{hours} hr {extra} min")
        }
    } else {
        format!("{mins} min")
    }
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

pub fn cadence_label(cadence: &str) -> &'static str {
    match cadence {
        "daily" => "Daily",
        "once" => "Once",
        "weekly" => "Weekly",
        "biweekly" => "Every two weeks",
        "monthly" => "Monthly",
        _ => "Repeating",
    }
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
