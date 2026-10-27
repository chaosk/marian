use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc, Weekday};
use chrono_tz::Tz;

use crate::clock;

/// Read a start time the way someone would type it in Discord, in English or Polish.
pub fn parse_when(input: &str, now: DateTime<Utc>, tz: Tz) -> Option<DateTime<Utc>> {
    let text = normalize(input);
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

    if let Some(tail) = rest.strip_prefix("overmorrow") {
        date = clock::shift_days(date, 2);
        rest = tail.trim();
        rest = rest.strip_prefix("at").unwrap_or(rest).trim();
    } else if let Some(tail) = rest.strip_prefix("tomorrow") {
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

/// Lower case, with Polish words swapped for the English ones the parser reads:
/// "jutro o 19" reads as "tomorrow at 19", "w piątek o 18:30" as "friday at
/// 18:30", "za 2 godziny" as "in 2 hours", "14 października" as "oct 14".
pub fn normalize(input: &str) -> String {
    let words: Vec<String> = input
        .trim()
        .to_lowercase()
        .split_whitespace()
        .map(|word| word.trim_end_matches(',').to_string())
        .filter(|word| !word.is_empty())
        .collect();
    let mut out: Vec<String> = Vec::new();
    for word in &words {
        let plain = fold(word);
        let last = out.last().map(String::as_str);
        let mapped: Option<&str> = match plain.as_str() {
            "za" => Some("in"),
            "o" | "godz" | "godz." | "godzinie" if last == Some("at") => None,
            "o" | "godz" | "godz." => Some("at"),
            "w" | "we" | "na" => None,
            "dzis" | "dzisiaj" => Some("today"),
            "jutro" => Some("tomorrow"),
            "pojutrze" => Some("overmorrow"),
            "wieczorem" => Some("8pm"),
            "rano" => Some("9am"),
            "teraz" => Some("now"),
            "nastepny" | "nastepna" | "nastepne" | "nastepnym" | "nastepnej" | "przyszly"
            | "przyszla" | "przyszle" | "przyszlym" | "przyszlej" => Some("next"),
            _ => None,
        };
        if let Some(mapped) = mapped {
            out.push(mapped.to_string());
            continue;
        }
        if matches!(plain.as_str(), "o" | "godz" | "godz." | "godzinie" | "w" | "we" | "na") {
            continue;
        }
        if let Some(day) = polish_weekday(&plain) {
            out.push(day.to_string());
            continue;
        }
        if let Some(unit) = polish_unit(&plain) {
            // "za godzinę" is one hour.
            if last == Some("in") {
                out.push("1".into());
            }
            out.push(unit.to_string());
            continue;
        }
        if let Some(month) = polish_month(&plain) {
            // Polish puts the day first: "14 października" reads as "oct 14".
            match out.pop() {
                Some(day) if day.chars().all(|ch| ch.is_ascii_digit()) => {
                    out.push(month.to_string());
                    out.push(day);
                }
                other => {
                    out.extend(other);
                    out.push(month.to_string());
                }
            }
            continue;
        }
        if let Some(dated) = dotted_date(&plain, last) {
            out.extend(dated);
            continue;
        }
        if last == Some("at") {
            // "o 19.30" is a clock time.
            if let Some((hour, minute)) = plain.split_once('.') {
                if hour.chars().all(|ch| ch.is_ascii_digit()) && minute.len() == 2 && minute.chars().all(|ch| ch.is_ascii_digit()) {
                    out.push(format!("{hour}:{minute}"));
                    continue;
                }
            }
        }
        out.push(word.clone());
    }
    out.join(" ")
}

/// Polish letters without their marks, so "środa" and "sroda" read the same.
fn fold(word: &str) -> String {
    word.chars()
        .map(|ch| match ch {
            'ą' => 'a',
            'ć' => 'c',
            'ę' => 'e',
            'ł' => 'l',
            'ń' => 'n',
            'ó' => 'o',
            'ś' => 's',
            'ź' | 'ż' => 'z',
            other => other,
        })
        .collect()
}

fn polish_weekday(word: &str) -> Option<&'static str> {
    Some(match word.trim_end_matches('.') {
        "poniedzialek" | "poniedzialku" | "pon" | "pn" => "monday",
        "wtorek" | "wtorku" | "wt" | "wto" => "tuesday",
        "sroda" | "srode" | "srody" | "sr" | "sro" => "wednesday",
        "czwartek" | "czwartku" | "czw" | "cz" => "thursday",
        "piatek" | "piatku" | "pt" | "pia" => "friday",
        "sobota" | "sobote" | "soboty" | "sob" | "sb" => "saturday",
        "niedziela" | "niedziele" | "niedzieli" | "niedz" | "nd" | "ndz" => "sunday",
        _ => return None,
    })
}

fn polish_unit(word: &str) -> Option<&'static str> {
    Some(match word.trim_end_matches('.') {
        "godzina" | "godziny" | "godzin" | "godzine" | "h" => "hours",
        "minuta" | "minuty" | "minut" | "minute" | "min" => "minutes",
        "dzien" | "dni" | "dnia" => "days",
        _ => return None,
    })
}

