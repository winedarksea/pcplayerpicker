-- Initial schema for PCPlayerPicker Worker D1 database.

CREATE TABLE IF NOT EXISTS sessions (
    id         TEXT PRIMARY KEY,
    created_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    seq        INTEGER NOT NULL,
    payload    TEXT NOT NULL,
    UNIQUE (session_id, seq)
);

CREATE TABLE IF NOT EXISTS share_tokens (
    token      TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    role       TEXT NOT NULL,
    created_at REAL NOT NULL
);
