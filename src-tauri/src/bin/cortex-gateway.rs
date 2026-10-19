//! cortex-gateway — an OpenAI-compatible local HTTP gateway over the Cortex KB.
//!
//! Test code is allowed to be messier; see the lib-level allow for the rationale.
#![cfg_attr(test, allow(clippy::all, dead_code, unused_imports))]
//!
//! Lets any OpenAI-protocol client (Cherry Studio, OpenWebUI, Cursor, curl, …)
//! query the user's knowledge base by pointing it at this server: the gateway
//! retrieves relevant chunks from the same SQLite file the desktop app uses and
//! answers with the configured chat model, returning an OpenAI-shaped response
//! plus a `citations` extension field.
//!
//! It is also a PRODUCT, not just an API: `GET /` serves a self-contained
//! read-only web view (subjects → sources → full text, plus a library
//! overview) over a handful of read-only JSON endpoints. The page is a single
//! embedded HTML file, so a homelab can serve it from any static host and
//! point it at the gateway with `?api=http://host:8788`.
//!
//! Endpoints:
//!   GET  /healthz              liveness probe
//!   GET  /                     read-only web view (embedded gateway.html)
//!   GET  /api/overview         library totals + subjects + recent notes
//!   GET  /api/sources          `?subject_id=` → source metadata (no text)
//!   GET  /api/source           `?id=` → one source with extracted text
//!   GET  /v1/models            logical model list (the KB answerer)
//!   POST /v1/chat/completions  retrieval-augmented completion (non-streaming)
//!
//! Usage:
//!   cortex-gateway --db /path/to/cortex.db [--port 8788] [--host 127.0.0.1]
//!   # or via env: CORTEX_DB / CORTEX_GATEWAY_PORT / CORTEX_GATEWAY_HOST
//!
//! Binds to 127.0.0.1 by default — this server exposes your whole KB and uses
//! your API keys; do not expose it to the network without adding auth.

use cortex_lib::llm;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::env;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

const DEFAULT_CHAT_MODEL: &str = "openrouter:deepseek/deepseek-v4-flash";
const TOP_K: usize = 8;
const SNIPPET_LEN: usize = 600;

fn arg_opt(flag: &str) -> Option<String> {
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        if a == flag {
            if let Some(p) = args.next() {
                return Some(p);
            }
        } else if let Some(p) = a.strip_prefix(&format!("{flag}=")) {
            return Some(p.to_string());
        }
    }
    None
}

fn open_db() -> rusqlite::Result<Connection> {
    let path = arg_opt("--db")
        .or_else(|| env::var("CORTEX_DB").ok())
        .ok_or_else(|| {
            rusqlite::Error::InvalidParameterName(
                "no --db <path> given and $CORTEX_DB is not set".into(),
            )
        })?;
    let conn = Connection::open(&path)?;
    conn.pragma_update(None, "journal_mode", "WAL").ok();
    conn.pragma_update(None, "foreign_keys", "ON").ok();
    Ok(conn)
}

fn get_setting(conn: &Connection, key: &str) -> Option<String> {
    cortex_lib::repo::get_setting(conn, key).ok().flatten()
}

fn load_keys(conn: &Connection) -> llm::Keys {
    llm::Keys {
        gemini: get_setting(conn, "gemini_api_key"),
        openrouter: get_setting(conn, "openrouter_api_key"),
        openai: get_setting(conn, "openai_api_key"),
        claude: get_setting(conn, "claude_api_key"),
        custom_api_key: get_setting(conn, "custom_api_key"),
        custom_endpoint: get_setting(conn, "custom_endpoint"),
        ollama_url: get_setting(conn, "ollama_url"),
        offline: get_setting(conn, "offline_mode").as_deref() == Some("true"),
        extra: std::collections::HashMap::new(),
    }
}

fn is_han(c: char) -> bool {
    matches!(c as u32,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F)
}

fn push_whole(
    t: &str,
    and_terms: &mut Vec<String>,
    score_terms: &mut Vec<String>,
    seen: &mut std::collections::HashSet<String>,
) {
    if seen.insert(t.to_string()) {
        and_terms.push(t.to_string());
        score_terms.push(t.to_string());
    }
}

