-- 0047_agent_threads.sql — multi-agent shared context (P0 #4).
--
-- Reviva's signature workflow: switch agents mid-conversation (课程助教 →
-- 复习教练 → 文档助手) WITHOUT losing context. This adds the thread link to
-- agent_runs; context assembly (agent_exec) then reads every run's messages
-- in the thread, in order — reference-based, never copying, so switching
-- costs zero extra tokens per switch. Empty thread_id = a legacy
-- ungrouped run (its own messages only).
ALTER TABLE agent_runs ADD COLUMN thread_id TEXT NOT NULL DEFAULT '';
CREATE INDEX IF NOT EXISTS agent_runs_thread_idx ON agent_runs(thread_id, started_at);
