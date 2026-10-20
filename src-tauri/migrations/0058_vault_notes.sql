-- Vault layer (P1): a note's Markdown file is the source of truth and the
-- `notes` row is a rebuildable index. vault_path holds the file location
-- relative to the vault root ("notes/标题.md"); NULL means the note has no file
-- yet (DB-only, exported on the next vault pass).
ALTER TABLE notes ADD COLUMN vault_path TEXT;
CREATE INDEX IF NOT EXISTS idx_notes_vault_path ON notes(vault_path);