/// Split the query into retrieval terms. Chinese has no whitespace, so without
/// this a whole sentence would become one giant AND term that never matches.
/// Returns (and_terms, score_terms):
/// - and_terms — all must match (Latin words, short CJK runs ≤ 4 chars);
/// - score_terms — relaxed-pass candidates ranked by hit count; long CJK runs
///   contribute overlapping bigrams here instead, since an AND over the full
///   run is nearly always a miss while ≥2 shared bigrams is a strong signal.
fn tokenize(query: &str) -> (Vec<String>, Vec<String>) {
    let mut and_terms = Vec::new();
    let mut score_terms = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for run in query.split(|c: char| !c.is_alphanumeric()) {
        if run.is_empty() {
            continue;
        }
        let chars: Vec<char> = run.chars().collect();
        let han = chars.iter().filter(|c| is_han(**c)).count();
        if han > 0 && han == chars.len() && chars.len() > 4 {
            for w in chars.windows(2) {
                let g: String = w.iter().collect();
                if seen.insert(g.clone()) {
                    score_terms.push(g);
                }
            }
        } else if chars.len() >= 2 {
            push_whole(run, &mut and_terms, &mut score_terms, &mut seen);
        }
    }
    (and_terms, score_terms)
}

/// Keyword retrieval, pure SQL — same layer as cortex-mcp, no vector extension
/// needed in the gateway process. Pass 1: AND over all short terms. Pass 2
/// (relaxed, only when pass 1 finds nothing): rank chunks by how many score
/// terms (bigrams included) hit; require ≥2 hits — or the single term when
/// only one exists — so scattered single-word noise loses to real overlaps.
fn retrieve(conn: &Connection, query: &str, limit: usize) -> Vec<Value> {
    let (and_terms, score_terms) = tokenize(query);
    let run = |pats: &[String]| -> rusqlite::Result<Vec<Value>> {
        let mut sql = format!(
            "SELECT c.id, c.source_id, c.ord, substr(c.text, 1, {SNIPPET_LEN}), s.name, s.subject_id
             FROM chunks c JOIN sources s ON s.id = c.source_id
             WHERE 1=1"
        );
        for _ in pats {
            sql.push_str(" AND c.text LIKE ?");
        }
        sql.push_str(" ORDER BY c.ord LIMIT ");
        sql.push_str(&limit.to_string());
        let mut stmt = conn.prepare(&sql)?;
        let pats: Vec<String> = pats.iter().map(|p| format!("%{p}%")).collect();
        let rows = stmt.query_map(rusqlite::params_from_iter(pats.iter()), |r| {
            Ok(json!({
                "chunk_id": r.get::<_, String>(0)?,
                "source_id": r.get::<_, String>(1)?,
                "ordinal": r.get::<_, i64>(2)?,
                "text": r.get::<_, String>(3)?,
                "source_name": r.get::<_, String>(4)?,
                "subject_id": r.get::<_, String>(5)?,
            }))
        })?;
        rows.collect()
    };

    if !and_terms.is_empty() {
        if let Ok(hits) = run(&and_terms) {
            if !hits.is_empty() {
                return hits;
            }
        }
    }
    if score_terms.is_empty() {
        return Vec::new();
    }
    let threshold = score_terms.len().min(2);
    let expr = score_terms
        .iter()
        .map(|_| "(c.text LIKE ?)")
        .collect::<Vec<_>>()
        .join(" + ");
    let sql = format!(
        "SELECT c.id, c.source_id, c.ord, substr(c.text, 1, {SNIPPET_LEN}), s.name, s.subject_id, ({expr}) AS score
         FROM chunks c JOIN sources s ON s.id = c.source_id
         WHERE ({expr}) >= {threshold}
         ORDER BY score DESC, c.ord LIMIT {limit}"
    );
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Vec::new();
    };
    let mut pats: Vec<String> = Vec::with_capacity(score_terms.len() * 2);
    for _ in 0..2 {
        for t in &score_terms {
            pats.push(format!("%{t}%"));
        }
    }
    let rows = stmt.query_map(rusqlite::params_from_iter(pats.iter()), |r| {
        Ok(json!({
            "chunk_id": r.get::<_, String>(0)?,
            "source_id": r.get::<_, String>(1)?,
            "ordinal": r.get::<_, i64>(2)?,
            "text": r.get::<_, String>(3)?,
            "source_name": r.get::<_, String>(4)?,
            "subject_id": r.get::<_, String>(5)?,
        }))
    });
    rows.map(|rs| rs.filter_map(|x| x.ok()).collect()).unwrap_or_default()
}

