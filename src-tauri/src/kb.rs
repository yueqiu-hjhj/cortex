//! External knowledge bases — the KBP-style "外挂大脑".
//!
//! Reviva lets users bolt an external KB onto the agent; Cortex gets the same
//! capability over three transports without inventing a new protocol surface:
//!
//! - **dify** — Dify dataset retrieve API (bearer dataset key, top_k, scores)
//! - **fastgpt** — FastGPT `searchTest` API (account key, q/a chunks + source file)
//! - **mcp** — any MCP tool the user already registered; its args template
//!   carries `{query}` placeholders that get substituted per call, so a
//!   knowledge/memory server becomes a retriever without code changes
//!
//! Every connection stores a *description protocol*: one sentence on what the
//! KB holds and when it should be consulted. That text (not the KB itself)
//! is what the agent sees in its prompt — the KB stays dumb storage, the
//! description does the routing. Keys are stored encrypted via `secrets`.

use crate::error::{Error, Result};
use crate::repo;
use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::Value;
use tauri::Manager;

pub const KINDS: &[&str] = &["dify", "fastgpt", "mcp"];
const HTTP_TIMEOUT_SECS: u64 = 30;
const MAX_CONTENT: usize = 8000;
const DEFAULT_TOP_K: usize = 6;

#[derive(Debug, Clone, Serialize)]
pub struct KbConn {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub dataset_id: String,
    pub args_json: String,
    pub description: String,
    pub enabled: bool,
    pub created_at: i64,
    /// Whether an API key is on file (never returns the key itself).
    pub has_key: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct KbHit {
    /// Which KB produced the hit (its display name).
    pub kb: String,
    /// Source label (file name / dataset / tool).
    pub source: String,
    pub content: String,
    pub score: f64,
}

/// One KB's answer inside a merged search — failures stay isolated per KB.
#[derive(Debug, Clone, Serialize)]
pub struct KbSearchReport {
    pub name: String,
    pub kind: String,
    pub hits: Vec<KbHit>,
    pub error: Option<String>,
}

fn row_to_conn(r: &rusqlite::Row) -> rusqlite::Result<KbConn> {
    let id: String = r.get(0)?;
    Ok(KbConn {
        has_key: false, // filled in below
        id: id.clone(),
        name: r.get(1)?,
        kind: r.get(2)?,
        base_url: r.get(3)?,
        dataset_id: r.get(4)?,
        args_json: r.get(5)?,
        description: r.get(6)?,
        enabled: r.get::<_, i64>(7)? != 0,
        created_at: r.get(8)?,
    })
}

const COLS: &str =
    "id, name, kind, base_url, dataset_id, args_json, description, enabled, created_at";

fn list_inner(c: &Connection, only_enabled: bool) -> Result<Vec<KbConn>> {
    let sql = if only_enabled {
        format!("SELECT {COLS} FROM kb_connections WHERE enabled=1 ORDER BY created_at")
    } else {
        format!("SELECT {COLS} FROM kb_connections ORDER BY created_at")
    };
    let mut out = Vec::new();
    let mut st = c.prepare(&sql)?;
    let mut rows = st.query([])?;
    while let Some(row) = rows.next()? {
        let mut conn = row_to_conn(row)?;
        conn.has_key = crate::secrets::get_secret(c, &secret_key(&conn.id))?.is_some();
        out.push(conn);
    }
    Ok(out)
}

fn get_inner(c: &Connection, id: &str) -> Result<KbConn> {
    let sql = format!("SELECT {COLS} FROM kb_connections WHERE id=?1");
    let mut conn = c
        .query_row(&sql, [id], row_to_conn)
        .map_err(|_| Error::Other("知识库连接不存在".into()))?;
    conn.has_key = crate::secrets::get_secret(c, &secret_key(id))?.is_some();
    Ok(conn)
}

fn secret_key(id: &str) -> String {
    format!("kb_{id}")
}

fn validate_kind(kind: &str) -> Result<()> {
    if !KINDS.contains(&kind) {
        return Err(Error::Other(format!(
            "未知知识库类型（应为 {}）",
            KINDS.join("/")
        )));
    }
    Ok(())
}

// ---- retrieval ---------------------------------------------------------------

/// Dify: `{base}/v1/datasets/{id}/retrieve` (base may already end in /v1).
fn dify_url(base: &str, dataset_id: &str) -> String {
    let b = base.trim_end_matches('/');
    if b.ends_with("/v1") {
        format!("{b}/datasets/{dataset_id}/retrieve")
    } else {
        format!("{b}/v1/datasets/{dataset_id}/retrieve")
    }
}

/// Parse a Dify retrieve response body into hits. Split from HTTP so it is
/// unit-testable without a server.
fn parse_dify(body: &str, kb_name: &str) -> Vec<KbHit> {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(records) = v.get("records").and_then(Value::as_array) {
        for rec in records {
            let content = rec
                .pointer("/segment/content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if content.is_empty() {
                continue;
            }
            let source = rec
                .pointer("/segment/document/name")
                .and_then(Value::as_str)
                .unwrap_or(kb_name);
            out.push(KbHit {
                kb: kb_name.to_string(),
                source: source.to_string(),
                content: trunc_content(content),
                score: rec.get("score").and_then(Value::as_f64).unwrap_or(0.0),
            });
        }
    }
    out
}

/// FastGPT: `{base}/api/core/dataset/searchTest`.
fn fastgpt_url(base: &str) -> String {
    format!("{}/api/core/dataset/searchTest", base.trim_end_matches('/'))
}

fn parse_fastgpt(body: &str, kb_name: &str) -> Vec<KbHit> {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(items) = v.get("data").and_then(Value::as_array) {
        for it in items {
            let q = it.get("q").and_then(Value::as_str).unwrap_or("").trim();
            let a = it.get("a").and_then(Value::as_str).unwrap_or("").trim();
            let mut content = q.to_string();
            if !a.is_empty() {
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(a);
            }
            if content.trim().is_empty() {
                continue;
            }
            out.push(KbHit {
                kb: kb_name.to_string(),
                source: it
                    .get("sourceName")
                    .and_then(Value::as_str)
                    .unwrap_or(kb_name)
                    .to_string(),
                content: trunc_content(&content),
                score: it.get("score").and_then(Value::as_f64).unwrap_or(0.0),
            });
        }
    }
    out
}

/// Substitute `{query}` placeholders in an MCP args template (recursively, so
/// nested objects/arrays work; re-serializing the Value keeps it valid JSON).
fn apply_args_template(template: &str, query: &str) -> Value {
    fn walk(v: &mut Value, q: &str) {
        match v {
            Value::String(s) => {
                if s.contains("{query}") {
                    *s = s.replace("{query}", q);
                }
            }
            Value::Array(items) => {
                for it in items {
                    walk(it, q);
                }
            }
            Value::Object(map) => {
                for (_, val) in map.iter_mut() {
                    walk(val, q);
                }
            }
            _ => {}
        }
    }
    let parsed = serde_json::from_str::<Value>(template.trim())
        .unwrap_or_else(|_| serde_json::json!({ "query": "{query}" }));
    let mut v = parsed;
    walk(&mut v, query);
    v
}

fn trunc_content(s: &str) -> String {
    if s.len() <= MAX_CONTENT {
        return s.to_string();
    }
    s.chars().take(MAX_CONTENT).collect::<String>() + "…"
}

fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .unwrap_or_default()
}

