//! App-wide reactive state and durable browser persistence.
//!
//! The source of truth is the `EventLog` inside `SessionManager`. All UI
//! components read derived `SessionState` reactively through the `AppContext`.
//! Full session event logs are stored in IDB and OPFS. localStorage is only
//! used for small synchronous metadata like preferences, the session index, and
//! session summaries.
//!
//! localStorage layout:
//!   `pcpp_sessions`           → JSON Vec<String>  (session UUID list)
//!   `pcpp_summary_{uuid}`     → JSON SessionSummary
//!
//! On app startup, restore tasks rebuild the localStorage session index and
//! summary cache from IDB/OPFS before the rest of the UI relies on it.

pub mod idb;
pub mod opfs;

use app_core::events::EventLog;
use app_core::session::SessionManager;
use leptos::prelude::*;
use serde::{Deserialize, Serialize};

// ── Storage keys ─────────────────────────────────────────────────────────────

const SESSION_LIST_KEY: &str = "pcpp_sessions";
const DARK_MODE_KEY: &str = "pcpp_dark_mode";

fn summary_key(id: &str) -> String {
    format!("pcpp_summary_{id}")
}

// ── App context ───────────────────────────────────────────────────────────────

/// App-wide context provided at the root and consumed by any component.
#[derive(Clone)]
pub struct AppContext {
    /// The currently-loaded session. `None` when on the home / setup pages.
    pub session: RwSignal<Option<SessionManager>>,
    /// Dark mode preference — `true` = dark.
    pub dark_mode: RwSignal<bool>,
    /// True while startup restore is scanning IDB/OPFS and repopulating localStorage.
    pub storage_restore_in_progress: RwSignal<bool>,
    /// Incremented after a restore pass so pages can re-read local storage.
    pub storage_restore_epoch: RwSignal<u32>,
}

impl AppContext {
    pub fn new() -> Self {
        // Read persisted preference; if absent, use system preference.
        let dark = storage_get(DARK_MODE_KEY)
            .map(|v| v != "light")
            .unwrap_or_else(system_prefers_dark);
        Self {
            session: RwSignal::new(None),
            dark_mode: RwSignal::new(dark),
            storage_restore_in_progress: RwSignal::new(false),
            storage_restore_epoch: RwSignal::new(0),
        }
    }
}

fn system_prefers_dark() -> bool {
    web_sys::window()
        .and_then(|w| w.match_media("(prefers-color-scheme: dark)").ok().flatten())
        .map(|m| m.matches())
        .unwrap_or(true)
}

fn sync_mobile_browser_theme(dark: bool) {
    let Some(win) = web_sys::window() else {
        return;
    };
    let Some(doc) = win.document() else {
        return;
    };

    if let Ok(Some(meta)) = doc.query_selector("meta[name='theme-color']") {
        let _ = meta.set_attribute("content", if dark { "#000000" } else { "#f8fafc" });
    }
    if let Ok(Some(meta)) = doc.query_selector("meta[name='apple-mobile-web-app-status-bar-style']")
    {
        let _ = meta.set_attribute(
            "content",
            if dark { "black-translucent" } else { "default" },
        );
    }
}

/// Apply the dark-mode class to `<html>`.
pub fn apply_dark_mode_class(dark: bool) {
    if let Some(win) = web_sys::window() {
        if let Some(doc) = win.document() {
            if let Some(root) = doc.document_element() {
                let list = root.class_list();
                if dark {
                    let _ = list.add_1("dark");
                } else {
                    let _ = list.remove_1("dark");
                }
            }
        }
    }
    sync_mobile_browser_theme(dark);
}

/// Persist and apply the dark-mode class to `<html>`.
pub fn apply_dark_mode(dark: bool) {
    storage_set(DARK_MODE_KEY, if dark { "dark" } else { "light" });
    apply_dark_mode_class(dark);
}

// ── localStorage helpers ──────────────────────────────────────────────────────

