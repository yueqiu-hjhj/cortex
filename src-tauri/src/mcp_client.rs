//! C2 (4.4): MCP client — Cortex talks TO external MCP servers (the mirror of
//! `mcp_server.rs`, which serves Cortex's own knowledge base out). Personal
//! edition: stdio transport only, newline-delimited JSON-RPC per the MCP spec.
//!
//! Flow per server: spawn the configured command → `initialize` → `initialized`
//! notification → `tools/list` / `tools/call` as needed → kill on drop. Each
//! session runs a reader thread pushing lines into a channel, so a chatty
//! server spamming notifications can't deadlock a blocking read.
//!
//! Tool exposure reuses the existing agent registry: synced tools land in
//! `agent_tools` named `mcp__<server>__<tool>`, so the Agents UI whitelist,
//! enable/disable toggles and per-agent grants all work unchanged. The
//! executor dispatches calls back here by that name prefix.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::Duration;
use tauri::Manager;

const PROTOCOL_VERSION: &str = "2024-11-05";
const INIT_TIMEOUT: Duration = Duration::from_secs(120); // npx cold start
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

// ---- configuration ------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpServerCfg {
    /// Server id used in tool names (`mcp__<name>__<tool>`): `[a-z0-9_-]+`.
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}
fn default_enabled() -> bool {
    true
}

/// Parse the `mcp_servers` setting (a JSON array of McpServerCfg). Tolerates
/// a missing/empty/garbage value — a broken config must never take the app
/// down, it just means no MCP tools.
pub fn parse_server_cfgs(raw: Option<&str>) -> Vec<McpServerCfg> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<McpServerCfg>>(raw).unwrap_or_default()
}

/// `mcp__<server>__<tool>` and its inverse. Server names are restricted to
/// `[a-z0-9_-]+` so the parse-back is unambiguous.
pub fn mcp_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
}

/// (server, original tool name) for a `mcp__…` registry name; None otherwise.
pub fn split_mcp_tool_name(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server.to_string(), tool.to_string()))
}

