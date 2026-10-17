-- Agent platform: custom agents, runs, messages, and skill allow-lists. Each
-- run gets a structured transcript (system/user/assistant/tool) plus the set
-- of skills the agent was permitted to invoke. Scoped to a single user; the
-- `user_id` columns and the `agent_tools` extension are added here so a fresh
-- install and an upgrade from 0030 both land on the same shape.
CREATE TABLE IF NOT EXISTS agents (
  id            TEXT PRIMARY KEY,
  user_id       TEXT NOT NULL,
  name          TEXT NOT NULL,
  description   TEXT,
  system_prompt TEXT NOT NULL,
  skills        TEXT NOT NULL DEFAULT '[]',
  tools         TEXT NOT NULL DEFAULT '[]',
  model         TEXT,
  temperature   REAL NOT NULL DEFAULT 0.3,
  max_steps     INTEGER NOT NULL DEFAULT 8,
  enabled       INTEGER NOT NULL DEFAULT 1,
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS agents_user_idx ON agents(user_id, updated_at DESC);

CREATE TABLE IF NOT EXISTS agent_runs (
  id            TEXT PRIMARY KEY,
  agent_id      TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
  user_id       TEXT NOT NULL,
  status        TEXT NOT NULL DEFAULT 'pending',
  input         TEXT NOT NULL,
  result        TEXT,
  error         TEXT,
  steps         INTEGER NOT NULL DEFAULT 0,
  started_at    INTEGER NOT NULL,
  finished_at   INTEGER
);
CREATE INDEX IF NOT EXISTS agent_runs_agent_idx ON agent_runs(agent_id, started_at DESC);
CREATE INDEX IF NOT EXISTS agent_runs_user_idx ON agent_runs(user_id, started_at DESC);

CREATE TABLE IF NOT EXISTS agent_messages (
  id            TEXT PRIMARY KEY,
  run_id        TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
  role          TEXT NOT NULL,
  content       TEXT NOT NULL,
  tool_name     TEXT,
  created_at    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS agent_messages_run_idx ON agent_messages(run_id, created_at);

CREATE TABLE IF NOT EXISTS agent_skills (
  id            TEXT PRIMARY KEY,
  user_id       TEXT NOT NULL,
  name          TEXT NOT NULL,
  description   TEXT NOT NULL,
  body          TEXT NOT NULL,
  enabled       INTEGER NOT NULL DEFAULT 1,
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL,
  UNIQUE(user_id, name)
);

-- The 0030 migration already created `agent_tools` for built-in entries;
-- extend it with user scoping so the same registry holds both built-in and
-- user-defined tools. The original `name` UNIQUE is replaced by a composite
-- (user_id, name) unique so different users can define a tool called
-- "search" without colliding with the built-in "search_notebook".
ALTER TABLE agent_tools ADD COLUMN user_id TEXT NOT NULL DEFAULT 'u-default';
ALTER TABLE agent_tools ADD COLUMN built_in INTEGER NOT NULL DEFAULT 0;
-- SQLite cannot drop a UNIQUE constraint by name; rebuild the table with the
-- new composite (user_id, name) uniqueness. Built-in rows are preserved with
-- user_id = 'u-default'.
CREATE TABLE agent_tools_new (
  id            TEXT PRIMARY KEY,
  user_id       TEXT NOT NULL,
  name          TEXT NOT NULL,
  description   TEXT NOT NULL,
  kind          TEXT NOT NULL DEFAULT 'read',
  schema_json   TEXT,
  enabled       INTEGER NOT NULL DEFAULT 1,
  built_in      INTEGER NOT NULL DEFAULT 0,
  created_at    INTEGER NOT NULL
);
INSERT INTO agent_tools_new (id, user_id, name, description, kind, schema_json, enabled, built_in, created_at)
  SELECT id, 'u-default', name, description, kind, schema_json, enabled, 1, created_at FROM agent_tools;
DROP TABLE agent_tools;
ALTER TABLE agent_tools_new RENAME TO agent_tools;
CREATE UNIQUE INDEX agent_tools_user_name_idx ON agent_tools(user_id, name);

-- Lightweight wiki pages for knowledge capture, audit, and productization.
CREATE TABLE IF NOT EXISTS wiki_pages (
  id            TEXT PRIMARY KEY,
  user_id       TEXT NOT NULL,
  slug          TEXT NOT NULL,
  title         TEXT NOT NULL,
  body          TEXT NOT NULL,
  source        TEXT,
  audit         TEXT,
  last_audited  INTEGER,
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL,
  UNIQUE(user_id, slug)
);
CREATE INDEX IF NOT EXISTS wiki_pages_user_updated_idx ON wiki_pages(user_id, updated_at DESC);

CREATE TABLE IF NOT EXISTS wiki_links (
  from_page_id  TEXT NOT NULL REFERENCES wiki_pages(id) ON DELETE CASCADE,
  to_slug       TEXT NOT NULL,
  to_page_id    TEXT REFERENCES wiki_pages(id) ON DELETE SET NULL,
  created_at    INTEGER NOT NULL,
  UNIQUE(from_page_id, to_slug)
);
CREATE INDEX IF NOT EXISTS wiki_links_to_idx ON wiki_links(to_slug);