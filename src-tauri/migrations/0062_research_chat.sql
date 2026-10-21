-- Research chat: a conversation with the literature, not a query box.
--
-- `literature_ai_search` already plans query angles, but it still returns a
-- ranked list and forgets everything afterwards. This is the other half: a
-- thread where the user says "who is working on this at Stanford?", the agent
-- picks tools, reads what they return, and answers — and the next question can
-- build on the previous answer.
--
-- Two tables, deliberately separate from `agent_messages`: the agent module is
-- a general automation surface with its own registry/whitelist, while this is
-- one focused conversational workspace over the literature. Mixing them would
-- make both harder to reason about.
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS research_threads (
  id         TEXT PRIMARY KEY,
  title      TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_research_threads_updated
  ON research_threads(updated_at DESC);

CREATE TABLE IF NOT EXISTS research_messages (
  id         TEXT PRIMARY KEY,
  thread_id  TEXT NOT NULL REFERENCES research_threads(id) ON DELETE CASCADE,
  -- user | agent | tool — `tool` rows keep the audit trail: what was searched,
  -- what came back, so an answer can always be traced to its evidence.
  role       TEXT NOT NULL,
  text       TEXT NOT NULL DEFAULT '',
  tool       TEXT,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_research_messages_thread
  ON research_messages(thread_id, created_at);

CREATE TRIGGER IF NOT EXISTS research_message_bump AFTER INSERT ON research_messages BEGIN
  UPDATE research_threads SET updated_at = NEW.created_at WHERE id = NEW.thread_id;
END;
