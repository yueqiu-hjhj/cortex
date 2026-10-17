-- User accounts for multi-user isolation. Each user gets a deterministic
-- cryptographic salt; their row keys (the `user_id` columns added in 0034) act
-- as a tenant key, so cross-user reads require an explicit `WHERE user_id=?`
-- that the data-access layer always supplies. The password field is
-- intentionally absent -- �� auth is a per-platform concern (DPAPI on Windows,
-- Keychain on macOS, libsecret on Linux). A bootstrap user is created so a
-- fresh install opens into a usable state.
CREATE TABLE IF NOT EXISTS users (
  id            TEXT PRIMARY KEY,
  display_name  TEXT NOT NULL,
  email         TEXT,
  role          TEXT NOT NULL DEFAULT 'user',    -- user | admin
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL,
  last_seen_at  INTEGER
);
INSERT OR IGNORE INTO users (id, display_name, role, created_at, updated_at)
VALUES ('u-default', 'Default', 'admin', CAST((julianday(CURRENT_TIMESTAMP) - 2440587.5) * 86400000 AS INTEGER),
                                 CAST((julianday(CURRENT_TIMESTAMP) - 2440587.5) * 86400000 AS INTEGER));

CREATE TABLE IF NOT EXISTS user_sessions (
  token         TEXT PRIMARY KEY,
  user_id       TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at    INTEGER NOT NULL,
  expires_at    INTEGER NOT NULL,
  revoked_at    INTEGER
);
CREATE INDEX IF NOT EXISTS user_sessions_user_idx ON user_sessions(user_id, expires_at DESC);
