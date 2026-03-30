//! IndexedDB backup for session event logs.
//!
//! Safari's ITP policy aggressively evicts localStorage after ~7 days of
//! inactivity. IndexedDB is treated as "persistent" when the user has added
//! the PWA to their home screen *and* `navigator.storage.persist()` was
//! granted. Even without that grant, IDB is evicted less aggressively than
//! localStorage.
//!
//! Strategy:
//! - Every `save_session()` writes the full event log to IDB (fire-and-forget).
//! - `restore_sessions_from_idb()` is called once at app startup: it scans IDB
//!   for session IDs that are missing from localStorage metadata and rebuilds
//!   their summary/index cache. This silently recovers data lost to metadata
//!   eviction.
//! - `delete_from_idb()` removes a session from IDB on explicit delete.
//!
//! All IDB operations are async and spawned via `leptos::task::spawn_local`.
//! They never block the UI and failures are silently ignored.

use js_sys::{Array, Promise};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{IdbDatabase, IdbOpenDbRequest, IdbTransactionMode};

const DB_NAME: &str = "pcplayerpicker_v1";
const EVENTS_STORE: &str = "session_events";
/// IDB schema version — bump when adding new object stores.
const DB_VERSION: u32 = 1;

// ── Promise helper ────────────────────────────────────────────────────────────

/// Wrap an `IdbRequest` in a `Promise` that resolves with the request result.
fn request_to_promise(req: web_sys::IdbRequest) -> Promise {
    Promise::new(&mut move |resolve, reject| {
        let req_s = req.clone();
        let req_e = req.clone();
        let onsuccess: Closure<dyn FnMut(web_sys::Event)> =
            Closure::once(move |_: web_sys::Event| {
                let val = req_s.result().unwrap_or(JsValue::UNDEFINED);
                let _ = resolve.call1(&JsValue::UNDEFINED, &val);
            });
        let onerror: Closure<dyn FnMut(web_sys::Event)> =
            Closure::once(move |_: web_sys::Event| {
                let _ = reject.call0(&JsValue::UNDEFINED);
            });
        req_e.set_onsuccess(Some(onsuccess.as_ref().unchecked_ref()));
        req_e.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        onsuccess.forget();
        onerror.forget();
    })
}

// ── Open ──────────────────────────────────────────────────────────────────────

/// Open (or create) the PCPlayerPicker IDB database.
/// Returns `None` on any error so callers can silently degrade.
async fn open_db() -> Option<IdbDatabase> {
    let window = web_sys::window()?;
    let factory = window.indexed_db().ok()??;
    let open_req: IdbOpenDbRequest = factory.open_with_u32(DB_NAME, DB_VERSION).ok()?;

    // Create the object store on first open / version upgrade.
    // `onupgradeneeded` fires only when the DB is new or the version is bumped.
    let on_upgrade: Closure<dyn FnMut(web_sys::Event)> =
        Closure::once(move |evt: web_sys::Event| {
            let target: IdbOpenDbRequest = match evt.target().and_then(|t| t.dyn_into().ok()) {
                Some(r) => r,
                None => return,
            };
            let db: IdbDatabase = match target.result().ok().and_then(|v| v.dyn_into().ok()) {
                Some(d) => d,
                None => return,
            };
            // Ignore "already exists" error — safe on version bumps.
            let _ = db.create_object_store(EVENTS_STORE);
        });
    open_req.set_onupgradeneeded(Some(on_upgrade.as_ref().unchecked_ref()));
    on_upgrade.forget();

    // Convert the open request to a promise and await it.
    let open_promise = Promise::new(&mut {
        let req = open_req.clone();
        move |resolve, reject| {
            let req_s = req.clone();
            let req_e = req.clone();
            let onsuccess: Closure<dyn FnMut(web_sys::Event)> =
                Closure::once(move |_: web_sys::Event| {
                    let val = req_s.result().unwrap_or(JsValue::UNDEFINED);
                    let _ = resolve.call1(&JsValue::UNDEFINED, &val);
                });
            let onerror: Closure<dyn FnMut(web_sys::Event)> =
                Closure::once(move |_: web_sys::Event| {
                    let _ = reject.call0(&JsValue::UNDEFINED);
                });
            req_e.set_onsuccess(Some(onsuccess.as_ref().unchecked_ref()));
            req_e.set_onerror(Some(onerror.as_ref().unchecked_ref()));
            onsuccess.forget();
            onerror.forget();
        }
    });

    JsFuture::from(open_promise)
        .await
        .ok()
        .and_then(|v| v.dyn_into::<IdbDatabase>().ok())
}

// ── Write ─────────────────────────────────────────────────────────────────────

/// Persist `events_json` for `session_id` in IDB. Fire-and-forget.
pub fn save_to_idb(session_id: String, events_json: String) {
    leptos::task::spawn_local(async move {
        let _ = save_to_idb_inner(&session_id, &events_json).await;
    });
}

async fn save_to_idb_inner(session_id: &str, events_json: &str) -> Option<()> {
    let db = open_db().await?;
    let tx = db
        .transaction_with_str_and_mode(EVENTS_STORE, IdbTransactionMode::Readwrite)
        .ok()?;
    let store = tx.object_store(EVENTS_STORE).ok()?;
    let key = JsValue::from_str(session_id);
    let val = JsValue::from_str(events_json);
    let req = store.put_with_key(&val, &key).ok()?;
    JsFuture::from(request_to_promise(req)).await.ok()?;
    Some(())
}

// ── Read ──────────────────────────────────────────────────────────────────────

/// Load the events JSON for `session_id` from IDB, or `None` if not found.
pub async fn load_from_idb(session_id: &str) -> Option<String> {
    let db = open_db().await?;
    let tx = db.transaction_with_str(EVENTS_STORE).ok()?;
    let store = tx.object_store(EVENTS_STORE).ok()?;
    let key = JsValue::from_str(session_id);
    let req = store.get(&key).ok()?;
    let result = JsFuture::from(request_to_promise(req)).await.ok()?;
    if result.is_undefined() || result.is_null() {
        return None;
    }
    result.as_string()
}

/// Return all session IDs stored in IDB.
pub async fn load_ids_from_idb() -> Vec<String> {
    let Some(db) = open_db().await else {
        return vec![];
    };
    let Ok(tx) = db.transaction_with_str(EVENTS_STORE) else {
        return vec![];
    };
    let Ok(store) = tx.object_store(EVENTS_STORE) else {
        return vec![];
    };
    let Ok(req) = store.get_all_keys() else {
        return vec![];
    };
    match JsFuture::from(request_to_promise(req)).await {
        Ok(val) => match val.dyn_into::<Array>() {
            Ok(arr) => arr.iter().filter_map(|v| v.as_string()).collect(),
            Err(_) => vec![],
        },
        Err(_) => vec![],
    }
}

// ── Delete ────────────────────────────────────────────────────────────────────

/// Remove a session from IDB. Fire-and-forget.
pub fn delete_from_idb(session_id: String) {
    leptos::task::spawn_local(async move {
        let _ = delete_from_idb_inner(&session_id).await;
    });
}

async fn delete_from_idb_inner(session_id: &str) -> Option<()> {
    let db = open_db().await?;
    let tx = db
        .transaction_with_str_and_mode(EVENTS_STORE, IdbTransactionMode::Readwrite)
        .ok()?;
    let store = tx.object_store(EVENTS_STORE).ok()?;
    let key = JsValue::from_str(session_id);
    let req = store.delete(&key).ok()?;
    JsFuture::from(request_to_promise(req)).await.ok()?;
    Some(())
}
