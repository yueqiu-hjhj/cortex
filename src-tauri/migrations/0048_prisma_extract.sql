-- 0048_prisma_extract.sql
--
-- 3.2: PRISMA 8-dimension extraction — the systematic-review twin of D8's
-- free-form extraction templates. The field list is FIXED in code (研究问题 /
-- 方法 / 贡献 / 结果 / 局限 / 适用人群 / 使用前提 / 复现成本); a run applies it
-- to N papers, one LLM call per paper. Two columns set it apart from
-- extract_cells:
--   evidence   — the verbatim card sentence the answer was extracted from
--   excerpt_id — back-link into excerpts(id): when the evidence matches an
--                existing excerpt card, or is verbatim in the card and can
--                become one, the cell carries its id so every value in the
--                review table is traceable to the quote it came from.
-- source follows the D8 reviewability contract: model cells are overwritable,
-- manual corrections are never clobbered by re-runs or retries.

CREATE TABLE IF NOT EXISTS prisma_runs (
  id         TEXT PRIMARY KEY,
  fields_rev TEXT NOT NULL,                   -- JSON snapshot of the fixed field list at run time
  title      TEXT NOT NULL,
  model      TEXT,
  status     TEXT NOT NULL DEFAULT 'running', -- running | done | error
  error      TEXT,
  item_ids   TEXT NOT NULL,                   -- JSON array of paper ids, column order
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_prisma_runs_created ON prisma_runs(created_at DESC);

CREATE TABLE IF NOT EXISTS prisma_cells (
  run_id     TEXT NOT NULL REFERENCES prisma_runs(id) ON DELETE CASCADE,
  item_id    TEXT NOT NULL,
  field_key  TEXT NOT NULL,
  value      TEXT NOT NULL DEFAULT '',
  evidence   TEXT,
  excerpt_id TEXT,                            -- back-link to excerpts(id)
  source     TEXT NOT NULL DEFAULT 'model',   -- model | manual
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (run_id, item_id, field_key)
);

CREATE TRIGGER IF NOT EXISTS tomb_prisma_runs AFTER DELETE ON prisma_runs BEGIN
  INSERT OR REPLACE INTO tombstones VALUES ('prisma_runs', OLD.id, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;
