-- 0039_drafts.sql
--
-- Closes the last leg of the research pipeline: excerpts (verbatim quotes with
-- page + note) existed, BibTeX export existed, but nothing turned a selection of
-- excerpts into citable prose. `drafts` stores the generated output together
-- with the exact excerpt ids it was built from — index+1 of `excerpt_ids` is the
-- citation number [n] used inside `payload`, so the numbering is reproducible
-- without re-asking the model.
--
-- Drafts are research artifacts, deliberately not bound to a subject: a review
-- paragraph routinely cites papers that live in different subjects.

CREATE TABLE IF NOT EXISTS drafts (
  id          TEXT PRIMARY KEY,
  title       TEXT NOT NULL,
  -- JSON array of excerpt ids, in citation order. [1] == excerpt_ids[0].
  excerpt_ids TEXT NOT NULL,
  meta        TEXT,
  payload     TEXT,
  created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_drafts_created ON drafts(created_at DESC);

CREATE TRIGGER IF NOT EXISTS tomb_drafts AFTER DELETE ON drafts BEGIN
  INSERT OR REPLACE INTO tombstones VALUES ('drafts', OLD.id, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;
