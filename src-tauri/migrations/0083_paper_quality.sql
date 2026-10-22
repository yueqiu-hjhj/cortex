-- 0083_paper_quality.sql — external quality signals for papers (retraction / OA).
--
-- Filled by the `cortex-paper-meta` tool (`--check`), not by the app: both
-- facts are network-bound and change over time, so they must never block an
-- import. Kept in its own table so a failed lookup can never poison `papers`.
--
-- `retracted` is what the literature list badges; `oa_pdf_url` is what
-- `--fetch-oa` downloads. `error` records a per-paper failure without
-- aborting the batch, and `checked_at` drives the 30-day freshness cache.
CREATE TABLE IF NOT EXISTS paper_quality (
  paper_id        TEXT PRIMARY KEY,
  doi             TEXT,
  retracted       INTEGER NOT NULL DEFAULT 0,
  retraction_doi  TEXT,
  retraction_kind TEXT,
  is_notice       INTEGER NOT NULL DEFAULT 0,
  oa_status       TEXT,
  oa_pdf_url      TEXT,
  checked_at      INTEGER NOT NULL,
  error           TEXT
);
CREATE INDEX IF NOT EXISTS idx_paper_quality_retracted ON paper_quality(retracted);
