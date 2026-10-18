//! Cortex MCP server (stdio) — expose the local library to external agents.
//!
//! KnowNote shipped `--mcp` first and proved the value: with one line of config
//! the user's own notebook becomes a tool inside Claude Code / Codex / Cursor.
//! The competitive report ranks this P0 #1 — it is the cheapest capability to
//! explain and the hardest to copy without a local library.
//!
//! Transport: MCP over stdio — newline-delimited JSON-RPC 2.0 on stdin/stdout,
//! logs on stderr. The desktop app is untouched; this is a separate binary
//! (`cortex-mcp`) that opens the SAME database **read-only**.
//!
//! Boundaries (deliberately KnowNote-grade strict):
//! - READ-ONLY connection (SQLITE_OPEN_READ_ONLY) — the server cannot write,
//!   no matter what a client asks for.
//! - No model calls. Retrieval is deterministic keyword search; external
//!   agents do the thinking.
//! - No path leaks: stored_path / origin never leave the process.
//!
//! Tools: list_notebooks, search_notebook, get_source, read_document,
//! search_notes.

use crate::error::{Error, Result};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::PathBuf;

const PROTOCOL_VERSION: &str = "2024-11-05";

/// Where the app keeps its DB. Same resolution order as the desktop app:
/// `CORTEX_DB` (exact file) → `CORTEX_DATA_DIR`/cortex.db → platform app-data
/// dir for the `app.study.cortex` identifier.
pub fn default_db_path() -> PathBuf {
    // `--db <path>` / `--db=<path>` — the CLI contract the previous binary
    // shipped; keep it working.
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--db" {
            if let Some(p) = args.next() {
                return PathBuf::from(p);
            }
        } else if let Some(p) = a.strip_prefix("--db=") {
            return PathBuf::from(p);
        }
    }
    if let Ok(p) = std::env::var("CORTEX_DB") {
        if !p.trim().is_empty() {
            return PathBuf::from(p.trim());
        }
    }
    let dir = match std::env::var("CORTEX_DATA_DIR") {
        Ok(d) if !d.trim().is_empty() => PathBuf::from(d.trim()),
        _ => platform_app_dir(),
    };
    dir.join("cortex.db")
}

fn platform_app_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let base = std::env::var("APPDATA").unwrap_or_default();
        return PathBuf::from(base).join("app.study.cortex");
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        return PathBuf::from(home).join("Library/Application Support/app.study.cortex");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let base = std::env::var("XDG_DATA_HOME")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share"));
        return base.join("app.study.cortex");
    }
}

pub fn open_readonly(path: &std::path::Path) -> Result<Connection> {
    if !path.is_file() {
        return Err(Error::Other(format!(
            "数据库不存在: {} —— 先打开一次 Cortex 桌面应用（或用 CORTEX_DB 指定路径）。",
            path.display()
        )));
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| Error::Other(format!("打开 {} 失败: {e}", path.display())))?;
    Ok(conn)
}

