-- 0038_papers.sql
--
-- The research pipeline had a hole in the middle. `literature_search` returned
-- hits, `LiteratureReader` rendered one, and `literature_save_source` turned a
-- paper into a generic `source` — but nothing recorded "I decided to read this",
-- "I read it", "this sentence matters". So the pipeline was search → reading →
-- (nothing), and nothing could be cited later.
--
-- Two tables close it:
--   papers   — the reading list, with status/rating and an optional source_id
--              once the full text has been ingested
--   excerpts — quoted passages with page + note, the raw material for a
--              literature review (linked to wiki pages via item_links)
--
-- Papers are deduplicated by DOI when one exists: importing the same paper from
-- OpenAlex and Crossref must not create two rows.

PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS papers (
  id         TEXT PRIMARY KEY,
  title      TEXT NOT NULL,
  -- Comma-separated display form ("Ashish Vaswani, Noam Shazeer"). A join table
  -- would be more correct and would also be unreadable in every query we need.
  authors    TEXT,
  year       INTEGER,
  venue      TEXT,
  doi        TEXT,
  url        TEXT,
  abstract   TEXT,
  -- to_read | reading | read | cited | dropped
  status     TEXT NOT NULL DEFAULT 'to_read',
  rating     INTEGER,          -- 1..5, your own judgement after reading
  notes      TEXT,
  subject_id TEXT REFERENCES subjects(id) ON DELETE SET NULL,
  -- Set once the full text is ingested as a source, so excerpts can quote from
  -- the chunked text and chat can retrieve it.
  source_id  TEXT REFERENCES sources(id) ON DELETE SET NULL,
  added_at   INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_papers_status  ON papers(status, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_papers_subject ON papers(subject_id, status);
-- Partial unique index: dedupe on DOI, but allow many papers without one.
CREATE UNIQUE INDEX IF NOT EXISTS idx_papers_doi
  ON papers(doi) WHERE doi IS NOT NULL AND doi <> '';

CREATE TABLE IF NOT EXISTS excerpts (
  id         TEXT PRIMARY KEY,
  paper_id   TEXT REFERENCES papers(id) ON DELETE CASCADE,
  -- Excerpts can also come from an ingested source that has no paper record
  -- (a lecture slide, a textbook PDF), so both parents are optional.
  source_id  TEXT REFERENCES sources(id) ON DELETE SET NULL,
  quote      TEXT NOT NULL,
  page       INTEGER,
  note       TEXT,
  tags       TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_excerpts_paper  ON excerpts(paper_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_excerpts_source ON excerpts(source_id, created_at DESC);

CREATE TRIGGER IF NOT EXISTS tomb_papers AFTER DELETE ON papers BEGIN
  INSERT OR REPLACE INTO tombstones VALUES ('papers', OLD.id, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;
CREATE TRIGGER IF NOT EXISTS tomb_excerpts AFTER DELETE ON excerpts BEGIN
  INSERT OR REPLACE INTO tombstones VALUES ('excerpts', OLD.id, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;
