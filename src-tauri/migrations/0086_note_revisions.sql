-- Editing a note, and the trail that makes editing safe.
--
-- Until now every agent write was append-only (0076: save_note, save_flashcards),
-- so an agent could create but never correct — a typo in a generated note was
-- permanent unless the user went and fixed it by hand. A real edit path is only
-- acceptable if the previous text survives it, because the agent cannot know
-- what it is destroying.
--
-- This table is a log, not a second truth: the live body stays in `notes` and in
-- the vault Markdown file. A revision answers "what did it look like before, and
-- who changed it" — pruned to the newest 20 per note by repo code.
CREATE TABLE IF NOT EXISTS note_revisions (
  id         TEXT PRIMARY KEY,
  note_id    TEXT NOT NULL,
  title      TEXT NOT NULL,
  body       TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  actor      TEXT,
  reason     TEXT
);
CREATE INDEX IF NOT EXISTS idx_note_revisions_note
  ON note_revisions(note_id, created_at DESC);

-- Two more built-in tools: read a note in full, edit it in place.
-- 0034 rebuilt agent_tools with user_id/built_in NOT NULL (no default), so seed
-- rows that omit them are silently dropped by INSERT OR IGNORE.
INSERT OR IGNORE INTO agent_tools (id, user_id, name, description, kind, enabled, schema_json, built_in, created_at)
VALUES
  ('tool-get-note', 'u-default', 'get_note', 'Read one note in full (title, body, revision history) before editing it.', 'read', 1, '{}', 1, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)),
  ('tool-update-note', 'u-default', 'update_note', 'Edit an existing note: find+replace, whole-body rewrite, or undo a revision.', 'write', 1, '{}', 1, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