fn tool_defs() -> Value {
    json!([
        {
            "name": "list_notebooks",
            "description": "List the user's notebooks (subjects) in their local Cortex library, with source/note counts.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "search_notebook",
            "description": "Keyword-search passages inside one notebook. Returns ranked passages with source name, location and chunk_id (feed chunk/source ids to read_document / get_source).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "notebook_id": { "type": "string", "description": "Subject id from list_notebooks" },
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 }
                },
                "required": ["notebook_id", "query"]
            }
        },
        {
            "name": "get_source",
            "description": "Metadata for one source (name, kind, size, dates). No filesystem paths are exposed.",
            "inputSchema": {
                "type": "object",
                "properties": { "source_id": { "type": "string" } },
                "required": ["source_id"]
            }
        },
        {
            "name": "read_document",
            "description": "Read extracted text of a source, paginated by character offset (max 20000 chars per call).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source_id": { "type": "string" },
                    "offset": { "type": "integer", "minimum": 0, "default": 0 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 20000, "default": 4000 }
                },
                "required": ["source_id"]
            }
        },
        {
            "name": "search_notes",
            "description": "Search the user's notes (title + body) by keyword, optionally within one notebook.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "notebook_id": { "type": "string" }
                },
                "required": ["query"]
            }
        },
        {
            "name": "get_knowledge_tree",
            "description": "Read a notebook's knowledge-framework nodes and edges (coverage/mastery/importance per node).",
            "inputSchema": {
                "type": "object",
                "properties": { "notebook_id": { "type": "string" } },
                "required": ["notebook_id"]
            }
        },
        {
            "name": "list_wiki",
            "description": "List wiki pages (title/slug/summary) from the user's knowledge wiki.",
            "inputSchema": {
                "type": "object",
                "properties": { "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 } }
            }
        },
        {
            "name": "get_wiki",
            "description": "Read one wiki page by id or slug.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" }, "slug": { "type": "string" } }
            }
        },
        {
            "name": "wiki_reverse_links",
            "description": "List wiki pages that link to the given slug.",
            "inputSchema": {
                "type": "object",
                "properties": { "slug": { "type": "string" } },
                "required": ["slug"]
            }
        },
        {
            "name": "list_agents",
            "description": "List configured agents (id, name, role).",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "agent_messages",
            "description": "Read the messages of an existing agent run.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "run_id": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 200 }
                },
                "required": ["run_id"]
            }
        },
        {
            "name": "cross_book_bridges",
            "description": "List cross-subject concept bridges touching one notebook (concepts shared with other subjects).",
            "inputSchema": {
                "type": "object",
                "properties": { "notebook_id": { "type": "string" } },
                "required": ["notebook_id"]
            }
        },
        {
            "name": "list_provenance",
            "description": "List recorded external provenance rows for a source (who provided the content, doc id, url).",
            "inputSchema": {
                "type": "object",
                "properties": { "source_id": { "type": "string" } },
                "required": ["source_id"]
            }
        }
    ])
}

// ---- tool implementations (all read-only) ------------------------------