fn build_context(hits: &[Value]) -> (String, Vec<Value>) {
    let mut blocks = String::new();
    let mut citations = Vec::new();
    for (i, h) in hits.iter().enumerate() {
        let n = i + 1;
        let src = h["source_name"].as_str().unwrap_or("unknown");
        let ord = h["ordinal"].as_i64().unwrap_or(0);
        let text = h["text"].as_str().unwrap_or("");
        blocks.push_str(&format!("[{n}] {src} · loc {ord}\n{text}\n\n"));
        citations.push(json!({
            "index": n,
            "source_name": src,
            "source_id": h["source_id"],
            "chunk_id": h["chunk_id"],
            "ordinal": ord,
        }));
    }
    (blocks, citations)
}

fn answer(conn: &Connection, body: &Value) -> Result<Value, String> {
    let messages = body
        .get("messages")
        .and_then(|v| v.as_array())
        .ok_or("missing `messages` array")?;
    let query = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .and_then(|m| m.get("content").and_then(|c| c.as_str()))
        .ok_or("no user message found")?
        .to_string();
    if query.trim().is_empty() {
        return Err("user message is empty".into());
    }

    let hits = retrieve(conn, &query, TOP_K);
    let (blocks, _citations) = build_context(&hits);
    let system = if blocks.is_empty() {
        "You answer questions using your general knowledge. The user's local knowledge base \
         had no relevant material for this question — say so briefly."
            .to_string()
    } else {
        format!(
            "You are the user's knowledge-base assistant. Answer the question using the \
             RETRIEVED CONTEXT below, citing sources inline as [n]. If the context is \
             insufficient, say what is missing — do not invent material. Answer in the \
             language of the question.\n\nRETRIEVED CONTEXT:\n\n{blocks}"
        )
    };

    let model = llm::from_spec_or_any(
        &get_setting(conn, "model_chat").unwrap_or_else(|| DEFAULT_CHAT_MODEL.into()),
        &load_keys(conn),
    )
    .ok_or_else(|| {
        "no chat model configured — add an API key in the Cortex desktop app (Settings → API keys)".to_string()
    })?;
    let raw_answer = model.complete(&system, &query).map_err(|e| e.to_string())?;

    // Citation existence verification: a [n] beyond the retrieved set is a
    // ghost — strip it, and expose only the citations actually cited. With
    // zero hits every [n] is fabricated by definition (max = 0 strips all),
    // which is the honest shape for "nothing in your KB covers this".
    let (answer, cited_idx) = cortex_lib::citecheck::verify_numbered(&raw_answer, hits.len());
    let citations: Vec<Value> = cited_idx
        .iter()
        .filter_map(|&n| hits.get(n - 1).cloned())
        .collect();

    Ok(json!({
        "id": format!("chatcmpl-cortex-{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)),
        "object": "chat.completion",
        "created": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        "model": body.get("model").and_then(|m| m.as_str()).unwrap_or("cortex-kb"),
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": answer,
                "citations": citations,
            },
            "finish_reason": "stop",
        }],
        "usage": { "prompt_tokens": null, "completion_tokens": null, "total_tokens": null },
        "cortex": {
            "retrieved": hits.len(),
            "abstained": hits.is_empty(),
            "citations": citations,
        },
    }))
}

struct Request {
    method: String,
    path: String,
    query: String,
    body: Vec<u8>,
}

