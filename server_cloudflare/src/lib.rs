#![allow(unused_must_use)]

use app_core::events::EventEnvelope;
use serde::{Deserialize, Serialize};

/// PCPlayerPicker — Cloudflare Worker entry point.
///
/// Thin API layer: validates requests and writes events to D1.
/// All ranking math and scheduling runs on the client device.
///
/// CPU budget: target < 5ms per request (free tier limit: 10ms).
/// Bundle budget: target < 2MB compressed (free tier limit: 3MB).
///
/// Routes:
///   POST /api/sessions                       → upload initial event log
///   GET  /api/sessions/:id/events?since=N    → fetch events since cursor
///   POST /api/sessions/:id/events            → append events
///   GET  /api/sessions/:id/share             → get/create share tokens
///   POST /api/sessions/:id/pin               → set/update coach recovery PIN
///   POST /api/sessions/:id/recover           → validate PIN, return all events
///   GET  /api/t/:token                       → resolve token to session
///   GET  /api/admin/sessions                 → list sessions (secret key)
///   GET  /api/health                         → liveness check
use worker::*;

// ── Retention constants ───────────────────────────────────────────────────────

/// Maximum events accepted in a single upload (initial session sync).
const MAX_EVENTS_UPLOAD: usize = 1_000;

/// Maximum events accepted in a single incremental append.
const MAX_EVENTS_APPEND: usize = 100;

/// Raw event log is deleted after this many days. The coach client writes an
/// archived summary before this window expires, so final results are preserved.
const ARCHIVE_AFTER_DAYS: f64 = 90.0;

/// Archived session summaries are pruned after this many days.
const ARCHIVE_RETENTION_DAYS: f64 = 365.0;

// ── Token generation ─────────────────────────────────────────────────────────

fn gen_token() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::getrandom(&mut bytes).is_err() {
        return format!("{:x}", js_sys::Date::now() as u64);
    }

    // 16 random bytes -> 32-char lowercase hex token
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── Request / Response types ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct UploadRequest {
    session_id: String,
    events: Vec<EventEnvelope>,
}

#[derive(Serialize)]
struct UploadResponse {
    ok: bool,
    cursor: u32,
    assistant_token: String,
    player_token: String,
    /// Opaque write key the coach must echo back as X-Coach-Key for future appends.
    coach_key: String,
}

#[derive(Deserialize)]
struct AppendRequest {
    events: Vec<EventEnvelope>,
    /// Optional token for role identification (assistant-submitted events).
    token: Option<String>,
}

#[derive(Serialize)]
struct EventsResponse {
    events: Vec<EventEnvelope>,
    cursor: u32,
}

#[derive(Serialize)]
struct ShareResponse {
    assistant_token: String,
    player_token: String,
}

#[derive(Serialize)]
struct TokenInfo {
    /// Empty string when requires_pin = true (session hidden until PIN verified)
    session_id: String,
    /// Empty string when requires_pin = true
    role: String,
    requires_pin: bool,
}

#[derive(Deserialize, Serialize)]
struct StoredEventRow {
    seq: u32,
    payload: String,
}

// ── Allowed CORS origins ──────────────────────────────────────────────────────

const ALLOWED_ORIGINS: &[&str] = &[
    "https://pcplayerpicker.com",
    "https://www.pcplayerpicker.com",
    "http://localhost:8080",
    "http://127.0.0.1:8080",
];

// ── CORS / response helpers ───────────────────────────────────────────────────

/// Returns the caller's Origin if it is in the allow-list, otherwise falls
/// back to the production origin (never a wildcard).
fn cors_origin(req: &Request) -> String {
    let origin = req
        .headers()
        .get("Origin")
        .ok()
        .flatten()
        .unwrap_or_default();
    if ALLOWED_ORIGINS.contains(&origin.as_str()) {
        origin
    } else {
        ALLOWED_ORIGINS[0].to_string()
    }
}

