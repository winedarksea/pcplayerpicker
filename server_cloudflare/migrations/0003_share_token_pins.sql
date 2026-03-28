-- Optional PIN protection for assistant and player share tokens.
-- If pin_hash is NULL the token is publicly accessible (existing behavior).
ALTER TABLE share_tokens ADD COLUMN pin_hash TEXT DEFAULT NULL;