/// `?a=b&c=d` → map. Values are percent-decoded (spaces as `+` or `%20`).
fn parse_query(q: &str) -> std::collections::HashMap<String, String> {
    fn decode(s: &str) -> String {
        let s = s.replace('+', " ");
        let raw = s.as_bytes();
        let mut bytes: Vec<u8> = Vec::with_capacity(raw.len());
        let mut i = 0;
        while i < raw.len() {
            if raw[i] == b'%' && i + 2 < raw.len() {
                if let Ok(b) = u8::from_str_radix(s.get(i + 1..i + 3).unwrap_or(""), 16) {
                    bytes.push(b);
                    i += 3;
                    continue;
                }
            }
            bytes.push(raw[i]);
            i += 1;
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }
    let mut out = std::collections::HashMap::new();
    for pair in q.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(decode(k), decode(v));
    }
    out
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let full_path = parts.next()?.to_string();
    let mut content_length = 0usize;
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).ok()?;
        let h = h.trim();
        if h.is_empty() {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = v.trim().parse().ok()?;
        }
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).ok()?;
    }
    // Route on the path; the query string is kept for the /api handlers.
    let (path, query) = match full_path.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (full_path, String::new()),
    };
    Some(Request { method, path, query, body })
}

fn write_html(stream: &mut TcpStream, body: &str) {
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

fn write_json(stream: &mut TcpStream, status: &str, body: &Value) {
    let payload = body.to_string();
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

// ------------------------------------------------------- read-only web API

/// Library totals + subjects + recent notes — the front page of the web view.
fn api_overview(conn: &Connection) -> Value {
    let one = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0);
    let total_sources = one("SELECT COUNT(*) FROM sources");
    let total_chunks = one("SELECT COUNT(*) FROM chunks");
    let total_notes = one("SELECT COUNT(*) FROM notes");
    let total_chars: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(length(content)), 0) FROM sources WHERE content IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let mut subjects: Vec<Value> = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT sub.id, sub.name,
                (SELECT COUNT(*) FROM topics t WHERE t.subject_id = sub.id),
                (SELECT COUNT(*) FROM sources s WHERE s.subject_id = sub.id),
                (SELECT COUNT(*) FROM chunks c JOIN sources s2 ON s2.id = c.source_id
                  WHERE s2.subject_id = sub.id)
           FROM subjects sub
          ORDER BY sub.created_at",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "topics": r.get::<_, i64>(2)?,
                "sources": r.get::<_, i64>(3)?,
                "chunks": r.get::<_, i64>(4)?,
            }))
        }) {
            for row in rows.flatten() {
                subjects.push(row);
            }
        }
    }

    let mut recent_notes: Vec<Value> = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT title, substr(body, 1, 160) FROM notes ORDER BY updated_at DESC LIMIT 5",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok(json!({ "title": r.get::<_, String>(0)?, "snippet": r.get::<_, String>(1)? }))
        }) {
            for row in rows.flatten() {
                recent_notes.push(row);
            }
        }
    }

    json!({
        "total": { "sources": total_sources, "chunks": total_chunks, "notes": total_notes, "chars": total_chars },
        "subjects": subjects,
        "recent_notes": recent_notes,
    })
}

/// Source metadata for one subject — names and status, never full text, so
/// the list view stays one round-trip even for a big library.
fn api_sources(conn: &Connection, subject_id: &str) -> Value {
    let mut out: Vec<Value> = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT id, name, kind, status, COALESCE(length(content), 0), created_at
           FROM sources WHERE subject_id = ?1 ORDER BY created_at DESC",
    ) {
        if let Ok(rows) = stmt.query_map(params![subject_id], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "kind": r.get::<_, String>(2)?,
                "status": r.get::<_, String>(3)?,
                "chars": r.get::<_, i64>(4)?,
                "created_at": r.get::<_, i64>(5)?,
            }))
        }) {
            for row in rows.flatten() {
                out.push(row);
            }
        }
    }
    json!(out)
}

