-- 0070_redteam.sql — top-venue red-team peer review.
--
-- 知源's 科研工作台 runs a simulated top-journal reviewer over a paper
-- before submission: Soundness / Novelty / Rigor scores, the flaws that
-- alone justify rejection, the rescue experiments that would answer them,
-- and the rebuttal strategy. The value is the HOSTILE reading — a summary
-- of your own paper tells you what you already believe.
--
-- Same run-shape as tensions (0050): one run per paper, findings as rows.
-- Every fatal flaw / weakness MUST carry a verbatim quote from the paper
-- card — a critique without a quote is just an assertion, and this table
-- refuses to store those (the same discipline as tensions). Rescue and
-- rebuttal rows are strategies, so quotes are optional there.
CREATE TABLE IF NOT EXISTS redteam_runs (
  id         TEXT PRIMARY KEY,
  title      TEXT NOT NULL,
  status     TEXT NOT NULL,            -- running | done | error
  error      TEXT,
  paper_id   TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
  venue      TEXT NOT NULL,            -- the bar being reviewed against
  soundness  INTEGER,                  -- 1-10, set when the run finishes
  novelty    INTEGER,
  rigor      INTEGER,
  verdict    TEXT,                     -- accept | minor | major | reject
  summary    TEXT,                     -- the reviewer's overall read
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS redteam_runs_created_idx ON redteam_runs(created_at DESC);

CREATE TABLE IF NOT EXISTS redteam_findings (
  id       TEXT PRIMARY KEY,
  run_id   TEXT NOT NULL REFERENCES redteam_runs(id) ON DELETE CASCADE,
  kind     TEXT NOT NULL,              -- fatal | weakness | rescue | rebuttal
  title    TEXT NOT NULL,              -- one-line label
  detail   TEXT NOT NULL,              -- the argument itself
  quote    TEXT,                       -- verbatim from the card (required for fatal/weakness)
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS redteam_findings_run_idx ON redteam_findings(run_id, created_at);
