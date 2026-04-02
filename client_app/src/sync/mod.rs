//! Online session sync — push/pull events to/from the Cloudflare Worker API.
//!
//! The sync protocol is intentionally simple:
//!   1. Coach "goes online": POST /api/sessions with full event log.
//!      Response includes assistant_token and player_token.
//!   2. Coach pushes new events: POST /api/sessions/:id/events
//!   3. Assistants/players fetch events: GET /api/sessions/:id/events?since=N
//!
//! All state reconstruction from events happens client-side. The worker
//! is a dumb event store + token registry.

use app_core::events::EventEnvelope;
use app_core::models::PlayerRanking;
use js_sys::Function;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestInit, RequestMode, Response};

/// The base API URL. In production this is the same origin; during local dev
/// it points to the wrangler dev server.
pub fn api_base() -> String {
    if let Some(origin) = browser_origin() {
        // If running on localhost:8080 (trunk serve), hit wrangler on 8787
        if origin.contains("localhost:8080") {
            return "http://localhost:8787".to_string();
        }
        return origin;
    }
    String::new()
}

/// Read the current browser origin without calling throwing web-sys getters.
///
/// Some browsers can reject direct `window.location.origin` access during app
/// startup or after restoring older persisted sessions, which turns into a wasm
/// trap in release builds. Reflection keeps that failure recoverable and lets
/// sync degrade instead of taking down the whole app shell.
fn browser_origin() -> Option<String> {
    let window = web_sys::window()?;
    reflected_property_string(window.location().as_ref(), "origin")
}

fn reflected_property_string(target: &JsValue, property_name: &str) -> Option<String> {
    js_sys::Reflect::get(target, &JsValue::from_str(property_name))
        .ok()
        .and_then(|value| value.as_string())
        .filter(|value| !value.is_empty())
}

fn parsed_url_origin(url: &str) -> Option<String> {
    let parsed = web_sys::Url::new(url).ok()?;
    reflected_property_string(parsed.as_ref(), "origin")
}

// ── Persistent sync state (stored in localStorage) ───────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncState {
    /// The session UUID
    pub session_id: String,
    /// Sequence number of the last event successfully pushed to the server
    pub last_pushed_seq: u64,
    /// Token for assistant access
    pub assistant_token: String,
    /// Token for player access
    pub player_token: String,
    /// Opaque write key returned by POST /api/sessions; must be sent as
    /// X-Coach-Key for all subsequent coach event appends.
    #[serde(default)]
    pub coach_key: String,
}

impl SyncState {
    pub fn assistant_url(&self) -> String {
        format!("{}/a/{}", api_base(), self.assistant_token)
    }
    pub fn player_url(&self) -> String {
        format!("{}/p/{}", api_base(), self.player_token)
    }
}

const SYNC_KEY_PREFIX: &str = "pcpp_sync_";
const TOKEN_INFO_CACHE_PREFIX: &str = "pcpp_token_info_";
const TOKEN_INFO_CACHE_TTL_MS: f64 = 12.0 * 60.0 * 60.0 * 1000.0;

fn sync_key(session_id: &str) -> String {
    format!("{SYNC_KEY_PREFIX}{session_id}")
}

fn token_info_cache_key(token: &str) -> String {
    format!("{TOKEN_INFO_CACHE_PREFIX}{token}")
}

pub fn load_sync_state(session_id: &str) -> Option<SyncState> {
    let key = sync_key(session_id);
    let json = crate::state::storage_get(&key)?;
    serde_json::from_str(&json).ok()
}

pub fn save_sync_state(state: &SyncState) {
    let key = sync_key(&state.session_id);
    if let Ok(json) = serde_json::to_string(state) {
        crate::state::storage_set(&key, &json);
    }
}

#[derive(Serialize, Deserialize)]
struct CachedTokenInfo {
    session_id: String,
    role: String,
    expires_at_ms: f64,
}

fn load_cached_token_info(token: &str) -> Option<TokenInfo> {
    let key = token_info_cache_key(token);
    let raw = crate::state::storage_get(&key)?;
    let cached: CachedTokenInfo = serde_json::from_str(&raw).ok()?;
    if cached.expires_at_ms <= js_sys::Date::now() {
        crate::state::storage_remove(&key);
        return None;
    }
    Some(TokenInfo {
        session_id: cached.session_id,
        role: cached.role,
        requires_pin: false,
    })
}

fn save_cached_token_info(token: &str, info: &TokenInfo) {
    if info.requires_pin || info.session_id.is_empty() || info.role.is_empty() {
        return;
    }

    let cached = CachedTokenInfo {
        session_id: info.session_id.clone(),
        role: info.role.clone(),
        expires_at_ms: js_sys::Date::now() + TOKEN_INFO_CACHE_TTL_MS,
    };
    if let Ok(json) = serde_json::to_string(&cached) {
        crate::state::storage_set(&token_info_cache_key(token), &json);
    }
}

