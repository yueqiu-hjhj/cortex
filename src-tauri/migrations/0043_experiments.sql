-- 0043_experiments.sql — 科研一体化 gap: 复现实验 (the last structural hole).
--
-- The LIGHTWEIGHT version of OpenScience/ResearchClaw's experiment layer: an
-- experiment CARD attached to a paper (or subject) that records what actually
-- makes research reproducible in practice —
--   hypothesis   what we're testing
--   code         the script/notebook cells as text (versioned by updated_at;
--                running it is the agent's job, not the DB's)
--   environment  "python 3.11 + torch 2.4, cuda 12.1, seed 42"
--   result       observed output, pasted or written back by the agent
--   status       planned | running | done | failed
-- Standard id/created_at/updated_at shape → livesync merges it generically.
CREATE TABLE IF NOT EXISTS experiments (
  id          TEXT PRIMARY KEY,
  paper_id    TEXT REFERENCES papers(id) ON DELETE SET NULL,
  subject_id  TEXT REFERENCES subjects(id) ON DELETE SET NULL,
  title       TEXT NOT NULL,
  hypothesis  TEXT,
  code        TEXT,
  environment TEXT,
  result      TEXT,
  status      TEXT NOT NULL DEFAULT 'planned',
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
);
CREATE INDEX idx_exp_paper   ON experiments(paper_id);
CREATE INDEX idx_exp_subject ON experiments(subject_id);
