-- Agent write-back: proposals made inside the Cortex agent platform.
--
-- 0060 gave MCP clients a queue. The built-in agent loop could already read
-- and write notes (0076), but an in-app chat had nowhere to show "here is the
-- draft I am about to write" — so authors quietly used save_note and the user
-- only discovered the write after the fact.
--
-- Fix: the agent gets its OWN propose_note tool (same queue, same diff, same
-- accept-only rule) and every in-app proposal records which run/thread/agent
-- produced it, so the conversation panel can render the pending drafts next
-- to the transcript that asked for them. The columns are nullable: MCP-queued
-- proposals (no run/thread) stay valid and still show in 笔记 → AI 提案.
ALTER TABLE agent_proposals ADD COLUMN run_id    TEXT;
ALTER TABLE agent_proposals ADD COLUMN thread_id TEXT;
ALTER TABLE agent_proposals ADD COLUMN agent_id  TEXT;

-- The conversation panel asks "pending drafts for THIS thread" on every poll;
-- without these it would scan the whole queue.
CREATE INDEX IF NOT EXISTS idx_agent_proposals_run
    ON agent_proposals(run_id, status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_agent_proposals_thread
    ON agent_proposals(thread_id, status, created_at DESC);

-- The built-in propose_note tool: 0034 rebuilt agent_tools with
-- user_id/built_in NOT NULL, so the seed row must set them.
INSERT OR IGNORE INTO agent_tools (id, user_id, name, description, kind, enabled, schema_json, built_in, created_at)
VALUES ('tool-propose-note', 'u-default', 'propose_note',
        'Propose a note draft (new/append/replace). It waits in the user''s review queue as a diff; nothing is written until the user accepts it.',
        'write', 1, '{}', 1, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));