fn clear_cached_token_info(token: &str) {
    crate::state::storage_remove(&token_info_cache_key(token));
}

// ── Upload response ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct UploadResponse {
    cursor: u64,
    assistant_token: String,
    player_token: String,
    coach_key: String,
}

#[derive(Deserialize)]
struct AppendEventsResponse {
    cursor: u64,
}

#[derive(Deserialize)]
pub struct EventsResponse {
    pub events: Vec<EventEnvelope>,
    pub cursor: u64,
}

#[derive(Deserialize)]
pub struct TokenInfo {
    pub session_id: String,
    pub role: String,
    #[serde(default)]
    pub requires_pin: bool,
}

// ── Fetch helpers ─────────────────────────────────────────────────────────────

pub async fn fetch_post_json(url: &str, body: &str) -> Result<serde_json::Value, String> {
    fetch_json(url, "POST", Some(body.to_string()), None).await
}

fn response_header_summary(resp: &Response) -> String {
    let mut parts = Vec::new();
    for header_name in ["cf-ray", "x-request-id", "content-type"] {
        if let Ok(Some(value)) = resp.headers().get(header_name) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                parts.push(format!("{header_name}={trimmed}"));
            }
        }
    }
    parts.join(", ")
}

fn body_matches_cloudflare_daily_quota_error(raw_body: &str) -> bool {
    let lowercase_body = raw_body.to_ascii_lowercase();
    lowercase_body.contains("error 1027")
        || lowercase_body.contains("error code: 1027")
        || lowercase_body.contains("cloudflare error 1027")
}

fn next_midnight_utc_in_local_timezone_for_display() -> Option<String> {
    Function::new_no_args(
        r#"
        const now = new Date();
        const nextUtcMidnight = new Date(Date.UTC(
            now.getUTCFullYear(),
            now.getUTCMonth(),
            now.getUTCDate() + 1,
            0,
            0,
            0,
            0
        ));
        return nextUtcMidnight.toLocaleString([], {
            weekday: "short",
            month: "short",
            day: "numeric",
            hour: "numeric",
            minute: "2-digit",
            timeZoneName: "short"
        });
    "#,
    )
    .call0(&JsValue::NULL)
    .ok()
    .and_then(|value| value.as_string())
    .filter(|value| !value.trim().is_empty())
}

fn format_cloudflare_daily_quota_error_for_display() -> String {
    let reset_time_text = next_midnight_utc_in_local_timezone_for_display()
        .map(|local_time| format!("midnight UTC ({local_time} in your local time)"))
        .unwrap_or_else(|| "midnight UTC".to_string());
    format!(
        "Cloudflare Worker daily quota reached (Error 1027). Online sync should reset at {reset_time_text}. This coach session is still saved on this device and should remain available offline."
    )
}

fn format_error_body_for_display(raw_body: &str) -> String {
    let trimmed = raw_body.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if body_matches_cloudflare_daily_quota_error(trimmed) {
        return format_cloudflare_daily_quota_error_for_display();
    }

    let Ok(json_value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return trimmed.to_string();
    };

    let Some(object) = json_value.as_object() else {
        return trimmed.to_string();
    };

    let mut parts = Vec::new();
    if let Some(value) = object.get("error").and_then(|value| value.as_str()) {
        parts.push(value.to_string());
    }
    if let Some(value) = object.get("route").and_then(|value| value.as_str()) {
        parts.push(format!("route={value}"));
    }
    if let Some(value) = object.get("message").and_then(|value| value.as_str()) {
        parts.push(format!("message={value}"));
    }
    if let Some(context) = object.get("context").and_then(|value| value.as_object()) {
        for field_name in ["session_id", "event_count", "method", "path"] {
            if let Some(field_value) = context.get(field_name) {
                parts.push(format!("{field_name}={field_value}"));
            }
        }
    }

    if parts.is_empty() {
        trimmed.to_string()
    } else {
        parts.join("; ")
    }
}

