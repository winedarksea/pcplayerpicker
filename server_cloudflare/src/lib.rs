use app_core::events::EventEnvelope;
use base64::Engine;
use core::cmp::Ordering;
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
///   GET  /api/admin/sessions                 → admin JSON overview (secret key)
///   GET  /_internal/admin/sessions          → hidden admin HTML overview
///   GET  /api/health                         → liveness check
use worker::*;

// ── Retention constants ───────────────────────────────────────────────────────

/// Maximum events accepted in a single upload (initial session sync).
const MAX_EVENTS_UPLOAD: usize = 1_000;

/// Maximum events accepted in a single incremental append.
const MAX_EVENTS_APPEND: usize = 100;

/// Keep D1 batches bounded so large uploads do not build oversized JS arrays.
const MAX_D1_BATCH_STATEMENTS: usize = 100;

/// Raw event log is deleted after this many days. The coach client writes an
/// archived summary before this window expires, so final results are preserved.
const ARCHIVE_AFTER_DAYS: f64 = 90.0;

/// Archived session summaries are pruned after this many days.
const ARCHIVE_RETENTION_DAYS: f64 = 365.0;

/// Hidden admin view should stay lightweight and scan a bounded page.
const ADMIN_SESSIONS_PAGE_LIMIT: usize = 200;

/// Single-row cache key for the precomputed admin payload.
const ADMIN_SNAPSHOT_PRIMARY_KEY: &str = "online_sessions_overview";

// ── Token generation ─────────────────────────────────────────────────────────

fn gen_token() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::getrandom(&mut bytes).is_err() {
        return format!("{:x}", js_sys::Date::now() as u64);
    }

    // 16 random bytes -> 32-char lowercase hex token
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// D1 rejects JS `BigInt` bind values, while Rust integer -> `JsValue`
/// conversions in wasm produce `BigInt` for 64-bit integers. Event sequence
/// numbers and timestamps in this app stay far below JS's safe integer limit,
/// so bind them as plain JS `Number`s instead.
fn d1_number(value: f64) -> worker::wasm_bindgen::JsValue {
    worker::wasm_bindgen::JsValue::from_f64(value)
}

fn d1_u64(value: u64) -> worker::wasm_bindgen::JsValue {
    d1_number(value as f64)
}

fn d1_u32(value: u32) -> worker::wasm_bindgen::JsValue {
    d1_number(f64::from(value))
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

#[derive(Deserialize)]
struct ReservedSequenceRangeRow {
    start_seq: u32,
    cursor: u32,
}

#[derive(Debug, Deserialize, Serialize)]
struct AdminOverviewResponse {
    generated_at: f64,
    generated_at_iso: String,
    totals: AdminOverviewTotals,
    sessions: Vec<AdminSessionOverviewRow>,
}

#[derive(Debug, Deserialize, Serialize)]
struct AdminOverviewTotals {
    total_sessions: u32,
    live_sessions: u32,
    archived_sessions: u32,
    sessions_last_24h: u32,
    sessions_last_7d: u32,
}

#[derive(Debug, Deserialize, Serialize)]
struct AdminSessionOverviewRow {
    session_id: String,
    created_at: f64,
    created_at_iso: String,
    archived_at: Option<f64>,
    archived_at_iso: Option<String>,
    status: String,
    sport: String,
    team_size: Option<u8>,
    player_count: usize,
    ranking_count: usize,
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
        "Authorization, Content-Type, X-Token, X-Coach-Key",
    );
    let _ = h.set("Vary", "Origin");
    resp
}

fn ok_json<T: Serialize>(val: &T, origin: &str) -> Result<Response> {
    Ok(with_cors(Response::from_json(val)?, origin))
}

fn ok_json_text(body: String, origin: &str) -> Result<Response> {
    let mut resp = Response::from_body(ResponseBody::Body(body.into_bytes()))?;
    let h = resp.headers_mut();
    let _ = h.set("Content-Type", "application/json");
    Ok(with_cors(resp, origin))
}

fn err_json(msg: &str, status: u16, origin: &str) -> Result<Response> {
    Ok(with_cors(Response::error(msg, status)?, origin))
}

fn diagnostic_json<T: Serialize>(val: &T, status: u16, origin: &str) -> Result<Response> {
    Ok(with_cors(
        Response::builder().with_status(status).from_json(val)?,
        origin,
    ))
}

fn internal_api_error_response(
    origin: &str,
    route: &str,
    error: &Error,
    context: serde_json::Value,
) -> Result<Response> {
    diagnostic_json(
        &serde_json::json!({
            "error": "internal_server_error",
            "route": route,
            "message": error.to_string(),
            "context": context,
        }),
        500,
        origin,
    )
}

