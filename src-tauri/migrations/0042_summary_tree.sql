-- 0042_summary_tree.sql
--
-- D6: hierarchical summary tree, the local-model answer to a small context
-- window. One row per (node_kind, node_id): subject-level overview, per-topic
-- rollups, per-source digests. Context assembly walks the tree top-down —
-- overview first, then matching topic/source summaries, and only then raw
-- chunk text — so a "whole book / whole semester" question sees the shape of
-- the entire library instead of six retrieved chunks.
--
-- Freshness is computed at read time (node.updated_at vs the underlying row's
-- updated_at); stale nodes are skipped, never shown. Nodes are derived cache
-- data, rebuildable at any time — no tombstones.

CREATE TABLE IF NOT EXISTS summary_nodes (
  id          TEXT PRIMARY KEY,
  subject_id  TEXT NOT NULL REFERENCES subjects(id) ON DELETE CASCADE,
  node_kind   TEXT NOT NULL,                 -- subject | topic | source
  node_id     TEXT NOT NULL,                 -- subjects.id / topics.id / sources.id
  summary     TEXT NOT NULL,
  keywords    TEXT,                          -- JSON array of key terms
  model       TEXT,
  chars_in    INTEGER NOT NULL DEFAULT 0,    -- source chars the summary covers
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL,
  UNIQUE(node_kind, node_id)
);
CREATE INDEX IF NOT EXISTS idx_summary_subject ON summary_nodes(subject_id);
