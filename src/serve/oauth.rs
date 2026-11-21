//! The OAuth server MCP clients sign in with, after the MCP authorization
//! spec: they find it from `/mcp`'s 401, register themselves, send the person
//! here to allow them, and swap the code for a token with PKCE.
//!
//! Marian doesn't sign anyone in itself: the consent page asks for the
//! browser's Discord sign-in, and the token is a sign-in of its own, for the
//! same person and servers, that answers only at `/mcp`.

use axum::Json;
use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use base64::Engine;
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use sha2::Digest;

use super::{AppState, Caller, read_session};
use crate::auth;
use crate::store;

/// Where Marian is reached, with no trailing slash: `MARIAN_PUBLIC_URL`, or the
/// request's host where that isn't set.
pub(super) fn public_base(state: &AppState, headers: &HeaderMap) -> Option<String> {
    super::oauth_redirect(state, headers)?
        .strip_suffix("/auth/discord/callback")
        .map(str::to_string)
}

/// What `/mcp` answers a request without a token: where to read how to get one.
pub(super) fn resource_metadata_url(base: &str) -> String {
    format!("{base}/.well-known/oauth-protected-resource")
}

pub(super) async fn protected_resource(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let Some(base) = public_base(&state, &headers) else {
        return not_configured();
    };
    Json(json!({
        "resource": format!("{base}/mcp"),
        "authorization_servers": [base],
        "bearer_methods_supported": ["header"],
        "resource_name": "Marian",
    }))
    .into_response()
}

pub(super) async fn authorization_server(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let Some(base) = public_base(&state, &headers) else {
        return not_configured();
    };
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/oauth/token"),
        "registration_endpoint": format!("{base}/oauth/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
    }))
    .into_response()
}

fn not_configured() -> Response {
    oauth_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "temporarily_unavailable",
        "Set MARIAN_PUBLIC_URL so clients know where Marian is.",
    )
}

/// An OAuth error, as RFC 6749 words them.
fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    let mut response = (
        status,
        Json(json!({ "error": error, "error_description": description })),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[derive(Deserialize)]
pub(super) struct Registration {
    #[serde(default)]
    redirect_uris: Vec<String>,
    #[serde(default)]
    client_name: Option<String>,
}

const MAX_REDIRECT_URIS: usize = 10;
const MAX_URI_LEN: usize = 500;
const MAX_NAME_LEN: usize = 100;

/// Dynamic client registration (RFC 7591), for public clients only.
pub(super) async fn register(
    State(state): State<AppState>,
    Json(registration): Json<Registration>,
) -> Response {
    let uris = registration.redirect_uris;
    if uris.is_empty() || uris.len() > MAX_REDIRECT_URIS {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_redirect_uri",
            "Send between one and ten redirect URIs.",
        );
    }
    if let Some(bad) = uris.iter().find(|uri| !allowed_redirect(uri)) {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_redirect_uri",
            &format!("Marian can't send a sign-in to {}.", clip(bad, 80)),
        );
    }
    let name = client_name(registration.client_name.as_deref());
    let id = match auth::random_token() {
        Ok(token) => token,
        Err(_) => {
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "Try again.",
            );
        }
    };
    let saved = match state.db.get().await {
        Ok(mut db) => store::insert_mcp_client(&mut db, &id, &name, &uris).await,
        Err(_) => {
            return oauth_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "temporarily_unavailable",
                "The database is busy.",
            );
        }
    };
    if let Err(err) = saved {
        tracing::error!(error = %err, "could not register an MCP client");
        return oauth_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "Try again.",
        );
    }
    tracing::info!(client = %name, "MCP client registered");
    (
        StatusCode::CREATED,
        Json(json!({
            "client_id": id,
            "client_id_issued_at": Utc::now().timestamp(),
            "client_name": name,
            "redirect_uris": uris,
            "grant_types": ["authorization_code"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        })),
    )
        .into_response()
}

/// A redirect URI a client may register: HTTPS, plain HTTP to this machine
/// only, or an app's own scheme. Never one that runs or reads something in
/// the browser, and never one with a fragment.
fn allowed_redirect(uri: &str) -> bool {
    if uri.len() > MAX_URI_LEN {
        return false;
    }
    let Ok(url) = reqwest::Url::parse(uri) else {
        return false;
    };
    if url.fragment().is_some() {
        return false;
    }
    match url.scheme() {
        "https" => url.host().is_some(),
        "http" => url.host_str().is_some_and(loopback),
        "javascript" | "data" | "file" | "blob" | "about" | "vbscript" | "ws" | "wss" | "ftp"
        | "filesystem" => false,
        _ => true,
    }
}

