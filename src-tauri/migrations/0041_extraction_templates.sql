-- 0041_extraction_templates.sql
--
-- D8: Elicit-style structured extraction. A template is a fixed list of fields
-- (e.g. 样本量 / 方法 / 效应量); a run applies that field list to a set of
-- papers and stores one cell per (paper, field). Cells are first-class rows so
-- every value is individually reviewable: the model writes source='model',
-- human corrections write source='manual' and are never overwritten by later
-- runs. `template_rev` snapshots the field list at run time, so an old run's
-- table shape survives template edits.

CREATE TABLE IF NOT EXISTS extract_templates (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  description TEXT,
  fields      TEXT NOT NULL,                 -- JSON [{key, prompt}]
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS extract_runs (
  id           TEXT PRIMARY KEY,
  template_id  TEXT NOT NULL REFERENCES extract_templates(id) ON DELETE CASCADE,
  template_rev TEXT NOT NULL,                -- JSON snapshot of fields at run time
  title        TEXT NOT NULL,
  model        TEXT,
  status       TEXT NOT NULL DEFAULT 'running', -- running | done | error
  error        TEXT,
  item_ids     TEXT NOT NULL,                -- JSON array of paper ids, column order
  created_at   INTEGER NOT NULL,
  updated_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_extract_runs_created ON extract_runs(created_at DESC);

CREATE TABLE IF NOT EXISTS extract_cells (
  run_id     TEXT NOT NULL REFERENCES extract_runs(id) ON DELETE CASCADE,
  item_id    TEXT NOT NULL,
  field_key  TEXT NOT NULL,
  value      TEXT NOT NULL DEFAULT '',
  source     TEXT NOT NULL DEFAULT 'model',  -- model | manual
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, item_id, field_key)
);

CREATE TRIGGER IF NOT EXISTS tomb_extract_templates AFTER DELETE ON extract_templates BEGIN
  INSERT OR REPLACE INTO tombstones VALUES ('extract_templates', OLD.id, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;

CREATE TRIGGER IF NOT EXISTS tomb_extract_runs AFTER DELETE ON extract_runs BEGIN
  INSERT OR REPLACE INTO tombstones VALUES ('extract_runs', OLD.id, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;
