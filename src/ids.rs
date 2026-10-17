//! Public identifiers shown in links. Integer keys stay inside the database.
//!
//! Event links use UUID version 7: a millisecond timestamp, then random bits.
//! A failed system random source stops the call instead of leaving those bits zero.

use uuid::Uuid;

pub fn routine_id() -> Uuid {
    event_id()
}

pub fn series_id() -> Uuid {
    event_id()
}

pub fn preset_id() -> Uuid {
    event_id()
}

pub fn event_id() -> Uuid {
    Uuid::now_v7()
}

pub fn parse_event_id(token: &str) -> Option<Uuid> {
    let end = token
        .find(|ch: char| !ch.is_ascii_hexdigit() && ch != '-')
        .unwrap_or(token.len());
    Uuid::parse_str(&token[..end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_id_is_a_version_7_uuid() {
        let id = event_id();
        assert_eq!(id.get_version(), Some(uuid::Version::SortRand));
        assert_eq!(parse_event_id(&id.to_string()), Some(id));
        assert_ne!(event_id(), id);
    }
}