/// Query one KB. MCP kind spawns the server session on demand.
fn fetch_one(
    c: &Connection,
    conn: &KbConn,
    query: &str,
    top_k: usize,
) -> Result<Vec<KbHit>> {
    match conn.kind.as_str() {
        "dify" => {
            let key = crate::secrets::get_secret(c, &secret_key(&conn.id))?
                .ok_or_else(|| Error::Other("该知识库还没有配置 API key".into()))?;
            let body = serde_json::json!({
                "query": query,
                "retrieval_model": {
                    "top_k": top_k,
                    "score_threshold_enabled": false,
                },
            });
            let resp = http_client()
                .post(dify_url(&conn.base_url, &conn.dataset_id))
                .bearer_auth(&key)
                .json(&body)
                .send()
                .map_err(|e| Error::Other(format!("Dify 请求失败：{e}")))?;
            let status = resp.status();
            let text = resp.text().map_err(|e| Error::Other(e.to_string()))?;
            if !status.is_success() {
                return Err(Error::Other(format!(
                    "Dify 返回 {status}：{}",
                    trunc_content(&text)
                )));
            }
            Ok(parse_dify(&text, &conn.name))
        }
        "fastgpt" => {
            let key = crate::secrets::get_secret(c, &secret_key(&conn.id))?
                .ok_or_else(|| Error::Other("该知识库还没有配置 API key".into()))?;
            let body = serde_json::json!({
                "datasetId": conn.dataset_id,
                "text": query,
                "limit": 4000,
                "similarity": 0,
                "searchMode": "embedding",
            });
            let resp = http_client()
                .post(fastgpt_url(&conn.base_url))
                .bearer_auth(&key)
                .json(&body)
                .send()
                .map_err(|e| Error::Other(format!("FastGPT 请求失败：{e}")))?;
            let status = resp.status();
            let text = resp.text().map_err(|e| Error::Other(e.to_string()))?;
            if !status.is_success() {
                return Err(Error::Other(format!(
                    "FastGPT 返回 {status}：{}",
                    trunc_content(&text)
                )));
            }
            Ok(parse_fastgpt(&text, &conn.name))
        }
        "mcp" => {
            let (server, tool) = crate::mcp_client::split_mcp_tool_name(&conn.dataset_id)
                .ok_or_else(|| Error::Other("MCP 工具名应为 mcp__<server>__<tool>".into()))?;
            let cfgs = crate::mcp_client::parse_server_cfgs(
                repo::get_setting(c, "mcp_servers")?.as_deref(),
            );
            let cfg = cfgs
                .into_iter()
                .find(|s| s.name == server && s.enabled)
                .ok_or_else(|| Error::Other(format!("没有名为 {server} 的已启用 MCP 服务器")))?;
            let mut session = crate::mcp_client::McpSession::spawn(&cfg)
                .map_err(|e| Error::Other(format!("MCP 服务器启动失败：{e}")))?;
            let args = apply_args_template(&conn.args_json, query);
            let text = session
                .call_tool(&tool, args)
                .map_err(|e| Error::Other(format!("MCP 工具调用失败：{e}")))?;
            Ok(vec![KbHit {
                kb: conn.name.clone(),
                source: tool,
                content: trunc_content(&text),
                score: 1.0,
            }])
        }
        other => Err(Error::Other(format!("未知知识库类型 {other}"))),
    }
}

