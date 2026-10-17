-- External provenance: a record of "this notebook content came from this
-- external document". KnowNote-style agents verify a snippet they hold was
-- actually ingested by the user. One row per (source, host, doc_id); the
-- snippet is the exact text returned by the host at retrieval time so future
-- audits can compare what was shown against what is stored.
CREATE TABLE IF NOT EXISTS source_provenance (
  id            TEXT PRIMARY KEY,
  source_id     TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
  host          TEXT NOT NULL,
  doc_id        TEXT NOT NULL,
  url           TEXT,
  snippet       TEXT,
  retrieved_at  INTEGER NOT NULL,
  UNIQUE(source_id, host, doc_id)
);
CREATE INDEX IF NOT EXISTS source_provenance_host_idx ON source_provenance(host, doc_id);