/// One source with its extracted text — the deepest the read-only view goes.
fn api_source(conn: &Connection, id: &str) -> Value {
    match conn.query_row(
        "SELECT name, kind, status, COALESCE(content, ''), COALESCE(length(content), 0)
           FROM sources WHERE id = ?1",
        params![id],
        |r| {
            Ok(json!({
                "name": r.get::<_, String>(0)?,
                "kind": r.get::<_, String>(1)?,
                "status": r.get::<_, String>(2)?,
                "content": r.get::<_, String>(3)?,
                "chars": r.get::<_, i64>(4)?,
            }))
        },
    ) {
        Ok(v) => v,
        Err(_) => json!({ "error": "source not found" }),
    }
}

/// The read-only web view: one embedded HTML file, no build step. A homelab
/// can also serve this file from any static host via `?api=<gateway-url>`.
const GATEWAY_HTML: &str = include_str!("gateway.html");

fn handle_client(mut stream: TcpStream) {
    let req = match read_request(&mut stream) {
        Some(r) => r,
        None => return,
    };
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/healthz") => write_json(&mut stream, "200 OK", &json!({"ok": true})),
        ("GET", "/") => write_html(&mut stream, GATEWAY_HTML),
        ("GET", "/api/overview") | ("GET", "/api/sources") | ("GET", "/api/source") => {
            let conn = match open_db() {
                Ok(c) => c,
                Err(e) => {
                    write_json(&mut stream, "500 Internal Server Error",
                        &json!({ "error": { "message": format!("db open failed: {e}"), "type": "cortex_gateway_error" } }));
                    return;
                }
            };
            let q = parse_query(&req.query);
            let body = match req.path.as_str() {
                "/api/overview" => api_overview(&conn),
                "/api/sources" => match q.get("subject_id") {
                    Some(sid) => api_sources(&conn, sid),
                    None => json!({ "error": "missing subject_id" }),
                },
                _ => match q.get("id") {
                    Some(id) => api_source(&conn, id),
                    None => json!({ "error": "missing id" }),
                },
            };
            write_json(&mut stream, "200 OK", &body);
        }
        ("GET", "/v1/models") => write_json(
            &mut stream,
            "200 OK",
            &json!({
                "object": "list",
                "data": [{
                    "id": "cortex-kb",
                    "object": "model",
                    "owned_by": "cortex",
                    "description": "Retrieval-augmented answers over your local Cortex knowledge base",
                }]
            }),
        ),
        ("POST", "/v1/chat/completions") => {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let conn = match open_db() {
                Ok(c) => c,
                Err(e) => {
                    write_json(
                        &mut stream,
                        "500 Internal Server Error",
                        &json!({ "error": { "message": format!("db open failed: {e}"), "type": "cortex_gateway_error" } }),
                    );
                    return;
                }
            };
            match answer(&conn, &body) {
                Ok(resp) => write_json(&mut stream, "200 OK", &resp),
                Err(e) => write_json(
                    &mut stream,
                    "500 Internal Server Error",
                    &json!({ "error": { "message": e, "type": "cortex_gateway_error" } }),
                ),
            }
        }
        ("OPTIONS", _) => {
            let resp = "HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\n\
                        Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
                        Access-Control-Allow-Headers: Content-Type, Authorization\r\n\
                        Connection: close\r\n\r\n";
            let _ = stream.write_all(resp.as_bytes());
        }
        _ => write_json(
            &mut stream,
            "404 Not Found",
            &json!({ "error": { "message": format!("no route for {} {}", req.method, req.path) } }),
        ),
    }
}

