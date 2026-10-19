# cortex-mcp

MCP server for [Cortex](https://github.com/yueqiu-hjhj/cortex) — exposes your
local Cortex library as **13 read-only tools** to Claude Code, Codex, Cursor,
and any MCP client. The server binary is the same one the Cortex desktop app
ships; this package downloads (and caches) it for your platform.

**Read-only by construction**: SQLITE_OPEN_READ_ONLY, no write tools, no path
leakage, `isError` protocol-errors, `-32601` unknown methods. Your library is
exposed for *reading and verification*, never for modification.

## Install / use

```bash
# one-shot (npx caches it)
npx -y cortex-mcp@latest

# or global
npm i -g cortex-mcp
```

First run downloads the platform binary from GitHub Releases into
`~/.cortex-mcp-bin/` and caches it. Already have Cortex desktop installed?
The `cortex-mcp` binary on your `$PATH` is used automatically — or point at it
explicitly: `CORTEX_MCP_BIN=/path/to/cortex-mcp`.

## Configure your client

### Claude Code

```bash
claude mcp add cortex -- npx -y cortex-mcp@latest --db "$HOME/.cortex/cortex.db"
```

(or the equivalent JSON in `.mcp.json`:)

```json
{
  "mcpServers": {
    "cortex": {
      "command": "npx",
      "args": ["-y", "cortex-mcp@latest", "--db", "/absolute/path/to/cortex.db"]
    }
  }
}
```

### Cursor / Codex / other MCP clients

Same JSON shape — `command: "npx"`, `args: ["-y", "cortex-mcp@latest", "--db", "<path>"]`.

**DB resolution order** (same as the app): `--db <path>` → `$CORTEX_DB` → the
folder of `$CORTEX_DATA_DIR` → platform app-data dir (`~/Library/Application
Support/app.study.cortex` on macOS, `%APPDATA%/app.study.cortex` on Windows).

## The 13 tools

Reading the library:

| Tool | What it returns |
|---|---|
| `list_subjects` | subjects (id, name, source count) |
| `get_subject` | one subject + topics + sources |
| `list_sources` / `get_source` | source rows / one source |
| `list_chunks` | a source's chunks (with `char_start`/`char_end` offsets when known) |
| `knowledge_tree` | the subject's framework nodes |
| `search_notebook` | keyword passages — matches record `chars a-b` so an agent can **slice-verify** any claim against `read_document` |
| `read_document` | a source's full extracted text |
| `search_notes` / `get_note` | notes by keyword / by id (wiki `[[links]]` included) |
| `list_agents` / `get_agent` | the agent roster / one agent |
| `cross_book` | concepts recurring across subjects (the cross-book network) |

## Boundaries (honest, enforced)

- **Read-only.** No tool writes, no tool calls a model, no tool takes an
  absolute path outside the resolved data dir.
- Passages carry **character offsets** (`char_start`/`char_end`) — the client
  can and should verify quotes by slicing `read_document` output.
- The server does **not** run without a DB. If your library lives in the
  desktop app, pass `--db` at the path shown in Settings → Data.

## License

Apache-2.0 (same as Cortex).
