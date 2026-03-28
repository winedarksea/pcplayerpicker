-- Add coach write-key hash to sessions table.
-- The coach receives a random coach_key on initial upload (POST /api/sessions)
-- and must echo it back as X-Coach-Key for all subsequent event appends.
-- This prevents arbitrary UUIDs from injecting events into a session.
ALTER TABLE sessions ADD COLUMN coach_key_hash TEXT NOT NULL DEFAULT '';