fn list_notebooks(conn: &Connection) -> Result<String> {
    let mut st = conn.prepare(
        "SELECT s.id, s.name, \
            (SELECT COUNT(*) FROM sources x WHERE x.subject_id = s.id), \
            (SELECT COUNT(*) FROM notes n WHERE n.subject_id = s.id) \
         FROM subjects s ORDER BY s.name",
    )?;
    let rows: Vec<String> = st
        .query_map([], |r| {
            Ok(format!(
                "- {} | id={} | {} sources | {} notes",
                r.get::<_, String>(1)?,
                r.get::<_, String>(0)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    if rows.is_empty() {
        return Ok("No notebooks yet — add a subject in the Cortex app first.".into());
    }
    Ok(rows.join("\n"))
}

fn search_notebook(conn: &Connection, args: &Value) -> Result<String> {
    let notebook = args.get("notebook_id").and_then(|v| v.as_str()).unwrap_or("");
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if notebook.is_empty() || query.trim().is_empty() {
        return Err(Error::Other("notebook_id and query are required".into()));
    }
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10).clamp(1, 50) as usize;
    let hits = crate::repo::keyword_search_chunks(conn, Some(notebook), None, query, limit)?;
    if hits.is_empty() {
        return Ok(format!("No passages matched \"{query}\" in this notebook."));
    }
    let mut out = String::new();
    for (i, h) in hits.iter().enumerate() {
        let loc = h.loc.as_deref().unwrap_or("");
        out.push_str(&format!(
            "[{}] {} {} (source_id={}, chunk_id={})\n{}\n\n",
            i + 1,
            h.source_name,
            loc,
            h.source_id,
            h.id,
            h.text.trim()
        ));
    }
    Ok(out.trim_end().to_string())
}

fn get_source(conn: &Connection, args: &Value) -> Result<String> {
    let sid = args.get("source_id").and_then(|v| v.as_str()).unwrap_or("");
    if sid.is_empty() {
        return Err(Error::Other("source_id is required".into()));
    }
    conn.query_row(
        "SELECT name, kind, COALESCE(meta,''), LENGTH(COALESCE(content,'')), \
                (SELECT COUNT(*) FROM chunks c WHERE c.source_id = s.id), created_at \
         FROM sources s WHERE id = ?1",
        [sid],
        |r| {
            Ok(format!(
                "name: {}\nkind: {}\nmeta: {}\nchars: {}\nchunks: {}\ncreated_at_ms: {}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?
            ))
        },
    )
    .map_err(|_| Error::Other(format!("source {sid} not found")))
}

fn read_document(conn: &Connection, args: &Value) -> Result<String> {
    let sid = args.get("source_id").and_then(|v| v.as_str()).unwrap_or("");
    if sid.is_empty() {
        return Err(Error::Other("source_id is required".into()));
    }
    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(4000).clamp(1, 20000) as usize;
    let content: String = conn
        .query_row(
            "SELECT COALESCE(content,'') FROM sources WHERE id = ?1",
            [sid],
            |r| r.get(0),
        )
        .map_err(|_| Error::Other(format!("source {sid} not found")))?;
    let chars: Vec<char> = content.chars().collect();
    let total = chars.len();
    if offset >= total {
        return Ok(format!("(offset {offset} is past the end; document has {total} chars)"));
    }
    let end = (offset + limit).min(total);
    let slice: String = chars[offset..end].iter().collect();
    Ok(format!(
        "[chars {offset}..{end} of {total}]\n{slice}"
    ))
}

fn search_notes(conn: &Connection, args: &Value) -> Result<String> {
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.trim().is_empty() {
        return Err(Error::Other("query is required".into()));
    }
    let notebook = args.get("notebook_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    // Escape LIKE wildcards so a literal % doesn't match everything.
    let pattern = format!(
        "%{}%",
        query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
    );
    let mut st = conn.prepare(
        "SELECT id, title, substr(body, 1, 240), updated_at FROM notes \
         WHERE (title LIKE ?1 ESCAPE '\\' OR body LIKE ?1 ESCAPE '\\') \
           AND (?2 IS NULL OR subject_id = ?2) \
         ORDER BY updated_at DESC LIMIT 20",
    )?;
    let rows: Vec<String> = st
        .query_map(rusqlite::params![pattern, notebook], |r| {
            Ok(format!(
                "- {} (id={}, updated_at_ms={})\n  {}",
                r.get::<_, String>(1)?,
                r.get::<_, String>(0)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(2)?.replace('\n', " ")
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    if rows.is_empty() {
        return Ok(format!("No notes matched \"{query}\"."));
    }
    Ok(rows.join("\n"))
}

fn call_tool(conn: &Connection, name: &str, args: &Value) -> std::result::Result<String, String> {
    let res = match name {
        "list_notebooks" => list_notebooks(conn),
        "search_notebook" => search_notebook(conn, args),
        "get_source" => get_source(conn, args),
        "read_document" => read_document(conn, args),
        "search_notes" => search_notes(conn, args),
        "get_knowledge_tree" => get_knowledge_tree(conn, args),
        "list_wiki" => list_wiki(conn, args),
        "get_wiki" => get_wiki(conn, args),
        "wiki_reverse_links" => wiki_reverse_links(conn, args),
        "list_agents" => list_agents(conn, args),
        "agent_messages" => agent_messages(conn, args),
        "cross_book_bridges" => cross_book_bridges(conn, args),
        "list_provenance" => list_provenance(conn, args),
        other => return Err(format!("unknown tool: {other}")),
    };
    res.map_err(|e| e.to_string())
}

// ---- additional read-only tools (ported from the previous binary) ------

fn json_text<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".into())
}

fn get_knowledge_tree(conn: &Connection, args: &Value) -> Result<String> {
    let nb = args.get("notebook_id").and_then(|v| v.as_str()).unwrap_or("");
    if nb.is_empty() {
        return Err(Error::Other("notebook_id is required".into()));
    }
    let mut st = conn.prepare(
        "SELECT id, parent_id, node_key, kind, title, coverage, mastery, importance, confidence \
         FROM knowledge_nodes WHERE subject_id = ?1 ORDER BY position",
    )?;
    let nodes: Vec<Value> = st
        .query_map([nb], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "parent_id": r.get::<_, Option<String>>(1)?,
                "key": r.get::<_, String>(2)?,
                "kind": r.get::<_, String>(3)?,
                "title": r.get::<_, String>(4)?,
                "coverage": r.get::<_, f64>(5)?,
                "mastery": r.get::<_, f64>(6)?,
                "importance": r.get::<_, f64>(7)?,
                "confidence": r.get::<_, f64>(8)?,
            }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut st2 = conn.prepare(
        "SELECT from_id, to_id, kind, weight FROM knowledge_edges WHERE subject_id = ?1",
    )?;
    let edges: Vec<Value> = st2
        .query_map([nb], |r| {
            Ok(json!({
                "from": r.get::<_, String>(0)?,
                "to": r.get::<_, String>(1)?,
                "kind": r.get::<_, String>(2)?,
                "weight": r.get::<_, f64>(3)?,
            }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(json_text(&json!({ "nodes": nodes, "edges": edges })))
}

fn list_wiki(conn: &Connection, args: &Value) -> Result<String> {
    let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(50).clamp(1, 200);
    let pages = crate::wiki::list_pages(conn, crate::wiki::DEFAULT_USER, limit)?;
    Ok(json_text(&pages))
}

fn get_wiki(conn: &Connection, args: &Value) -> Result<String> {
    let id = args.get("id").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let slug = args.get("slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let page = match (id, slug) {
        (Some(i), _) => crate::wiki::get_page(conn, crate::wiki::DEFAULT_USER, i)?,
        (None, Some(s)) => crate::wiki::get_page_by_slug(conn, crate::wiki::DEFAULT_USER, s)?,
        (None, None) => return Err(Error::Other("id or slug is required".into())),
    };
    Ok(json_text(&page))
}

fn wiki_reverse_links(conn: &Connection, args: &Value) -> Result<String> {
    let slug = args.get("slug").and_then(|v| v.as_str()).unwrap_or("");
    if slug.is_empty() {
        return Err(Error::Other("slug is required".into()));
    }
    let links = crate::wiki::reverse_links(conn, crate::wiki::DEFAULT_USER, slug)?;
    Ok(json_text(&links))
}

fn list_agents(conn: &Connection, _args: &Value) -> Result<String> {
    let agents = crate::agents::list_agents(conn, crate::agents::DEFAULT_USER)?;
    Ok(json_text(&agents))
}

fn agent_messages(conn: &Connection, args: &Value) -> Result<String> {
    let run_id = args.get("run_id").and_then(|v| v.as_str()).unwrap_or("");
    if run_id.is_empty() {
        return Err(Error::Other("run_id is required".into()));
    }
    let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(200).clamp(1, 500);
    let msgs = crate::agents::list_messages(conn, crate::agents::DEFAULT_USER, run_id, limit)?;
    Ok(json_text(&msgs))
}

fn cross_book_bridges(conn: &Connection, args: &Value) -> Result<String> {
    let nb = args.get("notebook_id").and_then(|v| v.as_str()).unwrap_or("");
    if nb.is_empty() {
        return Err(Error::Other("notebook_id is required".into()));
    }
    let bridges = crate::cross_book::bridges_for_subject(conn, nb)?;
    Ok(json_text(&bridges))
}

fn list_provenance(conn: &Connection, args: &Value) -> Result<String> {
    let sid = args.get("source_id").and_then(|v| v.as_str()).unwrap_or("");
    if sid.is_empty() {
        return Err(Error::Other("source_id is required".into()));
    }
    let rows = crate::repo::list_provenance(conn, sid)?;
    Ok(json_text(&rows))
}

/// Handle one JSON-RPC message. Returns the response (None for notifications).
pub fn handle_message(conn: &Connection, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    // Notifications carry no id and expect no response.
    if id.is_none() {
        return None;
    }
    let id = id.unwrap();
    let ok = |result: Value| Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    let err = |code: i64, message: &str| {
        Some(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }))
    };
    match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or(PROTOCOL_VERSION);
            ok(json!({
                "protocolVersion": requested,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "cortex", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Cortex local study library (read-only). Call list_notebooks first, then search_notebook / read_document. No model calls, no writes."
            }))
        }
        "ping" => ok(json!({})),
        "tools/list" => ok(json!({ "tools": tool_defs() })),
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call_tool(conn, name, &args) {
                Ok(text) => ok(json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": false
                })),
                // Tool-level failures are results with isError, per MCP — not
                // JSON-RPC errors (those are reserved for protocol faults).
                Err(text) => ok(json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": true
                })),
            }
        }
        other => err(-32601, &format!("method not found: {other}")),
    }
}

/// The stdio loop: one JSON-RPC message per line. Unknown/garbage lines get a
/// parse error; the loop never dies on client input.
pub fn run_stdio(conn: &Connection) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                let resp = json!({
                    "jsonrpc": "2.0", "id": Value::Null,
                    "error": { "code": -32700, "message": format!("parse error: {e}") }
                });
                writeln!(stdout, "{resp}").ok();
                stdout.flush().ok();
                continue;
            }
        };
        if let Some(resp) = handle_message(conn, &msg) {
            writeln!(stdout, "{resp}").ok();
            stdout.flush().ok();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Seed a tiny library in a temp FILE db, then reopen it READ-ONLY —
    /// exactly the production mode (the server must work on a read-only
    /// connection, and the tests prove it).
    fn seeded(tag: &str) -> (Connection, PathBuf) {
        let dir = std::env::temp_dir().join(format!("cortex-mcp-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cortex.db");
        {
            let st = crate::db::AppState::new(&path).unwrap();
            let c = st.db.lock().unwrap();
            c.execute_batch(
                "INSERT INTO subjects (id, name, created_at, updated_at) VALUES ('s1', '线性代数', 1, 1);
                 INSERT INTO sources (id, subject_id, name, kind, status, content, created_at, updated_at)
                   VALUES ('src1', 's1', '特征值讲义.md', 'md', 'ready', '特征值分解把矩阵拆成旋转与缩放。', 1, 1);
                 INSERT INTO chunks (id, source_id, subject_id, ord, text, loc, dim, embedding, created_at)
                   VALUES ('c1', 'src1', 's1', 0, '特征值分解把矩阵拆成旋转与缩放。', 'p.3', 2, X'0000000000000000', 1);
                 INSERT INTO notes (id, subject_id, title, body, created_at, updated_at)
                   VALUES ('n1', 's1', '特征值笔记', '相似变换保持特征值不变。', 1, 1);",
            )
            .unwrap();
            crate::wiki::upsert_page(
                &c,
                crate::wiki::DEFAULT_USER,
                None,
                Some("eigen"),
                "特征值",
                "相似变换保持特征值不变。",
                None,
            )
            .unwrap();
        }
        (open_readonly(&path).unwrap(), dir)
    }

    #[test]
    fn initialize_reports_tools_capability() {
        let (c, dir) = seeded("init");
        let resp = handle_message(&c, &json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2024-11-05", "capabilities": {} }
        }))
        .unwrap();
        assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(resp["result"]["serverInfo"]["name"], "cortex");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
        drop(c);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn notifications_get_no_response() {
        let (c, dir) = seeded("notif");
        assert!(handle_message(&c, &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).is_none());
        drop(c);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tools_list_has_the_five_tools() {
        let (c, dir) = seeded("tools");
        let resp = handle_message(&c, &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })).unwrap();
        let names: Vec<&str> = resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        for expected in [
            "list_notebooks", "search_notebook", "get_source", "read_document", "search_notes",
            "get_knowledge_tree", "list_wiki", "get_wiki", "wiki_reverse_links",
            "list_agents", "agent_messages", "cross_book_bridges", "list_provenance",
        ] {
            assert!(names.contains(&expected), "missing {expected}");
        }
        assert_eq!(names.len(), 13, "tool surface should be exactly 13 read-only tools");
        drop(c);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn call(c: &Connection, name: &str, args: Value) -> Value {
        handle_message(
            c,
            &json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call",
                     "params": { "name": name, "arguments": args } }),
        )
        .unwrap()
    }

    #[test]
    fn list_notebooks_reports_counts() {
        let (c, dir) = seeded("ln");
        let resp = call(&c, "list_notebooks", json!({}));
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("线性代数"));
        assert!(text.contains("1 sources"));
        assert!(text.contains("1 notes"));
        drop(c);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn search_notebook_returns_passages_with_ids() {
        let (c, dir) = seeded("sn");
        let resp = call(&c, "search_notebook", json!({ "notebook_id": "s1", "query": "特征值" }));
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("特征值讲义.md"));
        assert!(text.contains("source_id=src1"));
        assert!(text.contains("chunk_id=c1"));
        drop(c);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn get_source_never_leaks_paths() {
        let (c, dir) = seeded("gs");
        let resp = call(&c, "get_source", json!({ "source_id": "src1" }));
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("name: 特征值讲义.md"));
        assert!(text.contains("chars:"));
        // No path-ish keys at all.
        assert!(!text.contains("path"));
        assert!(!text.contains("origin"));
        assert!(!text.contains(":\\"));
        drop(c);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn read_document_paginates_by_chars() {
        let (c, dir) = seeded("rd");
        let resp = call(&c, "read_document", json!({ "source_id": "src1", "offset": 0, "limit": 5 }));
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("[chars 0..5 of"));
        // Past-the-end is an informative result, not an error.
        let resp2 = call(&c, "read_document", json!({ "source_id": "src1", "offset": 9999 }));
        assert!(resp2["result"]["content"][0]["text"].as_str().unwrap().contains("past the end"));
        drop(c);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn search_notes_matches_title_and_body() {
        let (c, dir) = seeded("notes");
        let resp = call(&c, "search_notes", json!({ "query": "相似变换" }));
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("特征值笔记"));
        // Wildcards are escaped: a literal % must not match everything.
        let resp2 = call(&c, "search_notes", json!({ "query": "%" }));
        assert!(resp2["result"]["content"][0]["text"].as_str().unwrap().contains("No notes matched"));
        drop(c);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn wiki_tools_read_pages_and_links() {
        let (c, dir) = seeded("wiki");
        let resp = call(&c, "get_wiki", json!({ "slug": "eigen" }));
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("特征值"));
        let resp2 = call(&c, "list_wiki", json!({}));
        assert!(resp2["result"]["content"][0]["text"].as_str().unwrap().contains("eigen"));
        // Missing id AND slug is a tool error, not a panic.
        let resp3 = call(&c, "get_wiki", json!({}));
        assert_eq!(resp3["result"]["isError"], true);
        drop(c);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unknown_tool_is_a_tool_error_not_a_protocol_error() {
        let (c, dir) = seeded("unk");
        let resp = call(&c, "delete_everything", json!({}));
        assert_eq!(resp["result"]["isError"], true);
        assert!(resp["error"].is_null());
        drop(c);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unknown_method_is_protocol_error() {
        let (c, dir) = seeded("meth");
        let resp = handle_message(&c, &json!({ "jsonrpc": "2.0", "id": 3, "method": "resources/list" })).unwrap();
        assert_eq!(resp["error"]["code"], -32601);
        drop(c);
        let _ = std::fs::remove_dir_all(dir);
    }
}
