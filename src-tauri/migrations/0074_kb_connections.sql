-- KBP-style external knowledge bases (the "外挂大脑"): Dify datasets,
-- FastGPT datasets, and MCP tools that behave like a retriever.
-- Each connection carries a *description protocol* — a human-written note on
-- what this KB holds and when the agent should consult it.
CREATE TABLE kb_connections (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,              -- dify | fastgpt | mcp
    base_url TEXT NOT NULL DEFAULT '',
    dataset_id TEXT NOT NULL DEFAULT '',  -- dify dataset id / fastgpt datasetId / mcp__server__tool
    args_json TEXT NOT NULL DEFAULT '',   -- mcp only: args template, "{query}" placeholders
    description TEXT NOT NULL DEFAULT '', -- 描述协议：这个库装什么、什么时候该查它
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_kb_enabled ON kb_connections(enabled);
