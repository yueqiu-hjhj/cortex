//! cortex-mcp — read-only stdio JSON-RPC 2.0 server for the Cortex knowledge base.
//!
//! Speaks MCP framing (one JSON object per line on stdout, lines on stdin) and
//! implements the five read-only tools seeded by migration 0030. The server is
//! intentionally side-effect free: it opens the same SQLite file the desktop
//! app uses (passed via `--db <path>` or $CORTEX_DB) and answers
//! `tools/call` requests by running SELECT queries.
//!
//! Wire format: newline-delimited JSON. Each line on stdin is a single request;
//! each line on stdout is a single response. Notifications (`tools/list` etc.)
//! are also supported but produce no response.
//!
//! Usage:
//!   cortex-mcp --db /path/to/cortex.db
//!   # or
//!   CORTEX_DB=/path/to/cortex.db cortex-mcp

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::env;
use std::io::{self, BufRead, Write};

fn db_path() -> Option<String> {
    // 1. --db <path>
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--db" {
            if let Some(p) = args.next() {
                return Some(p);
            }
        } else if let Some(p) = a.strip_prefix("--db=") {
            return Some(p.to_string());
        }
    }
    // 2. $CORTEX_DB
    env::var("CORTEX_DB").ok()
}

fn open_db() -> rusqlite::Result<Connection> {
    let path = db_path().ok_or_else(|| {
        rusqlite::Error::InvalidParameterName(
            "no --db <path> given and $CORTEX_DB is not set".into(),
        )
    })?;
    let conn = Connection::open(&path)?;
    conn.pragma_update(None, "journal_mode", "WAL").ok();
    conn.pragma_update(None, "foreign_keys", "ON").ok();
    Ok(conn)
}

/// One row per subject with a topic count + sources.
fn list_notebooks(conn: &Connection) -> rusqlite::Result<Value> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.name, s.code, s.color,
            (SELECT COUNT(*) FROM topics t WHERE t.subject_id = s.id) AS topic_count,
            (SELECT COUNT(*) FROM sources src WHERE src.subject_id = s.id) AS source_count
         FROM subjects s
         ORDER BY s.position, s.name",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "name": r.get::<_, String>(1)?,
            "code": r.get::<_, Option<String>>(2)?,
            "color": r.get::<_, Option<String>>(3)?,
            "topic_count": r.get::<_, i64>(4)?,
            "source_count": r.get::<_, i64>(5)?,
        }))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(Value::Array(out))
}

fn search_notebook(conn: &Connection, subject_id: &str, query: &str, limit: i64) -> rusqlite::Result<Value> {
    let lim = limit.clamp(1, 50);
    let pat = format!("%{}%", query);
    let mut stmt = conn.prepare(
        "SELECT c.id, c.source_id, c.ord, substr(c.text, 1, 240), s.name, s.subject_id
         FROM chunks c JOIN sources s ON s.id = c.source_id
         WHERE s.subject_id = ?1 AND c.text LIKE ?2
         ORDER BY c.ord LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![subject_id, pat, lim], |r| {
        Ok(json!({
            "chunk_id": r.get::<_, String>(0)?,
            "source_id": r.get::<_, String>(1)?,
            "ordinal": r.get::<_, i64>(2)?,
            "preview": r.get::<_, String>(3)?,
            "source_name": r.get::<_, String>(4)?,
            "subject_id": r.get::<_, String>(5)?,
        }))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(Value::Array(out))
}

fn get_source(conn: &Connection, source_id: &str) -> rusqlite::Result<Value> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.name, s.subject_id, s.kind, s.status, s.created_at,
            (SELECT COUNT(*) FROM chunks c WHERE c.source_id = s.id) AS chunk_count
         FROM sources s WHERE s.id = ?1",
    )?;
    let mut rows = stmt.query(params![source_id])?;
    let row = rows.next()?.ok_or_else(|| rusqlite::Error::QueryReturnedNoRows)?;
    let meta = json!({
        "id": row.get::<_, String>(0)?,
        "name": row.get::<_, String>(1)?,
        "subject_id": row.get::<_, String>(2)?,
        "kind": row.get::<_, String>(3)?,
        "status": row.get::<_, String>(4)?,
        "created_at": row.get::<_, i64>(5)?,
        "chunk_count": row.get::<_, i64>(6)?,
    });
    drop(rows);
    // Concatenate chunk text (capped to 32 KB to keep MCP responses bounded).
    let mut stmt = conn.prepare(
        "SELECT ord, text FROM chunks WHERE source_id = ?1 ORDER BY ord",
    )?;
    let chunks = stmt.query_map(params![source_id], |r| {
        Ok(json!({
            "ordinal": r.get::<_, i64>(0)?,
            "text": r.get::<_, String>(1)?,
        }))
    })?;
    let mut text = String::new();
    for c in chunks {
        let v = c?;
        text.push_str(&format!("[#{}] {}\n", v["ordinal"], v["text"].as_str().unwrap_or("")));
        if text.len() > 32 * 1024 {
            text.push_str("…(truncated)");
            break;
        }
    }
    Ok(json!({ "meta": meta, "text": text }))
}