fn get_storage() -> Option<web_sys::Storage> {
    web_sys::window()
        .and_then(|w| w.local_storage().ok())
        .flatten()
}

pub fn storage_get(key: &str) -> Option<String> {
    get_storage()?.get_item(key).ok().flatten()
}

pub fn storage_set(key: &str, value: &str) {
    if let Some(s) = get_storage() {
        let _ = s.set_item(key, value);
    }
}

pub fn storage_remove(key: &str) {
    if let Some(s) = get_storage() {
        let _ = s.remove_item(key);
    }
}

const DEVICE_ID_KEY: &str = "pcpp_device_id";

/// Return a stable random device ID for this browser, generating one if needed.
/// Used by the heartbeat feature to identify which coach device is active.
pub fn get_or_create_device_id() -> String {
    if let Some(id) = storage_get(DEVICE_ID_KEY) {
        if !id.is_empty() {
            return id;
        }
    }
    // Build a pseudo-random ID from timestamp + Math.random() — sufficient for
    // a device identifier (not cryptographic).
    let ts = js_sys::Date::now() as u64;
    let rnd = (js_sys::Math::random() * f64::from(u32::MAX)) as u64;
    let id = format!("{ts:016x}{rnd:08x}");
    storage_set(DEVICE_ID_KEY, &id);
    id
}

// ── Session persistence ───────────────────────────────────────────────────────

/// Persist the event log for a session to IDB and OPFS, while keeping only
/// lightweight metadata in localStorage.
pub fn save_session(manager: &SessionManager) {
    let config = match &manager.state.config {
        Some(c) => c,
        None => return,
    };
    let id = config.id.to_string();

    if let Ok(json) = serde_json::to_string(manager.log.all()) {
        // Async durable writes — fire and forget, failures are non-fatal.
        idb::save_to_idb(id.clone(), json.clone());
        opfs::save_to_opfs(id.clone(), json);
    }

    if let Some(summary) = session_summary_from_manager(manager) {
        save_summary_cache(&summary);
    }

    // Add to session list if not already present.
    let mut ids = load_session_ids();
    if !ids.contains(&id) {
        ids.push(id);
        if let Ok(json) = serde_json::to_string(&ids) {
            storage_set(SESSION_LIST_KEY, &json);
        }
    }
}

fn parse_session_manager(events_json: &str) -> Option<SessionManager> {
    let envelopes = serde_json::from_str(events_json).ok()?;
    let log = EventLog::from_saved(envelopes);
    Some(SessionManager::from_log(log))
}

fn save_summary_cache(summary: &SessionSummary) {
    if let Ok(json) = serde_json::to_string(summary) {
        storage_set(&summary_key(&summary.id), &json);
    }
}

fn latest_session_version(manager: &SessionManager) -> u64 {
    manager
        .log
        .all()
        .last()
        .map(|event| event.session_version)
        .unwrap_or(0)
}

