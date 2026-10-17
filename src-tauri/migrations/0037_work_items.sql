-- 0037_work_items.sql
--
-- The cross-subject spine. Until now every entity hung off `subjects`
-- (sources, topics, notes, materials, cards, events, citations), which fits the
-- Study and Exam modes but cannot express the other three: a shopping item is
-- not a subject, a research paper may span several, and "planning" is a
-- timeline rather than a knowledge tree.
--
-- Four tables fix that:
--   goals        — the anchor for planning ("pass the exam with 85 by 30 days")
--   work_items   — one row per unit of work, optionally attached to a subject,
--                  a goal, a parent item, and a recurrence rule
--   item_links   — polymorphic edges to sources/notes/citations/wiki/papers, so
--                  a single item can carry its evidence and its output
--   plan_blocks  — the persisted schedule (Phase 2 fills these; the table ships
--                  now so the planner has somewhere to write)
--
-- Everything is additive: no existing table is touched, so 0036 databases open
-- unchanged and every current feature keeps working.

PRAGMA foreign_keys = ON;

-- ---------------------------------------------------------------- goals
-- A goal is deliberately metric-agnostic: `metric` names what `current`/
-- `target` count (score | count | minutes | boolean), so one table serves
-- "exam in 30 days" and "read 20 papers this term" without a schema change.
CREATE TABLE IF NOT EXISTS goals (
  id          TEXT PRIMARY KEY,
  title       TEXT NOT NULL,
  kind        TEXT NOT NULL DEFAULT 'exam',   -- exam | skill | research | habit | project
  metric      TEXT,                           -- score | count | minutes | boolean
  target      REAL,
  current     REAL NOT NULL DEFAULT 0,
  deadline_ms INTEGER,
  subject_id  TEXT REFERENCES subjects(id) ON DELETE SET NULL,
  status      TEXT NOT NULL DEFAULT 'active', -- active | done | dropped
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_goals_status   ON goals(status, deadline_ms);
CREATE INDEX IF NOT EXISTS idx_goals_subject  ON goals(subject_id);

-- ----------------------------------------------------------- work_items
-- `status='inbox'` is the capture bucket: rows land here with no subject, no
-- date and no parent, and get triaged later. `kind` is a hint for the UI, not
-- an invariant — the pipeline (reading → excerpt → writing) is expressed with
-- item_links, not with a state machine.
CREATE TABLE IF NOT EXISTS work_items (
  id           TEXT PRIMARY KEY,
  kind         TEXT NOT NULL DEFAULT 'task',  -- task | reading | writing | question | excerpt | milestone | material
  title        TEXT NOT NULL,
  body         TEXT,
  status       TEXT NOT NULL DEFAULT 'open',  -- inbox | open | doing | blocked | done | dropped
  priority     TEXT,                          -- low | med | high | urgent
  due_ms       INTEGER,
  start_ms     INTEGER,
  estimate_min INTEGER,
  actual_min   INTEGER NOT NULL DEFAULT 0,
  parent_id    TEXT REFERENCES work_items(id) ON DELETE CASCADE,
  rrule        TEXT,                          -- RFC 5545 subset: FREQ=DAILY;INTERVAL=1[;BYDAY=..][;UNTIL=..]
  subject_id   TEXT REFERENCES subjects(id) ON DELETE SET NULL,
  topic_id     TEXT REFERENCES topics(id)   ON DELETE SET NULL,
  goal_id      TEXT REFERENCES goals(id)    ON DELETE SET NULL,
  sort_order   INTEGER NOT NULL DEFAULT 0,
  created_at   INTEGER NOT NULL,
  updated_at   INTEGER NOT NULL,
  done_at      INTEGER
);
CREATE INDEX IF NOT EXISTS idx_wi_status_due ON work_items(status, due_ms);
CREATE INDEX IF NOT EXISTS idx_wi_parent     ON work_items(parent_id);
CREATE INDEX IF NOT EXISTS idx_wi_subject    ON work_items(subject_id, status);
CREATE INDEX IF NOT EXISTS idx_wi_goal       ON work_items(goal_id, status);

-- ----------------------------------------------------------- item_links
-- Polymorphic by design: entity_table is a table name (sources | notes |
-- citations | wiki_pages | materials | papers), so adding a new linkable type
-- needs no migration. No FK here — the target table varies per row.
CREATE TABLE IF NOT EXISTS item_links (
  item_id      TEXT NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
  entity_table TEXT NOT NULL,
  entity_id    TEXT NOT NULL,
  role         TEXT NOT NULL DEFAULT 'context', -- context | output | evidence | derived_from
  created_at   INTEGER NOT NULL,
  PRIMARY KEY (item_id, entity_table, entity_id)
);
CREATE INDEX IF NOT EXISTS idx_links_entity ON item_links(entity_table, entity_id);

-- ---------------------------------------------------------- plan_blocks
-- `day` is a local-date string (YYYY-MM-DD) computed by the frontend, so a
-- device in another timezone still sees its own "today". `updated_at` is what
-- makes these rows sync (sync.rs upserts tables carrying id + updated_at).
CREATE TABLE IF NOT EXISTS plan_blocks (
  id         TEXT PRIMARY KEY,
  item_id    TEXT NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
  day        TEXT NOT NULL,
  start_min  INTEGER NOT NULL,
  end_min    INTEGER NOT NULL,
  status     TEXT NOT NULL DEFAULT 'planned', -- planned | done | skipped | moved
  origin     TEXT NOT NULL DEFAULT 'manual',  -- manual | planner | sprint | rollover
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_pb_day  ON plan_blocks(day, start_min);
CREATE INDEX IF NOT EXISTS idx_pb_item ON plan_blocks(item_id);

-- ------------------------------------------------------------ tombstones
-- Without these, a deleted work item would be resurrected by the next sync
-- merge (union-by-id). Same epoch-ms expression as 0019.
CREATE TRIGGER IF NOT EXISTS tomb_goals AFTER DELETE ON goals BEGIN
  INSERT OR REPLACE INTO tombstones VALUES ('goals', OLD.id, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;
CREATE TRIGGER IF NOT EXISTS tomb_work_items AFTER DELETE ON work_items BEGIN
  INSERT OR REPLACE INTO tombstones VALUES ('work_items', OLD.id, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;
CREATE TRIGGER IF NOT EXISTS tomb_plan_blocks AFTER DELETE ON plan_blocks BEGIN
  INSERT OR REPLACE INTO tombstones VALUES ('plan_blocks', OLD.id, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;
