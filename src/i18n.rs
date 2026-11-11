//! What Marian writes on Discord, in each server's language. The messages live
//! in Fluent files under `locales/`, one folder per language. English is the
//! source: a message missing from another language falls back to it.
//!
//! `tr!(lang, "post-waiting", count = 3)` formats a message now; `msg!(...)`, with
//! the same arguments, keeps it for later, for errors and notices that the web shows in English
//! and Discord shows in the server's language.

use std::sync::LazyLock;

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use chrono_tz::Tz;
use diesel::prelude::*;
use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource, FluentValue};

use crate::schema;
use crate::store::Connection;

/// A language Marian writes in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Lang {
    #[default]
    En,
    Pl,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::En, Lang::Pl];

    /// The code stored in a server's settings.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Pl => "pl",
        }
    }

    pub fn from_code(code: &str) -> Option<Lang> {
        match code.trim().to_ascii_lowercase().as_str() {
            "en" => Some(Lang::En),
            "pl" => Some(Lang::Pl),
            _ => None,
        }
    }

    /// The language's name in itself, as a picker shows it.
    pub fn name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Pl => "Polski",
        }
    }
}

/// The language a server picked; English when it hasn't, or can't be read.
pub fn lang_of(conn: &mut Connection, guild_id: i64) -> Lang {
    schema::settings::table
        .filter(schema::settings::guild_id.eq(guild_id))
        .select(schema::settings::language)
        .first::<String>(conn)
        .ok()
        .and_then(|code| Lang::from_code(&code))
        .unwrap_or_default()
}

const EN: [&str; 5] = [
    include_str!("../locales/en/errors.ftl"),
    include_str!("../locales/en/dm.ftl"),
    include_str!("../locales/en/posts.ftl"),
    include_str!("../locales/en/replies.ftl"),
    include_str!("../locales/en/dates.ftl"),
];

const PL: [&str; 6] = [
    include_str!("../locales/pl/errors.ftl"),
    include_str!("../locales/pl/dm.ftl"),
    include_str!("../locales/pl/posts.ftl"),
    include_str!("../locales/pl/replies.ftl"),
    include_str!("../locales/pl/dates.ftl"),
    include_str!("../locales/pl/commands.ftl"),
];

struct Bundles {
    en: FluentBundle<FluentResource>,
    pl: FluentBundle<FluentResource>,
}

fn bundle(lang: unic_langid::LanguageIdentifier, sources: &[&str]) -> FluentBundle<FluentResource> {
    let mut bundle = FluentBundle::new_concurrent(vec![lang]);
    // Discord shows Unicode isolation marks as stray characters.
    bundle.set_use_isolating(false);
    for source in sources {
        let resource = FluentResource::try_new(source.to_string())
            .unwrap_or_else(|(_, errors)| panic!("a locale file doesn't parse: {errors:?}"));
        bundle
            .add_resource(resource)
            .unwrap_or_else(|errors| panic!("a locale file repeats a message: {errors:?}"));
    }
    bundle
}

static BUNDLES: LazyLock<Bundles> = LazyLock::new(|| Bundles {
    en: bundle(unic_langid::langid!("en"), &EN),
    pl: bundle(unic_langid::langid!("pl"), &PL),
});

fn bundle_for(lang: Lang) -> &'static FluentBundle<FluentResource> {
    match lang {
        Lang::En => &BUNDLES.en,
        Lang::Pl => &BUNDLES.pl,
    }
}

/// A value a message is formatted with. Numbers pick plural forms.
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Text(String),
    Number(i64),
    /// Text already written in each language, in `Lang::ALL` order.
    Each(Vec<String>),
}

/// An argument written once per language, for text built from several
/// messages, such as a list.
pub fn each(write: impl Fn(Lang) -> String) -> Arg {
    Arg::Each(Lang::ALL.iter().map(|lang| write(*lang)).collect())
}

/// "a, b and c" in `lang`.
pub fn list(lang: Lang, items: &[String]) -> String {
    joined(lang, "list-and", items)
}

/// "a, b or c" in `lang`.
pub fn list_or(lang: Lang, items: &[String]) -> String {
    joined(lang, "list-or", items)
}

fn joined(lang: Lang, key: &str, items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => text(
            lang,
            key,
            &[("rest", Arg::from(rest.join(", "))), ("last", Arg::from(last))],
        ),
    }
}

impl From<&str> for Arg {
    fn from(value: &str) -> Self {
        Arg::Text(value.to_string())
    }
}