fn choose_fresher_session(
    left: Option<SessionManager>,
    right: Option<SessionManager>,
) -> Option<SessionManager> {
    match (left, right) {
        (Some(a), Some(b)) => {
            if latest_session_version(&a) >= latest_session_version(&b) {
                Some(a)
            } else {
                Some(b)
            }
        }
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Load a `SessionManager` from durable browser storage by UUID string.
/// Returns `None` if the session does not exist in either IDB or OPFS.
pub async fn load_session(id: &str) -> Option<SessionManager> {
    let idb_manager = idb::load_from_idb(id)
        .await
        .as_deref()
        .and_then(parse_session_manager);
    let opfs_manager = opfs::load_from_opfs(id)
        .await
        .as_deref()
        .and_then(parse_session_manager);

    let manager = choose_fresher_session(idb_manager, opfs_manager)?;
    if let Some(summary) = session_summary_from_manager(&manager) {
        save_summary_cache(&summary);
    }
    Some(manager)
}

/// Delete a session from localStorage metadata, IDB, and OPFS.
pub fn delete_session(id: &str) {
    storage_remove(&summary_key(id));
    let mut ids = load_session_ids();
    ids.retain(|i| i != id);
    if let Ok(json) = serde_json::to_string(&ids) {
        storage_set(SESSION_LIST_KEY, &json);
    }
    idb::delete_from_idb(id.to_string());
    opfs::delete_from_opfs(id.to_string());
}

/// Recover session ids and summaries from IDB when localStorage metadata was
/// evicted.
///
/// Called once at app startup. If IDB contains sessions that localStorage no
/// longer knows about, rebuild the local metadata cache so the rest of the app
/// sees them normally.
pub async fn restore_sessions_from_idb() {
    let idb_ids = idb::load_ids_from_idb().await;
    if idb_ids.is_empty() {
        return;
    }
    let local_ids = load_session_ids();
    let needs_metadata: Vec<String> = idb_ids
        .into_iter()
        .filter(|id| !local_ids.contains(id) || storage_get(&summary_key(id)).is_none())
        .collect();
    if needs_metadata.is_empty() {
        return;
    }
    let mut ids = local_ids;
    for id in needs_metadata {
        if let Some(events_json) = idb::load_from_idb(&id).await {
            if let Some(manager) = parse_session_manager(&events_json) {
                if let Some(summary) = session_summary_from_manager(&manager) {
                    save_summary_cache(&summary);
                }
            }
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    if let Ok(json) = serde_json::to_string(&ids) {
        storage_set(SESSION_LIST_KEY, &json);
    }
}

/// Recover session ids and summaries from OPFS that were evicted from both
/// localStorage and IDB.
///
/// Called once at startup after `restore_sessions_from_idb()`. OPFS is a
/// secondary recovery layer for session logs, while localStorage only keeps the
/// synchronous metadata cache.
pub async fn restore_sessions_from_opfs() {
    let opfs_ids = opfs::load_ids_from_opfs().await;
    if opfs_ids.is_empty() {
        return;
    }
    let local_ids = load_session_ids();
    let needs_metadata: Vec<String> = opfs_ids
        .into_iter()
        .filter(|id| !local_ids.contains(id) || storage_get(&summary_key(id)).is_none())
        .collect();
    if needs_metadata.is_empty() {
        return;
    }
    let mut ids = local_ids;
    for id in needs_metadata {
        if let Some(events_json) = opfs::load_from_opfs(&id).await {
            if let Some(manager) = parse_session_manager(&events_json) {
                if let Some(summary) = session_summary_from_manager(&manager) {
                    save_summary_cache(&summary);
                }
            }
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    if let Ok(json) = serde_json::to_string(&ids) {
        storage_set(SESSION_LIST_KEY, &json);
    }
}

/// All stored session UUID strings, in insertion order.
pub fn load_session_ids() -> Vec<String> {
    storage_get(SESSION_LIST_KEY)
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

// ── Session summary ───────────────────────────────────────────────────────────

/// Lightweight summary shown on the coach home screen, derived from the event log.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub sport: String,
    pub team_size: u8,
    pub player_count: usize,
    pub rounds_played: u32,
    pub created_at: String,
}

fn session_summary_from_manager(manager: &SessionManager) -> Option<SessionSummary> {
    let config = manager.state.config.as_ref()?;
    Some(SessionSummary {
        id: config.id.to_string(),
        sport: config.sport.to_string(),
        team_size: config.team_size,
        player_count: manager.state.players.len(),
        rounds_played: manager.state.current_round.0.saturating_sub(1),
        created_at: config.created_at.format("%Y-%m-%d %H:%M").to_string(),
    })
}

/// Read summaries for all stored sessions from the localStorage metadata cache.
pub fn load_session_summaries() -> Vec<SessionSummary> {
    load_session_ids()
        .iter()
        .filter_map(|id| storage_get(&summary_key(id)))
        .filter_map(|json| serde_json::from_str(&json).ok())
        .rev() // newest first
        .collect()
}
