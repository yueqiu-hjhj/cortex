-- Tiered tool permissions for agents (分级权限). Cumulative tiers:
-- read (只读内置工具) < write (可写应用数据，为 agent 写回预留) < network (MCP 外联).
-- Enforcement lives in agent_exec at dispatch time; denials are recorded in
-- agent_messages so the audit trail shows every attempt.
ALTER TABLE agents ADD COLUMN tool_tier TEXT NOT NULL DEFAULT 'read';
