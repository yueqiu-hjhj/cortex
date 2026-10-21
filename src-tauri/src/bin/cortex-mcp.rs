//! `cortex-mcp` — stdio MCP server exposing the local Cortex library.
//!
//! Thin shell over `cortex_lib::mcp_server` (which owns the protocol layer,
//! the 21 tools, and the tests). The protocol layer is
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
//! → platform app-data dir (`app.study.cortex`). The connection is writable,
//! but the only table the server ever writes is `agent_proposals` (L2 review
//! queue) — see `mcp_server::open_for_agent`. Logs go to stderr so they never
//! corrupt the JSON-RPC stream.

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
                 Tools (21: 18 read-only + 3 queue): list_notebooks, search_notebook, get_source,\n\
                 read_document, search_notes, get_knowledge_tree, list_wiki, get_wiki,\n\
                 wiki_reverse_links, list_agents, agent_messages, cross_book_bridges,\n\
                 list_provenance, list_papers, get_paper, search_papers, list_excerpts,\n\
                 get_note, propose_note, list_proposals, withdraw_proposal.\n\
                 \n\
                 Resources: cortex://note/<id>, cortex://paper/<cite_key>.\n\
                 \n\
                 Boundaries: the only writable table is agent_proposals — drafts land in a\n\
                 review queue and become notes only when the user accepts them in Cortex.\n\
                 No model calls, no absolute paths in responses."
            );
            return ExitCode::SUCCESS;
        }
    }
    let path = cortex_lib::mcp_server::default_db_path();
    let conn = match cortex_lib::mcp_server::open_for_agent(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cortex-mcp: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "cortex-mcp: serving {} (21 tools; writes limited to the proposal queue; logs on stderr)",
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
