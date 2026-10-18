//! `cortex-mcp` — stdio MCP server exposing the local Cortex library.
//!
//! Thin shell over `cortex_lib::mcp_server` (which owns the protocol layer,
//! the 13 read-only tools, and the tests). The protocol layer is
//! spec-compliant: `tools/list` carries real `inputSchema`s, tool results are
//! `content:[{type:"text"}]`, failures set `isError`, notifications get no
//! response, and unknown methods return JSON-RPC -32601.
//!
//! Wire it into Claude Code / Codex / Cursor:
//!
//! ```json
//! { "mcpServers": { "cortex": { "command": "cortex-mcp" } } }
//! ```
//!
//! DB resolution: `--db <path>` → `$CORTEX_DB` → `$CORTEX_DATA_DIR/cortex.db`
//! → platform app-data dir (`app.study.cortex`). The connection is READ-ONLY;
//! logs go to stderr so they never corrupt the JSON-RPC stream.

use std::process::ExitCode;

fn main() -> ExitCode {
    if let Some(a) = std::env::args().nth(1) {
        if a == "--help" || a == "-h" {
            eprintln!(
                "cortex-mcp — read-only MCP server over the local Cortex library.\n\
                 \n\
                 Usage: cortex-mcp [--db <path>]\n\
                 \n\
                 Env: CORTEX_DB (exact db file) or CORTEX_DATA_DIR (dir with cortex.db).\n\
                 Default: platform app-data dir for app.study.cortex.\n\
                 \n\
                 Tools (13, all read-only): list_notebooks, search_notebook, get_source,\n\
                 read_document, search_notes, get_knowledge_tree, list_wiki, get_wiki,\n\
                 wiki_reverse_links, list_agents, agent_messages, cross_book_bridges,\n\
                 list_provenance.\n\
                 \n\
                 Boundaries: read-only connection, no model calls, no filesystem paths\n\
                 in responses. Write tools are deliberately absent (KnowNote-grade)."
            );
            return ExitCode::SUCCESS;
        }
    }
    let path = cortex_lib::mcp_server::default_db_path();
    let conn = match cortex_lib::mcp_server::open_readonly(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cortex-mcp: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "cortex-mcp: serving {} (read-only; 13 tools; logs on stderr)",
        path.display()
    );
    match cortex_lib::mcp_server::run_stdio(&conn) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cortex-mcp: {e}");
            ExitCode::FAILURE
        }
    }
}
