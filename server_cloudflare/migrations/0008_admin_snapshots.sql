-- Precomputed admin overview snapshot.
--
-- The Worker CPU budget is too small for multi-query live admin aggregation on
-- each request, so we persist a single compact JSON payload and refresh it only
-- when online-session metadata changes.
CREATE TABLE IF NOT EXISTS admin_snapshots (
    snapshot_key TEXT PRIMARY KEY,
    updated_at   REAL NOT NULL,
    payload      TEXT NOT NULL
);
