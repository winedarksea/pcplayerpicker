-- Long-lived summary of completed sessions.
--
-- Written by the coach client whenever rankings are pushed to an online session
-- (POST /api/sessions/:id/archive). The cron cleanup job deletes the raw event
-- log on a shorter schedule (ARCHIVE_AFTER_DAYS) but keeps these summaries for
-- ARCHIVE_RETENTION_DAYS so final results remain queryable without the full log.
--
-- created_at mirrors the original sessions.created_at so we can age-prune
-- summaries independently of raw events.
CREATE TABLE archived_sessions (
    id              TEXT PRIMARY KEY,         -- original session_id
    sport           TEXT NOT NULL DEFAULT '', -- e.g. "Soccer"
    team_size       INTEGER NOT NULL DEFAULT 0,
    created_at      REAL NOT NULL DEFAULT 0,  -- original session creation (ms epoch)
    archived_at     REAL NOT NULL,            -- last time archive was written (ms epoch)
    player_names    TEXT NOT NULL DEFAULT '{}', -- JSON object: { "1": "Alice", "2": "Bob", ... }
    final_rankings  TEXT                      -- JSON array: Vec<PlayerRanking>, NULL if no rankings yet
);
