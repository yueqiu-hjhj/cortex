-- Two built-in network tools: the agent finally has a way out to the web.
--
-- Until now the only way an agent could touch the internet was a user-registered
-- MCP server, which meant "联网检索" required a separate wrapper process for
-- every search backend. The backend was already here and in use - chat's web
-- mode, cheatsheet images and literature search all query the configured
-- SearXNG - it simply was not exposed as a tool, so an agent could only ever
-- search what the user had already imported.
--
-- Both tools go through the same helpers as the Web view (commands::searxng_search
-- / fetch_readable), including the offline-mode guard: with offline mode on,
-- the tool refuses in plain language instead of silently reaching out.
--
-- kind='network' matches the tier these tools belong to. 0075 introduced tiered
-- permissions and agent_exec gates on the BuiltinTool tier; resolve_tools used to
-- drop any registry row whose kind was neither read nor write, so this migration
-- pairs with that one-line filter fix - otherwise these rows would be invisible
-- no matter what an agent whitelisted.
INSERT OR IGNORE INTO agent_tools (id, user_id, name, description, kind, enabled, schema_json, built_in, created_at)
VALUES
  ('tool-web-search', 'u-default', 'web_search',
        'Search the web through the configured SearXNG instance. Returns titles, hosts, URLs and snippets.',
        'network', 1, '{}', 1, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)),
  ('tool-fetch-page', 'u-default', 'fetch_page',
        'Fetch one URL and read its main text (for depth beyond a search snippet).',
        'network', 1, '{}', 1, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));