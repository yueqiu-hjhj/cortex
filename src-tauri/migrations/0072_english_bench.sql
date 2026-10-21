-- 0072_english_bench.sql — 考研式精读台 + 翻译训练.
--
-- 知源's 应试英语对「一篇篇章」做六维深度解析（精译 / 主旨态度 / 长难句
-- 拆解 / 同义替换 / 熟词生义 / 命题陷阱），翻译训练另有文化负载词、高分
-- 短语与评分自查。Cortex 的 SourceViewer 已有划词翻译与中英对照，但那是
-- 「读到哪问到哪」；六维是「把一篇读透」——两种读法，缺一不可。
--
-- 两种 kind 共用同一张 run/item 形状：一个 run 挂在 source 上，每个维度
-- 是一条 item，各自独立调用、独立失败。坏一个维度只坏一个维度（和 slides
-- 的坏页降级同一条规则），run 的状态是所有维度沉淀后的总和，不谎报。
CREATE TABLE IF NOT EXISTS bench_runs (
  id         TEXT PRIMARY KEY,
  kind       TEXT NOT NULL,              -- close | translate
  source_id  TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
  status     TEXT NOT NULL,              -- running | done | error
  error      TEXT,
  model      TEXT,
  truncated  INTEGER NOT NULL DEFAULT 0, -- 输入文本被截断（长文只读前段）
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS bench_runs_src_idx ON bench_runs(source_id, kind, created_at DESC);

CREATE TABLE IF NOT EXISTS bench_items (
  id         TEXT PRIMARY KEY,
  run_id     TEXT NOT NULL REFERENCES bench_runs(id) ON DELETE CASCADE,
  dimension  TEXT NOT NULL,
  ord        INTEGER NOT NULL,
  status     TEXT NOT NULL,              -- pending | done | error
  error      TEXT,
  payload    TEXT,                       -- JSON，形状随 dimension
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS bench_items_run_idx ON bench_items(run_id, ord);
