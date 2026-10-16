//! Public identifiers shown in links. Integer keys stay inside the database.
//!
//! Event links use UUID version 7: a millisecond timestamp, then random bits.
//! A failed system random source stops the call instead of leaving those bits zero.

use uuid::Uuid;

pub fn routine_id() -> String {
    event_id()
}

pub fn series_id() -> String {
    event_id()
}

pub fn preset_id() -> String {
    event_id()
}

pub fn event_id() -> String {
    Uuid::now_v7().to_string()
}

pub fn parse_event_id(token: &str) -> Option<&str> {
    let end = token
        .find(|ch: char| !ch.is_ascii_hexdigit() && ch != '-')
        .unwrap_or(token.len());
    let id = &token[..end];
    let mut parts = id.split('-');
    let widths = [8, 4, 4, 4, 12];
    for width in widths {
        let part = parts.next()?;
        if part.len() != width || !part.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return None;
        }
    }
    if parts.next().is_some() {
        return None;
    }
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_id_is_a_version_7_uuid() {
        let id = event_id();
        let parsed = Uuid::parse_str(&id).unwrap();
        assert_eq!(parsed.get_version(), Some(uuid::Version::SortRand));
        assert_eq!(parse_event_id(&id), Some(id.as_str()));
        assert_ne!(event_id(), id);
    }
}
