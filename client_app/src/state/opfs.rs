//! OPFS (Origin Private File System) storage for session event logs.
//!
//! Safari 16.4+ (iOS/macOS) supports OPFS and treats files written there as
//! truly persistent app data — NOT subject to the 7-day eviction cap that
//! applies to localStorage and IndexedDB when the PWA is not on the Home
//! Screen. This makes OPFS the correct layer for months-long persistence on
//! Apple devices.
//!
//! Strategy:
//!   - Every `save_session()` writes to localStorage → IDB → OPFS (all three).
//!   - `restore_sessions_from_opfs()` is called once at startup after IDB
//!     restore. It recovers any sessions that IDB also lost (edge case).
//!   - `delete_from_opfs()` is called on explicit session delete.
//!
//! Data layout in the OPFS root directory:
//!   pcpp_index.json           → JSON Vec<String>   (session UUID list)
//!   pcpp_events_{uuid}.json   → JSON Vec<EventEnvelope>
//!
//! All operations are async and spawned via `leptos::task::spawn_local`.
//! Browsers that don't support OPFS (Safari < 16.4, older Chromium) return
//! None from `get_opfs_root()` and degrade silently — localStorage + IDB
//! remain the active layers.

use js_sys::{Object, Promise, Reflect};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen::JsValue;

const INDEX_FILENAME: &str = "pcpp_index.json";

fn events_filename(session_id: &str) -> String {
    format!("pcpp_events_{session_id}.json")
}

// ── OPFS root ─────────────────────────────────────────────────────────────────

/// Get the OPFS root `FileSystemDirectoryHandle` via
/// `navigator.storage.getDirectory()`. Returns `None` if OPFS is not
/// supported in the current browser/context.
async fn get_opfs_root() -> Option<JsValue> {
    let window = web_sys::window()?;
    let storage = window.navigator().storage();
    // Call storage.getDirectory() — may not exist on older Safari
    let get_dir = Reflect::get(&storage, &JsValue::from_str("getDirectory")).ok()?;
    if get_dir.is_undefined() || get_dir.is_null() {
        return None;
    }
    let get_dir: js_sys::Function = get_dir.dyn_into().ok()?;
    let promise: Promise = get_dir.call0(&storage).ok()?.dyn_into().ok()?;
    JsFuture::from(promise).await.ok()
}

// ── Low-level read / write / delete ──────────────────────────────────────────

/// Write `content` (UTF-8 string) to `filename` in the OPFS root.
/// Creates or overwrites the file atomically via `createWritable()`.
async fn opfs_write(root: &JsValue, filename: &str, content: &str) -> Option<()> {
    // root.getFileHandle(filename, { create: true })
    let opts = Object::new();
    Reflect::set(&opts, &JsValue::from_str("create"), &JsValue::TRUE).ok()?;
    let get_fh: js_sys::Function = Reflect::get(root, &JsValue::from_str("getFileHandle"))
        .ok()?
        .dyn_into()
        .ok()?;
    let fh_promise: Promise = get_fh
        .call2(root, &JsValue::from_str(filename), &opts)
        .ok()?
        .dyn_into()
        .ok()?;
    let fh = JsFuture::from(fh_promise).await.ok()?;

    // fh.createWritable()
    let create_writable: js_sys::Function =
        Reflect::get(&fh, &JsValue::from_str("createWritable"))
            .ok()?
            .dyn_into()
            .ok()?;
    let w_promise: Promise = create_writable
        .call0(&fh)
        .ok()?
        .dyn_into()
        .ok()?;
    let w = JsFuture::from(w_promise).await.ok()?;

    // w.write(content_string)
    let write_fn: js_sys::Function = Reflect::get(&w, &JsValue::from_str("write"))
        .ok()?
        .dyn_into()
        .ok()?;
    let wr_promise: Promise = write_fn
        .call1(&w, &JsValue::from_str(content))
        .ok()?
        .dyn_into()
        .ok()?;
    JsFuture::from(wr_promise).await.ok()?;

    // w.close()
    let close_fn: js_sys::Function = Reflect::get(&w, &JsValue::from_str("close"))
        .ok()?
        .dyn_into()
        .ok()?;
    let cl_promise: Promise = close_fn.call0(&w).ok()?.dyn_into().ok()?;
    JsFuture::from(cl_promise).await.ok()?;

    Some(())
}