/// One-line usage for the system prompt, derived from the tool's input schema:
/// `mcp__fs__read_file {"path": string (required)}` — enough for the model to
/// call it right without dumping the full JSON Schema.
pub fn usage_for(name: &str, description: &str, input_schema: &Value) -> String {
    let mut sig = String::new();
    let required = input_schema
        .get("required")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(props) = input_schema.get("properties").and_then(Value::as_object) {
        let parts: Vec<String> = props
            .iter()
            .map(|(k, v)| {
                let ty = v.get("type").and_then(Value::as_str).unwrap_or("any");
                if required.iter().any(|r| r.as_str() == Some(k.as_str())) {
                    format!(r#""{k}": {ty}*"#)
                } else {
                    format!(r#""{k}": {ty}"#)
                }
            })
            .collect();
        sig = format!("{{{}}}", parts.join(", "));
    }
    let desc = description.split('\n').next().unwrap_or("").trim();
    if sig.is_empty() {
        format!("{name} {{}} — {desc}")
    } else {
        format!("{name} {sig} — {desc}")
    }
}

// ---- session ------------------------------------------------------------

pub struct McpSession {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
    next_id: u64,
}

impl Drop for McpSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl McpSession {
    /// Spawn the server process and complete the MCP handshake.
    pub fn spawn(cfg: &McpServerCfg) -> Result<Self> {
        if cfg.name.is_empty() || !cfg.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
            return Err(Error::Other(format!(
                "MCP server name {:?} 无效：只能用小写字母/数字/-/_",
                cfg.name
            )));
        }
        if cfg.command.trim().is_empty() {
            return Err(Error::Other(format!("MCP server {} 缺少 command", cfg.name)));
        }
        let mut child = crate::ingest::new_cmd(cfg.command.trim())
            .args(&cfg.args)
            .envs(&cfg.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Error::Other(format!("MCP server {} 启动失败（{}）：{e}", cfg.name, cfg.command)))?;

        let stdout = child.stdout.take().ok_or_else(|| Error::Other("MCP stdout 不可用".into()))?;
        let stdin = child.stdin.take().ok_or_else(|| Error::Other("MCP stdin 不可用".into()))?;
        let (tx, rx) = channel::<String>();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break; // session dropped
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let mut s = Self { child, stdin, rx, next_id: 1 };
        let _ = s.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "cortex", "version": "1.0" }
            }),
            INIT_TIMEOUT,
        )?;
        // The spec wants an `initialized` notification after the response.
        s.send_json(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))?;
        Ok(s)
    }

    fn send_json(&mut self, v: Value) -> Result<()> {
        let mut line = v.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|_| self.stdin.flush())
            .map_err(|e| Error::Other(format!("MCP 写入失败（进程可能已退出）：{e}")))
    }

    /// Send a request and wait for the response with the matching id, skipping
    /// any notifications/requests the server pushes in between (logs etc.).
    fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send_json(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let wait = deadline.saturating_duration_since(std::time::Instant::now());
            if wait.is_zero() {
                return Err(Error::Other(format!("MCP {method} 超时")));
            }
            let line = match self.rx.recv_timeout(wait) {
                Ok(l) => l,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(Error::Other(format!("MCP {method} 超时")));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Error::Other(format!("MCP 服务进程已退出（{method} 无响应）")));
                }
            };
            let v: Value = match serde_json::from_str(line.trim()) {
                Ok(v) => v,
                Err(_) => continue, // non-JSON line (banners) — skip
            };
            if v.get("id").and_then(Value::as_u64) != Some(id) {
                continue; // notification or stale response
            }
            if let Some(err) = v.get("error") {
                return Err(Error::Other(format!("MCP {method} 错误：{err}")));
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// `tools/list` → (name, description, inputSchema) triples.
    pub fn list_tools(&mut self) -> Result<Vec<(String, String, Value)>> {
        let result = self.request("tools/list", json!({}), INIT_TIMEOUT)?;
        let Some(tools) = result.get("tools").and_then(Value::as_array) else {
            return Ok(Vec::new());
        };
        Ok(tools
            .iter()
            .filter_map(|t| {
                let name = t.get("name")?.as_str()?.to_string();
                let desc = t.get("description").and_then(Value::as_str).unwrap_or("").to_string();
                let schema = t.get("inputSchema").cloned().unwrap_or_else(|| json!({ "type": "object" }));
                Some((name, desc, schema))
            })
            .collect())
    }

    /// `tools/call` → the text content of the result joined. `isError` results
    /// are surfaced as text too (the model reads them like any tool output).
    pub fn call_tool(&mut self, tool: &str, args: Value) -> Result<String> {
        let result = self.request("tools/call", json!({ "name": tool, "arguments": args }), CALL_TIMEOUT)?;
        let mut out = String::new();
        if let Some(content) = result.get("content").and_then(Value::as_array) {
            for item in content {
                match item.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = item.get("text").and_then(Value::as_str) {
                            if !out.is_empty() {
                                out.push('\n');
                            }
                            out.push_str(t);
                        }
                    }
                    Some(other) => {
                        if !out.is_empty() {
                            out.push('\n');
                        }
                        out.push_str(&format!("[{other} content omitted]"));
                    }
                    _ => {}
                }
            }
        }
        if out.is_empty() {
            out = serde_json::to_string(&result).unwrap_or_default();
        }
        Ok(out)
    }
}

// ---- registry sync ------------------------------------------------------

/// Connect to one server and list its tools — NO database involved, so the
/// caller can do the (slow, process-spawning) network phase without holding
/// the app's DB lock.
pub fn connect_and_list(cfg: &McpServerCfg) -> Result<Vec<(String, String, Value)>> {
    let mut session = McpSession::spawn(cfg)?;
    session.list_tools()
}

