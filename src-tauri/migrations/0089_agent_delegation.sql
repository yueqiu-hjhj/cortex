-- 0089_agent_delegation.sql — sub-agent runs are recorded as children.
--
-- `delegate` lets an agent hand a focused task to another agent. The child is
-- a REAL run (own thread, own steps, own transcript), not an inline prompt, so
-- the audit trail stays complete. This column is what makes the parent/child
-- relation visible instead of two unrelated runs appearing side by side.
--
-- Nullable: every existing run (and every top-level run) has no parent.
ALTER TABLE agent_runs ADD COLUMN parent_run_id TEXT;

CREATE INDEX IF NOT EXISTS idx_agent_runs_parent
    ON agent_runs(parent_run_id, started_at);
