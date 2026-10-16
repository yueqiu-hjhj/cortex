-- Knowledge framework, mastery observations, undo journal, and read-only tool
-- registry. These tables are intentionally additive so old libraries upgrade in
-- one migration and sync can carry the new rows like every other entity.

PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS knowledge_nodes (
  id              TEXT PRIMARY KEY,
  subject_id      TEXT NOT NULL REFERENCES subjects(id) ON DELETE CASCADE,
  topic_id        TEXT REFERENCES topics(id) ON DELETE SET NULL,
  source_id       TEXT REFERENCES sources(id) ON DELETE SET NULL,
  parent_id       TEXT REFERENCES knowledge_nodes(id) ON DELETE CASCADE,
  node_key        TEXT NOT NULL,
  kind            TEXT NOT NULL,              -- subject | topic | source | concept
  title           TEXT NOT NULL,
  summary         TEXT,
  position        INTEGER NOT NULL DEFAULT 0,
  coverage        REAL NOT NULL DEFAULT 0.0,   -- content coverage 0..1
  mastery         REAL NOT NULL DEFAULT 0.3,   -- BKT probability of knowledge
  importance      REAL NOT NULL DEFAULT 0.5,
  confidence      REAL NOT NULL DEFAULT 0.5,   -- confidence in the estimate
  observations    INTEGER NOT NULL DEFAULT 0,
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL,
  UNIQUE(subject_id, node_key)
);
CREATE INDEX IF NOT EXISTS idx_knowledge_nodes_subject ON knowledge_nodes(subject_id, position);
CREATE INDEX IF NOT EXISTS idx_knowledge_nodes_parent ON knowledge_nodes(parent_id);

CREATE TABLE IF NOT EXISTS knowledge_edges (
  id              TEXT PRIMARY KEY,
  subject_id      TEXT NOT NULL REFERENCES subjects(id) ON DELETE CASCADE,
  from_id         TEXT NOT NULL REFERENCES knowledge_nodes(id) ON DELETE CASCADE,
  to_id           TEXT NOT NULL REFERENCES knowledge_nodes(id) ON DELETE CASCADE,
  kind            TEXT NOT NULL DEFAULT 'contains', -- contains | related | backlink
  weight          REAL NOT NULL DEFAULT 1.0,
  created_at      INTEGER NOT NULL,
  UNIQUE(from_id, to_id, kind)
);
CREATE INDEX IF NOT EXISTS idx_knowledge_edges_subject ON knowledge_edges(subject_id);
CREATE INDEX IF NOT EXISTS idx_knowledge_edges_from ON knowledge_edges(from_id);
CREATE INDEX IF NOT EXISTS idx_knowledge_edges_to ON knowledge_edges(to_id);

CREATE TABLE IF NOT EXISTS knowledge_observations (
  id              TEXT PRIMARY KEY,
  node_id         TEXT NOT NULL REFERENCES knowledge_nodes(id) ON DELETE CASCADE,
  correct         INTEGER NOT NULL,
  confidence      REAL NOT NULL DEFAULT 0.7, -- learner confidence before reveal
  signal          TEXT NOT NULL DEFAULT 'review', -- review | quiz | flashcard | manual
  note            TEXT,
  p_before        REAL NOT NULL,
  p_after         REAL NOT NULL,
  observed_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_knowledge_obs_node ON knowledge_observations(node_id, observed_at);

-- A recoverable journal for destructive actions. The payload is a JSON object
-- containing one or more table row snapshots; restoring is whitelisted by
-- entity_table in repo::restore_undo.
CREATE TABLE IF NOT EXISTS undo_entries (
  id              TEXT PRIMARY KEY,
  action          TEXT NOT NULL,             -- delete_source | delete_note | ...
  entity_table    TEXT NOT NULL,
  entity_id       TEXT NOT NULL,
  label           TEXT NOT NULL,
  payload         TEXT NOT NULL,
  created_at      INTEGER NOT NULL,
  expires_at      INTEGER NOT NULL,
  undone_at       INTEGER
);
CREATE INDEX IF NOT EXISTS idx_undo_entries_created ON undo_entries(created_at DESC);

-- Built-in read-only tools exposed to local agents / MCP. Keeping this in the
-- database makes the registry inspectable while the first implementation
-- deliberately ships no write or shell capabilities.
CREATE TABLE IF NOT EXISTS agent_tools (
  id              TEXT PRIMARY KEY,
  name            TEXT NOT NULL UNIQUE,
  description     TEXT NOT NULL,
  kind            TEXT NOT NULL DEFAULT 'read', -- read | write | model | shell
  enabled         INTEGER NOT NULL DEFAULT 1,
  schema_json     TEXT,
  created_at      INTEGER NOT NULL
);

INSERT OR IGNORE INTO agent_tools (id, name, description, kind, enabled, schema_json, created_at)
VALUES
  ('tool-list-notebooks', 'list_notebooks', 'List subjects and their topics.', 'read', 1, '{}', CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)),
  ('tool-search-notebook', 'search_notebook', 'Search indexed source chunks within a subject.', 'read', 1, '{}', CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)),
  ('tool-get-source', 'get_source', 'Read source metadata and extracted text.', 'read', 1, '{}', CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)),
  ('tool-search-notes', 'search_notes', 'Search user notes by title and body.', 'read', 1, '{}', CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)),
  ('tool-knowledge-tree', 'get_knowledge_tree', 'Read the subject knowledge framework and mastery estimates.', 'read', 1, '{}', CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