fn main() {
    let db = match open_db() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cortex-gateway: failed to open database: {e}");
            std::process::exit(2);
        }
    };
    // Fail fast when no model is usable — better than 500s on every request.
    if llm::from_spec_or_any(
        &get_setting(&db, "model_chat").unwrap_or_else(|| DEFAULT_CHAT_MODEL.into()),
        &load_keys(&db),
    )
    .is_none()
    {
        eprintln!(
            "cortex-gateway: warning — no chat model/key configured; /v1/chat/completions will \
             return 500 until a key is added in the desktop app"
        );
    }
    drop(db);

    let host = arg_opt("--host")
        .or_else(|| env::var("CORTEX_GATEWAY_HOST").ok())
        .unwrap_or_else(|| "127.0.0.1".into());
    let port: u16 = arg_opt("--port")
        .or_else(|| env::var("CORTEX_GATEWAY_PORT").ok())
        .and_then(|p| p.parse().ok())
        .unwrap_or(8788);
    let addr = format!("{host}:{port}");
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cortex-gateway: cannot bind {addr}: {e}");
            std::process::exit(2);
        }
    };
    eprintln!("cortex-gateway: listening on http://{addr} (POST /v1/chat/completions)");
    for stream in listener.incoming().flatten() {
        std::thread::spawn(move || handle_client(stream));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sources (id TEXT PRIMARY KEY, name TEXT NOT NULL, subject_id TEXT NOT NULL);
             CREATE TABLE chunks (
               id TEXT PRIMARY KEY,
               source_id TEXT NOT NULL REFERENCES sources(id),
               ord INTEGER NOT NULL,
               text TEXT NOT NULL
             );",
        )
        .unwrap();
        conn.execute("INSERT INTO sources VALUES ('s1', '对比矩阵笔记.pdf', 'sub1')", []).unwrap();
        conn.execute("INSERT INTO sources VALUES ('s2', 'linked-list.md', 'sub1')", []).unwrap();
        conn.execute(
            "INSERT INTO chunks VALUES ('c1', 's1', 1, '对比矩阵用于横向比较多个概念的异同，学习指南则按分区给出要点与自测题。')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chunks VALUES ('c2', 's2', 1, 'A linked list stores nodes; an array uses contiguous memory.')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO chunks VALUES ('c3', 's1', 2, '番茄钟的计时采用时间戳制。')", []).unwrap();
        conn
    }

    fn texts(hits: &[Value]) -> Vec<String> {
        hits.iter()
            .map(|h| h["text"].as_str().unwrap_or("").to_string())
            .collect()
    }

    #[test]
    fn tokenize_splits_cjk_and_latin() {
        let (and, _) = tokenize("数组 链表");
        assert_eq!(and, vec!["数组".to_string(), "链表".to_string()]);
        let (and, _) = tokenize("what is a linked list");
        assert_eq!(
            and,
            vec!["what".to_string(), "is".to_string(), "linked".to_string(), "list".to_string()]
        );
        // long CJK run: no AND terms, only bigrams in the scored set
        let (and, score) = tokenize("对比矩阵和学习指南是什么？");
        assert!(and.is_empty());
        for g in ["对比", "矩阵", "学习", "指南"] {
            assert!(score.contains(&g.to_string()), "missing bigram {g}");
        }
    }

    #[test]
    fn retrieve_hits_chinese_sentence_without_spaces() {
        let conn = test_db();
        let hits = retrieve(&conn, "对比矩阵和学习指南是什么？", 8);
        let ts = texts(&hits);
        assert_eq!(ts.len(), 1);
        assert!(ts[0].contains("对比矩阵"));
    }

    #[test]
    fn retrieve_and_match_still_works() {
        let conn = test_db();
        assert_eq!(texts(&retrieve(&conn, "对比矩阵 学习指南", 8)).len(), 1);
        let hits = retrieve(&conn, "linked list array", 8);
        let ts = texts(&hits);
        assert_eq!(ts.len(), 1);
        assert!(ts[0].contains("linked list"));
    }

    #[test]
    fn retrieve_unrelated_query_is_empty() {
        let conn = test_db();
        assert!(retrieve(&conn, "量子纠缠 薛定谔方程", 8).is_empty());
    }

    #[test]
    fn parse_query_decodes_percent_and_plus() {
        let q = parse_query("subject_id=abc%20def&name=%E7%BA%BF%E4%BB%A3+x");
        assert_eq!(q.get("subject_id").unwrap(), "abc def");
        assert_eq!(q.get("name").unwrap(), "线代 x");
        assert!(q.get("missing").is_none());
        // Malformed escapes pass through instead of panicking.
        let q = parse_query("a=%zz&b=%4");
        assert_eq!(q.get("a").unwrap(), "%zz");
        assert_eq!(q.get("b").unwrap(), "%4");
    }
}
