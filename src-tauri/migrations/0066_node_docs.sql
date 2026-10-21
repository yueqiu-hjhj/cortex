-- Knowledge point docs — 把每个知识点讲成一篇能从头读到尾的材料。
--
-- The doc's coordinates (which layer a point sits in) are NOT stored here:
-- they are computed locally from knowledge_edges(kind='prerequisite') every
-- read, so a framework edit re-layers the docs without a regeneration. Only
-- the prose and the model's "why this order" note are cached.
CREATE TABLE IF NOT EXISTS node_docs (
  node_id     TEXT PRIMARY KEY REFERENCES knowledge_nodes(id) ON DELETE CASCADE,
  subject_id  TEXT NOT NULL REFERENCES subjects(id) ON DELETE CASCADE,
  body        TEXT NOT NULL,
  why         TEXT NOT NULL DEFAULT '',
  model       TEXT NOT NULL DEFAULT '',
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_node_docs_subject ON node_docs(subject_id);