/// Description protocol for prompts: one line per enabled KB so a model knows
/// what exists and when to reach for it. Empty string when nothing is enabled.
pub fn agent_context(c: &Connection) -> String {
    let conns = match list_inner(c, true) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    if conns.is_empty() {
        return String::new();
    }
    let mut out = String::from("可查询的外部知识库（kb_search_all 一次性检索，或按名字单查）：\n");
    for conn in conns {
        let desc = conn.description.trim();
        let desc = if desc.is_empty() { "（未填写用途说明）" } else { desc };
        out.push_str(&format!("- {}[{}]：{}\n", conn.name, conn.kind, desc));
    }
    out
}

// ---- commands -------------------------------------------------------------------

#[tauri::command]
pub fn kb_list(state: tauri::State<'_, crate::AppState>) -> Result<Vec<KbConn>> {
    let c = state.db.lock().unwrap();
    list_inner(&c, false)
}

#[tauri::command]
pub fn kb_add(
    state: tauri::State<'_, crate::AppState>,
    name: String,
    kind: String,
    base_url: String,
    dataset_id: String,
    api_key: Option<String>,
    description: String,
    args_json: Option<String>,
) -> Result<KbConn> {
    validate_kind(&kind)?;
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(Error::Other("知识库名称不能为空".into()));
    }
    if kind == "mcp" && crate::mcp_client::split_mcp_tool_name(&dataset_id).is_none() {
        return Err(Error::Other("MCP 类型需要填工具名，形如 mcp__<server>__<tool>".into()));
    }
    let c = state.db.lock().unwrap();
    let id = crate::db::new_id();
    c.execute(
        "INSERT INTO kb_connections (id, name, kind, base_url, dataset_id, args_json, description, enabled, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8)",
        params![
            id,
            name,
            kind,
            base_url.trim(),
            dataset_id.trim(),
            args_json.unwrap_or_default(),
            description.trim(),
            crate::db::now_ms(),
        ],
    )
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            Error::Other("同名知识库已存在".into())
        } else {
            Error::Other(e.to_string())
        }
    })?;
    if let Some(key) = api_key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty()) {
        crate::secrets::set_secret(&c, &secret_key(&id), &key)?;
    }
    drop(c);
    let c = state.db.lock().unwrap();
    get_inner(&c, &id)
}

