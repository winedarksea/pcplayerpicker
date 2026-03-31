use crate::state::{storage_get, storage_remove, storage_set};
use app_core::events::{EventEnvelope, EventLog};
use serde::{Deserialize, Serialize};

const READ_ONLY_EVENT_CACHE_PREFIX: &str = "pcpp_read_only_events_";
const READ_ONLY_EVENT_CACHE_TTL_MS: f64 = 12.0 * 60.0 * 60.0 * 1000.0;

#[derive(Serialize, Deserialize)]
struct CachedReadOnlyEventLog {
    saved_at_ms: f64,
    events: Vec<EventEnvelope>,
}

fn read_only_event_cache_key(session_id: &str) -> String {
    format!("{READ_ONLY_EVENT_CACHE_PREFIX}{session_id}")
}

fn cache_is_stale(saved_at_ms: f64, now_ms: f64) -> bool {
    saved_at_ms + READ_ONLY_EVENT_CACHE_TTL_MS <= now_ms
}

pub fn load_cached_read_only_event_log(session_id: &str) -> Option<EventLog> {
    let key = read_only_event_cache_key(session_id);
    let raw = storage_get(&key)?;
    let cached: CachedReadOnlyEventLog = serde_json::from_str(&raw).ok()?;
    if cache_is_stale(cached.saved_at_ms, js_sys::Date::now()) {
        storage_remove(&key);
        return None;
    }
    Some(EventLog::from_saved(cached.events))
}

pub fn save_cached_read_only_event_log(session_id: &str, log: &EventLog) {
    let cached = CachedReadOnlyEventLog {
        saved_at_ms: js_sys::Date::now(),
        events: log.all().to_vec(),
    };
    if let Ok(json) = serde_json::to_string(&cached) {
        storage_set(&read_only_event_cache_key(session_id), &json);
    }
}

#[cfg(test)]
mod tests {
    use super::{cache_is_stale, READ_ONLY_EVENT_CACHE_TTL_MS};

    #[test]
    fn cache_expires_at_the_ttl_boundary() {
        let now_ms = 50_000.0;
        let fresh_saved_at_ms = now_ms - (READ_ONLY_EVENT_CACHE_TTL_MS - 1.0);
        let expired_saved_at_ms = now_ms - READ_ONLY_EVENT_CACHE_TTL_MS;

        assert!(!cache_is_stale(fresh_saved_at_ms, now_ms));
        assert!(cache_is_stale(expired_saved_at_ms, now_ms));
    }
}