fn loopback(host: &str) -> bool {
    host == "localhost"
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// The client's name as the consent page shows it: printable, and short.
fn client_name(raw: Option<&str>) -> String {
    let name: String = raw
        .unwrap_or_default()
        .chars()
        .map(|ch| {
            if ch.is_control() || ch.is_whitespace() {
                ' '
            } else {
                ch
            }
        })
        .collect::<String>()
        .split(' ')
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if name.is_empty() {
        "An MCP client".to_string()
    } else {
        clip(&name, MAX_NAME_LEN)
    }
}

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

#[derive(Deserialize)]
pub(super) struct AuthorizeQuery {
    #[serde(default)]
    response_type: String,
    #[serde(default)]
    client_id: String,
    redirect_uri: Option<String>,
    #[serde(default)]
    code_challenge: String,
    #[serde(default)]
    code_challenge_method: String,
    state: Option<String>,
}

/// Checks the client's request, keeps it, and sends the person to the
/// consent page. A bad request is said here rather than sent back to the
/// client: anyone can register a client, and Marian shouldn't send people
/// to its address before they've seen whose it is.
pub(super) async fn authorize(
    State(state): State<AppState>,
    Query(query): Query<AuthorizeQuery>,
) -> Response {
    let Ok(mut db) = state.db.get().await else {
        return page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Marian is busy",
            "Try again in a moment.",
        );
    };
    let client = match store::mcp_client(&mut db, &query.client_id).await {
        Ok(Some(client)) => client,
        Ok(None) => {
            return page(
                StatusCode::BAD_REQUEST,
                "Unknown app",
                "Marian doesn't know the app that sent you here. Try connecting it again.",
            );
        }
        Err(err) => {
            tracing::error!(error = %err, "could not read an MCP client");
            return page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Something went wrong",
                "Try again.",
            );
        }
    };
    let redirect_uri = match query.redirect_uri {
        Some(uri) if client.redirect_uris.contains(&uri) => uri,
        None if client.redirect_uris.len() == 1 => client.redirect_uris[0].clone(),
        _ => {
            return page(
                StatusCode::BAD_REQUEST,
                "Wrong return address",
                "The app asked Marian to send you somewhere it didn't register. Try connecting it again.",
            );
        }
    };
    let bad = |text: &str| page(StatusCode::BAD_REQUEST, "The app's request is wrong", text);
    if query.response_type != "code" {
        return bad("Marian only answers response_type=code.");
    }
    if query.code_challenge_method != "S256" || !pkce_value(&query.code_challenge) {
        return bad("Send a PKCE code challenge, with S256.");
    }
    if query
        .state
        .as_ref()
        .is_some_and(|state| state.len() > MAX_URI_LEN)
    {
        return bad("The state is too long.");
    }
    let Ok(id) = auth::random_token() else {
        return page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Something went wrong",
            "Try again.",
        );
    };
    let authorization = store::McpAuthorization {
        id: id.clone(),
        client_id: client.id,
        redirect_uri: redirect_uri.clone(),
        code_challenge: query.code_challenge.clone(),
        state: query.state.clone(),
        created_at: Utc::now(),
        code: None,
        session_token: None,
    };
    if let Err(err) = store::insert_mcp_authorization(&mut db, &authorization).await {
        tracing::error!(error = %err, "could not keep an MCP authorization");
        return page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Something went wrong",
            "Try again.",
        );
    }
    Redirect::to(&consent_path(&id)).into_response()
}

/// A PKCE verifier or challenge: 43 to 128 of the unreserved characters.
fn pkce_value(value: &str) -> bool {
    (43..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
}

fn consent_path(id: &str) -> String {
    format!("/oauth/consent?request={id}")
}

fn redirect_with(uri: &str, params: &[(&str, &str)]) -> Response {
    let Ok(mut url) = reqwest::Url::parse(uri) else {
        return page(
            StatusCode::BAD_REQUEST,
            "Wrong return address",
            "Try connecting the app again.",
        );
    };
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in params {
            pairs.append_pair(key, value);
        }
    }
    Redirect::to(url.as_str()).into_response()
}

#[derive(Deserialize)]
pub(super) struct ConsentQuery {
    #[serde(default)]
    request: String,
}

