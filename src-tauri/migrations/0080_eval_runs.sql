-- Retrieval eval runs (KnowNote adoption-rule borrow): every `retrieval_eval`
-- run persists its four mode reports as JSON. The adoption rule compares a
-- candidate run against the previous run of the same kind — synthetic runs are
-- comparable across time (same corpus); real-corpus runs also reflect library
-- edits, which the verdict's note says out loud. Only the newest rows matter;
-- old ones are harmless history.
CREATE TABLE IF NOT EXISTS eval_runs (
  id          TEXT PRIMARY KEY,
  kind        TEXT NOT NULL,               -- synthetic | real
  report_json TEXT NOT NULL,
  created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_eval_runs_kind ON eval_runs(kind, created_at DESC);