/// Low-level fetch. `coach_key` is sent as the `X-Coach-Key` header when provided.
async fn fetch_json<T: DeserializeOwned>(
    url: &str,
    method: &str,
    body: Option<String>,
    coach_key: Option<&str>,
) -> Result<T, String> {
    let opts = RequestInit::new();
    opts.set_method(method);
    let window = web_sys::window().ok_or("no window")?;
    let current_origin = browser_origin().unwrap_or_default();
    let request_origin = parsed_url_origin(url).unwrap_or_default();
    if !current_origin.is_empty() && current_origin == request_origin {
        opts.set_mode(RequestMode::SameOrigin);
    } else {
        opts.set_mode(RequestMode::Cors);
    }

    let headers = web_sys::Headers::new().map_err(|e| format!("{e:?}"))?;
    if body.is_some() {
        headers
            .set("Content-Type", "application/json")
            .map_err(|e| format!("{e:?}"))?;
    }
    if let Some(key) = coach_key {
        if !key.is_empty() {
            headers
                .set("X-Coach-Key", key)
                .map_err(|e| format!("{e:?}"))?;
        }
    }
    opts.set_headers(&headers);

    if let Some(b) = body {
        opts.set_body(&JsValue::from_str(&b));
    }

    let request = Request::new_with_str_and_init(url, &opts).map_err(|e| format!("{e:?}"))?;
    let resp_val = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| format!("Network error while requesting {url}: {e:?}"))?;

    let resp: Response = resp_val.dyn_into().map_err(|_: JsValue| "not a Response")?;
    if !resp.ok() {
        let status = resp.status();
        let header_summary = response_header_summary(&resp);
        let text = match resp.text() {
            Ok(promise) => JsFuture::from(promise)
                .await
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_default(),
            Err(_) => String::new(),
        };
        let body_summary = format_error_body_for_display(&text);

        let mut details = Vec::new();
        if !body_summary.is_empty() {
            details.push(body_summary);
        }
        if !header_summary.is_empty() {
            details.push(header_summary);
        }

        return if details.is_empty() {
            Err(format!("HTTP {status} from {url}"))
        } else {
            Err(format!("HTTP {status} from {url}: {}", details.join(" | ")))
        };
    }

    let text = match resp.text() {
        Ok(promise) => JsFuture::from(promise)
            .await
            .map_err(|e| format!("{e:?}"))?
            .as_string()
            .unwrap_or_default(),
        Err(e) => return Err(format!("{e:?}")),
    };

    serde_json::from_str(&text).map_err(|e| format!("Invalid JSON from {url}: {e}"))
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Upload the full event log to the server ("Go Online").
/// Returns a SyncState with share tokens and coach write key on success.
pub async fn go_online(session_id: &str, events: &[EventEnvelope]) -> Result<SyncState, String> {
    let body = serde_json::json!({
        "session_id": session_id,
        "events": events,
    });
    let url = format!("{}/api/sessions", api_base());
    let resp: UploadResponse = fetch_json(&url, "POST", Some(body.to_string()), None).await?;

    let state = SyncState {
        session_id: session_id.to_string(),
        last_pushed_seq: resp.cursor,
        assistant_token: resp.assistant_token,
        player_token: resp.player_token,
        coach_key: resp.coach_key,
    };
    save_sync_state(&state);
    Ok(state)
}

/// Push new events to the server (events with seq > last_pushed_seq).
/// Sends the stored `coach_key` as `X-Coach-Key` for authorization.
pub async fn push_new_events(
    sync: &mut SyncState,
    all_events: &[EventEnvelope],
) -> Result<(), String> {
    let new_events: Vec<&EventEnvelope> = all_events
        .iter()
        .filter(|e| e.session_version > sync.last_pushed_seq)
        .collect();

    if new_events.is_empty() {
        return Ok(());
    }

    let body = serde_json::json!({ "events": new_events });
    let url = format!("{}/api/sessions/{}/events", api_base(), sync.session_id);
    let resp: AppendEventsResponse =
        fetch_json(&url, "POST", Some(body.to_string()), Some(&sync.coach_key)).await?;
    sync.last_pushed_seq = resp.cursor;
    save_sync_state(sync);
    Ok(())
}

/// Fetch events from server since the given cursor.
/// Used by assistants and players to get the latest state.
pub async fn pull_events(session_id: &str, since: u32) -> Result<EventsResponse, String> {
    let url = format!(
        "{}/api/sessions/{}/events?since={}",
        api_base(),
        session_id,
        since
    );
    fetch_json(&url, "GET", None, None).await
}

/// Resolve a share token to session_id + role.
/// If `requires_pin` is true in the response, the caller must use `auth_token`.
pub async fn resolve_token(token: &str) -> Result<TokenInfo, String> {
    if let Some(info) = load_cached_token_info(token) {
        return Ok(info);
    }

    let url = format!("{}/api/t/{}", api_base(), token);
    let info: TokenInfo = fetch_json(&url, "GET", None, None).await?;
    if info.requires_pin {
        clear_cached_token_info(token);
    } else {
        save_cached_token_info(token, &info);
    }
    Ok(info)
}

