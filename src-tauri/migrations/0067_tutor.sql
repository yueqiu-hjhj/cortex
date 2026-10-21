-- Tutor bench — 支架式陪练 + 学情校准.
--
-- Three tables: a session carries the scaffold level (1..5) and turn count;
-- every turn records the user's self-rated confidence and the tutor's
-- verdict; and the misjudged queue is the calibration leg — "answered wrong
-- but was sure" is a different problem from "answered wrong and knew it",
-- so it gets its own next-day list instead of being averaged away.
CREATE TABLE IF NOT EXISTS tutor_sessions (
  id          TEXT PRIMARY KEY,
  subject_id  TEXT NOT NULL REFERENCES subjects(id) ON DELETE CASCADE,
  node_id     TEXT REFERENCES knowledge_nodes(id) ON DELETE SET NULL,
  title       TEXT NOT NULL,
  level       INTEGER NOT NULL DEFAULT 3,
  turns       INTEGER NOT NULL DEFAULT 0,
  status      TEXT NOT NULL DEFAULT 'active',          -- active | done
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_tutor_sessions_subject ON tutor_sessions(subject_id, status, updated_at);

CREATE TABLE IF NOT EXISTS tutor_turns (
  id          TEXT PRIMARY KEY,
  session_id  TEXT NOT NULL REFERENCES tutor_sessions(id) ON DELETE CASCADE,
  role        TEXT NOT NULL,                            -- user | tutor
  text        TEXT NOT NULL,
  level       INTEGER NOT NULL DEFAULT 0,
  confidence  INTEGER,                                  -- user self-rating 1..5
  verdict     TEXT,                                     -- correct | partial | wrong
  created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_tutor_turns_session ON tutor_turns(session_id, created_at);

CREATE TABLE IF NOT EXISTS tutor_misjudged (
  id            TEXT PRIMARY KEY,
  session_id    TEXT NOT NULL REFERENCES tutor_sessions(id) ON DELETE CASCADE,
  subject_id    TEXT NOT NULL REFERENCES subjects(id) ON DELETE CASCADE,
  node_id       TEXT REFERENCES knowledge_nodes(id) ON DELETE SET NULL,
  question      TEXT NOT NULL,
  answer        TEXT NOT NULL,
  scheduled_for INTEGER NOT NULL,                       -- next day 00:00 (ms)
  resolved      INTEGER NOT NULL DEFAULT 0,
  created_at    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_tutor_misjudged_due ON tutor_misjudged(resolved, scheduled_for);
