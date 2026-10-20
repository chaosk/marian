use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc, Weekday};
use chrono_tz::Tz;

use crate::clock;

/// Read a start time the way someone would type it in Discord.
pub fn parse_when(input: &str, now: DateTime<Utc>, tz: Tz) -> Option<DateTime<Utc>> {
    let text = input.trim().to_lowercase();
    if text.is_empty() {
        return None;
    }
    if let Some(rest) = text.strip_prefix("in ") {
        return parse_relative(rest.trim(), now);
    }
    if let Some(dt) = parse_iso(&text, tz) {
        return Some(dt);
    }

    let local_now = now.with_timezone(&tz);
    let mut date = local_now.date_naive();
    let mut rest = text.as_str();

    if let Some(tail) = rest.strip_prefix("tomorrow") {
        date = clock::shift_days(date, 1);
        rest = tail.trim();
        rest = rest.strip_prefix("at").unwrap_or(rest).trim();
    } else if let Some(tail) = rest.strip_prefix("tonight") {
        date = local_now.date_naive();
        rest = tail.trim();
        rest = rest.strip_prefix("at").unwrap_or(rest).trim();
        if rest.is_empty() {
            rest = "8pm";
        }
    } else if let Some(tail) = rest.strip_prefix("today") {
        rest = tail.trim();
        rest = rest.strip_prefix("at").unwrap_or(rest).trim();
    } else if let Some(tail) = rest.strip_prefix("next ") {
        let (weekday, tail) = weekday_prefix(tail.trim())?;
        date = next_weekday(local_now.date_naive(), weekday, true);
        rest = tail;
    } else if let Some((weekday, tail)) = weekday_prefix(rest) {
        date = next_weekday(local_now.date_naive(), weekday, false);
        rest = tail;
    } else if let Some((parsed, tail)) = month_day(rest, local_now.date_naive()) {
        date = parsed;
        rest = tail;
    }

    rest = rest.trim().trim_start_matches("at").trim();
    let (hour, minute) = if rest.is_empty() {
        (19, 0)
    } else {
        parse_clock(rest)?
    };

    let mut when = clock::at_local(date, hour, minute, tz);
    if when <= now && text_starts_with_weekday(&text) {
        let weekday = date.weekday();
        let bumped = next_weekday(clock::shift_days(date, 1), weekday, false);
        when = clock::at_local(bumped, hour, minute, tz);
    }
    if when <= now && (text.starts_with("today") || text.starts_with("tonight")) {
        return None;
    }
    Some(when)
}

fn text_starts_with_weekday(text: &str) -> bool {
    weekday_prefix(text).is_some()
}

fn parse_relative(rest: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let mut parts = rest.split_whitespace();
    let n: i64 = parts.next()?.parse().ok()?;
    let unit = parts.next().unwrap_or("minutes");
    if n < 0 {
        return None;
    }
    let delta = if unit.starts_with("hour") {
        Duration::hours(n)
    } else if unit.starts_with("min") {
        Duration::minutes(n)
    } else if unit.starts_with("day") {
        Duration::days(n)
    } else {
        return None;
    };
    Some(now + delta)
}

fn parse_iso(text: &str, tz: Tz) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(text) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M") {
        return naive
            .and_local_timezone(tz)
            .earliest()
            .map(|dt| dt.with_timezone(&Utc));
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M") {
        return naive
            .and_local_timezone(tz)
            .earliest()
            .map(|dt| dt.with_timezone(&Utc));
    }
    None
}

fn weekday_prefix(text: &str) -> Option<(Weekday, &str)> {
    const DAYS: &[(&str, Weekday)] = &[
        ("monday", Weekday::Mon),
        ("tuesday", Weekday::Tue),
        ("wednesday", Weekday::Wed),
        ("thursday", Weekday::Thu),
        ("friday", Weekday::Fri),
        ("saturday", Weekday::Sat),
        ("sunday", Weekday::Sun),
        ("mon", Weekday::Mon),
        ("tue", Weekday::Tue),
        ("wed", Weekday::Wed),
        ("thu", Weekday::Thu),
        ("fri", Weekday::Fri),
        ("sat", Weekday::Sat),
        ("sun", Weekday::Sun),
    ];
    for (name, weekday) in DAYS {
        if let Some(tail) = text.strip_prefix(name) {
            if tail.is_empty() || tail.starts_with(' ') || tail.starts_with(',') {
                let tail = tail.trim_start_matches([',', ' ']);
                let tail = tail.strip_prefix("at").unwrap_or(tail).trim();
                return Some((*weekday, tail));
            }
        }
    }
    None
}

fn next_weekday(from: NaiveDate, target: Weekday, strict_next: bool) -> NaiveDate {
    let mut date = if strict_next {
        clock::shift_days(from, 1)
    } else {
        from
    };
    if strict_next {
        // "next friday" means the friday after the upcoming one when today is not that day,
        // and the friday of next week when today is that day. We treat it as strictly after today,
        // then if the phrase was "next", callers already passed strict_next.
    }
    for _ in 0..8 {
        if date.weekday() == target {
            return date;
        }
        date = date.succ_opt().unwrap_or(date);
    }
    from
}

