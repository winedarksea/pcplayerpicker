-- Give each session a server-owned append cursor so concurrent writers can
-- reserve sequence ranges without scanning the events table.
ALTER TABLE sessions ADD COLUMN next_seq INTEGER NOT NULL DEFAULT 0;

UPDATE sessions
SET next_seq = COALESCE(
    (
        SELECT MAX(seq) + 1
        FROM events
        WHERE events.session_id = sessions.id
    ),
    0
);

-- Keep the common cleanup and token lookups indexed.
CREATE INDEX IF NOT EXISTS idx_sessions_created_at
    ON sessions (created_at);

CREATE INDEX IF NOT EXISTS idx_share_tokens_session_role
    ON share_tokens (session_id, role);

CREATE INDEX IF NOT EXISTS idx_archived_sessions_created_at
    ON archived_sessions (created_at);
