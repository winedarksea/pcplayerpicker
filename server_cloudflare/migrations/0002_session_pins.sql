-- Session PIN hashes for cross-device coach recovery.
-- The PIN is stored as a simple hex hash (not bcrypt — Workers have no crypto libs,
-- and this protects convenience access, not sensitive data).
ALTER TABLE sessions ADD COLUMN coach_pin_hash TEXT DEFAULT NULL;