fn search_notes(conn: &Connection, query: &str, limit: i64) -> rusqlite::Result<Value> {
    let lim = limit.clamp(1, 50);
    let pat = format!("%{}%", query);
    let mut stmt = conn.prepare(
        "SELECT id, title, substr(body, 1, 240), subject_id, updated_at
         FROM notes WHERE title LIKE ?1 OR body LIKE ?1
         ORDER BY updated_at DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![pat, lim], |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "title": r.get::<_, String>(1)?,
            "preview": r.get::<_, String>(2)?,
            "subject_id": r.get::<_, Option<String>>(3)?,
            "updated_at": r.get::<_, i64>(4)?,
        }))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(Value::Array(out))
}

fn get_knowledge_tree(conn: &Connection, subject_id: &str) -> rusqlite::Result<Value> {
    let mut stmt = conn.prepare(
        "SELECT id, topic_id, source_id, parent_id, node_key, kind, title,
            position, coverage, mastery, importance, confidence, observations
         FROM knowledge_nodes WHERE subject_id = ?1 ORDER BY position",
    )?;
    let rows = stmt.query_map(params![subject_id], |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "topic_id": r.get::<_, Option<String>>(1)?,
            "source_id": r.get::<_, Option<String>>(2)?,
            "parent_id": r.get::<_, Option<String>>(3)?,
            "node_key": r.get::<_, String>(4)?,
            "kind": r.get::<_, String>(5)?,
            "title": r.get::<_, String>(6)?,
            "position": r.get::<_, i64>(7)?,
            "coverage": r.get::<_, f64>(8)?,
            "mastery": r.get::<_, f64>(9)?,
            "importance": r.get::<_, f64>(10)?,
            "confidence": r.get::<_, f64>(11)?,
            "observations": r.get::<_, i64>(12)?,
        }))
    })?;
    let mut nodes = Vec::new();
    for row in rows {
        nodes.push(row?);
    }
    drop(stmt);
    let mut stmt = conn.prepare(
        "SELECT id, from_id, to_id, kind, weight
         FROM knowledge_edges WHERE subject_id = ?1",
    )?;
    let rows = stmt.query_map(params![subject_id], |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "from_id": r.get::<_, String>(1)?,
            "to_id": r.get::<_, String>(2)?,
            "kind": r.get::<_, String>(3)?,
            "weight": r.get::<_, f64>(4)?,
        }))
    })?;
    let mut edges = Vec::new();
    for row in rows {
        edges.push(row?);
    }
    Ok(json!({ "nodes": nodes, "edges": edges }))
}

