-- Agent write-tier tools. 0030 seeded five read tools; tiered permissions
-- (0075) now give write-class agents real output paths — two bounded,
-- audited writers that go through the same repo helpers the UI uses.
-- No shell, no delete, no overwrite: agents append, humans curate.
-- 0034 rebuilt this table with user_id/built_in (both NOT NULL) — seed rows
-- must set them or INSERT OR IGNORE silently drops the row.
INSERT OR IGNORE INTO agent_tools (id, user_id, name, description, kind, enabled, schema_json, built_in, created_at)
VALUES
  ('tool-save-note', 'u-default', 'save_note', 'Create a new note (title + body) in the knowledge base.', 'write', 1, '{}', 1, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)),
  ('tool-save-flashcards', 'u-default', 'save_flashcards', 'Save a Q/A deck (1-50 cards) as a flashcards material.', 'write', 1, '{}', 1, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
