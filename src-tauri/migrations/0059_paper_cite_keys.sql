-- Stable citation keys for papers.
--
-- The BibTeX key used to be derived on the fly from first-author surname +
-- year + first title word. That breaks the moment a title is corrected: every
-- \cite{...} already typed into a manuscript silently points at nothing.
-- A key that an external agent (or a human) has quoted must never change, so
-- it is assigned once and stored here.
--
-- NULL until papers::ensure_cite_keys() backfills it; the partial unique index
-- keeps existing databases migratable without giving every row a placeholder.
ALTER TABLE papers ADD COLUMN cite_key TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS idx_papers_cite_key
  ON papers(cite_key) WHERE cite_key IS NOT NULL;
