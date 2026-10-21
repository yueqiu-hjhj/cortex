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
        PathBuf::from(base).join("app.study.cortex")
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
        },
        {
            "name": "list_papers",
            "description": "List the user's paper library (Zotero imports and saved search results). Every row carries a stable cite_key: quote that key when citing, and never invent one.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["to_read", "reading", "read", "cited", "dropped", "all"]
                    },
                    "notebook_id": { "type": "string", "description": "Only papers filed under this subject" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 }
                }
            }
        },
        {
            "name": "get_paper",
            "description": "One paper in full: metadata, abstract, the user's own notes, and every excerpt taken from it with page numbers. Call this before writing anything that cites the paper — excerpts are the citable evidence, not the abstract.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "paper_id": { "type": "string" },
                    "cite_key": {
                        "type": "string",
                        "description": "Alternative to paper_id, e.g. Vaswani2017Attention"
                    }
                }
            }
        },
        {
            "name": "search_papers",
            "description": "Search the paper library across title, authors, abstract, personal notes and excerpt text. Returns paper_id and cite_key for each hit.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 20 }
                },
                "required": ["query"]
            }
        },
        {
            "name": "list_excerpts",
            "description": "Quoted passages with page numbers, across the whole library or from one paper. Each excerpt links back to its paper, so a claim can always be traced to a page.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "paper_id": { "type": "string" },
                    "cite_key": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 }
                }
            }
        },
        {
            "name": "get_note",
            "description": "Read one note in full: Markdown body, vault file path, outgoing [[wiki links]] and backlinks. Notes live as .md files, so the same content is readable directly from disk.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "note_id": { "type": "string" },
                    "title": { "type": "string", "description": "Exact title, when the id is not known" }
                }
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
        // Character offsets (when the chunk has them) — an external agent can
        // slice-verify: the passage IS source_text[char_start..char_end].
        let span = match (h.char_start, h.char_end) {
            (Some(a), Some(b)) => format!(" chars {a}-{b}"),
            _ => String::new(),
        };
        out.push_str(&format!(
            "[{}] {} {}{} (source_id={}, chunk_id={})\n{}\n\n",
            i + 1,
            h.source_name,
            loc,
            span,
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
        "list_papers" => list_papers(conn, args),
        "get_paper" => get_paper(conn, args),
        "search_papers" => search_papers(conn, args),
        "list_excerpts" => list_excerpts(conn, args),
        "get_note" => get_note(conn, args),
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

// ---- paper library ----------------------------------------------------
//
// Papers are the half of a research library that a generic file search cannot
// serve: what matters is not just the PDF text but which passage on which page
// was singled out as evidence. Every returned row therefore carries the stable
// cite_key, so anything an agent writes can be traced back to a page.

fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let cut: String = flat.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// The key to show for a paper: the persisted one, or the proposal for a row
/// that predates 0059 and has not been backfilled yet.
fn paper_key(p: &crate::papers::Paper) -> String {
    p.cite_key
        .clone()
        .unwrap_or_else(|| crate::papers::derive_cite_key(p))
}

fn resolve_paper(conn: &Connection, args: &Value) -> Result<crate::papers::Paper> {
    if let Some(id) = args
        .get("paper_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        return crate::papers::get_paper(conn, id);
    }
    let key = args
        .get("cite_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let Some(key) = key else {
        return Err(Error::Other("paper_id or cite_key is required".into()));
    };
    let id: String = conn
        .query_row("SELECT id FROM papers WHERE cite_key = ?1", [key], |r| {
            r.get(0)
        })
        .map_err(|_| Error::Other(format!("no paper with cite_key {key}")))?;
    crate::papers::get_paper(conn, &id)
}

fn list_papers(conn: &Connection, args: &Value) -> Result<String> {
    let status = args
        .get("status")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && *s != "all");
    let subject = args
        .get("notebook_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(50)
        .clamp(1, 200) as usize;
    let papers = crate::papers::list_papers(conn, status, subject, None)?;
    if papers.is_empty() {
        return Ok("No papers in the library yet — import from Zotero or save a \
                   literature-search hit in the Cortex app first."
            .into());
    }
    let total = papers.len();
    let mut out = String::new();
    for (i, p) in papers.iter().take(limit).enumerate() {
        let venue = p
            .venue
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .map(|v| format!("{} · ", one_line(v, 40)))
            .unwrap_or_default();
        out.push_str(&format!(
            "[{}] {} | {} ({}) | {}\n    {venue}status={} · {} excerpt(s) · paper_id={}\n",
            i + 1,
            paper_key(p),
            one_line(p.authors.as_deref().unwrap_or("unknown authors"), 40),
            p.year.map(|y| y.to_string()).unwrap_or_else(|| "n.d.".into()),
            one_line(&p.title, 110),
            p.status,
            p.excerpt_count,
            p.id,
        ));
    }
    if total > limit {
        out.push_str(&format!("\n({total} papers total; showing {limit})\n"));
    }
    Ok(out.trim_end().to_string())
}

fn get_paper(conn: &Connection, args: &Value) -> Result<String> {
    let p = resolve_paper(conn, args)?;
    let mut out = String::new();
    out.push_str(&format!(
        "title: {}\ncite_key: {}\npaper_id: {}\n",
        p.title,
        paper_key(&p),
        p.id
    ));
    if let Some(a) = &p.authors {
        out.push_str(&format!("authors: {a}\n"));
    }
    if let Some(y) = p.year {
        out.push_str(&format!("year: {y}\n"));
    }
    if let Some(v) = &p.venue {
        out.push_str(&format!("venue: {v}\n"));
    }
    if let Some(d) = &p.doi {
        out.push_str(&format!("doi: {d}\n"));
    }
    if let Some(u) = &p.url {
        out.push_str(&format!("url: {u}\n"));
    }
    out.push_str(&format!(
        "status: {} | rating: {} | excerpts: {}\n",
        p.status,
        p.rating.map(|r| r.to_string()).unwrap_or_else(|| "-".into()),
        p.excerpt_count
    ));
    if let Some(n) = p.notes.as_deref().filter(|n| !n.trim().is_empty()) {
        out.push_str(&format!("\nmy notes:\n{}\n", n.trim()));
    }
    if let Some(a) = p
        .abstract_text
        .as_deref()
        .filter(|a| !a.trim().is_empty())
    {
        out.push_str(&format!("\nabstract:\n{}\n", a.trim()));
    }
    let excerpts = excerpt_block(conn, Some(&p.id), 200)?;
    if !excerpts.is_empty() {
        out.push_str("\nexcerpts (quote these, and keep the page):\n");
        out.push_str(&excerpts);
    }
    Ok(out.trim_end().to_string())
}

fn excerpt_block(conn: &Connection, paper_id: Option<&str>, limit: usize) -> Result<String> {
    let mut st = conn.prepare(
        "SELECT e.quote, e.page, COALESCE(e.note,''), COALESCE(p.cite_key,''), COALESCE(p.title,'') \
         FROM excerpts e LEFT JOIN papers p ON p.id = e.paper_id \
         WHERE (?1 IS NULL OR e.paper_id = ?1) \
         ORDER BY e.created_at DESC LIMIT ?2",
    )?;
    let rows: Vec<String> = st
        .query_map(rusqlite::params![paper_id, limit as i64], |r| {
            let quote: String = r.get(0)?;
            let page: Option<i64> = r.get(1)?;
            let note: String = r.get(2)?;
            let key: String = r.get(3)?;
            let title: String = r.get(4)?;
            let loc = page
                .map(|p| format!("p.{p}"))
                .unwrap_or_else(|| "p.?".to_string());
            let annotation = if note.trim().is_empty() {
                String::new()
            } else {
                format!(" — my note: {}", one_line(&note, 120))
            };
            Ok(format!(
                "- [{}] {} 「{}」{}\n  from: {}\n",
                key,
                loc,
                one_line(quote.trim(), 400),
                annotation,
                one_line(&title, 90)
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows.join(""))
}

fn search_papers(conn: &Connection, args: &Value) -> Result<String> {
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.trim().is_empty() {
        return Err(Error::Other("query is required".into()));
    }
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50) as i64;
    let pattern = format!(
        "%{}%",
        query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let mut st = conn.prepare(
        "SELECT COALESCE(p.cite_key,''), p.title, COALESCE(p.authors,''), p.year, p.id, p.status, \
            (SELECT COUNT(*) FROM excerpts e WHERE e.paper_id = p.id) \
         FROM papers p \
         WHERE p.title LIKE ?1 ESCAPE '\\' \
            OR p.authors LIKE ?1 ESCAPE '\\' \
            OR p.abstract LIKE ?1 ESCAPE '\\' \
            OR p.notes LIKE ?1 ESCAPE '\\' \
            OR EXISTS (SELECT 1 FROM excerpts e2 WHERE e2.paper_id = p.id AND e2.quote LIKE ?1 ESCAPE '\\') \
         ORDER BY p.year DESC, p.updated_at DESC LIMIT ?2",
    )?;
    let rows: Vec<String> = st
        .query_map(rusqlite::params![pattern, limit], |r| {
            let key: String = r.get(0)?;
            let title: String = r.get(1)?;
            let authors: String = r.get(2)?;
            let year: Option<i64> = r.get(3)?;
            let id: String = r.get(4)?;
            let status: String = r.get(5)?;
            let n: i64 = r.get(6)?;
            Ok(format!(
                "- {} | {} ({}) | {}\n  status={} · {} excerpt(s) · paper_id={}",
                key,
                one_line(&authors, 40),
                year.map(|y| y.to_string()).unwrap_or_else(|| "n.d.".into()),
                one_line(&title, 110),
                status,
                n,
                id
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    if rows.is_empty() {
        return Ok(format!(
            "No paper matched \"{query}\" (title, authors, abstract, notes, excerpt text)."
        ));
    }
    Ok(rows.join("\n"))
}

fn list_excerpts(conn: &Connection, args: &Value) -> Result<String> {
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(50)
        .clamp(1, 200) as usize;
    let has_scope = args
        .get("paper_id")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
        || args
            .get("cite_key")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty());
    let paper_id = if has_scope {
        Some(resolve_paper(conn, args)?.id)
    } else {
        None
    };
    let block = excerpt_block(conn, paper_id.as_deref(), limit)?;
    if block.is_empty() {
        return Ok("No excerpts yet — highlight a passage in the PDF reader to create one.".into());
    }
    Ok(block.trim_end().to_string())
}

fn get_note(conn: &Connection, args: &Value) -> Result<String> {
    let note_id: String = if let Some(id) = args
        .get("note_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        id.to_string()
    } else if let Some(title) = args
        .get("title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        conn.query_row(
            "SELECT id FROM notes WHERE lower(trim(title)) = lower(trim(?1)) LIMIT 1",
            [title],
            |r| r.get(0),
        )
        .map_err(|_| Error::Other(format!("no note titled \"{title}\"")))?
    } else {
        return Err(Error::Other("note_id or title is required".into()));
    };

    let (title, body, vault_path, updated_at): (String, String, String, i64) = conn
        .query_row(
            "SELECT title, COALESCE(body,''), COALESCE(vault_path,''), updated_at \
             FROM notes WHERE id = ?1",
            [&note_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(|_| Error::Other(format!("note {note_id} not found")))?;

    let mut out = format!(
        "title: {title}\nnote_id: {note_id}\nupdated_at_ms: {updated_at}\n"
    );
    if vault_path.is_empty() {
        out.push_str("vault_path: (not on disk yet)\n");
    } else {
        out.push_str(&format!("vault_path: {vault_path}\n"));
    }

    let mut st = conn.prepare(
        "SELECT target_slug, COALESCE(alias,'') FROM note_links \
         WHERE from_note_id = ?1 ORDER BY target_slug",
    )?;
    let links: Vec<String> = st
        .query_map([&note_id], |r| {
            let target: String = r.get(0)?;
            let alias: String = r.get(1)?;
            Ok(if alias.is_empty() {
                format!("[[{target}]]")
            } else {
                format!("[[{target}|{alias}]]")
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(st);
    if !links.is_empty() {
        out.push_str(&format!("outgoing links: {}\n", links.join(", ")));
    }

    let backs = crate::note_links::backlinks(conn, &title)?;
    if !backs.is_empty() {
        let titles: Vec<String> = backs.iter().map(|b| b.title.clone()).collect();
        out.push_str(&format!("backlinks: {}\n", titles.join(", ")));
    }

    out.push_str(&format!("\n---\n{}\n", body.trim_end()));
    Ok(out)
}

// ---- resources --------------------------------------------------------
//
// Notes and papers are addressable by URI so a client can attach one directly
// (`@cortex://note/…`) instead of spending a tool call to discover it. Notes
// are .md files on disk, so the same content is also reachable by path — the URI
// is just a stable handle that survives a rename.

fn resource_list(conn: &Connection) -> Result<Value> {
    let mut out: Vec<Value> = Vec::new();

    let mut st = conn.prepare(
        "SELECT id, title, COALESCE(vault_path,'') FROM notes \
         ORDER BY updated_at DESC LIMIT 200",
    )?;
    let notes: Vec<(String, String, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(st);
    for (id, title, path) in notes {
        out.push(json!({
            "uri": format!("cortex://note/{id}"),
            "name": title,
            "description": if path.is_empty() {
                "note (not yet exported to disk)".to_string()
            } else {
                path
            },
            "mimeType": "text/markdown"
        }));
    }

    let mut st = conn.prepare(
        "SELECT cite_key, title, COALESCE(authors,''), year FROM papers \
         WHERE cite_key IS NOT NULL AND cite_key <> '' \
         ORDER BY updated_at DESC LIMIT 200",
    )?;
    let papers: Vec<(String, String, String, Option<i64>)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(st);
    for (key, title, authors, year) in papers {
        let who = if authors.is_empty() {
            "unknown authors".to_string()
        } else {
            one_line(&authors, 60)
        };
        out.push(json!({
            "uri": format!("cortex://paper/{key}"),
            "name": title,
            "description": format!(
                "{who} ({}) — metadata, notes and page-anchored excerpts",
                year.map(|y| y.to_string()).unwrap_or_else(|| "n.d.".into())
            ),
            "mimeType": "text/markdown"
        }));
    }

    Ok(Value::Array(out))
}

fn resource_read(conn: &Connection, uri: &str) -> Result<String> {
    if let Some(id) = uri.strip_prefix("cortex://note/") {
        return get_note(conn, &json!({ "note_id": id }));
    }
    if let Some(key) = uri.strip_prefix("cortex://paper/") {
        return get_paper(conn, &json!({ "cite_key": key }));
    }
    Err(Error::Other(format!("unsupported resource uri: {uri}")))
}

/// Handle one JSON-RPC message. Returns the response (None for notifications).
pub fn handle_message(conn: &Connection, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    // Notifications carry no id and expect no response.
    id.as_ref()?;
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
                "capabilities": {
                    "tools": { "listChanged": false },
                    "resources": { "listChanged": false, "subscribe": false }
                },
                "serverInfo": { "name": "cortex", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Cortex is the user's local study library, in three shelves. \
(1) notes — Markdown files with [[wiki links]]: search_notes, get_note. \
(2) sources — ingested PDFs and pages, addressable by character offset: search_notebook, get_source, read_document. \
(3) papers — a curated library where excerpts carry page numbers: list_papers, get_paper, search_papers, list_excerpts. \
When you cite, use the paper's cite_key and the excerpt's page — both are stable. \
Notes and papers are also exposed as resources (cortex://note/<id>, cortex://paper/<cite_key>). \
Read-only: no tool here writes to the library."
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
        "resources/list" => match resource_list(conn) {
            Ok(list) => ok(json!({ "resources": list })),
            Err(e) => err(-32603, &e.to_string()),
        },
        "resources/read" => {
            let uri = params.get("uri").and_then(|v| v.as_str()).unwrap_or("");
            match resource_read(conn, uri) {
                Ok(text) => ok(json!({
                    "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": text }]
                })),
                Err(e) => err(-32602, &e.to_string()),
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
                   VALUES ('n1', 's1', '特征值笔记', '相似变换保持特征值不变。', 1, 1);
                 INSERT INTO papers (id, title, authors, year, venue, abstract, status, notes, added_at, updated_at)
                   VALUES ('p1', 'Attention is all you need', 'Ashish Vaswani, Noam Shazeer',
                           2017, 'NeurIPS', 'The dominant sequence transduction models are based on RNNs.',
                           'read', '核心贡献是自注意力。', 1, 1);
                 INSERT INTO excerpts (id, paper_id, quote, page, note, created_at, updated_at)
                   VALUES ('e1', 'p1', 'We propose a new simple network architecture, the Transformer.',
                           3, '本文核心贡献', 1, 1);",
            )
            .unwrap();
            crate::papers::ensure_cite_keys(&c).unwrap();
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
    fn tools_list_advertises_the_full_read_surface() {
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
        assert_eq!(names.len(), 18, "tool surface should be exactly 18 read-only tools");
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
        // prompts are not advertised, so the method must stay unknown.
        let resp = handle_message(
            &c,
            &json!({ "jsonrpc": "2.0", "id": 3, "method": "prompts/list" }),
        )
        .unwrap();
        assert_eq!(resp["error"]["code"], -32601);
        drop(c);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn paper_tools_carry_cite_key_and_page() {
        let (c, dir) = seeded("papers");
        let list = call(&c, "list_papers", json!({}));
        let text = list["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Vaswani2017Attention"), "{text}");
        assert!(text.contains("NeurIPS"), "{text}");

        // Cited by key, not id: an agent that only saw the list can still reach
        // the page-anchored evidence.
        let one = call(&c, "get_paper", json!({ "cite_key": "Vaswani2017Attention" }));
        let text = one["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("ph.), p.3") || text.contains("p.3"), "{text}");
        assert!(text.contains("the Transformer"), "{text}");
        assert!(text.contains("核心贡献是自注意力"), "{text}");
        drop(c);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn search_papers_matches_excerpt_text_not_just_title() {
        let (c, dir) = seeded("psearch");
        let hit = call(&c, "search_papers", json!({ "query": "Transformer" }));
        let text = hit["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Vaswani2017Attention"), "{text}");

        let miss = call(&c, "search_papers", json!({ "query": "diffusion" }));
        let text = miss["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("No paper matched"), "{text}");
        drop(c);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn resources_expose_notes_and_papers() {
        let (c, dir) = seeded("res");
        let resp = handle_message(
            &c,
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "resources/list" }),
        )
        .unwrap();
        assert_eq!(resp["error"], Value::Null, "resources/list must succeed now");
        let list = resp["result"]["resources"].as_array().unwrap();
        assert!(list.iter().any(|r| r["uri"] == "cortex://note/n1"));
        assert!(list
            .iter()
            .any(|r| r["uri"] == "cortex://paper/Vaswani2017Attention"));

        let read = handle_message(
            &c,
            &json!({ "jsonrpc": "2.0", "id": 2, "method": "resources/read",
                     "params": { "uri": "cortex://note/n1" } }),
        )
        .unwrap();
        let text = read["result"]["contents"][0]["text"].as_str().unwrap();
        assert!(text.contains("相似变换"), "{text}");

        let bad = handle_message(
            &c,
            &json!({ "jsonrpc": "2.0", "id": 3, "method": "resources/read",
                     "params": { "uri": "cortex://nope/x" } }),
        )
        .unwrap();
        assert_eq!(bad["error"]["code"], -32602);
        drop(c);
        let _ = std::fs::remove_dir_all(dir);
    }
}