impl From<String> for Arg {
    fn from(value: String) -> Self {
        Arg::Text(value)
    }
}

impl From<&String> for Arg {
    fn from(value: &String) -> Self {
        Arg::Text(value.clone())
    }
}

macro_rules! number_arg {
    ($($ty:ty),*) => {
        $(impl From<$ty> for Arg {
            fn from(value: $ty) -> Self {
                Arg::Number(value as i64)
            }
        })*
    };
}

number_arg!(i64, i32, u8, u16, u32, u64, usize);

/// Formats a message in `lang`, or in English when `lang` doesn't have it.
/// An unknown key reads as itself, so a typo shows rather than hides.
pub fn text(lang: Lang, key: &str, args: &[(&str, Arg)]) -> String {
    let (bundle, message) = match bundle_for(lang).get_message(key) {
        Some(message) => (bundle_for(lang), message),
        None => match BUNDLES.en.get_message(key) {
            Some(message) => (&BUNDLES.en, message),
            None => {
                tracing::warn!("no message {key}");
                return key.to_string();
            }
        },
    };
    let Some(pattern) = message.value() else {
        return key.to_string();
    };
    let mut values = FluentArgs::new();
    for (name, value) in args {
        match value {
            Arg::Text(text) => values.set(*name, FluentValue::from(text.as_str())),
            Arg::Number(number) => values.set(*name, FluentValue::from(*number)),
            Arg::Each(texts) => {
                let at = Lang::ALL.iter().position(|each| *each == lang).unwrap_or(0);
                let text = texts.get(at).or(texts.first()).map(String::as_str).unwrap_or("");
                values.set(*name, FluentValue::from(text))
            }
        }
    }
    let mut errors = Vec::new();
    let out = bundle.format_pattern(pattern, Some(&values), &mut errors);
    if !errors.is_empty() {
        tracing::warn!("message {key} in {} has errors: {errors:?}", lang.code());
    }
    out.into_owned()
}

/// Whether `lang` has its own `key`, rather than falling back to English.
pub fn has(lang: Lang, key: &str) -> bool {
    bundle_for(lang).has_message(key)
}

/// A message to format later, in whichever language reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct Msg {
    key: &'static str,
    args: Vec<(&'static str, Arg)>,
}

impl Msg {
    pub fn new(key: &'static str, args: Vec<(&'static str, Arg)>) -> Self {
        Self { key, args }
    }

    pub fn key(&self) -> &'static str {
        self.key
    }

    pub fn render(&self, lang: Lang) -> String {
        text(lang, self.key, &self.args)
    }
}

/// English, as the web and the logs show it.
impl std::fmt::Display for Msg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render(Lang::En))
    }
}

/// Formats a message now: `tr!(lang, "post-waiting", count = 3)`.
#[macro_export]
macro_rules! tr {
    ($lang:expr, $key:literal $(, $name:ident = $value:expr)* $(,)?) => {
        $crate::i18n::text(
            $lang,
            $key,
            &[$((stringify!($name), $crate::i18n::Arg::from($value))),*],
        )
    };
}

/// Keeps a message for later: `msg!("err-option-full", option = name)`.
#[macro_export]
macro_rules! msg {
    ($key:literal $(, $name:ident = $value:expr)* $(,)?) => {
        $crate::i18n::Msg::new(
            $key,
            vec![$((stringify!($name), $crate::i18n::Arg::from($value))),*],
        )
    };
}

/// "Mon" through "Sun" as `lang` writes them, for day 0 (Monday) to 6.
pub fn weekday_short(lang: Lang, day: u8) -> String {
    text(lang, "weekday-short", &[("day", Arg::from(day % 7))])
}

/// "Monday" through "Sunday" as `lang` writes them.
pub fn weekday_name(lang: Lang, day: u8) -> String {
    text(lang, "weekday-long", &[("day", Arg::from(day % 7))])
}

fn month_short(lang: Lang, date: NaiveDate) -> String {
    text(lang, "month-short", &[("month", Arg::from(date.month()))])
}

/// "Tue, Oct 13": a day as a post or a thread title shows it.
pub fn format_day(lang: Lang, date: NaiveDate) -> String {
    text(
        lang,
        "date-day",
        &[
            ("weekday", Arg::from(weekday_short(lang, date.weekday().num_days_from_monday() as u8))),
            ("month", Arg::from(month_short(lang, date))),
            ("day", Arg::from(date.day())),
        ],
    )
}