#[tauri::command]
pub fn kb_update(
    state: tauri::State<'_, crate::AppState>,
    id: String,
    name: String,
    base_url: String,
    dataset_id: String,
    description: String,
    args_json: Option<String>,
    api_key: Option<String>,
) -> Result<()> {
    let c = state.db.lock().unwrap();
    let conn = get_inner(&c, &id)?;
    if conn.kind == "mcp" && crate::mcp_client::split_mcp_tool_name(&dataset_id).is_none() {
        return Err(Error::Other("MCP 类型需要填工具名，形如 mcp__<server>__<tool>".into()));
    }
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(Error::Other("知识库名称不能为空".into()));
    }
    let args = args_json.unwrap_or_else(|| conn.args_json.clone());
    c.execute(
        "UPDATE kb_connections SET name=?1, base_url=?2, dataset_id=?3, description=?4, args_json=?5 WHERE id=?6",
        params![name, base_url.trim(), dataset_id.trim(), description.trim(), args, id],
    )
    .map_err(|e| Error::Other(e.to_string()))?;
    // api_key semantics: None = keep, Some("") = clear, Some(text) = replace
    if let Some(key) = api_key {
        if key.trim().is_empty() {
            crate::secrets::delete_secret(&c, &secret_key(&id))?;
        } else {
            crate::secrets::set_secret(&c, &secret_key(&id), key.trim())?;
        }
    }
    Ok(())
}

#[tauri::command]
pub fn kb_set_enabled(
    state: tauri::State<'_, crate::AppState>,
    id: String,
    enabled: bool,
) -> Result<()> {
    let c = state.db.lock().unwrap();
    c.execute(
        "UPDATE kb_connections SET enabled=?1 WHERE id=?2",
        params![enabled as i64, id],
    )
    .map_err(|e| Error::Other(e.to_string()))?;
    Ok(())
}

#[tauri::command]
pub fn kb_remove(state: tauri::State<'_, crate::AppState>, id: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    c.execute("DELETE FROM kb_connections WHERE id=?1", [id.as_str()])
        .map_err(|e| Error::Other(e.to_string()))?;
    crate::secrets::delete_secret(&c, &secret_key(&id))?;
    Ok(())
}

#[tauri::command]
pub async fn kb_query(
    app: tauri::AppHandle,
    id: String,
    query: String,
    top_k: Option<usize>,
) -> Result<Vec<KbHit>> {
    let top_k = top_k.unwrap_or(DEFAULT_TOP_K).clamp(1, 20);
    let conn = {
        let st = app.state::<crate::AppState>();
        let c = st.db.lock().unwrap();
        get_inner(&c, &id)?
    };
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<crate::AppState>();
        let c = st.db.lock().unwrap();
        fetch_one(&c, &conn, &query, top_k)
    })
    .await
    .map_err(|e| Error::Other(e.to_string()))?
}

