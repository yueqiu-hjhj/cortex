-- 0050_paper_tensions.sql — cross-paper tension detection.
--
-- 知源's 整理助手 reads the whole knowledge base and surfaces where the
-- literature DISAGREES. A systematic review that only summarizes consensus
-- is half a review; this run-shape (mirroring PRISMA 0048) stores one row per
-- detected tension: the topic, both papers (id-validated against the run's
-- papers — no hallucinated literature), both claims, the kind of conflict,
-- severity, and the VERBATIM quotes that evidence each side. A tension
-- without quotes is an assertion; the extractor refuses to store those.
CREATE TABLE IF NOT EXISTS tension_runs (
  id         TEXT PRIMARY KEY,
  title      TEXT NOT NULL,
  status     TEXT NOT NULL,            -- running | done | error
  error      TEXT,
  item_ids   TEXT NOT NULL,            -- JSON array of paper ids
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS tension_runs_created_idx ON tension_runs(created_at DESC);

CREATE TABLE IF NOT EXISTS tensions (
  id         TEXT PRIMARY KEY,
  run_id     TEXT NOT NULL REFERENCES tension_runs(id) ON DELETE CASCADE,
  topic      TEXT NOT NULL,
  paper_a_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
  paper_b_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
  claim_a    TEXT NOT NULL,
  claim_b    TEXT NOT NULL,
  conflict   TEXT NOT NULL,            -- contradiction | complementary | evidence | scope
  severity   TEXT NOT NULL,            -- high | medium | low
  quote_a    TEXT NOT NULL,
  quote_b    TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS tensions_run_idx ON tensions(run_id, created_at);