/// Upsert one server's tool list into `agent_tools` (the registry the Agents
/// UI and the whitelist read) and remove stale rows. All rows are `kind='mcp'`,
/// `built_in=0`, enabled — the per-agent grant still decides which agents may
/// call them. Call AFTER the network phase; this only touches SQLite.
pub fn upsert_server_tools(
    conn: &rusqlite::Connection,
    user_id: &str,
    server: &str,
    tools: &[(String, String, Value)],
) -> Result<usize> {
    let now = crate::db::now_ms();
    let mut synced = 0;
    let mut live: Vec<String> = Vec::new();
    for (tool, desc, schema) in tools {
        let name = mcp_tool_name(server, tool);
        live.push(name.clone());
        let usage = usage_for(&name, desc, schema);
        conn.execute(
            "INSERT INTO agent_tools (id, user_id, name, description, kind, schema_json, enabled, built_in, created_at)
             VALUES (?1,?2,?3,?4,'mcp',?5,1,0,?6)
             ON CONFLICT(user_id, name) DO UPDATE SET description=excluded.description, schema_json=excluded.schema_json",
            rusqlite::params![crate::db::new_id(), user_id, name, usage, schema.to_string(), now],
        )?;
        synced += 1;
    }
    // Stale: rows for this server that no longer exist upstream.
    let prefix = format!("mcp__{server}__");
    let stale: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT name FROM agent_tools WHERE user_id=?1 AND kind='mcp' AND name LIKE ?2",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![user_id, format!("{prefix}%")], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter().filter(|n| !live.contains(n)).collect()
    };
    for name in stale {
        conn.execute(
            "DELETE FROM agent_tools WHERE user_id=?1 AND name=?2",
            rusqlite::params![user_id, name],
        )?;
    }
    Ok(synced)
}

// ---- tauri commands -----------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct McpSyncReport {
    pub server: String,
    pub ok: bool,
    pub tools: usize,
    pub error: Option<String>,
}

/// Sync every enabled server: connect + tools/list (no DB lock), then one
/// short lock to upsert the registry. Per-server report so one dead server
/// doesn't hide the others.
#[tauri::command]
pub async fn mcp_sync_tools(app: tauri::AppHandle) -> Result<Vec<McpSyncReport>> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<crate::db::AppState>();
        let cfgs = {
            let c = state.db.lock().unwrap();
            parse_server_cfgs(crate::repo::get_setting(&c, "mcp_servers").ok().flatten().as_deref())
        };
        // Phase 1 — network, no lock held.
        let mut phase1: Vec<(McpServerCfg, Result<Vec<(String, String, Value)>>)> = Vec::new();
        for cfg in cfgs.iter().filter(|c| c.enabled) {
            phase1.push((cfg.clone(), connect_and_list(cfg)));
        }
        // Phase 2 — short lock, registry writes.
        let c = state.db.lock().unwrap();
        let mut reports = Vec::new();
        for (cfg, res) in phase1 {
            match res {
                Ok(tools) => {
                    let n = tools.len();
                    match upsert_server_tools(&c, crate::agents::DEFAULT_USER, &cfg.name, &tools) {
                        Ok(_) => reports.push(McpSyncReport { server: cfg.name, ok: true, tools: n, error: None }),
                        Err(e) => reports.push(McpSyncReport { server: cfg.name, ok: false, tools: 0, error: Some(e.to_string()) }),
                    }
                }
                Err(e) => reports.push(McpSyncReport { server: cfg.name, ok: false, tools: 0, error: Some(e.to_string()) }),
            }
        }
        Ok(reports)
    })
    .await
    .map_err(|e| Error::Other(format!("mcp sync task failed: {e}")))?
}

