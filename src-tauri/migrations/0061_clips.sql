-- Clips: one selected passage, captured from anywhere (an AI answer, a note, a
-- PDF page, the web) and carried to a note. Distinct from `excerpts`, which is
-- the *paper evidence* shelf: an excerpt must belong to a paper or a source and
-- is meant to be cited. A clip is a sentence you thought was worth keeping —
-- it has no owner until you give it one, which is exactly why `note_id` is
-- nullable: NULL means "still in the inbox, waiting to be filed".
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS clips (
  id           TEXT PRIMARY KEY,
  quote        TEXT NOT NULL,
  source_kind  TEXT NOT NULL,                    -- chat | agent | note | paper | source | web | manual
  source_id    TEXT,                             -- thread / run / note / paper / source id
  source_label TEXT,                             -- what the user should see: "AI 回答 · deepseek"
  page         INTEGER,
  note_id      TEXT,                             -- NULL = unfiled
  block        TEXT,                             -- exact Markdown written into the note (for undo)
  created_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_clips_pending ON clips(note_id, created_at);
CREATE INDEX IF NOT EXISTS idx_clips_source ON clips(source_kind, source_id);