fn with_cors(mut resp: Response, origin: &str) -> Response {
    let h = resp.headers_mut();
    let _ = h.set("Access-Control-Allow-Origin", origin);
    let _ = h.set("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
    let _ = h.set(
        "Access-Control-Allow-Headers",
        "Content-Type, X-Token, X-Coach-Key",
    );
    let _ = h.set("Vary", "Origin");
    resp
}

fn ok_json<T: Serialize>(val: &T, origin: &str) -> Result<Response> {
    Ok(with_cors(Response::from_json(val)?, origin))
}

fn err_json(msg: &str, status: u16, origin: &str) -> Result<Response> {
    Ok(with_cors(Response::error(msg, status)?, origin))
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[event(fetch)]
pub async fn main(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();

    let method = req.method();
    let path = req.path();
    let origin = cors_origin(&req);

    // CORS preflight
    if method == Method::Options {
        let mut resp = Response::empty()?;
        let h = resp.headers_mut();
        let _ = h.set("Access-Control-Allow-Origin", &origin);
        let _ = h.set("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
        let _ = h.set(
            "Access-Control-Allow-Headers",
            "Content-Type, X-Token, X-Coach-Key",
        );
        let _ = h.set("Vary", "Origin");
        return Ok(resp);
    }

    match (method.clone(), path.as_str()) {
        (Method::Get, "/api/health") => ok_json(&serde_json::json!({"ok": true}), &origin),

        // Upload initial event log and create session
        (Method::Post, "/api/sessions") => handle_upload(req, &env, &origin).await,

        // Token routes: GET /api/t/:token, POST /api/t/:token/auth, POST /api/t/:token/pin
        _ if path.starts_with("/api/t/") => {
            let after = path.trim_start_matches("/api/t/");
            match after.split_once('/') {
                Some((token, "auth")) if method == Method::Post => {
                    handle_auth_token(req, token, &env, &origin).await
                }
                Some((token, "pin")) if method == Method::Post => {
                    handle_set_token_pin(req, token, &env, &origin).await
                }
                _ => handle_resolve_token(after, &env, &origin).await,
            }
        }

        // Session-scoped routes
        _ if path.starts_with("/api/sessions/") => route_session(req, &env, &path, &origin).await,

        // Admin: list all sessions (requires X-Token: $PPP_ADMIN_SECRET header)
        (Method::Get, "/api/admin/sessions") => handle_admin_sessions(req, &env, &origin).await,

        _ if path.starts_with("/api/") => err_json("Not found", 404, &origin),

        // Static assets handled by Workers Assets; fall through
        _ => Response::error("Not found", 404),
    }
}

// ── Route dispatcher for /api/sessions/:id/* ─────────────────────────────────

async fn route_session(req: Request, env: &Env, path: &str, origin: &str) -> Result<Response> {
    // Extract session_id from path
    let after_prefix = path.trim_start_matches("/api/sessions/");
    let (session_id, rest) = match after_prefix.split_once('/') {
        Some((id, rest)) => (id, rest),
        None => (after_prefix, ""),
    };

    if session_id.is_empty() {
        return err_json("Missing session id", 400, origin);
    }

    match (req.method(), rest) {
        (Method::Get, "events") => handle_get_events(req, session_id, env, origin).await,
        (Method::Post, "events") => handle_append_events(req, session_id, env, origin).await,
        (Method::Get, "share") => handle_get_share(session_id, env, origin).await,
        (Method::Post, "pin") => handle_set_pin(req, session_id, env, origin).await,
        (Method::Post, "recover") => handle_recover(req, session_id, env, origin).await,
        (Method::Post, "archive") => handle_archive_session(req, session_id, env, origin).await,
        (Method::Post, "heartbeat") => handle_heartbeat_post(req, session_id, env, origin).await,
        (Method::Get, "heartbeat") => handle_heartbeat_get(req, session_id, env, origin).await,
        _ => err_json("Not found", 404, origin),
    }
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// POST /api/sessions
/// Body: { session_id, events: Vec<EventEnvelope> }
/// Creates session + uploads event log, generates share tokens.
/// Returns a `coach_key` that must be provided as `X-Coach-Key` for future appends.
async fn handle_upload(mut req: Request, env: &Env, origin: &str) -> Result<Response> {
    let body: UploadRequest = match req.json().await {
        Ok(b) => b,
        Err(_) => return err_json("Invalid JSON body", 400, origin),
    };
    if body.session_id.is_empty() || body.events.is_empty() {
        return err_json("session_id and events required", 400, origin);
    }
    if body.events.len() > MAX_EVENTS_UPLOAD {
        return err_json(
            &format!("Too many events (max {MAX_EVENTS_UPLOAD} per upload)"),
            400,
            origin,
        );
    }

    let db = env.d1("DB")?;

    // Generate a random coach write key and store its hash
    let coach_key = gen_token();
    let coach_key_hash = simple_hash(&coach_key, &body.session_id);

    // Upsert session record
    db.prepare(
        "INSERT OR IGNORE INTO sessions (id, created_at, coach_key_hash) VALUES (?1, ?2, ?3)",
    )
    .bind(&[
        body.session_id.as_str().into(),
        js_sys::Date::now().into(),
        coach_key_hash.as_str().into(),
    ])?
    .run()
    .await?;

    // Insert events (batch)
    let mut cursor = 0u32;
    for env_event in &body.events {
        let payload = serde_json::to_string(env_event).map_err(|e| Error::from(e.to_string()))?;
        db.prepare("INSERT OR IGNORE INTO events (session_id, seq, payload) VALUES (?1, ?2, ?3)")
            .bind(&[
                body.session_id.as_str().into(),
                env_event.session_version.into(),
                payload.as_str().into(),
            ])?
            .run()
            .await?;
        cursor = cursor.max(env_event.session_version as u32);
    }

    // Generate share tokens if they don't exist yet
    let (assistant_token, player_token) = get_or_create_tokens(&body.session_id, &db).await?;

    ok_json(
        &UploadResponse {
            ok: true,
            cursor,
            assistant_token,
            player_token,
            coach_key,
        },
        origin,
    )
}

/// GET /api/sessions/:id/events?since=N
async fn handle_get_events(
    req: Request,
    session_id: &str,
    env: &Env,
    origin: &str,
) -> Result<Response> {
    let db = env.d1("DB")?;

    // Parse `since` query param
    let url = req.url()?;
    let since: u64 = url
        .query_pairs()
        .find(|(key, _)| key == "since")
        .and_then(|(_, value)| value.parse::<u64>().ok())
        .unwrap_or(0);

    let rows = db
        .prepare(
            "SELECT seq, payload FROM events \
             WHERE session_id = ?1 AND seq > ?2 \
             ORDER BY seq ASC LIMIT 500",
        )
        .bind(&[session_id.into(), since.into()])?
        .all()
        .await?;

    // Deserialize event envelopes from stored JSON
    let raw: Vec<StoredEventRow> = rows.results()?;
    let mut events: Vec<EventEnvelope> = Vec::with_capacity(raw.len());
    let mut cursor = since as u32;
    for row in raw {
        if let Ok(ev) = serde_json::from_str::<EventEnvelope>(&row.payload) {
            cursor = cursor.max(row.seq);
            events.push(ev);
        }
    }

    ok_json(&EventsResponse { events, cursor }, origin)
}

/// POST /api/sessions/:id/events
/// Body: { events: Vec<EventEnvelope>, token: Option<String> }
///
/// Authorization:
///   - Coach path (no body token): must supply `X-Coach-Key` header matching the stored key.
///   - Assistant path (body token): token is validated against share_tokens table.
async fn handle_append_events(
    mut req: Request,
    session_id: &str,
    env: &Env,
    origin: &str,
) -> Result<Response> {
    let coach_key_header = req.headers().get("X-Coach-Key")?.unwrap_or_default();

    let body: AppendRequest = match req.json().await {
        Ok(b) => b,
        Err(_) => return err_json("Invalid JSON body", 400, origin),
    };
    if body.events.is_empty() {
        return err_json("events required", 400, origin);
    }
    if body.events.len() > MAX_EVENTS_APPEND {
        return err_json(
            &format!("Too many events (max {MAX_EVENTS_APPEND} per append)"),
            400,
            origin,
        );
    }

    let db = env.d1("DB")?;

    // Verify caller is authorized: assistant token OR valid coach key
    if let Some(token) = &body.token {
        // Assistant path: validate token belongs to this session
        let row = db
            .prepare("SELECT session_id, role FROM share_tokens WHERE token = ?1")
            .bind(&[token.as_str().into()])?
            .first::<serde_json::Value>(None)
            .await?;
        if let Some(row) = row {
            if row.get("session_id").and_then(|v| v.as_str()) != Some(session_id) {
                return err_json("Token does not match session", 403, origin);
            }
            if row.get("role").and_then(|v| v.as_str()) != Some("assistant") {
                return err_json("This token cannot append events", 403, origin);
            }
        } else {
            return err_json("Invalid token", 403, origin);
        }
    } else {
        // Coach path: require X-Coach-Key header
        if coach_key_header.is_empty() {
            return err_json("Missing X-Coach-Key header", 401, origin);
        }
        let row = db
            .prepare("SELECT coach_key_hash FROM sessions WHERE id = ?1")
            .bind(&[session_id.into()])?
            .first::<serde_json::Value>(None)
            .await?;
        let stored = match row
            .as_ref()
            .and_then(|r| r.get("coach_key_hash"))
            .and_then(|v| v.as_str())
        {
            Some(h) if !h.is_empty() => h.to_string(),
            _ => return err_json("Session not found", 404, origin),
        };
        if !hash_matches(&stored, &coach_key_header, session_id) {
            return err_json("Invalid coach key", 403, origin);
        }
    }

    let row = db
        .prepare("SELECT COALESCE(MAX(seq), -1) AS max_seq FROM events WHERE session_id = ?1")
        .bind(&[session_id.into()])?
        .first::<serde_json::Value>(None)
        .await?;
    let mut next_seq = row
        .as_ref()
        .and_then(|r| r.get("max_seq"))
        .and_then(|v| v.as_i64())
        .unwrap_or(-1)
        + 1;
    let mut cursor = next_seq.saturating_sub(1) as u32;

    for ev in body.events {
        let mut stored = ev;
        stored.id = app_core::models::EventId(next_seq as u64);
        stored.session_version = next_seq as u64;
        let payload = serde_json::to_string(&stored).map_err(|e| Error::from(e.to_string()))?;
        db.prepare("INSERT INTO events (session_id, seq, payload) VALUES (?1, ?2, ?3)")
            .bind(&[
                session_id.into(),
                (next_seq as u32).into(),
                payload.as_str().into(),
            ])?
            .run()
            .await?;
        cursor = next_seq as u32;
        next_seq += 1;
    }

    ok_json(&serde_json::json!({ "ok": true, "cursor": cursor }), origin)
}

/// POST /api/sessions/:id/pin
///
/// Body: `{ "pin": "1234" }` (4–8 digit string).
/// Stores a salted SHA-256 hash in the sessions table.
/// Returns 200 OK or 400/404.
async fn handle_set_pin(
    mut req: Request,
    session_id: &str,
    env: &Env,
    origin: &str,
) -> Result<Response> {
    #[derive(serde::Deserialize)]
    struct PinBody {
        pin: String,
    }
    let body: PinBody = match req.json().await {
        Ok(b) => b,
        Err(_) => return err_json("Invalid JSON body", 400, origin),
    };
    if body.pin.len() < 4 || body.pin.len() > 8 || !body.pin.chars().all(|c| c.is_ascii_digit()) {
        return err_json("PIN must be 4–8 digits", 400, origin);
    }

    let db = env.d1("DB")?;
    // Confirm session exists
    let row = db
        .prepare("SELECT id FROM sessions WHERE id = ?1")
        .bind(&[session_id.into()])?
        .first::<serde_json::Value>(None)
        .await?;
    if row.is_none() {
        return err_json("Session not found", 404, origin);
    }

    let hash = simple_hash(&body.pin, session_id);
    db.prepare("UPDATE sessions SET coach_pin_hash = ?1 WHERE id = ?2")
        .bind(&[hash.as_str().into(), session_id.into()])?
        .run()
        .await?;

    ok_json(&serde_json::json!({ "ok": true }), origin)
}

/// POST /api/sessions/:id/recover
///
/// Body: `{ "pin": "1234" }`.
/// Validates PIN and returns all events so the coach can reconstruct on a new device.
async fn handle_recover(
    mut req: Request,
    session_id: &str,
    env: &Env,
    origin: &str,
) -> Result<Response> {
    #[derive(serde::Deserialize)]
    struct RecoverBody {
        pin: String,
    }
    let body: RecoverBody = match req.json().await {
        Ok(b) => b,
        Err(_) => return err_json("Invalid JSON body", 400, origin),
    };

    let db = env.d1("DB")?;
    let row = db
        .prepare("SELECT coach_pin_hash FROM sessions WHERE id = ?1")
        .bind(&[session_id.into()])?
        .first::<serde_json::Value>(None)
        .await?;

    let stored_hash = match row
        .as_ref()
        .and_then(|r| r.get("coach_pin_hash"))
        .and_then(|v| v.as_str())
    {
        Some(h) if !h.is_empty() => h.to_string(),
        _ => return err_json("No recovery PIN set for this session", 404, origin),
    };

    if !hash_matches(&stored_hash, &body.pin, session_id) {
        return err_json("Invalid PIN", 403, origin);
    }

    // PIN valid — return all events
    let rows = db
        .prepare("SELECT seq, payload FROM events WHERE session_id = ?1 ORDER BY seq ASC")
        .bind(&[session_id.into()])?
        .all()
        .await?;

    let raw: Vec<StoredEventRow> = rows.results()?;
    let mut events: Vec<EventEnvelope> = Vec::with_capacity(raw.len());
    let mut cursor = 0u32;
    for row in raw {
        if let Ok(ev) = serde_json::from_str::<EventEnvelope>(&row.payload) {
            cursor = cursor.max(row.seq);
            events.push(ev);
        }
    }
    ok_json(&EventsResponse { events, cursor }, origin)
}

/// Deterministic salted SHA-256 hash for PINs and coach keys.
fn simple_hash(pin: &str, salt: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(b":");
    hasher.update(pin.as_bytes());
    let digest = hasher.finalize();

    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn legacy_simple_hash(pin: &str, salt: &str) -> String {
    let mut acc: u64 = 0xcafe_babe_dead_beef;
    for b in salt
        .bytes()
        .chain(core::iter::once(b':'))
        .chain(pin.bytes())
    {
        acc = acc.wrapping_mul(6364136223846793005).wrapping_add(b as u64);
    }
    format!("{acc:016x}")
}

fn hash_matches(stored: &str, secret: &str, salt: &str) -> bool {
    stored == simple_hash(secret, salt) || stored == legacy_simple_hash(secret, salt)
}

/// GET /api/sessions/:id/share
async fn handle_get_share(session_id: &str, env: &Env, origin: &str) -> Result<Response> {
    let db = env.d1("DB")?;
    let (assistant_token, player_token) = get_or_create_tokens(session_id, &db).await?;
    ok_json(
        &ShareResponse {
            assistant_token,
            player_token,
        },
        origin,
    )
}

/// GET /api/t/:token — resolve token to session + role.
/// If the token has a PIN set, returns `requires_pin: true` and hides session_id/role
/// until the coach authenticates via POST /api/t/:token/auth.
async fn handle_resolve_token(token: &str, env: &Env, origin: &str) -> Result<Response> {
    let db = env.d1("DB")?;
    let row = db
        .prepare("SELECT session_id, role, pin_hash FROM share_tokens WHERE token = ?1")
        .bind(&[token.into()])?
        .first::<serde_json::Value>(None)
        .await?;

    match row {
        Some(r) => {
            let session_id = r
                .get("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let role = r
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let pin_hash = r.get("pin_hash").and_then(|v| v.as_str()).unwrap_or("");
            let requires_pin = !pin_hash.is_empty();
            if requires_pin {
                // Do not reveal session_id or role until PIN is verified
                ok_json(
                    &TokenInfo {
                        session_id: String::new(),
                        role: String::new(),
                        requires_pin: true,
                    },
                    origin,
                )
            } else {
                ok_json(
                    &TokenInfo {
                        session_id,
                        role,
                        requires_pin: false,
                    },
                    origin,
                )
            }
        }
        None => err_json("Invalid or expired token", 404, origin),
    }
}

/// POST /api/t/:token/auth
/// Body: `{ "pin": "1234" }`
/// Validates PIN and returns { session_id, role } on success.
async fn handle_auth_token(
    mut req: Request,
    token: &str,
    env: &Env,
    origin: &str,
) -> Result<Response> {
    #[derive(serde::Deserialize)]
    struct AuthBody {
        pin: String,
    }
    let body: AuthBody = match req.json().await {
        Ok(b) => b,
        Err(_) => return err_json("Invalid JSON body", 400, origin),
    };

    let db = env.d1("DB")?;
    let row = db
        .prepare("SELECT session_id, role, pin_hash FROM share_tokens WHERE token = ?1")
        .bind(&[token.into()])?
        .first::<serde_json::Value>(None)
        .await?;

    match row {
        None => err_json("Invalid or expired token", 404, origin),
        Some(r) => {
            let session_id = r
                .get("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let role = r
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let pin_hash = r.get("pin_hash").and_then(|v| v.as_str()).unwrap_or("");
            if pin_hash.is_empty() {
                // No PIN set — auth endpoint not needed but harmless
                return ok_json(
                    &TokenInfo {
                        session_id,
                        role,
                        requires_pin: false,
                    },
                    origin,
                );
            }
            if !hash_matches(pin_hash, &body.pin, token) {
                return err_json("Invalid PIN", 403, origin);
            }
            ok_json(
                &TokenInfo {
                    session_id,
                    role,
                    requires_pin: false,
                },
                origin,
            )
        }
    }
}

/// POST /api/t/:token/pin
/// Body: `{ "pin": "1234" }` to set, or `{ "pin": "" }` to clear.
/// Only the coach who knows the session should call this (no auth required —
/// the token itself is the authorization, same as the share-link model).
async fn handle_set_token_pin(
    mut req: Request,
    token: &str,
    env: &Env,
    origin: &str,
) -> Result<Response> {
    #[derive(serde::Deserialize)]
    struct PinBody {
        pin: String,
    }
    let body: PinBody = match req.json().await {
        Ok(b) => b,
        Err(_) => return err_json("Invalid JSON body", 400, origin),
    };

    // Validate PIN format (4–8 digits) unless clearing
    if !body.pin.is_empty()
        && (body.pin.len() < 4
            || body.pin.len() > 8
            || !body.pin.chars().all(|c| c.is_ascii_digit()))
    {
        return err_json("PIN must be 4–8 digits, or empty to clear", 400, origin);
    }

    let db = env.d1("DB")?;
    // Confirm token exists
    let exists = db
        .prepare("SELECT token FROM share_tokens WHERE token = ?1")
        .bind(&[token.into()])?
        .first::<serde_json::Value>(None)
        .await?;
    if exists.is_none() {
        return err_json("Token not found", 404, origin);
    }

    let new_hash: Option<String> = if body.pin.is_empty() {
        None
    } else {
        Some(simple_hash(&body.pin, token))
    };

    match new_hash {
        Some(h) => {
            db.prepare("UPDATE share_tokens SET pin_hash = ?1 WHERE token = ?2")
                .bind(&[h.as_str().into(), token.into()])?
                .run()
                .await?;
        }
        None => {
            db.prepare("UPDATE share_tokens SET pin_hash = NULL WHERE token = ?1")
                .bind(&[token.into()])?
                .run()
                .await?;
        }
    }

    ok_json(&serde_json::json!({ "ok": true }), origin)
}

/// GET /api/admin/sessions — requires X-Token matching PPP_ADMIN_SECRET env var
async fn handle_admin_sessions(req: Request, env: &Env, origin: &str) -> Result<Response> {
    let provided = req.headers().get("X-Token")?.unwrap_or_default();
    let secret = env
        .var("PPP_ADMIN_SECRET")
        .map(|v| v.to_string())
        .unwrap_or_default();
    if secret.is_empty() || provided != secret {
        return err_json("Unauthorized", 401, origin);
    }

    let db = env.d1("DB")?;
    let rows = db
        .prepare("SELECT id, created_at FROM sessions ORDER BY created_at DESC LIMIT 100")
        .all()
        .await?;

    let sessions: Vec<serde_json::Value> = rows.results()?;
    ok_json(&serde_json::json!({ "sessions": sessions }), origin)
}

/// POST /api/sessions/:id/archive
///
/// Body: `{ sport, team_size, player_names, final_rankings }`
/// Requires `X-Coach-Key` header.
///
/// Upserts a long-lived summary of the session into `archived_sessions` so that
/// final results survive the raw event log retention window. Called by the coach
/// client whenever rankings are pushed to an online session.
async fn handle_archive_session(
    mut req: Request,
    session_id: &str,
    env: &Env,
    origin: &str,
) -> Result<Response> {
    let coach_key_header = req.headers().get("X-Coach-Key")?.unwrap_or_default();
    if coach_key_header.is_empty() {
        return err_json("Missing X-Coach-Key header", 401, origin);
    }

    #[derive(Deserialize)]
    struct ArchiveBody {
        sport: String,
        team_size: u8,
        /// JSON object mapping player_id string → player name
        player_names: serde_json::Value,
        /// Vec<PlayerRanking> — stored as raw JSON
        final_rankings: Option<serde_json::Value>,
    }

    let body: ArchiveBody = match req.json().await {
        Ok(b) => b,
        Err(_) => return err_json("Invalid JSON body", 400, origin),
    };

    let db = env.d1("DB")?;

    // Verify coach key and fetch session creation time
    let row = db
        .prepare("SELECT coach_key_hash, created_at FROM sessions WHERE id = ?1")
        .bind(&[session_id.into()])?
        .first::<serde_json::Value>(None)
        .await?;
    let (stored_hash, created_at) = match row.as_ref() {
        Some(r) => {
            let h = r
                .get("coach_key_hash")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let ca = r.get("created_at").and_then(|v| v.as_f64()).unwrap_or(0.0);
            (h, ca)
        }
        None => return err_json("Session not found", 404, origin),
    };
    if stored_hash.is_empty() || !hash_matches(&stored_hash, &coach_key_header, session_id) {
        return err_json("Invalid coach key", 403, origin);
    }

    let player_names_json =
        serde_json::to_string(&body.player_names).map_err(|e| Error::from(e.to_string()))?;
    let final_rankings_json = match &body.final_rankings {
        Some(r) => serde_json::to_string(r).map_err(|e| Error::from(e.to_string()))?,
        None => "null".to_string(),
    };

    db.prepare(
        "INSERT INTO archived_sessions \
             (id, sport, team_size, created_at, archived_at, player_names, final_rankings) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(id) DO UPDATE SET \
             sport=excluded.sport, team_size=excluded.team_size, \
             archived_at=excluded.archived_at, \
             player_names=excluded.player_names, \
             final_rankings=excluded.final_rankings",
    )
    .bind(&[
        session_id.into(),
        body.sport.as_str().into(),
        (body.team_size as u32).into(),
        created_at.into(),
        js_sys::Date::now().into(),
        player_names_json.as_str().into(),
        final_rankings_json.as_str().into(),
    ])?
    .run()
    .await?;

    ok_json(&serde_json::json!({ "ok": true }), origin)
}

// ── Heartbeat ─────────────────────────────────────────────────────────────────

/// POST /api/sessions/:id/heartbeat
///
/// Body: `{ "device_id": "...", "label": "..." }` (label is optional).
/// Requires `X-Coach-Key` header.
///
/// Upserts a row in `device_heartbeats` with the current timestamp.
/// Used by the coach client to announce its presence so other coach devices
/// (and the OnlineTab) can show which devices have the session open.
async fn handle_heartbeat_post(
    mut req: Request,
    session_id: &str,
    env: &Env,
    origin: &str,
) -> Result<Response> {
    // Verify coach key
    let coach_key_header = req.headers().get("X-Coach-Key")?.unwrap_or_default();
    if coach_key_header.is_empty() {
        return err_json("Missing X-Coach-Key header", 401, origin);
    }

    #[derive(Deserialize)]
    struct HeartbeatBody {
        device_id: String,
        label: Option<String>,
    }

    let body: HeartbeatBody = match req.json().await {
        Ok(b) => b,
        Err(_) => return err_json("Invalid JSON body", 400, origin),
    };
    if body.device_id.is_empty() || body.device_id.len() > 64 {
        return err_json("device_id must be 1–64 characters", 400, origin);
    }

    let db = env.d1("DB")?;

    // Validate coach key
    let row = db
        .prepare("SELECT coach_key_hash FROM sessions WHERE id = ?1")
        .bind(&[session_id.into()])?
        .first::<serde_json::Value>(None)
        .await?;
    let stored = match row.as_ref().and_then(|r| r.get("coach_key_hash")).and_then(|v| v.as_str()) {
        Some(h) if !h.is_empty() => h.to_string(),
        _ => return err_json("Session not found", 404, origin),
    };
    if !hash_matches(&stored, &coach_key_header, session_id) {
        return err_json("Invalid coach key", 403, origin);
    }

    let now = js_sys::Date::now();
    let label = body.label.unwrap_or_default();
    db.prepare(
        "INSERT INTO device_heartbeats (session_id, device_id, last_seen, label) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (session_id, device_id) DO UPDATE SET last_seen = ?3, label = ?4",
    )
    .bind(&[
        session_id.into(),
        body.device_id.as_str().into(),
        now.into(),
        label.as_str().into(),
    ])?
    .run()
    .await?;

    ok_json(&serde_json::json!({ "ok": true }), origin)
}

/// GET /api/sessions/:id/heartbeat
///
/// Requires `X-Coach-Key` header.
///
/// Returns devices seen in the last 5 minutes (active threshold).
async fn handle_heartbeat_get(
    req: Request,
    session_id: &str,
    env: &Env,
    origin: &str,
) -> Result<Response> {
    let coach_key_header = req.headers().get("X-Coach-Key")?.unwrap_or_default();
    if coach_key_header.is_empty() {
        return err_json("Missing X-Coach-Key header", 401, origin);
    }

    let db = env.d1("DB")?;

    // Validate coach key
    let row = db
        .prepare("SELECT coach_key_hash FROM sessions WHERE id = ?1")
        .bind(&[session_id.into()])?
        .first::<serde_json::Value>(None)
        .await?;
    let stored = match row.as_ref().and_then(|r| r.get("coach_key_hash")).and_then(|v| v.as_str()) {
        Some(h) if !h.is_empty() => h.to_string(),
        _ => return err_json("Session not found", 404, origin),
    };
    if !hash_matches(&stored, &coach_key_header, session_id) {
        return err_json("Invalid coach key", 403, origin);
    }

    // Devices active in the last 5 minutes
    let active_threshold = js_sys::Date::now() - 5.0 * 60.0 * 1000.0;
    let rows = db
        .prepare(
            "SELECT device_id, label, last_seen FROM device_heartbeats \
             WHERE session_id = ?1 AND last_seen >= ?2 \
             ORDER BY last_seen DESC",
        )
        .bind(&[session_id.into(), active_threshold.into()])?
        .all()
        .await?;

    let devices: Vec<serde_json::Value> = rows.results()?;
    ok_json(&serde_json::json!({ "devices": devices }), origin)
}

// ── Scheduled cleanup ─────────────────────────────────────────────────────────

/// Cron-triggered cleanup. Runs on the schedule defined in wrangler.toml.
///
/// Responsibility split:
///   Coach client  → writes `archived_sessions` whenever rankings are pushed.
///   This handler  → deletes old raw event logs + very old archived summaries.
///
/// Raw events are safe to delete after ARCHIVE_AFTER_DAYS: the coach will have
/// had ample opportunity to push an archive snapshot during an active session.
/// Sessions that were never shared online have no archive, but that is acceptable
/// because offline-only sessions store everything in localStorage on the coach
/// device, which is not subject to D1 retention.
#[allow(unused_must_use)]
#[event(scheduled)]
pub async fn cleanup(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) -> Result<()> {
    let db = env.d1("DB")?;

    // Millisecond cutoffs
    let now = js_sys::Date::now();
    let raw_cutoff = now - ARCHIVE_AFTER_DAYS * 86_400_000.0;
    let archive_cutoff = now - ARCHIVE_RETENTION_DAYS * 86_400_000.0;

    // Delete raw events for sessions past the retention window
    db.prepare(
        "DELETE FROM events WHERE session_id IN \
         (SELECT id FROM sessions WHERE created_at < ?1)",
    )
    .bind(&[raw_cutoff.into()])?
    .run()
    .await?;

    // Delete share tokens for those sessions
    db.prepare(
        "DELETE FROM share_tokens WHERE session_id IN \
         (SELECT id FROM sessions WHERE created_at < ?1)",
    )
    .bind(&[raw_cutoff.into()])?
    .run()
    .await?;

    // Delete the session rows themselves
    db.prepare("DELETE FROM sessions WHERE created_at < ?1")
        .bind(&[raw_cutoff.into()])?
        .run()
        .await?;

    // Prune old archived summaries
    db.prepare("DELETE FROM archived_sessions WHERE created_at < ?1")
        .bind(&[archive_cutoff.into()])?
        .run()
        .await?;

    // Prune stale device heartbeats older than 24 hours
    let heartbeat_cutoff = now - 86_400_000.0;
    db.prepare("DELETE FROM device_heartbeats WHERE last_seen < ?1")
        .bind(&[heartbeat_cutoff.into()])?
        .run()
        .await?;

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn get_or_create_tokens(session_id: &str, db: &D1Database) -> Result<(String, String)> {
    // Check for existing tokens
    let rows = db
        .prepare("SELECT token, role FROM share_tokens WHERE session_id = ?1")
        .bind(&[session_id.into()])?
        .all()
        .await?;
    let existing: Vec<serde_json::Value> = rows.results()?;

    let mut assistant = None;
    let mut player = None;
    for row in &existing {
        let role = row.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let token = row
            .get("token")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        match role {
            "assistant" => assistant = Some(token),
            "player" => player = Some(token),
            _ => {}
        }
    }

    let now = js_sys::Date::now();

    let assistant_token = match assistant {
        Some(t) => t,
        None => {
            let t = gen_token();
            db.prepare(
                "INSERT INTO share_tokens (token, session_id, role, created_at) \
                 VALUES (?1, ?2, 'assistant', ?3)",
            )
            .bind(&[t.as_str().into(), session_id.into(), now.into()])?
            .run()
            .await?;
            t
        }
    };

    let player_token = match player {
        Some(t) => t,
        None => {
            let t = gen_token();
            db.prepare(
                "INSERT INTO share_tokens (token, session_id, role, created_at) \
                 VALUES (?1, ?2, 'player', ?3)",
            )
            .bind(&[t.as_str().into(), session_id.into(), now.into()])?
            .run()
            .await?;
            t
        }
    };

    Ok((assistant_token, player_token))
}
