//! Discord sign-in. The cookie is a random token; Marian remembers who it is.

use std::collections::HashSet;

use crate::error::OpError;

pub fn random_token() -> Result<String, OpError> {
    let mut bytes = [0u8; 32];
    let mut file = std::fs::File::open("/dev/urandom")
        .map_err(|_| OpError::message("Could not open the system random source."))?;
    std::io::Read::read_exact(&mut file, &mut bytes)
        .map_err(|_| OpError::message("Could not read the system random source."))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub fn discord_display_name(global_name: Option<&str>, username: &str) -> String {
    let picked = global_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| username.trim());
    let picked = if picked.is_empty() {
        "Discord member"
    } else {
        picked
    };
    picked.chars().take(80).collect()
}

/// Guild ids whose Discord id is in the signed-in member's guild list.
pub fn guilds_in_common(guilds: &[(i64, String)], member_of: &[String]) -> Vec<i64> {
    let member: HashSet<&str> = member_of.iter().map(String::as_str).collect();
    let mut ids: Vec<i64> = guilds
        .iter()
        .filter(|(_, discord_id)| member.contains(discord_id.as_str()))
        .map(|(id, _)| *id)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

pub fn safe_next(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.len() > 200 {
        return None;
    }
    if !raw.starts_with('/') || raw.starts_with("//") {
        return None;
    }
    if raw.chars().any(|ch| {
        !(ch.is_ascii_alphanumeric() || matches!(ch, '/' | '-' | '_' | '.' | '?' | '=' | '&'))
    }) {
        return None;
    }
    if raw.starts_with("/auth")
        || raw.starts_with("/logout")
        || raw.starts_with("/health")
        || raw.starts_with("/ready")
    {
        return None;
    }
    Some(raw.to_string())
}

pub fn login_location(path_and_query: &str) -> String {
    match safe_next(path_and_query) {
        Some(next) if next != "/" => format!("/?next={}", encode(&next)),
        _ => "/".to_string(),
    }
}

pub fn redirect_uri(public_url: &str, host: Option<&str>, proto: Option<&str>) -> Option<String> {
    let base = if !public_url.trim().is_empty() {
        public_url.trim().trim_end_matches('/').to_string()
    } else {
        let host = host?.trim();
        if host.is_empty() || host.contains('/') || host.contains(' ') {
            return None;
        }
        let proto = if proto == Some("https") {
            "https"
        } else {
            "http"
        };
        format!("{proto}://{host}")
    };
    Some(format!("{base}/auth/discord/callback"))
}

pub fn cookie_secure(public_url: &str, proto: Option<&str>) -> bool {
    public_url.trim().starts_with("https://") || proto == Some("https")
}

pub fn authorize_url(client_id: &str, redirect_uri: &str, state: &str) -> String {
    format!(
        "https://discord.com/api/oauth2/authorize?client_id={}&response_type=code&scope=identify%20guilds&redirect_uri={}&state={}",
        encode(client_id.trim()),
        encode(redirect_uri),
        encode(state),
    )
}

pub fn pack_oauth(state: &str, next: &str) -> String {
    format!("{state}.{}", encode(next))
}

pub fn unpack_oauth(raw: &str) -> Option<(String, String)> {
    let (state, next) = raw.split_once('.')?;
    if state.len() < 16 || !state.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    let next = decode(next);
    Some((state.to_string(), safe_next(&next).unwrap_or_default()))
}

pub fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn is_public(path: &str) -> bool {
    matches!(
        path,
        "/" | "/healthz"
            | "/readyz"
            | "/help"
            | "/auth/discord"
            | "/auth/discord/callback"
            | "/logout"
    ) || crate::assets::is_static(path)
        || personal_calendar(path)
        || server_calendar(path)
        || google_connect(path)
}

fn google_connect(path: &str) -> bool {
    if path == "/auth/google/callback" {
        return true;
    }
    let Some(token) = path.strip_prefix("/auth/google/start/") else {
        return false;
    };
    token.len() == 64 && token.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn personal_calendar(path: &str) -> bool {
    let Some(file) = path.strip_prefix("/calendar/") else {
        return false;
    };
    if file.contains('/') {
        return false;
    }
    let Some(token) = file.strip_suffix(".ics") else {
        return false;
    };
    token.len() == 64 && token.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn server_calendar(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/servers/") else {
        return false;
    };
    let mut parts = rest.split('/');
    let id = parts.next().unwrap_or("");
    if id.is_empty() || !id.chars().all(|ch| ch.is_ascii_digit()) {
        return false;
    }
    match (parts.next(), parts.next(), parts.next()) {
        (Some("calendar"), Some(file), None) => {
            file.ends_with(".ics") && !file[..file.len() - 4].is_empty()
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_stays_on_this_site() {
        assert_eq!(login_location("/events/12"), "/?next=%2Fevents%2F12");
        assert_eq!(login_location("/"), "/");
        assert_eq!(login_location("//evil.example"), "/");
        assert_eq!(login_location("https://evil.example"), "/");
        assert_eq!(safe_next("/auth/discord"), None);
    }

    #[test]
    fn authorize_url_asks_for_identity_and_guilds() {
        let url = authorize_url(
            "123",
            "https://marian.example/auth/discord/callback",
            "abcdef0123456789",
        );
        assert!(url.contains("client_id=123"));
        assert!(url.contains("scope=identify%20guilds"));
        assert!(
            url.contains("redirect_uri=https%3A%2F%2Fmarian.example%2Fauth%2Fdiscord%2Fcallback")
        );
        assert_eq!(
            redirect_uri("https://marian.example/", None, None).as_deref(),
            Some("https://marian.example/auth/discord/callback")
        );
    }

    #[test]
    fn common_guilds_are_the_overlap() {
        let guilds = vec![(1, "10".into()), (2, "20".into()), (3, "30".into())];
        let member = vec!["30".into(), "10".into(), "99".into()];
        assert_eq!(guilds_in_common(&guilds, &member), vec![1, 3]);
        assert_eq!(discord_display_name(Some("Nell"), "nell"), "Nell");
        assert_eq!(discord_display_name(None, "nell"), "nell");
    }

    #[test]
    fn oauth_cookie_round_trip() {
        let packed = pack_oauth("abcdef0123456789", "/events/4");
        assert_eq!(
            unpack_oauth(&packed),
            Some(("abcdef0123456789".into(), "/events/4".into()))
        );
        assert_eq!(unpack_oauth("short./events/4"), None);
    }

    #[test]
    fn a_personal_calendar_is_public_and_the_site_is_not() {
        let token = "ab".repeat(32);
        assert!(is_public("/help"));
        assert!(is_public(&format!("/calendar/{token}.ics")));
        assert!(!is_public("/calendar/short.ics"));
        assert!(!is_public("/servers/4/calendar.ics"));
        assert!(is_public("/servers/4/calendar/9.ics"));
        assert!(is_public("/auth/google/callback"));
        let state = "ab".repeat(32);
        assert!(is_public(&format!("/auth/google/start/{state}")));
        assert!(!is_public("/auth/google"));
        assert!(!is_public("/auth/google/start/short"));
        assert!(is_public("/static/app.abc.css"));
        assert!(is_public("/favicon.svg"));
        assert!(!is_public("/static"));
        assert!(!is_public("/servers/4/settings"));
        assert!(!is_public("/servers/4/events/9"));
    }
}