fn build_events_response_json_body(rows: &[StoredEventRow], fallback_cursor: u32) -> String {
    let cursor = rows.last().map(|row| row.seq).unwrap_or(fallback_cursor);
    let payload_capacity = rows.iter().map(|row| row.payload.len() + 1).sum::<usize>();
    let mut body = String::with_capacity(payload_capacity + 32);
    body.push_str("{\"events\":[");
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            body.push(',');
        }
        body.push_str(&row.payload);
    }
    body.push_str("],\"cursor\":");
    body.push_str(&cursor.to_string());
    body.push('}');
    body
}

fn build_event_insert_statement(
    db: &D1Database,
    session_id: &str,
    seq: u32,
    payload: &str,
    insert_or_ignore: bool,
) -> Result<D1PreparedStatement> {
    let query = if insert_or_ignore {
        "INSERT OR IGNORE INTO events (session_id, seq, payload) VALUES (?1, ?2, ?3)"
    } else {
        "INSERT INTO events (session_id, seq, payload) VALUES (?1, ?2, ?3)"
    };
    db.prepare(query)
        .bind(&[session_id.into(), d1_u32(seq), payload.into()])
}

async fn run_d1_statement_batches(
    db: &D1Database,
    statements: Vec<D1PreparedStatement>,
) -> Result<()> {
    for batch in statements.chunks(MAX_D1_BATCH_STATEMENTS) {
        db.batch(batch.to_vec()).await?;
    }
    Ok(())
}

async fn parse_json_req<T: serde::de::DeserializeOwned>(
    req: &mut Request,
) -> std::result::Result<T, String> {
    let body_text = req
        .text()
        .await
        .map_err(|e| format!("Failed to read body: {}", e))?;
    serde_json::from_str(&body_text).map_err(|e| format!("Invalid JSON: {}", e))
}

#[cfg(target_arch = "wasm32")]
fn format_timestamp_ms_as_iso(timestamp_ms: f64) -> String {
    js_sys::Date::new(&worker::wasm_bindgen::JsValue::from_f64(timestamp_ms))
        .to_iso_string()
        .into()
}

#[cfg(not(target_arch = "wasm32"))]
fn format_timestamp_ms_as_iso(timestamp_ms: f64) -> String {
    format!("{timestamp_ms:.0}")
}

fn parse_optional_u32(row: &serde_json::Value, field_name: &str) -> u32 {
    row.get(field_name)
        .and_then(|value| value.as_u64().or_else(|| value.as_f64().map(|n| n as u64)))
        .unwrap_or(0) as u32
}

fn parse_optional_u8(row: &serde_json::Value, field_name: &str) -> Option<u8> {
    row.get(field_name)
        .and_then(|value| value.as_u64().or_else(|| value.as_f64().map(|n| n as u64)))
        .and_then(|value| u8::try_from(value).ok())
}

fn parse_optional_f64(row: &serde_json::Value, field_name: &str) -> Option<f64> {
    row.get(field_name).and_then(|value| value.as_f64())
}

