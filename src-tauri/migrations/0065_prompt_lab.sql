-- Prompt lab: fixtures you can re-run, and the record of what each run said.
--
-- A prompt change is a behavioural change, so it needs the same thing code has:
-- a test set and a history. Fixtures are per prompt key and deliberately tiny —
-- an editable input plus deterministic rules (contains / absent / json / length
-- / regex). No LLM judging: a scorer that can be talked into a pass is not a
-- scorer, and "it felt better" is not a result.
CREATE TABLE IF NOT EXISTS prompt_evals (
  id         TEXT PRIMARY KEY,
  prompt_key TEXT NOT NULL,
  name       TEXT NOT NULL,
  input      TEXT NOT NULL,
  rules      TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_prompt_evals_key ON prompt_evals(prompt_key, created_at);

-- One row per comparison, so "when did this prompt last get better?" has an
-- answer that doesn't depend on remembering.
CREATE TABLE IF NOT EXISTS prompt_eval_runs (
  id         TEXT PRIMARY KEY,
  prompt_key TEXT NOT NULL,
  label_a    TEXT NOT NULL,
  label_b    TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_prompt_eval_runs_key ON prompt_eval_runs(prompt_key, created_at);

CREATE TABLE IF NOT EXISTS prompt_eval_results (
  id         TEXT PRIMARY KEY,
  run_id     TEXT NOT NULL REFERENCES prompt_eval_runs(id) ON DELETE CASCADE,
  eval_id    TEXT NOT NULL,
  eval_name  TEXT NOT NULL,
  variant    TEXT NOT NULL,
  output     TEXT NOT NULL DEFAULT '',
  passed     INTEGER NOT NULL,
  failures   TEXT NOT NULL DEFAULT '',
  ms         INTEGER NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_prompt_eval_results_run ON prompt_eval_results(run_id, variant);
