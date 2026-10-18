-- 0045_note_links.sql — wiki-style [[double links]] between notes.
--
-- The competitive gap: notes were plain Markdown; 知源's core UX is that
-- writing [[德布罗意波长|物质波]] in any note creates a link and the target's
-- "backlinks" panel shows every note that mentions it. This table is the index
-- (rebuilt on save, deleted with the source note); the link TARGET is stored as
-- a lowercased title slug so renaming a target only needs a re-slug pass, not
-- an id hunt.
CREATE TABLE IF NOT EXISTS note_links (
  id           TEXT PRIMARY KEY,
  from_note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
  target_slug  TEXT NOT NULL,
  alias        TEXT,
  created_at   INTEGER NOT NULL,
  updated_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_nl_from   ON note_links(from_note_id);
CREATE INDEX IF NOT EXISTS idx_nl_target ON note_links(target_slug);