fn parse_optional_string(row: &serde_json::Value, field_name: &str) -> Option<String> {
    row.get(field_name)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn json_member_count(raw_json: Option<&str>) -> usize {
    let Some(raw_json) = raw_json else {
        return 0;
    };
    match serde_json::from_str::<serde_json::Value>(raw_json) {
        Ok(serde_json::Value::Array(items)) => items.len(),
        Ok(serde_json::Value::Object(map)) => map.len(),
        _ => 0,
    }
}

fn build_admin_session_overview_row(
    row: &serde_json::Value,
    is_live: bool,
) -> AdminSessionOverviewRow {
    let session_id = parse_optional_string(row, "id").unwrap_or_default();
    let created_at = parse_optional_f64(row, "created_at").unwrap_or(0.0);
    let archived_at = parse_optional_f64(row, "archived_at");
    let sport = parse_optional_string(row, "sport")
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let team_size = parse_optional_u8(row, "team_size");
    let player_count = json_member_count(parse_optional_string(row, "player_names").as_deref());
    let ranking_count = json_member_count(parse_optional_string(row, "final_rankings").as_deref());
    let has_archive = archived_at.is_some();
    let status = match (is_live, has_archive) {
        (true, true) => "live+archived",
        (true, false) => "live-only",
        (false, true) => "archived-only",
        (false, false) => "unknown",
    };

    AdminSessionOverviewRow {
        session_id,
        created_at,
        created_at_iso: format_timestamp_ms_as_iso(created_at),
        archived_at,
        archived_at_iso: archived_at.map(format_timestamp_ms_as_iso),
        status: status.to_string(),
        sport,
        team_size,
        player_count,
        ranking_count,
    }
}

fn html_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn timing_safe_eq(left: &str, right: &str) -> bool {
    let left_bytes = left.as_bytes();
    let right_bytes = right.as_bytes();
    let mut diff = left_bytes.len() ^ right_bytes.len();
    let max_len = left_bytes.len().max(right_bytes.len());

    for index in 0..max_len {
        let left_byte = left_bytes.get(index).copied().unwrap_or(0);
        let right_byte = right_bytes.get(index).copied().unwrap_or(0);
        diff |= usize::from(left_byte ^ right_byte);
    }

    diff == 0
}

fn parse_basic_auth_password(header_value: &str) -> Option<String> {
    let encoded = header_value.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (_, password) = decoded.split_once(':')?;
    Some(password.to_string())
}

fn header_has_admin_access(
    x_token_header: Option<&str>,
    authorization_header: Option<&str>,
    admin_secret: &str,
) -> bool {
    if admin_secret.is_empty() {
        return false;
    }

    if let Some(header_value) = x_token_header {
        if timing_safe_eq(header_value.trim(), admin_secret) {
            return true;
        }
    }

    if let Some(header_value) = authorization_header {
        if let Some(token) = header_value.strip_prefix("Bearer ") {
            if timing_safe_eq(token.trim(), admin_secret) {
                return true;
            }
        }

        if let Some(password) = parse_basic_auth_password(header_value) {
            if timing_safe_eq(password.trim(), admin_secret) {
                return true;
            }
        }
    }

    false
}

fn request_has_admin_access(req: &Request, admin_secret: &str) -> bool {
    let x_token_header = req.headers().get("X-Token").ok().flatten();
    let authorization_header = req.headers().get("Authorization").ok().flatten();
    header_has_admin_access(
        x_token_header.as_deref(),
        authorization_header.as_deref(),
        admin_secret,
    )
}

fn with_admin_security_headers(mut resp: Response, is_html: bool) -> Response {
    let headers = resp.headers_mut();
    let _ = headers.set("Cache-Control", "no-store, max-age=0");
    let _ = headers.set("Pragma", "no-cache");
    let _ = headers.set("X-Robots-Tag", "noindex, nofollow, noarchive");
    let _ = headers.set("X-Frame-Options", "DENY");
    let _ = headers.set("Referrer-Policy", "same-origin");
    if is_html {
        let _ = headers.set(
            "Content-Security-Policy",
            "default-src 'none'; style-src 'unsafe-inline'; img-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'",
        );
    }
    resp
}

fn admin_unauthorized_json(origin: &str) -> Result<Response> {
    let mut resp = Response::error("Unauthorized", 401)?;
    let _ = resp
        .headers_mut()
        .set("WWW-Authenticate", r#"Basic realm="PPP Admin""#);
    Ok(with_admin_security_headers(with_cors(resp, origin), false))
}

fn admin_unauthorized_html() -> Result<Response> {
    let mut resp = Response::error("Unauthorized", 401)?;
    let headers = resp.headers_mut();
    let _ = headers.set("Content-Type", "text/plain; charset=utf-8");
    let _ = headers.set("WWW-Authenticate", r#"Basic realm="PPP Admin""#);
    Ok(with_admin_security_headers(resp, false))
}

async fn fetch_count(statement: D1PreparedStatement) -> Result<u32> {
    let row = statement.first::<serde_json::Value>(None).await?;
    Ok(row
        .as_ref()
        .map(|row| parse_optional_u32(row, "count"))
        .unwrap_or(0))
}

async fn build_admin_overview(env: &Env) -> Result<AdminOverviewResponse> {
    let db = env.d1("DB")?;
    let now = js_sys::Date::now();
    let last_24h_cutoff = now - 86_400_000.0;
    let last_7d_cutoff = now - 7.0 * 86_400_000.0;

    let live_sessions = fetch_count(db.prepare("SELECT COUNT(*) AS count FROM sessions")).await?;
    let archived_sessions =
        fetch_count(db.prepare("SELECT COUNT(*) AS count FROM archived_sessions")).await?;
    let total_sessions = fetch_count(db.prepare(
        "SELECT COUNT(*) AS count FROM \
             (SELECT id, created_at FROM sessions \
              UNION \
              SELECT id, created_at FROM archived_sessions)",
    ))
    .await?;
    let sessions_last_24h = fetch_count(
        db.prepare(
            "SELECT COUNT(*) AS count FROM \
             (SELECT id, created_at FROM sessions \
              UNION \
              SELECT id, created_at FROM archived_sessions) \
             WHERE created_at >= ?1",
        )
        .bind(&[d1_number(last_24h_cutoff)])?,
    )
    .await?;
    let sessions_last_7d = fetch_count(
        db.prepare(
            "SELECT COUNT(*) AS count FROM \
             (SELECT id, created_at FROM sessions \
              UNION \
              SELECT id, created_at FROM archived_sessions) \
             WHERE created_at >= ?1",
        )
        .bind(&[d1_number(last_7d_cutoff)])?,
    )
    .await?;
    let live_rows = db
        .prepare(
            "SELECT s.id, s.created_at, \
                    a.archived_at, a.sport, a.team_size, a.player_names, a.final_rankings \
             FROM sessions s \
             LEFT JOIN archived_sessions a ON a.id = s.id \
             ORDER BY s.created_at DESC \
             LIMIT ?1",
        )
        .bind(&[d1_u32(ADMIN_SESSIONS_PAGE_LIMIT as u32)])?
        .all()
        .await?;
    let archived_only_rows = db
        .prepare(
            "SELECT a.id, a.created_at, \
                    a.archived_at, a.sport, a.team_size, a.player_names, a.final_rankings \
             FROM archived_sessions a \
             LEFT JOIN sessions s ON s.id = a.id \
             WHERE s.id IS NULL \
             ORDER BY a.created_at DESC \
             LIMIT ?1",
        )
        .bind(&[d1_u32(ADMIN_SESSIONS_PAGE_LIMIT as u32)])?
        .all()
        .await?;

    let mut sessions = Vec::new();
    for row in live_rows.results::<serde_json::Value>()? {
        sessions.push(build_admin_session_overview_row(&row, true));
    }
    for row in archived_only_rows.results::<serde_json::Value>()? {
        sessions.push(build_admin_session_overview_row(&row, false));
    }

    sessions.sort_by(|left, right| {
        right
            .created_at
            .partial_cmp(&left.created_at)
            .unwrap_or(Ordering::Equal)
    });
    sessions.truncate(ADMIN_SESSIONS_PAGE_LIMIT);

    Ok(AdminOverviewResponse {
        generated_at: now,
        generated_at_iso: format_timestamp_ms_as_iso(now),
        totals: AdminOverviewTotals {
            total_sessions,
            live_sessions,
            archived_sessions,
            sessions_last_24h,
            sessions_last_7d,
        },
        sessions,
    })
}

async fn refresh_admin_overview_snapshot(env: &Env) -> Result<()> {
    let overview = build_admin_overview(env).await?;
    let payload =
        serde_json::to_string(&overview).map_err(|error| Error::from(error.to_string()))?;
    let db = env.d1("DB")?;
    db.prepare(
        "INSERT INTO admin_snapshots (snapshot_key, updated_at, payload) \
         VALUES (?1, ?2, ?3) \
         ON CONFLICT(snapshot_key) DO UPDATE SET \
             updated_at = excluded.updated_at, \
             payload = excluded.payload",
    )
    .bind(&[
        ADMIN_SNAPSHOT_PRIMARY_KEY.into(),
        d1_number(js_sys::Date::now()),
        payload.as_str().into(),
    ])?
    .run()
    .await?;
    Ok(())
}

async fn load_admin_overview_snapshot(env: &Env) -> Result<AdminOverviewResponse> {
    let db = env.d1("DB")?;
    let row = db
        .prepare("SELECT payload FROM admin_snapshots WHERE snapshot_key = ?1")
        .bind(&[ADMIN_SNAPSHOT_PRIMARY_KEY.into()])?
        .first::<serde_json::Value>(None)
        .await?;

    if let Some(row) = row {
        if let Some(payload) = parse_optional_string(&row, "payload") {
            if let Ok(overview) = serde_json::from_str::<AdminOverviewResponse>(&payload) {
                return Ok(overview);
            }
        }
    }

    // Backfill only when the snapshot is missing or corrupted. Normal admin
    // requests read the precomputed row and do not run the overview queries.
    let overview = build_admin_overview(env).await?;
    let payload =
        serde_json::to_string(&overview).map_err(|error| Error::from(error.to_string()))?;
    db.prepare(
        "INSERT INTO admin_snapshots (snapshot_key, updated_at, payload) \
         VALUES (?1, ?2, ?3) \
         ON CONFLICT(snapshot_key) DO UPDATE SET \
             updated_at = excluded.updated_at, \
             payload = excluded.payload",
    )
    .bind(&[
        ADMIN_SNAPSHOT_PRIMARY_KEY.into(),
        d1_number(js_sys::Date::now()),
        payload.as_str().into(),
    ])?
    .run()
    .await?;
    Ok(overview)
}

fn render_admin_html_page(overview: &AdminOverviewResponse) -> String {
    let mut rows_html = String::new();
    for session in &overview.sessions {
        let team_size = session
            .team_size
            .map(|value| value.to_string())
            .unwrap_or_else(|| "—".to_string());
        let sport = if session.sport.is_empty() {
            "—".to_string()
        } else {
            html_escape(&session.sport)
        };
        let archived_at = session
            .archived_at_iso
            .as_deref()
            .map(html_escape)
            .unwrap_or_else(|| "—".to_string());
        rows_html.push_str(&format!(
            "<tr>\
                <td><code>{}</code></td>\
                <td>{}</td>\
                <td>{}</td>\
                <td>{}</td>\
                <td>{}</td>\
                <td>{}</td>\
                <td>{}</td>\
                <td>{}</td>\
            </tr>",
            html_escape(&session.session_id),
            html_escape(&session.created_at_iso),
            archived_at,
            html_escape(&session.status),
            sport,
            team_size,
            session.player_count,
            session.ranking_count,
        ));
    }

    if rows_html.is_empty() {
        rows_html.push_str(
            "<tr><td colspan=\"8\">No uploaded sessions found in live or archived storage.</td></tr>",
        );
    }

    format!(
        "<!doctype html>\
         <html lang=\"en\">\
         <head>\
           <meta charset=\"utf-8\">\
           <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
           <title>PPP Admin Sessions</title>\
           <style>\
             :root {{ color-scheme: light; }}\
             body {{ margin: 0; font: 14px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace; background: #f5f1e8; color: #1f2937; }}\
             main {{ max-width: 1200px; margin: 0 auto; padding: 24px; }}\
             h1 {{ margin: 0 0 8px; font-size: 22px; }}\
             p {{ margin: 0 0 16px; color: #4b5563; }}\
             .stats {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 10px; margin: 20px 0 24px; }}\
             .card {{ background: #fffdf8; border: 1px solid #d6d0c4; border-radius: 10px; padding: 12px; }}\
             .label {{ display: block; font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; color: #6b7280; }}\
             .value {{ display: block; margin-top: 6px; font-size: 22px; color: #111827; }}\
             table {{ width: 100%; border-collapse: collapse; background: #fffdf8; border: 1px solid #d6d0c4; }}\
             th, td {{ padding: 10px 12px; border-bottom: 1px solid #e5ded0; text-align: left; vertical-align: top; }}\
             th {{ font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; color: #6b7280; background: #f8f4eb; }}\
             tr:last-child td {{ border-bottom: none; }}\
             code {{ font-size: 12px; }}\
           </style>\
         </head>\
         <body>\
           <main>\
             <h1>Online Session Overview</h1>\
             <p>Generated {generated_at}. This hidden page is served directly by the Worker and is not linked from the public app.</p>\
             <section class=\"stats\">\
               <div class=\"card\"><span class=\"label\">Total Sessions</span><span class=\"value\">{total_sessions}</span></div>\
               <div class=\"card\"><span class=\"label\">Live Sessions</span><span class=\"value\">{live_sessions}</span></div>\
               <div class=\"card\"><span class=\"label\">Archived Sessions</span><span class=\"value\">{archived_sessions}</span></div>\
               <div class=\"card\"><span class=\"label\">Created 24h</span><span class=\"value\">{sessions_last_24h}</span></div>\
               <div class=\"card\"><span class=\"label\">Created 7d</span><span class=\"value\">{sessions_last_7d}</span></div>\
             </section>\
             <table>\
               <thead>\
                 <tr>\
                   <th>Session</th>\
                   <th>Created</th>\
                   <th>Archived</th>\
                   <th>Status</th>\
                   <th>Sport</th>\
                   <th>Team</th>\
                   <th>Players</th>\
                   <th>Ranked</th>\
                 </tr>\
               </thead>\
               <tbody>{rows}</tbody>\
             </table>\
           </main>\
         </body>\
         </html>",
        generated_at = html_escape(&overview.generated_at_iso),
        total_sessions = overview.totals.total_sessions,
        live_sessions = overview.totals.live_sessions,
        archived_sessions = overview.totals.archived_sessions,
        sessions_last_24h = overview.totals.sessions_last_24h,
        sessions_last_7d = overview.totals.sessions_last_7d,
        rows = rows_html,
    )
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
            "Authorization, Content-Type, X-Token, X-Coach-Key",
        );
        let _ = h.set("Vary", "Origin");
        return Ok(resp);
    }

    let route_result = match (method.clone(), path.as_str()) {
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

        // Admin JSON: summarized session overview (secret required)
        (Method::Get, "/api/admin/sessions") => handle_admin_sessions(req, &env, &origin).await,

        // Hidden admin page: server-rendered overview (secret required)
        (Method::Get, "/_internal/admin/sessions") => handle_admin_sessions_page(req, &env).await,

        _ if path.starts_with("/api/") => err_json("Not found", 404, &origin),

        // Static assets handled by Workers Assets; fall through
        _ => Response::error("Not found", 404),
    };

    match route_result {
        Ok(resp) => Ok(resp),
        Err(error) => internal_api_error_response(
            &origin,
            "request_dispatch",
            &error,
            serde_json::json!({
                "method": format!("{method:?}"),
                "path": path,
            }),
        ),
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
    let body: UploadRequest = match parse_json_req(&mut req).await {
        Ok(b) => b,
        Err(e) => return err_json(&e, 400, origin),
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

    let upload_result = async {
        let db = env.d1("DB")?;

        // Keep step-local diagnostics so worker failures identify whether the
        // D1 setup, event serialization, insert, or token generation failed.
        let created_at = js_sys::Date::now();
        let coach_key = gen_token();
        let coach_key_hash = simple_hash(&coach_key, &body.session_id);
        let cursor = body
            .events
            .iter()
            .map(|event| event.session_version as u32)
            .max()
            .unwrap_or(0);
        let next_seq = cursor.saturating_add(1);

        let session_insert_result = db
            .prepare(
                "INSERT OR IGNORE INTO sessions (id, created_at, coach_key_hash, next_seq) VALUES (?1, ?2, ?3, ?4)",
            )
            .bind(&[
                body.session_id.as_str().into(),
                d1_number(created_at),
                coach_key_hash.as_str().into(),
                d1_u32(next_seq),
            ])?
            .run()
            .await?;

        if session_insert_result
            .meta()?
            .and_then(|meta| meta.changes)
            .unwrap_or(0)
            == 0
        {
            return Err(Error::from(format!(
                "SESSION_ALREADY_EXISTS:{}",
                body.session_id
            )));
        }

        let mut event_insert_statements = Vec::with_capacity(body.events.len());
        for env_event in &body.events {
            let payload =
                serde_json::to_string(env_event).map_err(|e| Error::from(e.to_string()))?;
            event_insert_statements.push(build_event_insert_statement(
                &db,
                &body.session_id,
                env_event.session_version as u32,
                &payload,
                true,
            )?);
        }
        run_d1_statement_batches(&db, event_insert_statements).await?;

        let assistant_token = gen_token();
        let player_token = gen_token();
        run_d1_statement_batches(
            &db,
            vec![
                db.prepare(
                    "INSERT INTO share_tokens (token, session_id, role, created_at) \
                     VALUES (?1, ?2, 'assistant', ?3)",
                )
                .bind(&[
                    assistant_token.as_str().into(),
                    body.session_id.as_str().into(),
                    d1_number(created_at),
                ])?,
                db.prepare(
                    "INSERT INTO share_tokens (token, session_id, role, created_at) \
                     VALUES (?1, ?2, 'player', ?3)",
                )
                .bind(&[
                    player_token.as_str().into(),
                    body.session_id.as_str().into(),
                    d1_number(created_at),
                ])?,
            ],
        )
        .await?;

        Ok::<(u32, String, String, String), Error>((
            cursor,
            assistant_token,
            player_token,
            coach_key,
        ))
    }
    .await;

    let (cursor, assistant_token, player_token, coach_key) = match upload_result {
        Ok(result) => result,
        Err(error) => {
            if error.to_string().starts_with("SESSION_ALREADY_EXISTS:") {
                return err_json(
                    "Session already exists online. Reload the existing sync state on this device, or recover on a new device with the coach PIN.",
                    409,
                    origin,
                );
            }
            return internal_api_error_response(
                origin,
                "POST /api/sessions",
                &error,
                serde_json::json!({
                    "session_id": body.session_id,
                    "event_count": body.events.len(),
                }),
            );
        }
    };

    let _ = refresh_admin_overview_snapshot(env).await;
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
        .bind(&[session_id.into(), d1_u64(since)])?
        .all()
        .await?;

    let raw: Vec<StoredEventRow> = rows.results()?;
    ok_json_text(build_events_response_json_body(&raw, since as u32), origin)
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

    let body: AppendRequest = match parse_json_req(&mut req).await {
        Ok(b) => b,
        Err(e) => return err_json(&e, 400, origin),
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

    let reserved_range = db
        .prepare(
            "UPDATE sessions \
             SET next_seq = next_seq + ?1 \
             WHERE id = ?2 \
             RETURNING next_seq - ?1 AS start_seq, next_seq - 1 AS cursor",
        )
        .bind(&[d1_u32(body.events.len() as u32), session_id.into()])?
        .first::<ReservedSequenceRangeRow>(None)
        .await?;
    let reserved_range = match reserved_range {
        Some(row) => row,
        None => return err_json("Session not found", 404, origin),
    };

    let mut event_insert_statements = Vec::with_capacity(body.events.len());
    for (offset, ev) in body.events.into_iter().enumerate() {
        let seq = reserved_range.start_seq + offset as u32;
        let mut stored = ev;
        stored.id = app_core::models::EventId(seq as u64);
        stored.session_version = seq as u64;
        let payload = serde_json::to_string(&stored).map_err(|e| Error::from(e.to_string()))?;
        event_insert_statements.push(build_event_insert_statement(
            &db, session_id, seq, &payload, false,
        )?);
    }
    run_d1_statement_batches(&db, event_insert_statements).await?;

    ok_json(
        &serde_json::json!({ "ok": true, "cursor": reserved_range.cursor }),
        origin,
    )
}

/// POST /api/sessions/:id/pin
///
/// Body: `{ "pin": "1234" }` (4–8 digit string).
/// Stores a salted SHA-256 hash in the sessions table.
/// Requires `X-Coach-Key` header.
/// Returns 200 OK or 400/401/403/404.
async fn handle_set_pin(
    mut req: Request,
    session_id: &str,
    env: &Env,
    origin: &str,
) -> Result<Response> {
    let coach_key_header = req.headers().get("X-Coach-Key")?.unwrap_or_default();
    if coach_key_header.is_empty() {
        return err_json("Missing X-Coach-Key header", 401, origin);
    }

    #[derive(serde::Deserialize)]
    struct PinBody {
        pin: String,
    }
    let body: PinBody = match parse_json_req(&mut req).await {
        Ok(b) => b,
        Err(e) => return err_json(&e, 400, origin),
    };
    if body.pin.len() < 4 || body.pin.len() > 8 || !body.pin.chars().all(|c| c.is_ascii_digit()) {
        return err_json("PIN must be 4–8 digits", 400, origin);
    }

    let db = env.d1("DB")?;
    // Confirm session exists and validate coach key
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
    let body: RecoverBody = match parse_json_req(&mut req).await {
        Ok(b) => b,
        Err(e) => return err_json(&e, 400, origin),
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
    ok_json_text(build_events_response_json_body(&raw, 0), origin)
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
    let body: AuthBody = match parse_json_req(&mut req).await {
        Ok(b) => b,
        Err(e) => return err_json(&e, 400, origin),
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
    let body: PinBody = match parse_json_req(&mut req).await {
        Ok(b) => b,
        Err(e) => return err_json(&e, 400, origin),
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

/// GET /api/admin/sessions — returns a high-level overview for live and archived
/// online sessions. Accepts `X-Token`, `Authorization: Bearer`, or HTTP Basic
/// auth where the password matches `PPP_ADMIN_SECRET`.
async fn handle_admin_sessions(req: Request, env: &Env, origin: &str) -> Result<Response> {
    let secret = env
        .var("PPP_ADMIN_SECRET")
        .map(|v| v.to_string())
        .unwrap_or_default();
    if !request_has_admin_access(&req, &secret) {
        return admin_unauthorized_json(origin);
    }

    let overview = load_admin_overview_snapshot(env).await?;
    Ok(with_admin_security_headers(
        with_cors(Response::from_json(&overview)?, origin),
        false,
    ))
}

/// GET /_internal/admin/sessions — hidden server-rendered admin page.
/// Uses the same authorization rules as the JSON endpoint and is intentionally
/// not linked from the public SPA.
async fn handle_admin_sessions_page(req: Request, env: &Env) -> Result<Response> {
    let secret = env
        .var("PPP_ADMIN_SECRET")
        .map(|v| v.to_string())
        .unwrap_or_default();
    if !request_has_admin_access(&req, &secret) {
        return admin_unauthorized_html();
    }

    let overview = load_admin_overview_snapshot(env).await?;
    let html = render_admin_html_page(&overview);
    let mut resp = Response::from_body(ResponseBody::Body(html.into_bytes()))?;
    let _ = resp
        .headers_mut()
        .set("Content-Type", "text/html; charset=utf-8");
    Ok(with_admin_security_headers(resp, true))
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

    let body: ArchiveBody = match parse_json_req(&mut req).await {
        Ok(b) => b,
        Err(e) => return err_json(&e, 400, origin),
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
        d1_u32(body.team_size as u32),
        d1_number(created_at),
        d1_number(js_sys::Date::now()),
        player_names_json.as_str().into(),
        final_rankings_json.as_str().into(),
    ])?
    .run()
    .await?;

    let _ = refresh_admin_overview_snapshot(env).await;
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

    let body: HeartbeatBody = match parse_json_req(&mut req).await {
        Ok(b) => b,
        Err(e) => return err_json(&e, 400, origin),
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
        d1_number(now),
        label.as_str().into(),
    ])?
    .run()
    .await?;

    let active_threshold = now - 5.0 * 60.0 * 1000.0;
    let rows = db
        .prepare(
            "SELECT device_id, label, last_seen FROM device_heartbeats \
             WHERE session_id = ?1 AND last_seen >= ?2 \
             ORDER BY last_seen DESC",
        )
        .bind(&[session_id.into(), d1_number(active_threshold)])?
        .all()
        .await?;

    let devices: Vec<serde_json::Value> = rows.results()?;
    ok_json(
        &serde_json::json!({ "ok": true, "devices": devices }),
        origin,
    )
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

    // Devices active in the last 5 minutes
    let active_threshold = js_sys::Date::now() - 5.0 * 60.0 * 1000.0;
    let rows = db
        .prepare(
            "SELECT device_id, label, last_seen FROM device_heartbeats \
             WHERE session_id = ?1 AND last_seen >= ?2 \
             ORDER BY last_seen DESC",
        )
        .bind(&[session_id.into(), d1_number(active_threshold)])?
        .all()
        .await?;

    let devices: Vec<serde_json::Value> = rows.results()?;
    ok_json(&serde_json::json!({ "devices": devices }), origin)
}

// ── Scheduled cleanup ─────────────────────────────────────────────────────────
// Isolated in a submodule so we can suppress the `unused_must_use` warning that
// the `worker` proc-macro generates around `#[event(scheduled)]` without using
// a crate-level `#![allow(…)]`.

#[allow(unused_must_use)]
mod cleanup_job {
    use super::*;

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
        .bind(&[d1_number(raw_cutoff)])?
        .run()
        .await?;

        // Delete share tokens for those sessions
        db.prepare(
            "DELETE FROM share_tokens WHERE session_id IN \
             (SELECT id FROM sessions WHERE created_at < ?1)",
        )
        .bind(&[d1_number(raw_cutoff)])?
        .run()
        .await?;

        // Delete the session rows themselves
        db.prepare("DELETE FROM sessions WHERE created_at < ?1")
            .bind(&[d1_number(raw_cutoff)])?
            .run()
            .await?;

        // Prune old archived summaries
        db.prepare("DELETE FROM archived_sessions WHERE created_at < ?1")
            .bind(&[d1_number(archive_cutoff)])?
            .run()
            .await?;

        // Prune stale device heartbeats older than 24 hours
        let heartbeat_cutoff = now - 86_400_000.0;
        db.prepare("DELETE FROM device_heartbeats WHERE last_seen < ?1")
            .bind(&[d1_number(heartbeat_cutoff)])?
            .run()
            .await?;

        let _ = refresh_admin_overview_snapshot(&env).await;
        Ok(())
    }
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
            .bind(&[t.as_str().into(), session_id.into(), d1_number(now)])?
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
            .bind(&[t.as_str().into(), session_id.into(), d1_number(now)])?
            .run()
            .await?;
            t
        }
    };

    Ok((assistant_token, player_token))
}

#[cfg(test)]
mod tests {
    use super::{
        build_admin_session_overview_row, build_events_response_json_body, header_has_admin_access,
        parse_basic_auth_password, timing_safe_eq, StoredEventRow,
    };

    #[test]
    fn build_events_response_json_body_keeps_stored_payloads_verbatim() {
        let rows = vec![
            StoredEventRow {
                seq: 3,
                payload: "{\"session_version\":3}".to_string(),
            },
            StoredEventRow {
                seq: 4,
                payload: "{\"session_version\":4}".to_string(),
            },
        ];

        assert_eq!(
            build_events_response_json_body(&rows, 1),
            "{\"events\":[{\"session_version\":3},{\"session_version\":4}],\"cursor\":4}"
        );
    }

    #[test]
    fn build_events_response_json_body_uses_fallback_cursor_for_empty_pages() {
        assert_eq!(
            build_events_response_json_body(&[], 12),
            "{\"events\":[],\"cursor\":12}"
        );
    }

    #[test]
    fn parse_basic_auth_password_reads_the_password_field() {
        assert_eq!(
            parse_basic_auth_password("Basic dXNlcjpzZWNyZXQ=").as_deref(),
            Some("secret")
        );
        assert_eq!(parse_basic_auth_password("Bearer secret"), None);
    }

    #[test]
    fn timing_safe_eq_matches_only_identical_values() {
        assert!(timing_safe_eq("secret", "secret"));
        assert!(!timing_safe_eq("secret", "Secret"));
        assert!(!timing_safe_eq("secret", "secret2"));
    }

    #[test]
    fn header_has_admin_access_accepts_basic_bearer_and_x_token() {
        assert!(header_has_admin_access(
            None,
            Some("Basic dXNlcjpzZWNyZXQ="),
            "secret",
        ));
        assert!(header_has_admin_access(
            None,
            Some("Bearer secret"),
            "secret"
        ));
        assert!(header_has_admin_access(Some("secret"), None, "secret"));
        assert!(!header_has_admin_access(Some("wrong"), None, "secret"));
    }

    #[test]
    fn build_admin_session_overview_row_counts_players_and_rankings() {
        let row = serde_json::json!({
            "id": "session-1",
            "created_at": 1_700_000_000_000.0,
            "archived_at": 1_700_000_100_000.0,
            "sport": "Soccer",
            "team_size": 5,
            "player_names": "{\"1\":\"Alice\",\"2\":\"Bob\",\"3\":\"Casey\"}",
            "final_rankings": "[{\"player_id\":\"1\"},{\"player_id\":\"2\"}]"
        });

        let built = build_admin_session_overview_row(&row, true);
        assert_eq!(built.session_id, "session-1");
        assert_eq!(built.status, "live+archived");
        assert_eq!(built.player_count, 3);
        assert_eq!(built.ranking_count, 2);
        assert_eq!(built.team_size, Some(5));
    }
}