/// Authenticate a PIN-protected share token, returning session_id + role on success.
pub async fn auth_token(token: &str, pin: &str) -> Result<TokenInfo, String> {
    let body = serde_json::json!({ "pin": pin });
    let url = format!("{}/api/t/{}/auth", api_base(), token);
    fetch_json(&url, "POST", Some(body.to_string()), None).await
}

/// Set or clear a PIN for a share token (assistant or player link).
/// Pass `pin = ""` to remove the PIN.
pub async fn set_token_pin(token: &str, pin: &str) -> Result<(), String> {
    let body = serde_json::json!({ "pin": pin });
    let url = format!("{}/api/t/{}/pin", api_base(), token);
    let _: serde_json::Value = fetch_json(&url, "POST", Some(body.to_string()), None).await?;
    clear_cached_token_info(token);
    Ok(())
}

/// Set or update the coach recovery PIN for a session in D1.
pub async fn set_recovery_pin(sync: &SyncState, pin: &str) -> Result<(), String> {
    let body = serde_json::json!({ "pin": pin });
    let url = format!("{}/api/sessions/{}/pin", api_base(), sync.session_id);
    let _: serde_json::Value =
        fetch_json(&url, "POST", Some(body.to_string()), Some(&sync.coach_key)).await?;
    Ok(())
}

/// Validate PIN and return all events for cross-device coach recovery.
pub async fn recover_session(session_id: &str, pin: &str) -> Result<EventsResponse, String> {
    let body = serde_json::json!({ "pin": pin });
    let url = format!("{}/api/sessions/{}/recover", api_base(), session_id);
    fetch_json(&url, "POST", Some(body.to_string()), None).await
}

// ── Archive snapshot ──────────────────────────────────────────────────────────

/// Summary pushed to POST /api/sessions/:id/archive whenever rankings are synced.
/// Survives the raw event-log retention window (90 days) for up to 365 days.
#[derive(Serialize)]
pub struct SessionArchive {
    pub sport: String,
    pub score_entry_mode: String,
    pub team_size: u8,
    /// Map of player_id (as string) → player name, e.g. `{"1": "Alice", "2": "Bob"}`.
    pub player_names: HashMap<String, String>,
    /// Latest computed rankings; `None` if rankings have not been computed yet.
    pub final_rankings: Option<Vec<PlayerRanking>>,
}

/// Push a session archive snapshot to the server.
///
/// Best-effort: the caller should ignore errors — a missing archive is not
/// fatal since offline sessions never go online and are stored in localStorage.
pub async fn push_session_archive(
    sync: &SyncState,
    archive: &SessionArchive,
) -> Result<(), String> {
    if archive.final_rankings.is_none() {
        return Ok(());
    }

    let body = serde_json::to_string(archive).map_err(|e| e.to_string())?;
    let url = format!("{}/api/sessions/{}/archive", api_base(), sync.session_id);
    let _: serde_json::Value = fetch_json(&url, "POST", Some(body), Some(&sync.coach_key)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        body_matches_cloudflare_daily_quota_error, format_error_body_for_display,
        reflected_property_string,
    };
    use wasm_bindgen::JsValue;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    fn reflected_property_string_reads_plain_string_fields() {
        let object = js_sys::Object::new();
        assert!(js_sys::Reflect::set(
            &object,
            &JsValue::from_str("origin"),
            &JsValue::from_str("https://pcplayerpicker.com")
        )
        .expect("Reflect.set should succeed"));

        assert_eq!(
            reflected_property_string(object.as_ref(), "origin"),
            Some("https://pcplayerpicker.com".to_string())
        );
        assert_eq!(reflected_property_string(object.as_ref(), "missing"), None);
    }

    #[test]
    fn format_error_body_for_display_includes_high_signal_context() {
        let body = r#"{
            "error":"internal_server_error",
            "route":"POST /api/sessions",
            "message":"D1_TYPE_ERROR",
            "context":{"session_id":"abc","event_count":11}
        }"#;

        assert_eq!(
            format_error_body_for_display(body),
            "internal_server_error; route=POST /api/sessions; message=D1_TYPE_ERROR; session_id=\"abc\"; event_count=11"
        );
    }

    #[test]
    fn body_matches_cloudflare_daily_quota_error_for_html_error_pages() {
        let body =
            r#"<html><title>Cloudflare Error 1027</title><body>error code: 1027</body></html>"#;
        assert!(body_matches_cloudflare_daily_quota_error(body));
    }

    #[wasm_bindgen_test]
    fn format_error_body_for_display_rewrites_cloudflare_daily_quota_errors() {
        let formatted = format_error_body_for_display("Cloudflare Error 1027");
        assert!(formatted.contains("Cloudflare Worker daily quota reached (Error 1027)."));
        assert!(formatted.contains("midnight UTC"));
        assert!(formatted.contains("remain available offline"));
    }
}
