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
            { "name": "get_knowledge_tree","description": "Read the knowledge framework nodes/edges and mastery for a subject.", "params": ["subject_id"] }
        ]
    })
}

fn dispatch(conn: &Connection, name: &str, args: &Value) -> Value {
    let result = match name {
        "list_notebooks" => list_notebooks(conn),
        "search_notebook" => {
            let sid = args.get("subject_id").and_then(|v| v.as_str()).unwrap_or("");
            let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let lim = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(10);
            search_notebook(conn, sid, q, lim)
        }
        "get_source" => {
            let sid = args.get("source_id").and_then(|v| v.as_str()).unwrap_or("");
            get_source(conn, sid)
        }
        "search_notes" => {
            let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let lim = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(10);
            search_notes(conn, q, lim)
        }
        "get_knowledge_tree" => {
            let sid = args.get("subject_id").and_then(|v| v.as_str()).unwrap_or("");
            get_knowledge_tree(conn, sid)
        }
        other => Err(rusqlite::Error::InvalidParameterName(format!("unknown tool {other}"))),
    };
    match result {
        Ok(v) => v,
        Err(e) => json!({ "error": e.to_string() }),
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
