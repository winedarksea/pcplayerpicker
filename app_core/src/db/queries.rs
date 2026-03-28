// SQL schema constants for the future OPFS/SQLite data store path.
//
// These are NOT used by the Cloudflare Worker (server_cloudflare/src/lib.rs).
// The Worker stores the entire session as an append-only event log with a
// minimal schema (sessions + events + share_tokens) defined in
// server_cloudflare/migrations/. That schema is intentionally simpler because
// the Worker is a dumb event store — all business logic runs client-side.
//
// When OPFS SQLite storage is implemented on the client, these constants will
// provide the full normalized schema needed to reconstruct rich query views
// without replaying the entire event log.

pub const CREATE_SESSIONS_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS sessions (
        id          TEXT PRIMARY KEY,
        team_size   INTEGER NOT NULL,
        scheduling_frequency INTEGER NOT NULL,
        sport       TEXT NOT NULL,
        seed        INTEGER NOT NULL,
        reseed_count INTEGER NOT NULL DEFAULT 0,
        match_duration_minutes INTEGER,
        created_at  TEXT NOT NULL
    );
";

pub const CREATE_EVENTS_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS events (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id      TEXT NOT NULL REFERENCES sessions(id),
        session_version INTEGER NOT NULL,
        entered_by      TEXT NOT NULL,
        payload         TEXT NOT NULL  -- JSON-encoded Event
    );
";

pub const CREATE_PLAYERS_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS players (
        id          INTEGER NOT NULL,
        session_id  TEXT NOT NULL REFERENCES sessions(id),
        name        TEXT NOT NULL,
        status      TEXT NOT NULL DEFAULT 'Active',
        joined_at_round INTEGER NOT NULL,
        deactivated_at_round INTEGER,
        PRIMARY KEY (id, session_id)
    );
";

pub const CREATE_MATCHES_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS matches (
        id          INTEGER NOT NULL,
        session_id  TEXT NOT NULL REFERENCES sessions(id),
        round       INTEGER NOT NULL,
        field       INTEGER NOT NULL,
        team_a      TEXT NOT NULL,  -- JSON array of player IDs
        team_b      TEXT NOT NULL,  -- JSON array of player IDs
        status      TEXT NOT NULL DEFAULT 'Scheduled',
        PRIMARY KEY (id, session_id)
    );
";

pub const CREATE_RESULTS_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS results (
        match_id    INTEGER NOT NULL,
        session_id  TEXT NOT NULL REFERENCES sessions(id),
        scores      TEXT NOT NULL,  -- JSON: { player_id: { goals: u16 | null } }
        duration_multiplier REAL NOT NULL DEFAULT 1.0,
        entered_by  TEXT NOT NULL,
        PRIMARY KEY (match_id, session_id)
    );
";

pub const CREATE_RANKINGS_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS rankings (
        player_id   INTEGER NOT NULL,
        session_id  TEXT NOT NULL REFERENCES sessions(id),
        round       INTEGER NOT NULL,
        rating      REAL NOT NULL,
        uncertainty REAL NOT NULL,
        rank        INTEGER NOT NULL,
        rank_lo_90  INTEGER NOT NULL,
        rank_hi_90  INTEGER NOT NULL,
        matches_played INTEGER NOT NULL DEFAULT 0,
        total_goals INTEGER NOT NULL DEFAULT 0,
        prob_top_k  REAL NOT NULL DEFAULT 0.0,
        is_active   INTEGER NOT NULL DEFAULT 1,  -- boolean
        PRIMARY KEY (player_id, session_id, round)
    );
";

pub const ALL_MIGRATIONS: &[&str] = &[
    CREATE_SESSIONS_TABLE,
    CREATE_EVENTS_TABLE,
    CREATE_PLAYERS_TABLE,
    CREATE_MATCHES_TABLE,
    CREATE_RESULTS_TABLE,
    CREATE_RANKINGS_TABLE,
];
