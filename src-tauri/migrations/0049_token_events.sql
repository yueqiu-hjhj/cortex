-- 0049_token_events.sql — token accounting (Reviva's token dashboard).
--
-- Honest estimates, not provider usage: the Llm trait exposes no usage
-- numbers, so each completed call is billed by a chars→tokens estimate
-- (CJK ~1/char, latin ~4 chars/token). Enough to answer "where did the
-- budget go, when, and how much" — and the UI labels it 估算. NULL
-- subject_id = system-wide kinds (materials etc. logged without scope).
CREATE TABLE IF NOT EXISTS token_events (
  id                TEXT PRIMARY KEY,
  ts                INTEGER NOT NULL,
  kind              TEXT NOT NULL,
  model             TEXT NOT NULL,
  prompt_tokens     INTEGER NOT NULL,
  completion_tokens INTEGER NOT NULL,
  subject_id        TEXT
);
CREATE INDEX IF NOT EXISTS token_events_ts_idx ON token_events(ts DESC);