fn month_day<'a>(text: &'a str, today: NaiveDate) -> Option<(NaiveDate, &'a str)> {
    const MONTHS: &[(&str, u32)] = &[
        ("january", 1),
        ("february", 2),
        ("march", 3),
        ("april", 4),
        ("may", 5),
        ("june", 6),
        ("july", 7),
        ("august", 8),
        ("september", 9),
        ("october", 10),
        ("november", 11),
        ("december", 12),
        ("jan", 1),
        ("feb", 2),
        ("mar", 3),
        ("apr", 4),
        ("jun", 6),
        ("jul", 7),
        ("aug", 8),
        ("sep", 9),
        ("oct", 10),
        ("nov", 11),
        ("dec", 12),
    ];
    for (name, month) in MONTHS {
        if let Some(tail) = text.strip_prefix(name) {
            let tail = tail.trim().trim_start_matches(['.', ',']);
            let mut bits = tail.splitn(2, char::is_whitespace);
            let day_token = bits.next()?.trim_end_matches([',', '.']);
            let day: u32 = day_token.parse().ok()?;
            let rest = bits.next().unwrap_or("").trim();
            let rest = rest.strip_prefix("at").unwrap_or(rest).trim();
            let mut year = today.year();
            let candidate = NaiveDate::from_ymd_opt(year, *month, day)?;
            if candidate < today {
                year += 1;
            }
            let date = NaiveDate::from_ymd_opt(year, *month, day)?;
            return Some((date, rest));
        }
    }
    None
}

fn parse_clock(text: &str) -> Option<(u32, u32)> {
    let text = text.trim().trim_end_matches('.');
    let (core, ampm) = if let Some(core) = text.strip_suffix("am") {
        (core.trim(), Some(false))
    } else if let Some(core) = text.strip_suffix("a.m") {
        (core.trim(), Some(false))
    } else if let Some(core) = text.strip_suffix("pm") {
        (core.trim(), Some(true))
    } else if let Some(core) = text.strip_suffix("p.m") {
        (core.trim(), Some(true))
    } else {
        (text, None)
    };
    let core = core.trim();
    let (hour_s, minute_s) = if let Some((h, m)) = core.split_once(':') {
        (h, m)
    } else {
        (core, "0")
    };
    let mut hour: u32 = hour_s.trim().parse().ok()?;
    let minute: u32 = minute_s.trim().parse().ok()?;
    if minute > 59 || hour > 24 {
        return None;
    }
    match ampm {
        Some(false) => {
            if hour == 12 {
                hour = 0;
            }
            if hour > 12 {
                return None;
            }
        }
        Some(true) => {
            if hour == 12 {
                hour = 12;
            } else if hour < 12 {
                hour += 12;
            } else {
                return None;
            }
        }
        None => {
            if hour == 24 && minute == 0 {
                hour = 0;
            }
            if hour > 23 {
                return None;
            }
        }
    }
    Some((hour, minute))
}

pub fn tokenize(input: &str) -> Vec<(Option<String>, String)> {
    let mut out = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let start = i;
        if chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '-' {
            let mut j = i;
            while j < chars.len()
                && (chars[j].is_ascii_alphanumeric() || chars[j] == '_' || chars[j] == '-')
            {
                j += 1;
            }
            if j < chars.len() && chars[j] == ':' {
                let key: String = chars[start..j].iter().collect();
                i = j + 1;
                let value = read_value(&chars, &mut i);
                out.push((Some(key.to_lowercase()), value));
                continue;
            }
        }
        i = start;
        let value = read_value(&chars, &mut i);
        if !value.is_empty() {
            out.push((None, value));
        }
    }
    out
}

fn read_value(chars: &[char], i: &mut usize) -> String {
    if *i >= chars.len() {
        return String::new();
    }
    if chars[*i] == '"' {
        *i += 1;
        let mut value = String::new();
        while *i < chars.len() && chars[*i] != '"' {
            value.push(chars[*i]);
            *i += 1;
        }
        if *i < chars.len() && chars[*i] == '"' {
            *i += 1;
        }
        return value;
    }
    let mut value = String::new();
    while *i < chars.len() && !chars[*i].is_whitespace() {
        value.push(chars[*i]);
        *i += 1;
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike};

    fn la() -> Tz {
        "America/Los_Angeles".parse().unwrap()
    }

    fn now() -> DateTime<Utc> {
        // Wednesday 14 Oct 2026, 11:00 Pacific (UTC-7) = 18:00 UTC.
        Utc.with_ymd_and_hms(2026, 10, 14, 18, 0, 0).unwrap()
    }

    #[test]
    fn relative_and_tomorrow() {
        let tz = la();
        let when = parse_when("in 2 hours", now(), tz).unwrap();
        assert_eq!(when, now() + Duration::hours(2));
        let tomorrow = parse_when("tomorrow at 7pm", now(), tz).unwrap();
        let local = tomorrow.with_timezone(&tz);
        assert_eq!(local.date_naive().to_string(), "2026-10-15");
        assert_eq!(local.hour(), 19);
    }

    #[test]
    fn weekday_and_month() {
        let tz = la();
        let friday = parse_when("friday at 9am", now(), tz).unwrap();
        let local = friday.with_timezone(&tz);
        assert_eq!(local.weekday(), Weekday::Fri);
        assert_eq!(local.hour(), 9);
        let dated = parse_when("oct 20 at 8:00pm", now(), tz).unwrap();
        let local = dated.with_timezone(&tz);
        assert_eq!(local.date_naive().to_string(), "2026-10-20");
        assert_eq!(local.hour(), 20);
    }

    #[test]
    fn tokenizer_keeps_quoted_values() {
        let tokens = tokenize(r#"title:"Shore cleanup" start:"tomorrow at 4pm" channel:social"#);
        assert_eq!(tokens[0], (Some("title".into()), "Shore cleanup".into()));
        assert_eq!(tokens[1].1, "tomorrow at 4pm");
        assert_eq!(tokens[2], (Some("channel".into()), "social".into()));
    }
}