/// Asks the signed-in person whether the client may act as them.
pub(super) async fn consent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ConsentQuery>,
) -> Response {
    let Ok(mut db) = state.db.get().await else {
        return page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Marian is busy",
            "Try again in a moment.",
        );
    };
    let Some(pending) = pending(&mut db, &query.request).await else {
        return expired();
    };
    let Some(viewer) = read_session(&mut db, Caller::Browser(&headers)).await else {
        let next = auth::encode(&consent_path(&pending.id));
        return Redirect::to(&format!("/auth/discord?next={next}")).into_response();
    };
    let client = match store::mcp_client(&mut db, &pending.client_id).await {
        Ok(Some(client)) => client,
        _ => return expired(),
    };
    let to = reqwest::Url::parse(&pending.redirect_uri)
        .ok()
        .map(|url| match url.host_str() {
            Some(host) => host.to_string(),
            None => format!("{}:", url.scheme()),
        })
        .unwrap_or_default();
    let body = format!(
        r#"<h1>Connect {name} to Marian?</h1>
<p>You're signed in as <strong>{who}</strong>.</p>
<p>{name} will act as you in your Discord servers with Marian, for 30 days:</p>
<ul>
<li>see your servers, their events, and who signed up</li>
<li>create, change, post and delete events, where Marian lets you</li>
</ul>
<p class="small">It will send you back to <strong>{to}</strong>. Only allow it if you started this from an app you trust.</p>
<form method="post" action="/oauth/consent">
<input type="hidden" name="request" value="{request}">
<button type="submit" name="decision" value="allow">Allow</button>
<button type="submit" name="decision" value="deny" class="ghost">Cancel</button>
</form>"#,
        name = escape(&client.name),
        who = escape(&viewer.display_name),
        to = escape(&to),
        request = escape(&pending.id),
    );
    html(StatusCode::OK, "Connect an app", &body)
}

async fn pending(db: &mut store::Connection, id: &str) -> Option<store::McpAuthorization> {
    if id.is_empty() {
        return None;
    }
    store::pending_mcp_authorization(db, id, Utc::now())
        .await
        .ok()
        .flatten()
}

fn expired() -> Response {
    page(
        StatusCode::BAD_REQUEST,
        "This request expired",
        "Start connecting again from your app.",
    )
}

#[derive(Deserialize)]
pub(super) struct ConsentForm {
    #[serde(default)]
    request: String,
    #[serde(default)]
    decision: String,
}

/// The person's answer. The cookie only comes with a form sent from Marian's
/// own page (it's SameSite=Lax), so another site can't answer for them.
pub(super) async fn decide(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<ConsentForm>,
) -> Response {
    let Ok(mut db) = state.db.get().await else {
        return page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Marian is busy",
            "Try again in a moment.",
        );
    };
    let Some(viewer) = read_session(&mut db, Caller::Browser(&headers)).await else {
        return page(
            StatusCode::UNAUTHORIZED,
            "Sign in first",
            "Your Marian sign-in ended. Start connecting again from your app.",
        );
    };
    let Some(pending) = pending(&mut db, &form.request).await else {
        return expired();
    };
    let state_param = pending.state.clone();
    let with_state = |mut params: Vec<(&'static str, String)>| {
        if let Some(state) = &state_param {
            params.push(("state", state.clone()));
        }
        params
    };
    if form.decision != "allow" {
        let _ = store::delete_mcp_authorization(&mut db, &pending.id).await;
        let params = with_state(vec![
            ("error", "access_denied".into()),
            ("error_description", "You didn't allow it.".into()),
        ]);
        return redirect_with(&pending.redirect_uri, &borrowed(&params));
    }
    let Ok(code) = auth::random_token() else {
        return page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Something went wrong",
            "Try again.",
        );
    };
    match store::approve_mcp_authorization(&mut db, &pending.id, &code, &viewer.token, Utc::now())
        .await
    {
        Ok(true) => {}
        Ok(false) => return expired(),
        Err(err) => {
            tracing::error!(error = %err, "could not allow an MCP client");
            return page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Something went wrong",
                "Try again.",
            );
        }
    }
    let mut params = with_state(vec![("code", code)]);
    if let Some(base) = public_base(&state, &headers) {
        params.push(("iss", base));
    }
    redirect_with(&pending.redirect_uri, &borrowed(&params))
}

fn borrowed<'a>(params: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    params
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect()
}

#[derive(Deserialize)]
pub(super) struct TokenForm {
    #[serde(default)]
    grant_type: String,
    #[serde(default)]
    code: String,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    code_verifier: String,
}

