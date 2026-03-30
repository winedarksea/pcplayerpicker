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
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestInit, RequestMode, Response};

/// The base API URL. In production this is the same origin; during local dev
/// it points to the wrangler dev server.
pub fn api_base() -> String {
    if let Some(window) = web_sys::window() {
        let origin = window.location().origin().unwrap_or_default();
        // If running on localhost:8080 (trunk serve), hit wrangler on 8787
        if origin.contains("localhost:8080") {
            return "http://localhost:8787".to_string();
        }
        if origin == "https://www.pcplayerpicker.com" {
            return "https://pcplayerpicker.com".to_string();
        }
        return origin;
    }
    String::new()
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

fn sync_key(session_id: &str) -> String {
    format!("{SYNC_KEY_PREFIX}{session_id}")
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

// ── Upload response ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct UploadResponse {
    cursor: u64,
    assistant_token: String,
    player_token: String,
    coach_key: String,
}

#[derive(Deserialize)]
pub struct EventsResponse {
    pub events: Vec<EventEnvelope>,
}

#[derive(Deserialize)]
pub struct TokenInfo {
    pub session_id: String,
    pub role: String,
    #[serde(default)]
    pub requires_pin: bool,
}

// ── Fetch helpers ─────────────────────────────────────────────────────────────

pub async fn fetch_post_json(url: &str, body: &str) -> Result<JsValue, String> {
    fetch_json(url, "POST", Some(body.to_string()), None).await
}

/// Low-level fetch. `coach_key` is sent as the `X-Coach-Key` header when provided.
async fn fetch_json(
    url: &str,
    method: &str,
    body: Option<String>,
    coach_key: Option<&str>,
) -> Result<JsValue, String> {
    let opts = RequestInit::new();
    opts.set_method(method);
    let window = web_sys::window().ok_or("no window")?;
    let current_origin = window.location().origin().unwrap_or_default();
    let request_origin = web_sys::Url::new(url)
        .ok()
        .map(|parsed| parsed.origin())
        .unwrap_or_default();
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
        let text = match resp.text() {
            Ok(promise) => JsFuture::from(promise)
                .await
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_default(),
            Err(_) => String::new(),
        };
        let text = text.trim();
        return if text.is_empty() {
            Err(format!("HTTP {status} from {url}"))
        } else {
            Err(format!("HTTP {status} from {url}: {text}"))
        };
    }

    let json_promise = resp.json().map_err(|e| format!("{e:?}"))?;
    JsFuture::from(json_promise)
        .await
        .map_err(|e| format!("{e:?}"))
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
    let resp_val = fetch_json(&url, "POST", Some(body.to_string()), None).await?;

    let resp: UploadResponse =
        serde_wasm_bindgen::from_value(resp_val).map_err(|e| e.to_string())?;

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
    fetch_json(&url, "POST", Some(body.to_string()), Some(&sync.coach_key)).await?;

    if let Some(last) = new_events.last() {
        sync.last_pushed_seq = last.session_version;
        save_sync_state(sync);
    }
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
    let resp_val = fetch_json(&url, "GET", None, None).await?;
    serde_wasm_bindgen::from_value(resp_val).map_err(|e| e.to_string())
}

/// Resolve a share token to session_id + role.
/// If `requires_pin` is true in the response, the caller must use `auth_token`.
pub async fn resolve_token(token: &str) -> Result<TokenInfo, String> {
    let url = format!("{}/api/t/{}", api_base(), token);
    let resp_val = fetch_json(&url, "GET", None, None).await?;
    serde_wasm_bindgen::from_value(resp_val).map_err(|e| e.to_string())
}

/// Authenticate a PIN-protected share token, returning session_id + role on success.
pub async fn auth_token(token: &str, pin: &str) -> Result<TokenInfo, String> {
    let body = serde_json::json!({ "pin": pin });
    let url = format!("{}/api/t/{}/auth", api_base(), token);
    let resp_val = fetch_json(&url, "POST", Some(body.to_string()), None).await?;
    serde_wasm_bindgen::from_value(resp_val).map_err(|e| e.to_string())
}

/// Set or clear a PIN for a share token (assistant or player link).
/// Pass `pin = ""` to remove the PIN.
pub async fn set_token_pin(token: &str, pin: &str) -> Result<(), String> {
    let body = serde_json::json!({ "pin": pin });
    let url = format!("{}/api/t/{}/pin", api_base(), token);
    fetch_json(&url, "POST", Some(body.to_string()), None)
        .await
        .map(|_| ())
}

/// Set or update the coach recovery PIN for a session in D1.
pub async fn set_recovery_pin(sync: &SyncState, pin: &str) -> Result<(), String> {
    let body = serde_json::json!({ "pin": pin });
    let url = format!("{}/api/sessions/{}/pin", api_base(), sync.session_id);
    fetch_json(&url, "POST", Some(body.to_string()), Some(&sync.coach_key))
        .await
        .map(|_| ())
}

/// Validate PIN and return all events for cross-device coach recovery.
pub async fn recover_session(session_id: &str, pin: &str) -> Result<EventsResponse, String> {
    let body = serde_json::json!({ "pin": pin });
    let url = format!("{}/api/sessions/{}/recover", api_base(), session_id);
    let resp_val = fetch_json(&url, "POST", Some(body.to_string()), None).await?;
    serde_wasm_bindgen::from_value(resp_val).map_err(|e| e.to_string())
}

// ── Archive snapshot ──────────────────────────────────────────────────────────

/// Summary pushed to POST /api/sessions/:id/archive whenever rankings are synced.
/// Survives the raw event-log retention window (90 days) for up to 365 days.
#[derive(Serialize)]
pub struct SessionArchive {
    pub sport: String,
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
    let body = serde_json::to_string(archive).map_err(|e| e.to_string())?;
    let url = format!("{}/api/sessions/{}/archive", api_base(), sync.session_id);
    fetch_json(&url, "POST", Some(body), Some(&sync.coach_key))
        .await
        .map(|_| ())
}

// ── Heartbeat ─────────────────────────────────────────────────────────────────

/// An active coach device as returned by GET /api/sessions/:id/heartbeat.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DeviceHeartbeat {
    pub device_id: String,
    #[serde(default)]
    pub label: String,
    pub last_seen: f64,
}

/// Record this device's presence on the server. Fire-and-forget.
///
/// `device_id` should be a stable UUID stored in localStorage.
/// `label` is an optional human-readable string (e.g. "iPhone 15 · Safari").
pub async fn send_heartbeat(sync: &SyncState, device_id: &str, label: &str) -> Result<(), String> {
    let body = serde_json::json!({ "device_id": device_id, "label": label });
    let url = format!("{}/api/sessions/{}/heartbeat", api_base(), sync.session_id);
    fetch_json(&url, "POST", Some(body.to_string()), Some(&sync.coach_key))
        .await
        .map(|_| ())
}

/// Fetch the list of devices that have sent a heartbeat in the last 5 minutes.
pub async fn fetch_active_devices(sync: &SyncState) -> Result<Vec<DeviceHeartbeat>, String> {
    let url = format!("{}/api/sessions/{}/heartbeat", api_base(), sync.session_id);
    let resp_val = fetch_json(&url, "GET", None, Some(&sync.coach_key)).await?;

    #[derive(Deserialize)]
    struct Resp {
        devices: Vec<DeviceHeartbeat>,
    }
    let resp: Resp = serde_wasm_bindgen::from_value(resp_val).map_err(|e| e.to_string())?;
    Ok(resp.devices)
}
