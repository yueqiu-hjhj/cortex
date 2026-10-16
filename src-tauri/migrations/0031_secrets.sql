-- Encrypted secret store. On Windows rows are DPAPI-encrypted (flags=1);
-- the same user on the same machine is the only one who can decrypt them.
-- The known-bytes setting values that used to live in the `settings` table
-- are migrated across here on upgrade; the next set_* call removes them from
-- the plaintext table.

PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS secrets (
  key          TEXT PRIMARY KEY,
  ciphertext   BLOB NOT NULL,
  flags        INTEGER NOT NULL DEFAULT 0,
  updated_at   INTEGER NOT NULL
);

-- One-shot migration: any *_api_key / *_token / oauth_* / homelab_token row
-- that pre-existed in `settings` is moved into `secrets` so the plaintext
-- copy can be dropped. We don't know which strings are actually sensitive,
-- so we promote the obvious-shaped keys only.
INSERT OR IGNORE INTO secrets (key, ciphertext, flags, updated_at)
SELECT key, value, 0, strftime('%s','now')*1000 FROM settings
 WHERE key LIKE '%_api_key'
    OR key LIKE '%_api_secret'
    OR key LIKE '%_token'
    OR key LIKE 'oauth_%'
    OR key LIKE 'homelab_token';

DELETE FROM settings
 WHERE key LIKE '%_api_key'
    OR key LIKE '%_api_secret'
    OR key LIKE '%_token'
    OR key LIKE 'oauth_%'
    OR key LIKE 'homelab_token';