/// Swaps an allowed code for a token, once.
pub(super) async fn token(State(state): State<AppState>, Form(form): Form<TokenForm>) -> Response {
    if form.grant_type != "authorization_code" {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "Marian only takes authorization codes.",
        );
    }
    let invalid = || {
        oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "That code isn't valid. Connect again.",
        )
    };
    let Ok(mut db) = state.db.get().await else {
        return oauth_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "temporarily_unavailable",
            "The database is busy.",
        );
    };
    let taken = match store::take_mcp_code(&mut db, &form.code, Utc::now()).await {
        Ok(Some(taken)) => taken,
        Ok(None) => return invalid(),
        Err(err) => {
            tracing::error!(error = %err, "could not read an MCP code");
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "Try again.",
            );
        }
    };
    let redirect_matches = form
        .redirect_uri
        .as_ref()
        .is_none_or(|uri| *uri == taken.redirect_uri);
    if taken.client_id != form.client_id
        || !redirect_matches
        || !pkce_value(&form.code_verifier)
        || challenge(&form.code_verifier) != taken.code_challenge
    {
        return invalid();
    }
    let Some(from) = taken.session_token.as_deref() else {
        return invalid();
    };
    // The browser sign-in that allowed it, while it still counts.
    let Some(browser) = crate::api::browser_session(&mut db, from).await else {
        return invalid();
    };
    let Ok(access) = auth::random_token() else {
        return oauth_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "Try again.",
        );
    };
    if let Err(err) = store::insert_mcp_session(&mut db, &access, &browser, &taken.client_id).await
    {
        tracing::error!(error = %err, "could not give an MCP client its token");
        return oauth_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "Try again.",
        );
    }
    tracing::info!("MCP client signed in");
    let mut response = Json(json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": store::SESSION_DAYS * 24 * 3600,
    }))
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// The S256 code challenge for a verifier.
fn challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier))
}

/// The pages here are plain HTML of their own, outside the app, so they send
/// their own policy: no scripts, only the inline style below. It names no
/// form-action, since the form's answer redirects to the client.
const PAGE_POLICY: &str =
    "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'";

fn page(status: StatusCode, title: &str, text: &str) -> Response {
    html(
        status,
        title,
        &format!("<h1>{}</h1>\n<p>{}</p>", escape(title), escape(text)),
    )
}

fn html(status: StatusCode, title: &str, body: &str) -> Response {
    let document = format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} · Marian</title>
<style>
body {{ font: 16px/1.5 system-ui, sans-serif; background: #f6f3ec; color: #1d2433; margin: 0; }}
main {{ max-width: 32rem; margin: 10vh auto; padding: 2rem; background: #fff; border-radius: 12px; box-shadow: 0 1px 3px rgb(0 0 0 / 12%); }}
h1 {{ font-size: 1.4rem; margin-top: 0; color: #1e3a6e; }}
.small {{ font-size: .9rem; color: #555; }}
form {{ display: flex; gap: .75rem; margin-top: 1.5rem; }}
button {{ font: inherit; padding: .5rem 1.25rem; border-radius: 8px; border: 1px solid #1e3a6e; background: #1e3a6e; color: #fff; cursor: pointer; }}
button.ghost {{ background: transparent; color: #1e3a6e; }}
</style>
</head>
<body><main>
{body}
</main></body>
</html>
"#,
        title = escape(title),
    );
    let mut response = (status, Html(document)).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(PAGE_POLICY),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_is_rfc_7636s_example() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn clients_may_send_people_back_over_https_to_this_machine_or_to_an_app() {
        assert!(allowed_redirect("https://claude.ai/api/mcp/auth_callback"));
        assert!(allowed_redirect("http://localhost:33418/callback"));
        assert!(allowed_redirect("http://127.0.0.1:6274/oauth/callback"));
        assert!(allowed_redirect("http://[::1]:8080/cb"));
        assert!(allowed_redirect(
            "cursor://anysphere.cursor-mcp/oauth/callback"
        ));
        assert!(!allowed_redirect("http://evil.example/callback"));
        assert!(!allowed_redirect("javascript:alert(1)"));
        assert!(!allowed_redirect("data:text/html,hi"));
        assert!(!allowed_redirect("https://claude.ai/cb#fragment"));
        assert!(!allowed_redirect("not a url"));
    }

    #[test]
    fn a_client_name_is_printable_and_short() {
        assert_eq!(client_name(Some("  Claude\n Code ")), "Claude Code");
        assert_eq!(client_name(None), "An MCP client");
        assert_eq!(client_name(Some(&"x".repeat(300))).len(), MAX_NAME_LEN);
    }

    #[test]
    fn pkce_values_use_only_unreserved_characters() {
        assert!(pkce_value(&"a".repeat(43)));
        assert!(!pkce_value(&"a".repeat(42)));
        assert!(!pkce_value(&format!("{}+", "a".repeat(43))));
    }
}