/// Read `filename` from the OPFS root. Returns `None` if not found.
async fn opfs_read(root: &JsValue, filename: &str) -> Option<String> {
    // root.getFileHandle(filename, { create: false })  — throws if missing
    let opts = Object::new();
    Reflect::set(&opts, &JsValue::from_str("create"), &JsValue::FALSE).ok()?;
    let get_fh: js_sys::Function = Reflect::get(root, &JsValue::from_str("getFileHandle"))
        .ok()?
        .dyn_into()
        .ok()?;
    let fh_promise: Promise = get_fh
        .call2(root, &JsValue::from_str(filename), &opts)
        .ok()?
        .dyn_into()
        .ok()?;
    // A missing file rejects the promise — JsFuture returns Err, we return None.
    let fh = JsFuture::from(fh_promise).await.ok()?;

    // fh.getFile() → File
    let get_file: js_sys::Function = Reflect::get(&fh, &JsValue::from_str("getFile"))
        .ok()?
        .dyn_into()
        .ok()?;
    let file_promise: Promise = get_file.call0(&fh).ok()?.dyn_into().ok()?;
    let file = JsFuture::from(file_promise).await.ok()?;

    // file.text() → string
    let text_fn: js_sys::Function = Reflect::get(&file, &JsValue::from_str("text"))
        .ok()?
        .dyn_into()
        .ok()?;
    let text_promise: Promise = text_fn.call0(&file).ok()?.dyn_into().ok()?;
    JsFuture::from(text_promise).await.ok()?.as_string()
}

/// Delete `filename` from the OPFS root. Silently ignores missing files.
async fn opfs_delete(root: &JsValue, filename: &str) {
    let remove: js_sys::Function =
        match Reflect::get(root, &JsValue::from_str("removeEntry"))
            .ok()
            .and_then(|v| v.dyn_into().ok())
        {
            Some(f) => f,
            None => return,
        };
    if let Ok(promise) = remove.call1(root, &JsValue::from_str(filename)) {
        if let Ok(promise) = promise.dyn_into::<Promise>() {
            let _ = JsFuture::from(promise).await;
        }
    }
}

// ── Index helpers ─────────────────────────────────────────────────────────────

async fn read_index(root: &JsValue) -> Vec<String> {
    match opfs_read(root, INDEX_FILENAME).await {
        Some(json) => serde_json::from_str(&json).unwrap_or_default(),
        None => vec![],
    }
}

async fn write_index(root: &JsValue, ids: &[String]) -> Option<()> {
    let json = serde_json::to_string(ids).ok()?;
    opfs_write(root, INDEX_FILENAME, &json).await
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Persist `events_json` for `session_id` in OPFS. Fire-and-forget.
pub fn save_to_opfs(session_id: String, events_json: String) {
    leptos::task::spawn_local(async move {
        let _ = save_to_opfs_inner(&session_id, &events_json).await;
    });
}

async fn save_to_opfs_inner(session_id: &str, events_json: &str) -> Option<()> {
    let root = get_opfs_root().await?;
    opfs_write(&root, &events_filename(session_id), events_json).await?;

    let mut ids = read_index(&root).await;
    let id_str = session_id.to_string();
    if !ids.contains(&id_str) {
        ids.push(id_str);
        write_index(&root, &ids).await?;
    }
    Some(())
}

/// Load events JSON for `session_id` from OPFS. Returns `None` if not found.
pub async fn load_from_opfs(session_id: &str) -> Option<String> {
    let root = get_opfs_root().await?;
    opfs_read(&root, &events_filename(session_id)).await
}

/// Return all session IDs stored in OPFS.
pub async fn load_ids_from_opfs() -> Vec<String> {
    let Some(root) = get_opfs_root().await else {
        return vec![];
    };
    read_index(&root).await
}

/// Remove a session from OPFS. Fire-and-forget.
pub fn delete_from_opfs(session_id: String) {
    leptos::task::spawn_local(async move {
        let _ = delete_from_opfs_inner(&session_id).await;
    });
}

async fn delete_from_opfs_inner(session_id: &str) -> Option<()> {
    let root = get_opfs_root().await?;
    opfs_delete(&root, &events_filename(session_id)).await;
    let mut ids = read_index(&root).await;
    ids.retain(|id| id != session_id);
    write_index(&root, &ids).await
}