// ---- tests --------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_cfgs_parse_tolerates_garbage() {
        assert!(parse_server_cfgs(None).is_empty());
        assert!(parse_server_cfgs(Some("")).is_empty());
        assert!(parse_server_cfgs(Some("not json")).is_empty());
        let cfgs = parse_server_cfgs(Some(
            r#"[{"name":"fs","command":"npx","args":["-y","@modelcontextprotocol/server-filesystem","/data"],"env":{"X":"1"},"enabled":false}]"#,
        ));
        assert_eq!(cfgs.len(), 1);
        assert_eq!(cfgs[0].name, "fs");
        assert_eq!(cfgs[0].args.len(), 3);
        assert!(!cfgs[0].enabled);
        // enabled defaults to true
        let cfgs = parse_server_cfgs(Some(r#"[{"name":"fs","command":"npx"}]"#));
        assert!(cfgs[0].enabled);
    }

    #[test]
    fn tool_names_roundtrip_and_reject_ambiguity() {
        let n = mcp_tool_name("homelab", "query_db");
        assert_eq!(n, "mcp__homelab__query_db");
        assert_eq!(split_mcp_tool_name(&n), Some(("homelab".into(), "query_db".into())));
        // non-MCP names don't parse
        assert_eq!(split_mcp_tool_name("list_notebooks"), None);
        assert_eq!(split_mcp_tool_name("mcp__noseparator"), None);
        assert_eq!(split_mcp_tool_name("mcp____empty_tool"), None);
    }

    #[test]
    fn usage_lines_summarize_the_schema() {
        let schema = json!({
            "type": "object",
            "properties": { "path": { "type": "string" }, "depth": { "type": "number" } },
            "required": ["path"]
        });
        let u = usage_for("mcp__fs__read_file", "Read a file from disk.\nLong details…", &schema);
        assert!(u.contains(r#""path": string*"#), "required prop marked: {u}");
        assert!(u.contains(r#""depth": number"#), "optional prop listed: {u}");
        assert!(u.starts_with("mcp__fs__read_file {"));
        assert!(!u.contains("Long details"), "only first description line: {u}");
        // schemaless tool still gets a usable line
        assert_eq!(usage_for("mcp__x__ping", "ping", &Value::Null), "mcp__x__ping {} — ping");
    }

    #[test]
    fn upsert_server_tools_syncs_and_prunes() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let uid = crate::agents::DEFAULT_USER;
        let schema = json!({"type":"object","properties":{"sql":{"type":"string"}},"required":["sql"]});
        let tools = vec![
            ("query_db".to_string(), "Run a query.".to_string(), schema),
        ];
        let n = upsert_server_tools(&c, uid, "homelab", &tools).unwrap();
        assert_eq!(n, 1);
        let full = mcp_tool_name("homelab", "query_db");
        let (name, desc, kind): (String, String, String) = c
            .query_row(
                "SELECT name, description, kind FROM agent_tools WHERE user_id=?1 AND name=?2",
                rusqlite::params![uid, full],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(name, full);
        assert_eq!(kind, "mcp");
        assert!(desc.contains(r#""sql": string*"#), "usage stored: {desc}");

        // re-sync with a renamed tool: old row is pruned, new one appears
        let tools2 = vec![("list_tables".to_string(), "List tables.".to_string(), json!({"type":"object"}))];
        assert_eq!(upsert_server_tools(&c, uid, "homelab", &tools2).unwrap(), 1);
        let names: Vec<String> = {
            let mut stmt = c
                .prepare("SELECT name FROM agent_tools WHERE user_id=?1 AND kind='mcp' ORDER BY name")
                .unwrap();
            stmt.query_map(rusqlite::params![uid], |r| r.get(0))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(names, vec![mcp_tool_name("homelab", "list_tables")]);
        // other servers' rows are untouched by the prune
        upsert_server_tools(&c, uid, "fs", &[("read".into(), "r".into(), json!({"type":"object"}))]).unwrap();
        upsert_server_tools(&c, uid, "homelab", &tools2).unwrap();
        let count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM agent_tools WHERE user_id=?1 AND kind='mcp'",
                rusqlite::params![uid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }
}
