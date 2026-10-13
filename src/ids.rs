//! Public identifiers shown in links. Integer keys stay inside the database.
//!
//! Event links use UUID version 7: a millisecond timestamp, then random bits.

use std::io::Read;

pub fn event_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0);
    let mut random = [0u8; 10];
    if let Ok(mut source) = std::fs::File::open("/dev/urandom") {
        let _ = source.read_exact(&mut random);
    }
    let mut bytes = [0u8; 16];
    bytes[0] = (millis >> 40) as u8;
    bytes[1] = (millis >> 32) as u8;
    bytes[2] = (millis >> 24) as u8;
    bytes[3] = (millis >> 16) as u8;
    bytes[4] = (millis >> 8) as u8;
    bytes[5] = millis as u8;
    bytes[6] = 0x70 | (random[0] & 0x0f);
    bytes[7] = random[1];
    bytes[8] = 0x80 | (random[2] & 0x3f);
    bytes[9] = random[3];
    bytes[10..16].copy_from_slice(&random[4..10]);
    format_uuid(&bytes)
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

fn format_uuid(bytes: &[u8; 16]) -> String {
    let hex = |slice: &[u8]| {
        slice
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    format!(
        "{}-{}-{}-{}-{}",
        hex(&bytes[0..4]),
        hex(&bytes[4..6]),
        hex(&bytes[6..8]),
        hex(&bytes[8..10]),
        hex(&bytes[10..16])
    )
}