fn polish_month(word: &str) -> Option<&'static str> {
    let word = word.trim_end_matches('.');
    const MONTHS: [(&str, &str); 12] = [
        ("stycz", "jan"),
        ("lut", "feb"),
        ("mar", "mar"),
        ("kwie", "apr"),
        ("maj", "may"),
        ("czerw", "jun"),
        ("lip", "jul"),
        ("sierp", "aug"),
        ("wrze", "sep"),
        ("pazdz", "oct"),
        ("listop", "nov"),
        ("grud", "dec"),
    ];
    const SHORT: [(&str, &str); 7] = [
        ("sty", "jan"),
        ("kwi", "apr"),
        ("cze", "jun"),
        ("sie", "aug"),
        ("wrz", "sep"),
        ("paz", "oct"),
        ("lis", "nov"),
    ];
    if let Some((_, month)) = SHORT.iter().find(|(name, _)| *name == word) {
        return Some(month);
    }
    if word == "gru" {
        return Some("dec");
    }
    // Every form of a Polish month starts with its stem: "października", "maja".
    MONTHS
        .iter()
        .find(|(stem, _)| word.starts_with(stem) && (word.len() > 3 || word == "maj" || word == "mar" || word == "lut"))
        .map(|(_, month)| *month)
}

/// "14.10" or "14.10.2026" as a day: "oct 14", unless it follows "at" and is a time.
fn dotted_date(word: &str, last: Option<&str>) -> Option<Vec<String>> {
    if last == Some("at") {
        return None;
    }
    let parts: Vec<&str> = word.trim_end_matches('.').split('.').collect();
    if !(2..=3).contains(&parts.len()) || parts.iter().any(|part| part.is_empty() || !part.chars().all(|ch| ch.is_ascii_digit())) {
        return None;
    }
    let day: u32 = parts[0].parse().ok()?;
    let month: u32 = parts[1].parse().ok()?;
    if !(1..=31).contains(&day) || !(1..=12).contains(&month) {
        return None;
    }
    const NAMES: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    Some(vec![NAMES[month as usize - 1].to_string(), day.to_string()])
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
    fn polish_times_read_like_english_ones() {
        let tz = la();
        let at = |text: &str| parse_when(text, now(), tz).map(|when| when.with_timezone(&tz));
        let tomorrow = at("jutro o 19").unwrap();
        assert_eq!((tomorrow.date_naive().to_string(), tomorrow.hour()), ("2026-10-15".into(), 19));
        let friday = at("w piątek o 18:30").unwrap();
        assert_eq!((friday.weekday(), friday.hour(), friday.minute()), (Weekday::Fri, 18, 30));
        assert_eq!(at("we wtorek").unwrap().weekday(), Weekday::Tue);
        assert_eq!(at("sroda o godz. 20").unwrap().hour(), 20);
        assert_eq!(parse_when("za 2 godziny", now(), tz), Some(now() + Duration::hours(2)));
        assert_eq!(parse_when("za godzinę", now(), tz), Some(now() + Duration::hours(1)));
        assert_eq!(parse_when("za 30 minut", now(), tz), Some(now() + Duration::minutes(30)));
        let dated = at("20 października o 20:00").unwrap();
        assert_eq!((dated.date_naive().to_string(), dated.hour()), ("2026-10-20".into(), 20));
        let dotted = at("20.10 o 19.30").unwrap();
        assert_eq!((dotted.date_naive().to_string(), dotted.hour(), dotted.minute()), ("2026-10-20".into(), 19, 30));
        let later = at("pojutrze o 9").unwrap();
        assert_eq!((later.date_naive().to_string(), later.hour()), ("2026-10-16".into(), 9));
        assert_eq!(at("dziś wieczorem").unwrap().hour(), 20);
        let next = at("następny piątek o 18").unwrap();
        assert_eq!(next.weekday(), Weekday::Fri);
        assert_eq!(at("1 maja o 12").unwrap().date_naive().to_string(), "2027-05-01");
        assert_eq!(normalize("Friday at 9pm"), "friday at 9pm", "English stays as it is");
    }

    #[test]
    fn tokenizer_keeps_quoted_values() {
        let tokens = tokenize(r#"title:"Shore cleanup" start:"tomorrow at 4pm" channel:social"#);
        assert_eq!(tokens[0], (Some("title".into()), "Shore cleanup".into()));
        assert_eq!(tokens[1].1, "tomorrow at 4pm");
        assert_eq!(tokens[2], (Some("channel".into()), "social".into()));
    }
}