/// "Thursday, Oct 15": a day spelled out, as a poll offers it.
pub fn format_long_day(lang: Lang, date: NaiveDate) -> String {
    text(
        lang,
        "date-long-day",
        &[
            ("weekday", Arg::from(weekday_name(lang, date.weekday().num_days_from_monday() as u8))),
            ("month", Arg::from(month_short(lang, date))),
            ("day", Arg::from(date.day())),
        ],
    )
}

/// "Tue, Oct 13 · 6:00 PM" in the server's zone and clock.
pub fn format_when(lang: Lang, dt: DateTime<Utc>, tz: Tz, hour24: bool) -> String {
    let local = dt.with_timezone(&tz);
    format!(
        "{} · {}",
        format_day(lang, local.date_naive()),
        crate::clock::format_clock(local, hour24)
    )
}

/// "Oct 13": a day in a short menu label.
pub fn format_month_day(lang: Lang, date: NaiveDate) -> String {
    text(
        lang,
        "date-month-day",
        &[("month", Arg::from(month_short(lang, date))), ("day", Arg::from(date.day()))],
    )
}

/// Makes the first letter a capital, for a label that starts with a word
/// `lang` writes in lower case, such as a Polish weekday.
pub fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use chrono::TimeZone;

    use super::*;

    /// Each `tr!` and `msg!` in the code, with the names it passes.
    fn used_messages() -> BTreeMap<String, BTreeSet<String>> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                    continue;
                }
                let source = std::fs::read_to_string(&path).unwrap();
                for (key, names) in calls_in(&source) {
                    found.entry(key).or_default().extend(names);
                }
            }
        }
        found
    }

    /// Reads `tr!(...)` and `msg!(...)` calls: the key, and the `name =` parts
    /// at the call's own level.
    fn calls_in(source: &str) -> Vec<(String, BTreeSet<String>)> {
        let mut calls = Vec::new();
        for marker in ["tr!(", "msg!("] {
            let mut rest = source;
            while let Some(pos) = rest.find(marker) {
                let before = rest[..pos].chars().last();
                rest = &rest[pos + marker.len()..];
                if before.is_some_and(|ch| ch.is_alphanumeric() || ch == '_') {
                    continue;
                }
                let mut depth = 1;
                let mut end = 0;
                for (index, ch) in rest.char_indices() {
                    match ch {
                        '(' | '[' | '{' => depth += 1,
                        ')' | ']' | '}' => {
                            depth -= 1;
                            if depth == 0 {
                                end = index;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let body = &rest[..end];
                let Some(start) = body.find('"') else { continue };
                let Some(len) = body[start + 1..].find('"') else { continue };
                let key = body[start + 1..start + 1 + len].to_string();
                if key.contains('{') || key.contains(' ') {
                    continue;
                }
                let mut names = BTreeSet::new();
                let mut level = 0;
                let chars: Vec<char> = body.chars().collect();
                for index in 0..chars.len() {
                    match chars[index] {
                        '(' | '[' | '{' => level += 1,
                        ')' | ']' | '}' => level -= 1,
                        '=' if level == 0 => {
                            let next = chars.get(index + 1).copied();
                            let prev = index.checked_sub(1).map(|at| chars[at]);
                            if matches!(next, Some('=') | Some('>')) || matches!(prev, Some('=') | Some('!') | Some('<') | Some('>')) {
                                continue;
                            }
                            let name: String = chars[..index]
                                .iter()
                                .rev()
                                .skip_while(|ch| ch.is_whitespace())
                                .take_while(|ch| ch.is_alphanumeric() || **ch == '_')
                                .collect::<Vec<_>>()
                                .into_iter()
                                .rev()
                                .collect();
                            if !name.is_empty() {
                                names.insert(name);
                            }
                        }
                        _ => {}
                    }
                }
                calls.push((key, names));
            }
        }
        calls
    }

    fn message_ids(sources: &[&str]) -> BTreeSet<String> {
        let mut ids = BTreeSet::new();
        for source in sources {
            for line in source.lines() {
                let Some((id, _)) = line.split_once('=') else { continue };
                let id = id.trim();
                if !line.starts_with(char::is_alphabetic) || id.contains(' ') {
                    continue;
                }
                ids.insert(id.to_string());
            }
        }
        ids
    }

    #[test]
    fn every_message_the_code_uses_is_in_english_and_polish() {
        let used = used_messages();
        assert!(used.len() > 100, "found {} messages; is the scan reading the code?", used.len());
        for (key, names) in &used {
            for lang in Lang::ALL {
                assert!(has(lang, key), "{key} is missing in {}", lang.code());
                let pattern = bundle_for(lang).get_message(key).unwrap().value().unwrap();
                for count in [1, 2, 5, 22] {
                    let mut values = FluentArgs::new();
                    for name in names {
                        values.set(name.as_str(), FluentValue::from(count));
                    }
                    let mut errors = Vec::new();
                    bundle_for(lang).format_pattern(pattern, Some(&values), &mut errors);
                    assert!(
                        errors.is_empty(),
                        "{key} in {} with {names:?}: {errors:?}",
                        lang.code()
                    );
                }
            }
        }
    }

    #[test]
    fn polish_has_every_english_message_and_nothing_else() {
        let en = message_ids(&EN);
        let pl = message_ids(&PL[..PL.len() - 1]);
        let missing: Vec<_> = en.difference(&pl).collect();
        let extra: Vec<_> = pl.difference(&en).collect();
        assert!(missing.is_empty(), "Polish is missing {missing:?}");
        assert!(extra.is_empty(), "Polish has messages English doesn't: {extra:?}");
    }

    #[test]
    fn polish_counts_take_three_forms() {
        assert_eq!(tr!(Lang::Pl, "post-waiting", count = 1), "1 osoba czeka");
        assert_eq!(tr!(Lang::Pl, "post-waiting", count = 3), "3 osoby czekają");
        assert_eq!(tr!(Lang::Pl, "post-waiting", count = 5), "5 osób czeka");
        assert_eq!(tr!(Lang::Pl, "post-waiting", count = 22), "22 osoby czekają");
        assert_eq!(tr!(Lang::En, "post-waiting", count = 1), "1 waiting");
    }

    #[test]
    fn guests_read_naturally_in_both_languages() {
        let signed = |lang, guests: i64| tr!(lang, "notice-signed-up-guests", name = "Ola", option = "Tak", guests = guests);
        assert_eq!(signed(Lang::Pl, 1), "Zapisano Ola na opcję Tak z 1 gościem.");
        assert_eq!(signed(Lang::Pl, 3), "Zapisano Ola na opcję Tak z 3 gośćmi.");
        assert_eq!(signed(Lang::En, 1), "Ola signed up as Tak, bringing one guest.");
        let left = |left: i64| tr!(Lang::Pl, "err-party-too-big", option = "Tak", left = left);
        assert_eq!(left(1), "W opcji Tak zostało tylko jedno miejsce.");
        assert_eq!(left(3), "W opcji Tak zostały tylko 3 miejsca.");
        assert_eq!(left(5), "W opcji Tak zostało tylko 5 miejsc.");
        let most = |most: i64| tr!(Lang::Pl, "err-guests-most", option = "Tak", most = most);
        assert_eq!(most(1), "W opcji Tak można przyprowadzić najwyżej 1 gościa.");
        assert_eq!(most(4), "W opcji Tak można przyprowadzić najwyżej 4 gości.");
        assert_eq!(tr!(Lang::En, "notice-guests", name = "Ola", option = "Yes", guests = 0), "Ola now brings no guests to Yes.");
    }

    #[test]
    fn dates_read_in_the_server_language() {
        let date = NaiveDate::from_ymd_opt(2026, 10, 13).unwrap();
        assert_eq!(format_day(Lang::En, date), "Tue, Oct 13");
        assert_eq!(format_day(Lang::Pl, date), "wt., 13 paź");
        assert_eq!(format_long_day(Lang::En, date), "Tuesday, Oct 13");
        assert_eq!(format_long_day(Lang::Pl, date), "wtorek, 13 paź");
        let at = Utc.with_ymd_and_hms(2026, 10, 13, 18, 0, 0).unwrap();
        assert_eq!(format_when(Lang::Pl, at, Tz::UTC, true), "wt., 13 paź · 18:00");
        assert_eq!(capitalized("środa"), "Środa");
    }

    #[test]
    fn a_message_is_kept_until_someone_reads_it() {
        let kept = msg!("err-signups-closed", title = "Raid");
        assert_eq!(kept.to_string(), "Signups for Raid are closed.");
        assert_eq!(kept.render(Lang::Pl), "Zapisy na Raid są zamknięte.");
    }

    #[test]
    fn a_server_code_reads_back() {
        for lang in Lang::ALL {
            assert_eq!(Lang::from_code(lang.code()), Some(lang));
        }
        assert_eq!(Lang::from_code("de"), None);
    }
}
