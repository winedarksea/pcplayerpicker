-- Track which coach devices have the session open.
-- Used to display an active-device count in the OnlineTab so the coach knows
-- how many devices are connected and can hand off to another device if needed.
--
-- last_seen: Unix timestamp in milliseconds (js_sys::Date::now()).
-- label: optional human-readable device identifier (e.g. "iPhone 15").

CREATE TABLE IF NOT EXISTS device_heartbeats (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    device_id  TEXT NOT NULL,
    last_seen  INTEGER NOT NULL,
    label      TEXT,
    PRIMARY KEY (session_id, device_id)
);

CREATE INDEX IF NOT EXISTS idx_heartbeats_session
    ON device_heartbeats (session_id, last_seen);
