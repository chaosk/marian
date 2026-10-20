//! Files compiled into the binary. A fingerprinted URL is safe to cache for a
//! year. `/favicon.svg` keeps a stable address and must be revalidated.

use std::sync::OnceLock;

use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

const FAVICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="6" fill="#14284d"/><path d="M8 22V10h3.2l4.8 7.2L20.8 10H24v12h-2.6v-7.1L17.2 22h-2.4l-4.2-7.1V22H8z" fill="#f4efe4"/></svg>"##;

struct File {
    path: String,
    body: &'static [u8],
    content_type: &'static str,
    etag: String,
    immutable: bool,
}

struct Catalog {
    files: Vec<File>,
    mark: String,
    icon: String,
}

pub fn mark() -> &'static str {
    catalog().mark.as_str()
}

pub fn icon() -> &'static str {
    catalog().icon.as_str()
}

pub fn is_static(path: &str) -> bool {
    path == "/favicon.svg" || path.starts_with("/static/")
}

pub fn response(path: &str, if_none_match: Option<&HeaderValue>) -> Response {
    let Some(file) = catalog().files.iter().find(|file| file.path == path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if not_modified(if_none_match, &file.etag) {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        write_headers(response.headers_mut(), file);
        return response;
    }
    let mut response = file.body.into_response();
    write_headers(response.headers_mut(), file);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(file.content_type),
    );
    response
}

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mark_body = include_bytes!("../static/marian.png").as_slice();
        let icon_body = FAVICON.as_bytes();
        let mark = fingerprinted("/static/marian", "png", mark_body);
        let icon = fingerprinted("/static/favicon", "svg", icon_body);
        let files = vec![
            file(&mark, mark_body, "image/png", true),
            file(&icon, icon_body, "image/svg+xml", true),
            file("/favicon.svg", icon_body, "image/svg+xml", false),
        ];
        Catalog {
            files,
            mark,
            icon,
        }
    })
}

fn fingerprinted(stem: &str, ext: &str, body: &[u8]) -> String {
    format!("{stem}.{}.{ext}", fingerprint(body))
}

fn file(path: &str, body: &'static [u8], content_type: &'static str, immutable: bool) -> File {
    File {
        path: path.to_string(),
        body,
        content_type,
        etag: format!("\"{}\"", fingerprint(body)),
        immutable,
    }
}

fn fingerprint(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn not_modified(header: Option<&HeaderValue>, etag: &str) -> bool {
    let Some(value) = header.and_then(|value| value.to_str().ok()) else {
        return false;
    };
    value.split(',').any(|part| {
        let part = part.trim();
        part == "*" || part == etag || part.strip_prefix("W/") == Some(etag)
    })
}

fn write_headers(headers: &mut HeaderMap, file: &File) {
    let cache = if file.immutable {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=0, must-revalidate"
    };
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(&file.etag).expect("etag is quoted hex"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fingerprinted_file_is_immutable_and_revalidates() {
        let path = mark();
        assert!(path.starts_with("/static/marian."));
        assert!(path.ends_with(".png"));
        assert_ne!(path, "/static/marian.png");
        let first = response(path, None);
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(
            first.headers().get(header::CACHE_CONTROL).unwrap(),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            first.headers().get(header::X_CONTENT_TYPE_OPTIONS).unwrap(),
            "nosniff"
        );
        let etag = first.headers().get(header::ETAG).unwrap().clone();
        let again = response(path, Some(&etag));
        assert_eq!(again.status(), StatusCode::NOT_MODIFIED);
        assert!(again.headers().get(header::CONTENT_TYPE).is_none());
        assert_eq!(
            response("/static/missing.png", None).status(),
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn the_stable_favicon_must_be_revalidated() {
        let response = response("/favicon.svg", None);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "public, max-age=0, must-revalidate"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/svg+xml"
        );
    }
}