fn tool_definitions() -> Value {
    json!({
        "tools": [
            { "name": "list_notebooks",   "description": "List all subjects (notebooks) and their topic/source counts." },
            { "name": "search_notebook",  "description": "Search indexed chunks within one subject.", "params": ["subject_id", "query", "limit?"] },
            { "name": "get_source",       "description": "Read source metadata and the concatenated chunk text.", "params": ["source_id"] },
            { "name": "search_notes",     "description": "Search user notes by title or body.", "params": ["query", "limit?"] },
            { "name": "get_knowledge_tree","description": "Read the knowledge framework nodes/edges and mastery for a subject.", "params": ["subject_id"] },
            { "name": "list_wiki",        "description": "List wiki pages for the active user.", "params": ["limit?"] },
            { "name": "get_wiki",         "description": "Read a wiki page by id or slug.", "params": ["id", "slug?"] },
            { "name": "upsert_wiki",      "description": "Create or update a wiki page; returns the saved page (with audit trace).", "params": ["id?", "slug?", "title", "body", "source?"] },
            { "name": "wiki_reverse_links","description": "List pages that link to a given slug.", "params": ["slug"] },
            { "name": "list_agents",      "description": "List configured agents for the active user.", "params": [] },
            { "name": "agent_run",        "description": "Start a new run for an agent.", "params": ["agent_id", "input"] },
            { "name": "agent_messages",   "description": "List the messages of an agent run (caller must own it).", "params": ["run_id", "limit?"] },
            { "name": "cross_book_for_subject","description": "List cross-subject concept bridges touching the given subject.", "params": ["subject_id"] },
            { "name": "search_notebook_isolated","description": "Per-notebook search: hits MUST come from chunks whose source belongs to the given subject.", "params": ["subject_id", "query", "limit?"] },
            { "name": "record_provenance","description": "Record external provenance for a source (KnowNote-style: who provided this content).", "params": ["source_id", "host", "doc_id", "url?", "snippet?"] },
            { "name": "list_provenance",  "description": "List external provenance rows for a source.", "params": ["source_id"] }
        ]
    })
}

