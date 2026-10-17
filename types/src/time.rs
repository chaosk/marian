use std::collections::HashMap;

use chrono::{DateTime, Duration, NaiveDate, Offset, TimeZone, Utc};
use chrono_tz::{Tz, TZ_VARIANTS};
use serde::{Deserialize, Serialize};

pub fn zone_label(tz: Tz, now: DateTime<Utc>) -> String {
    let offset = offset_text(offset_seconds(tz, now));
    if tz.name() == "UTC" {
        return "UTC".into();
    }
    if fixed_gmt_alias(tz.name()) {
        return offset;
    }
    format!("{} · {offset}", place_name(tz.name()))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ZoneGroup {
    pub offset: String,
    pub zones: Vec<ZoneRow>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ZoneRow {
    pub id: String,
    pub label: String,
    pub selected: bool,
}

pub fn zone_groups(now: DateTime<Utc>, selected: &str) -> Vec<ZoneGroup> {
    let mut rows = TZ_VARIANTS
        .iter()
        .filter(|tz| tz.name() == selected || !fixed_gmt_alias(tz.name()))
        .map(|tz| (offset_seconds(*tz, now), tz.name().to_string()))
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

pub fn datetime_local(dt: DateTime<Utc>, tz: Tz) -> String {
    dt.with_timezone(&tz).format("%Y-%m-%dT%H:%M").to_string()
}

pub fn date_local(dt: DateTime<Utc>, tz: Tz) -> String {
    dt.with_timezone(&tz).format("%Y-%m-%d").to_string()
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
        if hours == 1 {
            "1 hour".into()
        } else {
            format!("{hours} hours")
        }
    } else {
        format!("{mins} min")
    }
}

/// How an event's times read in one server's zone and clock style.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    pub tz: Tz,
    pub hour_24: bool,
}

impl Clock {
    pub fn new(timezone: &str, hour_24: bool) -> Self {
        Self {
            tz: timezone.parse().unwrap_or(Tz::UTC),
            hour_24,
        }
    }

    pub fn when(&self, dt: DateTime<Utc>) -> String {
        format_when(dt, self.tz, self.hour_24)
    }

    pub fn end(&self, start: DateTime<Utc>, duration_minutes: i64) -> String {
        format_end(start, duration_minutes, self.tz, self.hour_24)
    }

    pub fn clock(&self, dt: DateTime<Utc>) -> String {
        format_clock(dt.with_timezone(&self.tz), self.hour_24)
    }

    pub fn day(&self, dt: DateTime<Utc>) -> NaiveDate {
        dt.with_timezone(&self.tz).date_naive()
    }

    pub fn input(&self, dt: DateTime<Utc>) -> String {
        datetime_local(dt, self.tz)
    }

    pub fn date_input(&self, dt: DateTime<Utc>) -> String {
        date_local(dt, self.tz)
    }

    pub fn label(&self, now: DateTime<Utc>) -> String {
        zone_label(self.tz, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_menu_names_the_city_and_groups_by_offset() {
        let now = Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap();
        let warsaw: Tz = "Europe/Warsaw".parse().unwrap();
        let label = zone_label(warsaw, now);
        assert_eq!(label, "Warsaw · UTC+1");
        let groups = zone_groups(now, "Europe/Warsaw");
        let chosen = groups
            .iter()
            .find_map(|group| group.zones.iter().find(|zone| zone.selected))
            .unwrap();
        assert_eq!(chosen.id, "Europe/Warsaw");
        assert_eq!(chosen.label, "Warsaw");
        assert!(groups.iter().any(|group| group.offset == "UTC+1"));
        let backwards = zone_groups(now, "Etc/GMT+11");
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
    }

    #[test]
    fn times_read_in_the_server_zone() {
        let clock = Clock::new("Europe/Warsaw", false);
        let start = Utc.with_ymd_and_hms(2026, 1, 15, 18, 30, 0).unwrap();
        assert_eq!(clock.when(start), "Thu, Jan 15 · 7:30 PM");
        assert_eq!(clock.end(start, 90), "9:00 PM");
        assert_eq!(clock.end(start, 300), "Fri, Jan 16 · 12:30 AM");
        assert_eq!(Clock::new("UTC", true).clock(start), "18:30");
        assert_eq!(human_duration(120), "2 hours");
        assert_eq!(human_offset(1440), "1 day");
        assert_eq!(format_rel(start, start - Duration::hours(3)), "in 3 hr");
    }
}