/// Query every enabled KB; each KB's failure is reported, never aborts the rest.
#[tauri::command]
pub async fn kb_search_all(
    app: tauri::AppHandle,
    query: String,
    top_k: Option<usize>,
) -> Result<Vec<KbSearchReport>> {
    let top_k = top_k.unwrap_or(DEFAULT_TOP_K).clamp(1, 20);
    let conns = {
        let st = app.state::<crate::AppState>();
        let c = st.db.lock().unwrap();
        list_inner(&c, true)?
    };
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<crate::AppState>();
        let c = st.db.lock().unwrap();
        let mut out = Vec::new();
        for conn in conns {
            match fetch_one(&c, &conn, &query, top_k) {
                Ok(hits) => out.push(KbSearchReport {
                    name: conn.name,
                    kind: conn.kind,
                    hits,
                    error: None,
                }),
                Err(e) => out.push(KbSearchReport {
                    name: conn.name,
                    kind: conn.kind,
                    hits: Vec::new(),
                    error: Some(e.to_string()),
                }),
            }
        }
        Ok(out)
    })
    .await
    .map_err(|e| Error::Other(e.to_string()))?
}

/// The description protocol as prompt text (for agents / tutor injection).
#[tauri::command]
pub fn kb_context(state: tauri::State<'_, crate::AppState>) -> Result<String> {
    let c = state.db.lock().unwrap();
    Ok(agent_context(&c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::new_id;

    fn add_conn(c: &Connection, name: &str, kind: &str, dataset: &str) -> String {
        let id = new_id();
        c.execute(
            "INSERT INTO kb_connections (id, name, kind, base_url, dataset_id, args_json, description, enabled, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, '', '测试', 1, 1)",
            params![id, name, kind, "https://x.example", dataset],
        )
        .unwrap();
        id
    }

    #[test]
    fn add_list_remove_roundtrip_and_secret() {
        let st = crate::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let id = new_id();
        c.execute(
            "INSERT INTO kb_connections (id, name, kind, base_url, dataset_id, args_json, description, enabled, created_at)
             VALUES (?1, 'Dify 主库', 'dify', 'https://dify.example', 'ds-1', '', '课程资料', 1, 1)",
            params![id],
        )
        .unwrap();
        // Plant a *plaintext* secret row (flags=0, the non-DPAPI path). Calling
        // platform::encrypt here would hit DPAPI, which sandboxed/test runs
        // can block on; the flags=0 branch exercises the same store plumbing.
        c.execute(
            "INSERT INTO secrets (key, ciphertext, flags, updated_at) VALUES (?1, ?2, 0, 1)",
            params![secret_key(&id), b"dataset-abc".to_vec()],
        )
        .unwrap();

        let conns = list_inner(&c, false).unwrap();
        assert_eq!(conns.len(), 1);
        assert_eq!(conns[0].name, "Dify 主库");
        assert!(conns[0].has_key);
        // enabled-only filter
        assert!(list_inner(&c, true).unwrap().len() == 1);
        // key must never leak through the struct
        assert!(serde_json::to_string(&conns).unwrap().find("dataset-abc").is_none());

        // remove also deletes the secret
        c.execute("DELETE FROM kb_connections WHERE id=?1", [&id]).unwrap();
        crate::secrets::delete_secret(&c, &secret_key(&id)).unwrap();
        assert!(list_inner(&c, false).unwrap().is_empty());
        assert!(crate::secrets::get_secret(&c, &secret_key(&id)).unwrap().is_none());
    }

    #[test]
    fn kind_validation_rejects_unknown() {
        assert!(validate_kind("dify").is_ok());
        assert!(validate_kind("fastgpt").is_ok());
        assert!(validate_kind("mcp").is_ok());
        assert!(validate_kind("notion").is_err());
    }

    #[test]
    fn dify_url_handles_v1_suffix() {
        assert_eq!(
            dify_url("https://api.dify.ai/v1", "ds1"),
            "https://api.dify.ai/v1/datasets/ds1/retrieve"
        );
        assert_eq!(
            dify_url("https://api.dify.ai", "ds1"),
            "https://api.dify.ai/v1/datasets/ds1/retrieve"
        );
        assert_eq!(
            dify_url("https://api.dify.ai/v1/", "ds1"),
            "https://api.dify.ai/v1/datasets/ds1/retrieve"
        );
    }

    #[test]
    fn fastgpt_url_is_search_test() {
        assert_eq!(
            fastgpt_url("http://localhost:3000/"),
            "http://localhost:3000/api/core/dataset/searchTest"
        );
    }

    #[test]
    fn parse_dify_extracts_segments_and_scores() {
        let body = r#"{"query":{"content":"q"},"records":[
            {"score":0.87,"segment":{"content":"段落A","document":{"name":"课件.pdf"}}},
            {"score":0.42,"segment":{"content":""}},
            {"score":0.5,"segment":{"content":"段落B"}}
        ]}"#;
        let hits = parse_dify(body, "主库");
        assert_eq!(hits.len(), 2); // empty content dropped
        assert_eq!(hits[0].source, "课件.pdf");
        assert_eq!(hits[0].content, "段落A");
        assert!((hits[0].score - 0.87).abs() < 1e-9);
        assert_eq!(hits[1].source, "主库"); // fallback to KB name
        assert_eq!(hits[1].kb, "主库");
    }

    #[test]
    fn parse_fastgpt_joins_q_and_a() {
        let body = r#"{"code":200,"data":[
            {"id":"1","q":"问题","a":"","sourceName":"讲义.pdf","score":0.8},
            {"id":"2","q":"问题2","a":"答案2","sourceName":"讲义.pdf","score":0.6}
        ]}"#;
        let hits = parse_fastgpt(body, "FastGPT");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].content, "问题");
        assert_eq!(hits[1].content, "问题2\n答案2");
        assert_eq!(hits[0].source, "讲义.pdf");
    }

    #[test]
    fn args_template_substitutes_nested() {
        let v = apply_args_template(
            r#"{"query":"{query}","filter":{"topic":"{query}","limit":5}}"#,
            "什么是熵",
        );
        assert_eq!(v["query"], "什么是熵");
        assert_eq!(v["filter"]["topic"], "什么是熵");
        assert_eq!(v["filter"]["limit"], 5);
        // garbage template falls back to a sane default
        let v2 = apply_args_template("not json", "q1");
        assert_eq!(v2["query"], "q1");
    }

    #[test]
    fn agent_context_lists_enabled_only() {
        let st = crate::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let a = add_conn(&c, "库A", "dify", "ds-a");
        let b = add_conn(&c, "库B", "fastgpt", "ds-b");
        c.execute("UPDATE kb_connections SET enabled=0 WHERE id=?1", [&b]).unwrap();
        let ctx = agent_context(&c);
        assert!(ctx.contains("库A[dify]"));
        assert!(!ctx.contains("库B"));
        // disabled everything → empty block
        c.execute("UPDATE kb_connections SET enabled=0 WHERE id=?1", [&a]).unwrap();
        assert!(agent_context(&c).is_empty());
    }

    #[test]
    fn fetch_one_errors_cleanly_for_missing_mcp_server() {
        let st = crate::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let id = add_conn(&c, "MCP库", "mcp", "mcp__nosuch__search");
        let conn = get_inner(&c, &id).unwrap();
        let err = fetch_one(&c, &conn, "q", 5).unwrap_err().to_string();
        assert!(err.contains("nosuch"));
    }
}