fn dispatch(conn: &Connection, name: &str, args: &Value) -> Value {
    let err = |msg: &str| -> Value { json!({ "error": msg }) };
    let from_err = |res: Result<Value, String>| -> Value { match res { Ok(v) => v, Err(e) => json!({ "error": e }) } };
    match name {
        "list_notebooks" => list_notebooks(conn).unwrap_or(Value::Null),
        "search_notebook" => {
            let sid = args.get("subject_id").and_then(|v| v.as_str()).unwrap_or("");
            let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let lim = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(10);
            search_notebook(conn, sid, q, lim).unwrap_or(Value::Null)
        }
        "get_source" => {
            let sid = args.get("source_id").and_then(|v| v.as_str()).unwrap_or("");
            get_source(conn, sid).unwrap_or(Value::Null)
        }
        "search_notes" => {
            let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let lim = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(10);
            search_notes(conn, q, lim).unwrap_or(Value::Null)
        }
        "get_knowledge_tree" => {
            let sid = args.get("subject_id").and_then(|v| v.as_str()).unwrap_or("");
            get_knowledge_tree(conn, sid).unwrap_or(Value::Null)
        }
        "list_wiki" => from_err(
            cortex_lib::wiki::list_pages(conn, cortex_lib::wiki::DEFAULT_USER,
                args.get("limit").and_then(|v| v.as_i64()).unwrap_or(50))
                .map(|v| serde_json::to_value(v).unwrap_or(Value::Null))
                .map_err(|e| e.to_string())
        ),
        "get_wiki" => {
            let slug = args.get("slug").and_then(|v| v.as_str());
            let id = args.get("id").and_then(|v| v.as_str());
            match (id, slug) {
                (Some(i), _) => from_err(
                    cortex_lib::wiki::get_page(conn, cortex_lib::wiki::DEFAULT_USER, i)
                        .map(|p| serde_json::to_value(p).unwrap_or(Value::Null))
                        .map_err(|e| e.to_string())
                ),
                (None, Some(s)) => from_err(
                    cortex_lib::wiki::get_page_by_slug(conn, cortex_lib::wiki::DEFAULT_USER, s)
                        .map(|p| serde_json::to_value(p).unwrap_or(Value::Null))
                        .map_err(|e| e.to_string())
                ),
                (None, None) => err("id or slug required"),
            }
        }
        "upsert_wiki" => from_err(
            cortex_lib::wiki::upsert_page(
                conn, cortex_lib::wiki::DEFAULT_USER,
                args.get("id").and_then(|v| v.as_str()),
                args.get("slug").and_then(|v| v.as_str()),
                args.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                args.get("body").and_then(|v| v.as_str()).unwrap_or(""),
                args.get("source").and_then(|v| v.as_str()),
            ).map(|p| serde_json::to_value(p).unwrap_or(Value::Null))
             .map_err(|e| e.to_string())
        ),
        "wiki_reverse_links" => from_err(
            cortex_lib::wiki::reverse_links(conn, cortex_lib::wiki::DEFAULT_USER,
                args.get("slug").and_then(|v| v.as_str()).unwrap_or(""))
                .map(|v| serde_json::to_value(v).unwrap_or(Value::Null))
                .map_err(|e| e.to_string())
        ),
        "list_agents" => from_err(
            cortex_lib::agents::list_agents(conn, cortex_lib::agents::DEFAULT_USER)
                .map(|v| serde_json::to_value(v).unwrap_or(Value::Null))
                .map_err(|e| e.to_string())
        ),
        "agent_run" => from_err(
            cortex_lib::agents::start_run(
                conn, cortex_lib::agents::DEFAULT_USER,
                args.get("agent_id").and_then(|v| v.as_str()).unwrap_or(""),
                args.get("input").and_then(|v| v.as_str()).unwrap_or(""),
            ).map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
             .map_err(|e| e.to_string())
        ),
        "agent_messages" => from_err(
            cortex_lib::agents::list_messages(
                conn, cortex_lib::agents::DEFAULT_USER,
                args.get("run_id").and_then(|v| v.as_str()).unwrap_or(""),
                args.get("limit").and_then(|v| v.as_i64()).unwrap_or(200),
            ).map(|v| serde_json::to_value(v).unwrap_or(Value::Null))
             .map_err(|e| e.to_string())
        ),
        "cross_book_for_subject" => from_err(
            cortex_lib::cross_book::bridges_for_subject(conn,
                args.get("subject_id").and_then(|v| v.as_str()).unwrap_or(""))
                .map(|v| serde_json::to_value(v).unwrap_or(Value::Null))
                .map_err(|e| e.to_string())
        ),
        "search_notebook_isolated" => {
            let sid = args.get("subject_id").and_then(|v| v.as_str()).unwrap_or("");
            let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let lim = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(15) as usize;
            match cortex_lib::repo::keyword_search_chunks(conn, Some(sid), None, q, lim) {
                Ok(chunks) => Value::Array(chunks.into_iter().map(|h| json!({
                    "id": h.id, "source_id": h.source_id, "source_name": h.source_name,
                    "text": h.text, "loc": h.loc, "score": h.score, "subject_id": sid,
                })).collect()),
                Err(e) => json!({ "error": e.to_string() }),
            }
        }
        "record_provenance" => {
            let sid = args.get("source_id").and_then(|v| v.as_str()).unwrap_or("");
            let host = args.get("host").and_then(|v| v.as_str()).unwrap_or("");
            let doc = args.get("doc_id").and_then(|v| v.as_str()).unwrap_or("");
            let url = args.get("url").and_then(|v| v.as_str());
            let snippet = args.get("snippet").and_then(|v| v.as_str());
            match cortex_lib::repo::record_provenance(conn, sid, host, doc, url, snippet) {
                Ok(id) => Value::String(id),
                Err(e) => json!({ "error": e.to_string() }),
            }
        }
        "list_provenance" => match cortex_lib::repo::list_provenance(conn,
            args.get("source_id").and_then(|v| v.as_str()).unwrap_or(""))
        {
            Ok(v) => serde_json::to_value(v).unwrap_or(Value::Null),
            Err(e) => json!({ "error": e.to_string() }),
        },
        other => err(&format!("unknown tool {other}")),
    }
}

fn handle(req: Value, conn: &Connection) -> Option<Value> {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));

    // Notifications (no id) are fire-and-forget.
    if matches!(id, Value::Null) {
        return None;
    }

    let result = match method {
        "initialize" => json!({
            "protocolVersion": "2024-11-05",
            "serverInfo": { "name": "cortex-mcp", "version": "0.1.0" },
            "capabilities": { "tools": {} }
        }),
        "tools/list" => tool_definitions(),
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            json!({
                "content": [{ "type": "json", "json": dispatch(conn, name, &args) }]
            })
        }
        "ping" => json!({}),
        other => json!({ "error": format!("unknown method {other}") }),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn main() {
    let conn = match open_db() {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(
                io::stderr(),
                "cortex-mcp: failed to open database: {e}"
            );
            std::process::exit(2);
        }
    };

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                let err = json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("parse error: {e}") }
                });
                let _ = writeln!(out, "{}", err.to_string());
                let _ = out.flush();
                continue;
            }
        };
        if let Some(resp) = handle(req, &conn) {
            let _ = writeln!(out, "{}", resp.to_string());
            let _ = out.flush();
        }
    }
}
