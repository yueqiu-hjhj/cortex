//! Connector plugins — the "fetch + ingest" primitives SurfSense has 50+ of.
//!
//! A connector manifest (the `connectors` section of a plugins/*.json file)
//! declares *data*, never code: one HTTP fetch template (with pagination),
//! a field map that pulls `id`/`title`/`text`/`url` out of each JSON item,
//! and an ingest rule that turns every item into a source. The host ships a
//! single generic executor for that shape, so a contributor can wire Zotero,
//! Notion, GitHub, or any JSON API without touching the binary.
//!
//! Ingest reuses the normal `add_source` pipeline (parse → chunk → embed →
//! store), so connector content lands exactly where a pasted link or text
//! would: searchable, citable, and part of the context tree.
//!
//! Dedup is by `origin`:
//!   • text items  → `connector:{connector_id}:{item_id}`
//!   • url items   → the item URL itself (same rule as the web clipper)
//! Syncing twice never duplicates; re-running after an item changes upstream
//! updates nothing (content is ingested once — re-sync picks up new items).

use crate::error::{Error, Result};
use crate::models::AddSourceInput;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use tauri::{AppHandle, Manager};

// ---- manifest types (the `connectors:` section of a plugins/*.json) --------

/// One connector contributed by a plugin manifest. Everything is data: URLs,
/// dot-paths into the JSON responses, and templates. The executor is generic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConnector {
    pub id: String,
    pub label: String,
    /// Where to read about the upstream API (shown as a link in the UI).
    #[serde(default)]
    pub docs_url: String,
    pub auth: ConnectorAuth,
    /// Extra non-secret fields the user fills in (user id, repo, database id…).
    #[serde(default)]
    pub config: Vec<ConnectorConfigField>,
    pub fetch: ConnectorFetch,
    pub ingest: ConnectorIngest,
}

/// How the API key is attached. `none` = public API (no key needed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorAuth {
    /// none | bearer | header | query
    pub kind: String,
    /// Header or query-param name carrying the key. Default "Authorization"
    /// (used with kind=bearer); for kind=header/query this is required.
    #[serde(default)]
    pub name: String,
    /// Where the user obtains the key — shown next to the input.
    #[serde(default)]
    pub key_note: String,
    /// Constant headers every request carries (API versioning etc.).
    #[serde(default)]
    pub extra_headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorConfigField {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub placeholder: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorFetch {
    /// URL template; `{config_key}` placeholders are filled from the saved
    /// config (NOT the API key — the key only travels via `auth`).
    pub url: String,
    /// GET (default) | POST
    #[serde(default)]
    pub method: String,
    /// POST body template; `{cursor}` is replaced in cursor mode, and
    /// top-level fields whose value becomes "" are dropped (empty cursor).
    #[serde(default)]
    pub body: Option<Value>,
    /// Dot path to the items array ("" = the response root IS the array).
    #[serde(default)]
    pub items_path: String,
    /// none (default) | offset (start=0, start+=n) | page (page=1, page+=1) |
    /// cursor (value taken from `next_cursor_path`, loop until it vanishes)
    #[serde(default)]
    pub page_mode: String,
    #[serde(default)]
    pub page_param: String,
    #[serde(default)]
    pub page_size_param: String,
    #[serde(default)]
    pub page_size: i64,
    /// Dot path to the next-page cursor value (cursor mode).
    #[serde(default)]
    pub next_cursor_path: String,
    /// Hard stop so a misdeclared pagination can't loop forever. Default 10.
    #[serde(default)]
    pub max_pages: i64,
    pub fields: ConnectorFields,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorFields {
    /// Dot path to a stable per-item id (drives the text-mode dedup origin).
    pub id: String,
    pub title: String,
    /// ingest.kind=text: dot path to the item body.
    #[serde(default)]
    pub text: String,
    /// ingest.kind=url: dot path to the item link.
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorIngest {
    /// text (body becomes a txt source) | url (link goes through the web/yt
    /// clipper pipeline exactly like a pasted link).
    pub kind: String,
    /// Source name template. Default "{title}".
    #[serde(default)]
    pub name_template: String,
    /// ingest.kind=text: body template. Default "{text}".
    #[serde(default)]
    pub text_template: String,
}

// ---- plugin lookup ----------------------------------------------------------

pub struct ConnectorInstance {
    pub plugin: String,
    pub connector: PluginConnector,
}

impl Clone for ConnectorInstance {
    fn clone(&self) -> Self {
        ConnectorInstance {
            plugin: self.plugin.clone(),
            connector: self.connector.clone(),
        }
    }
}

/// Find a connector across all loaded plugin manifests.
pub fn find(connector_id: &str) -> Option<ConnectorInstance> {
    for p in crate::plugins::all() {
        for c in &p.connectors {
            if c.id == connector_id {
                return Some(ConnectorInstance { plugin: p.name.clone(), connector: c.clone() });
            }
        }
    }
    None
}

// ---- value helpers ----------------------------------------------------------

/// Dot-path lookup into JSON: `data.title` for objects, `results.0.id` where a
/// numeric segment indexes an array. Empty path = the value itself.
pub fn dot_get<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    if path.trim().is_empty() {
        return Some(cur);
    }
    for seg in path.split('.') {
        if seg.is_empty() {
            continue;
        }
        match cur {
            Value::Object(map) => {
                cur = map.get(seg)?;
            }
            Value::Array(arr) => {
                let idx: usize = seg.parse().ok()?;
                cur = arr.get(idx)?;
            }
            _ => return None,
        }
    }
    Some(cur)
}

fn dot_string(v: &Value, path: &str) -> Option<String> {
    let val = dot_get(v, path)?;
    Some(match val {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => return None,
    })
}

/// Fill `{key}` placeholders from the config map. Unknown or missing keys are
/// an error — caught at configure time, not silently producing a bad URL.
pub fn fill_template(tpl: &str, cfg: &BTreeMap<String, String>) -> Result<String> {
    let mut out = String::with_capacity(tpl.len());
    let mut rest = tpl;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            return Err(Error::Other(format!("模板里有未闭合的 '{{': {tpl}")));
        };
        out.push_str(&rest[..start]);
        let key = &rest[start + 1..start + end];
        let val = cfg
            .get(key)
            .map(|s| s.trim().to_string())
            .ok_or_else(|| Error::Other(format!("模板占位符 {{{key}}} 没有对应的配置项（检查清单的 config 段）")))?;
        if val.is_empty() {
            return Err(Error::Other(format!("配置项 {key} 是空的 — 先在连接器设置里填好")));
        }
        out.push_str(&val);
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Scan a template for placeholder names (manifest validation helper).
pub fn template_keys(tpl: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut rest = tpl;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else { break };
        let key = &rest[start + 1..start + end];
        if !key.is_empty() {
            keys.push(key.to_string());
        }
        rest = &rest[start + end + 1..];
    }
    keys
}

// ---- config & secret storage -------------------------------------------------

fn key_secret(connector_id: &str) -> String {
    format!("connector:{connector_id}:key")
}
fn cfg_setting(connector_id: &str, key: &str) -> String {
    format!("connector:{connector_id}:cfg:{key}")
}

fn load_cfg(c: &Connection, connector_id: &str) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    let prefix = format!("connector:{connector_id}:cfg:");
    let mut q = c.prepare("SELECT key, value FROM settings WHERE key LIKE ?1")?;
    let rows = q.query_map(params![format!("{prefix}%")], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (k, v) = row?;
        if let Some(short) = k.strip_prefix(&prefix) {
            out.insert(short.to_string(), v);
        }
    }
    Ok(out)
}

fn has_key(c: &Connection, connector_id: &str) -> Result<bool> {
    Ok(crate::secrets::get_secret(c, &key_secret(connector_id))?
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false))
}

// ---- fetch executor -----------------------------------------------------------

fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        // Many APIs (GitHub included) reject requests without a UA.
        .user_agent("cortex-base/1.0 (local study workspace)")
        .build()
        .unwrap_or_default()
}

/// Apply auth to a request builder: the key travels only via the declared
/// channel, never through URL templates.
fn apply_auth(inst: &ConnectorInstance, key: &str, rb: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
    let auth = &inst.connector.auth;
    let mut rb = rb;
    for (k, v) in &auth.extra_headers {
        rb = rb.header(k, v);
    }
    match auth.kind.as_str() {
        "bearer" => rb.bearer_auth(key),
        "header" => {
            let name: &str = if auth.name.is_empty() { "Authorization" } else { &auth.name };
            rb.header(name, key)
        }
        _ => rb,
    }
}

fn query_key(inst: &ConnectorInstance, key: &str) -> Option<(String, String)> {
    if inst.connector.auth.kind == "query" {
        let name = if inst.connector.auth.name.is_empty() {
            "token".to_string()
        } else {
            inst.connector.auth.name.clone()
        };
        Some((name, key.to_string()))
    } else {
        None
    }
}

/// One page of raw JSON. Returns the parsed body.
fn fetch_page(
    inst: &ConnectorInstance,
    url: &str,
    method: &str,
    body: Option<&Value>,
    key: &str,
    cfg: &BTreeMap<String, String>,
) -> Result<Value> {
    let client = http_client();
    let has_query_key = query_key(inst, key);
    let rb0 = if method.eq_ignore_ascii_case("post") {
        client.post(url)
    } else {
        client.get(url)
    };
    let rb0 = apply_auth(inst, key, rb0);
    let rb0 = if let (Some(b), true) = (body, method.eq_ignore_ascii_case("post")) {
        rb0.json(b)
    } else {
        rb0
    };
    let rb0 = if let Some((n, v)) = has_query_key {
        rb0.query(&[(n, v)])
    } else {
        rb0
    };
    let resp = rb0.send()?;
    let status = resp.status();
    let text = resp.text()?;
    if !status.is_success() {
        let head: String = text.chars().take(300).collect();
        return Err(Error::Other(format!("连接器请求失败（HTTP {status}）: {head}")));
    }
    let val: Value = serde_json::from_str(&text).map_err(|e| {
        Error::Other(format!(
            "连接器响应不是 JSON ({e}): {}",
            text.chars().take(200).collect::<String>()
        ))
    })?;
    let _ = cfg;
    Ok(val)
}

/// Pull every item across pages: fetch loop (none/offset/page/cursor) →
/// items array extraction. Returns raw JSON item values in page order.
fn fetch_items(inst: &ConnectorInstance, key: &str, cfg: &BTreeMap<String, String>) -> Result<Vec<Value>> {
    let f = &inst.connector.fetch;
    let url = fill_template(&f.url, cfg)?;
    let method = if f.method.is_empty() { "GET".to_string() } else { f.method.clone() };
    let max_pages = f.max_pages.clamp(1, 500) as usize;
    let page_size = if f.page_size > 0 { f.page_size } else { 50 };
    let mode = if f.page_mode.is_empty() { "none" } else { f.page_mode.as_str() };

    let mut items: Vec<Value> = Vec::new();
    let mut cursor = String::new();
    for _page in 0..max_pages {
        // Per-page URL: append/replace pagination params on the template URL.
        let page_url = match mode {
            "offset" => {
                let sep = if url.contains('?') { '&' } else { '?' };
                let mut u = format!("{url}{sep}{}={}", f.page_param, items.len());
                if !f.page_size_param.is_empty() {
                    u.push_str(&format!("&{}={page_size}", f.page_size_param));
                }
                u
            }
            "page" => {
                let page_no = 1 + items.len() / page_size as usize;
                let sep = if url.contains('?') { '&' } else { '?' };
                let mut u = format!("{url}{sep}{}={}", f.page_param, page_no);
                if !f.page_size_param.is_empty() {
                    u.push_str(&format!("&{}={page_size}", f.page_size_param));
                }
                u
            }
            "cursor" => {
                if cursor.is_empty() || f.page_param.is_empty() {
                    url.clone()
                } else {
                    let sep = if url.contains('?') { '&' } else { '?' };
                    format!("{url}{sep}{}={}", f.page_param, cursor)
                }
            }
            _ => url.clone(),
        };
        // POST body: fill {cursor}, drop fields that became empty strings.
        let body: Option<Value> = match (&f.body, mode) {
            (Some(b), "cursor") => {
                let mut filled = b.clone();
                fill_body_cursor(&mut filled, &cursor);
                Some(filled)
            }
            (Some(b), _) => Some(b.clone()),
            _ => None,
        };
        let val = fetch_page(inst, &page_url, &method, body.as_ref(), key, cfg)?;

        let arr = match dot_get(&val, &f.items_path) {
            Some(Value::Array(a)) => a.clone(),
            _ => {
                if items.is_empty() {
                    return Err(Error::Other(format!(
                        "items_path '{}' 没有取到数组 — 检查清单字段映射",
                        f.items_path
                    )));
                }
                break; // a later page lost the shape; keep what we have
            }
        };
        let got = arr.len();
        items.extend(arr);
        if mode == "cursor" {
            match dot_string(&val, &f.next_cursor_path) {
                Some(next) if !next.is_empty() => cursor = next,
                _ => break,
            }
        } else if got < page_size as usize {
            break; // short page = last page
        }
    }
    Ok(items)
}

/// Replace `{cursor}` recursively inside the POST body template; remove
/// object fields whose string value became empty (Notion rejects
/// `"start_cursor": ""`).
fn fill_body_cursor(v: &mut Value, cursor: &str) {
    match v {
        Value::Object(map) => {
            let empties: Vec<String> = map
                .iter()
                .filter_map(|(k, val)| {
                    if let Value::String(s) = val {
                        if s == "{cursor}" {
                            return Some((k.clone(), s.clone()));
                        }
                    }
                    None
                })
                .map(|(k, _)| k)
                .collect();
            for k in empties {
                if cursor.is_empty() {
                    map.remove(&k);
                } else {
                    map.insert(k, Value::String(cursor.to_string()));
                }
            }
            for (_, val) in map.iter_mut() {
                fill_body_cursor(val, cursor);
            }
        }
        Value::Array(a) => {
            for item in a.iter_mut() {
                fill_body_cursor(item, cursor);
            }
        }
        _ => {}
    }
}

// ---- mapping & ingest ----------------------------------------------------------

pub struct MappedItem {
    pub item_id: String,
    pub title: String,
    /// text mode: rendered body; url mode: the link.
    pub payload: String,
}

/// Map one raw JSON item through the manifest's field paths + ingest templates.
pub fn map_item(inst: &ConnectorInstance, item: &Value) -> Result<MappedItem> {
    let f = &inst.connector.fetch;
    let ing = &inst.connector.ingest;
    let item_id = dot_string(item, &f.fields.id)
        .ok_or_else(|| Error::Other(format!("字段路径 '{}' 没取到条目 id", f.fields.id)))?;
    let title = dot_string(item, &f.fields.title).unwrap_or_default();
    let mut vars = BTreeMap::new();
    vars.insert("id".to_string(), item_id.clone());
    vars.insert("title".to_string(), title.clone());

    let payload = match ing.kind.as_str() {
        "text" => {
            let text = dot_string(item, &f.fields.text).unwrap_or_default();
            // Empty-body items (a Zotero entry with no abstract, an issue with
            // no text) are skipped here — otherwise fill_template would reject
            // the {text} placeholder with a misleading "config is empty" error.
            if text.trim().is_empty() {
                return Err(Error::Other("条目正文为空，跳过".into()));
            }
            vars.insert("text".to_string(), text);
            let tpl = if ing.text_template.is_empty() { "{text}" } else { &ing.text_template };
            fill_template(tpl, &vars)?
        }
        "url" => {
            let url = dot_string(item, &f.fields.url)
                .ok_or_else(|| Error::Other(format!("字段路径 '{}' 没取到条目链接", f.fields.url)))?;
            if url.trim().is_empty() {
                return Err(Error::Other("条目链接为空".into()));
            }
            vars.insert("url".to_string(), url.clone());
            url
        }
        other => return Err(Error::Other(format!("ingest.kind '{other}' 不是 text 或 url"))),
    };
    if payload.trim().is_empty() {
        return Err(Error::Other("条目内容为空，跳过".into()));
    }
    Ok(MappedItem { item_id, title, payload })
}

/// The dedup origin for a mapped item.
fn origin_of(inst: &ConnectorInstance, m: &MappedItem) -> String {
    if inst.connector.ingest.kind == "url" {
        m.payload.clone()
    } else {
        format!("connector:{}:{}", inst.connector.id, m.item_id)
    }
}

fn origin_exists(c: &Connection, subject_id: &str, origin: &str) -> Result<bool> {
    let hit: Option<i64> = c
        .query_row(
            "SELECT 1 FROM sources WHERE subject_id=?1 AND origin=?2 LIMIT 1",
            params![subject_id, origin],
            |r| r.get(0),
        )
        .optional()?;
    Ok(hit.is_some())
}

// ---- sync -----------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ConnectorSyncSummary {
    pub fetched: usize,
    pub imported: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

/// Fetch everything the connector declares, map items, dedup by origin, and
/// push each new item through the normal add_source pipeline. Runs as a
/// background job from the UI.
pub async fn sync(
    app: AppHandle,
    connector_id: &str,
    subject_id: String,
    topic_id: Option<String>,
) -> Result<ConnectorSyncSummary> {
    let inst = find(connector_id)
        .ok_or_else(|| Error::Other(format!("连接器 {connector_id} 不存在（检查插件清单）")))?;

    let (key, cfg) = {
        let state = app.state::<crate::db::AppState>();
        let c = state.db.lock().unwrap();
        let key = crate::secrets::get_secret(&c, &key_secret(connector_id))?.unwrap_or_default();
        (key, load_cfg(&c, connector_id)?)
    };
    if inst.connector.auth.kind != "none" && key.trim().is_empty() {
        return Err(Error::Other("连接器还没有配置 API Key — 先在设置里保存".into()));
    }

    // Network phase off the async thread (blocking reqwest, like moodle).
    let fetch_inst = inst.clone();
    let fetched = tauri::async_runtime::spawn_blocking(move || fetch_items(&fetch_inst, &key, &cfg))
        .await
        .map_err(|e| Error::Other(format!("后台任务失败: {e}")))??;

    let mut summary = ConnectorSyncSummary {
        fetched: fetched.len(),
        imported: 0,
        skipped: 0,
        errors: Vec::new(),
    };
    let mut seen_origins: HashSet<String> = HashSet::new();
    for item in &fetched {
        let m = match map_item(&inst, item) {
            Ok(m) => m,
            Err(e) => {
                summary.skipped += 1;
                if summary.errors.len() < 5 {
                    summary.errors.push(e.to_string());
                }
                continue;
            }
        };
        let origin = origin_of(&inst, &m);
        if !seen_origins.insert(origin.clone()) {
            summary.skipped += 1; // duplicate within this run
            continue;
        }
        let already = {
            let state = app.state::<crate::db::AppState>();
            let c = state.db.lock().unwrap();
            origin_exists(&c, &subject_id, &origin)
        };
        match already {
            Ok(true) => {
                summary.skipped += 1;
                continue;
            }
            Ok(false) => {}
            Err(e) => {
                summary.errors.push(e.to_string());
                continue;
            }
        }
        let input = if inst.connector.ingest.kind == "url" {
            AddSourceInput {
                subject_id: subject_id.clone(),
                topic_id: topic_id.clone(),
                name: Some(m.title),
                kind: None, // let detect_kind route web vs yt
                text: None,
                path: None,
                url: Some(m.payload),
                tags: vec![format!("connector:{}", inst.connector.id)],
            }
        } else {
            AddSourceInput {
                subject_id: subject_id.clone(),
                topic_id: topic_id.clone(),
                name: Some(m.title),
                kind: Some("txt".into()), // pin txt so the connector-origin URL stays metadata
                text: Some(m.payload),
                path: None,
                url: Some(origin.clone()),
                tags: vec![format!("connector:{}", inst.connector.id)],
            }
        };
        match crate::commands::add_source(app.clone(), input).await {
            Ok(_) => summary.imported += 1,
            Err(e) => {
                if summary.errors.len() < 5 {
                    summary.errors.push(format!("{}: {e}", m.item_id));
                }
            }
        }
    }
    Ok(summary)
}

// ---- Tauri command surface --------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ConnectorInfo {
    pub id: String,
    pub label: String,
    pub plugin: String,
    pub docs_url: String,
    pub auth_kind: String,
    pub key_note: String,
    pub config_fields: Vec<ConnectorConfigField>,
    pub configured: bool,
    /// Saved non-secret config values, for re-editing in the UI.
    pub config_values: BTreeMap<String, String>,
}

#[tauri::command]
pub fn connector_list(state: tauri::State<crate::db::AppState>) -> Result<Vec<ConnectorInfo>> {
    let c = state.db.lock().unwrap();
    let mut out = Vec::new();
    for p in crate::plugins::all() {
        for cn in &p.connectors {
            let cfg = load_cfg(&c, &cn.id).unwrap_or_default();
            let configured = if cn.auth.kind == "none" {
                true
            } else {
                has_key(&c, &cn.id).unwrap_or(false)
            };
            out.push(ConnectorInfo {
                id: cn.id.clone(),
                label: cn.label.clone(),
                plugin: p.name.clone(),
                docs_url: cn.docs_url.clone(),
                auth_kind: cn.auth.kind.clone(),
                key_note: cn.auth.key_note.clone(),
                config_fields: cn.config.clone(),
                configured,
                config_values: cfg,
            });
        }
    }
    Ok(out)
}

#[tauri::command]
pub fn connector_configure(
    state: tauri::State<crate::db::AppState>,
    connector_id: String,
    key: String,
    config: BTreeMap<String, String>,
) -> Result<()> {
    let inst = find(&connector_id)
        .ok_or_else(|| Error::Other(format!("连接器 {connector_id} 不存在")))?;
    let c = state.db.lock().unwrap();
    if inst.connector.auth.kind != "none" {
        if key.trim().is_empty() {
            crate::secrets::delete_secret(&c, &key_secret(&connector_id))?;
        } else {
            crate::secrets::set_secret(&c, &key_secret(&connector_id), key.trim())?;
        }
    }
    for field in &inst.connector.config {
        let val = config.get(&field.key).map(|s| s.trim().to_string()).unwrap_or_default();
        repo_set(&c, &cfg_setting(&connector_id, &field.key), &val)?;
    }
    Ok(())
}

fn repo_set(c: &Connection, k: &str, v: &str) -> Result<()> {
    crate::repo::set_setting(c, k, v)
}

#[tauri::command]
pub fn connector_disconnect(state: tauri::State<crate::db::AppState>, connector_id: String) -> Result<()> {
    let inst = find(&connector_id).ok_or_else(|| Error::Other(format!("连接器 {connector_id} 不存在")))?;
    let c = state.db.lock().unwrap();
    crate::secrets::delete_secret(&c, &key_secret(&connector_id))?;
    for field in &inst.connector.config {
        crate::repo::set_setting(&c, &cfg_setting(&connector_id, &field.key), "")?;
    }
    Ok(())
}

#[tauri::command]
pub async fn connector_sync(
    app: AppHandle,
    connector_id: String,
    subject_id: String,
    topic_id: Option<String>,
) -> Result<ConnectorSyncSummary> {
    sync(app, &connector_id, subject_id, topic_id).await
}

// ---- built-in research connectors (the community-manifest example) ----------

/// Shipped on first run into `<data_dir>/plugins/builtin-connectors.json`.
/// This is itself the reference manifest for the connector primitives: copy a
/// block, point it at another JSON API, restart. Users may edit the file or
/// delete it — it is only re-created while no manifest with connectors exists.
pub const BUILTIN_MANIFEST: &str = r###"{
  "name": "builtin-research",
  "connectors": [
    {
      "id": "zotero-library",
      "label": "Zotero 文献库",
      "docs_url": "https://www.zotero.org/support/dev/client_coding/javascript",
      "auth": {
        "kind": "header",
        "name": "Zotero-API-Key",
        "key_note": "zotero.org/settings/keys 新建一个无写权限的 key",
        "extra_headers": { "Zotero-API-Version": "3" }
      },
      "config": [
        { "key": "user_id", "label": "Zotero 用户 ID", "placeholder": "在 zotero.org/settings/keys 页可见" }
      ],
      "fetch": {
        "url": "https://api.zotero.org/users/{user_id}/items?format=json&itemType=-attachment",
        "page_mode": "offset",
        "page_param": "start",
        "page_size_param": "limit",
        "page_size": 100,
        "max_pages": 20,
        "fields": { "id": "key", "title": "data.title", "text": "data.abstractNote" }
      },
      "ingest": {
        "kind": "text",
        "name_template": "{title}",
        "text_template": "《{title}》\n\n{text}\n\n[Zotero 条目 {id}]"
      }
    },
    {
      "id": "notion-database",
      "label": "Notion 数据库",
      "docs_url": "https://developers.notion.com/reference/post-database-query",
      "auth": {
        "kind": "bearer",
        "key_note": "notion.so/profile/integrations 新建 internal integration，并把数据库连接给它",
        "extra_headers": { "Notion-Version": "2022-06-28" }
      },
      "config": [
        { "key": "database_id", "label": "数据库 ID", "placeholder": "数据库页面 URL 里 32 位十六进制串" }
      ],
      "fetch": {
        "url": "https://api.notion.com/v1/databases/{database_id}/query",
        "method": "POST",
        "body": { "page_size": 50, "start_cursor": "{cursor}" },
        "items_path": "results",
        "page_mode": "cursor",
        "next_cursor_path": "start_cursor",
        "max_pages": 20,
        "fields": {
          "id": "id",
          "title": "properties.Name.title.0.plain_text",
          "text": "properties.Excerpt.rich_text.0.plain_text"
        }
      },
      "ingest": {
        "kind": "text",
        "name_template": "{title}",
        "text_template": "{title}\n\n{text}"
      }
    },
    {
      "id": "github-issues",
      "label": "GitHub Issues",
      "docs_url": "https://docs.github.com/rest/issues/issues",
      "auth": {
        "kind": "bearer",
        "key_note": "github.com/settings/tokens 新建 classic token（read:issues 权限）",
        "extra_headers": { "X-GitHub-Api-Version": "2022-11-28", "Accept": "application/vnd.github+json" }
      },
      "config": [
        { "key": "repo", "label": "仓库", "placeholder": "owner/name" }
      ],
      "fetch": {
        "url": "https://api.github.com/repos/{repo}/issues?state=open",
        "page_mode": "page",
        "page_param": "page",
        "page_size_param": "per_page",
        "page_size": 50,
        "max_pages": 20,
        "fields": { "id": "number", "title": "title", "text": "body", "url": "html_url" }
      },
      "ingest": {
        "kind": "text",
        "name_template": "{title}",
        "text_template": "#{id} {title}\n\n{text}"
      }
    }
  ]
}
"###;

/// Write the built-in connector manifest on first run (and whenever the user
/// has deleted it without providing any other manifest with connectors — so
/// deleting is a reset, not a permanent removal, while a user's own connector
/// manifests are never touched).
pub fn seed_builtin(data_dir: &std::path::Path) {
    let plugins_dir = data_dir.join("plugins");
    if std::fs::create_dir_all(&plugins_dir).is_err() {
        return;
    }
    let seed_path = plugins_dir.join("builtin-connectors.json");
    if seed_path.exists() {
        return;
    }
    let has_own = std::fs::read_dir(&plugins_dir)
        .map(|entries| {
            entries.filter_map(|e| e.ok()).any(|e| {
                let p = e.path();
                if p.extension().map(|x| x == "json").unwrap_or(false) {
                    return std::fs::read_to_string(&p)
                        .ok()
                        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                        .and_then(|v| {
                            v.get("connectors")
                                .and_then(|c| c.as_array())
                                .map(|a| !a.is_empty())
                        })
                        .unwrap_or(false);
                }
                false
            })
        })
        .unwrap_or(false);
    if !has_own {
        let _ = std::fs::write(&seed_path, BUILTIN_MANIFEST);
    }
}

// ---- tests -----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn inst_with(connector: PluginConnector) -> ConnectorInstance {
        ConnectorInstance { plugin: "test".into(), connector }
    }

    fn sample_connector() -> PluginConnector {
        serde_json::from_value(json!({
            "id": "test-c",
            "label": "Test",
            "auth": { "kind": "bearer", "key_note": "x" },
            "fetch": {
                "url": "https://api.example.test/users/{user_id}/items",
                "page_mode": "offset",
                "page_param": "start",
                "page_size_param": "limit",
                "page_size": 2,
                "fields": { "id": "key", "title": "data.title", "text": "data.body" }
            },
            "ingest": { "kind": "text" }
        }))
        .unwrap()
    }

    #[test]
    fn dot_paths_walk_objects_arrays_and_indices() {
        let v = json!({ "data": { "title": "BM25", "tags": ["ir", "search"] } });
        assert_eq!(dot_string(&v, "data.title").unwrap(), "BM25");
        assert_eq!(dot_string(&v, "data.tags.1").unwrap(), "search");
        assert!(dot_string(&v, "data.missing").is_none());
        assert_eq!(dot_get(&v, ""), Some(&v)); // empty path = the value itself
        assert!(dot_string(&v, "").is_none()); // root is an object, not a scalar
    }

    #[test]
    fn templates_fill_and_reject_unknown_or_empty_keys() {
        let mut cfg = BTreeMap::new();
        cfg.insert("user_id".into(), "42".into());
        assert_eq!(
            fill_template("https://x.test/u/{user_id}/items", &cfg).unwrap(),
            "https://x.test/u/42/items"
        );
        let err = fill_template("https://x.test/{nope}", &cfg).unwrap_err();
        assert!(err.to_string().contains("nope"));
        cfg.insert("empty".into(), "".into());
        assert!(fill_template("https://x.test/{empty}", &cfg).is_err());
    }

    #[test]
    fn text_items_map_through_templates() {
        let inst = inst_with(sample_connector());
        let item = json!({ "key": "ABCD1234", "data": { "title": "IR Basics", "body": "词频与idf" } });
        let m = map_item(&inst, &item).unwrap();
        assert_eq!(m.item_id, "ABCD1234");
        assert_eq!(m.payload, "词频与idf"); // default {text} template
        let origin = origin_of(&inst, &m);
        assert_eq!(origin, "connector:test-c:ABCD1234");
    }

    #[test]
    fn url_items_keep_the_link_as_payload_and_origin() {
        let mut c = sample_connector();
        c.ingest.kind = "url".into();
        c.fetch.fields.url = "data.link".into();
        let inst = inst_with(c);
        let item = json!({ "key": "Z1", "data": { "title": "A paper", "link": "https://example.test/a" } });
        let m = map_item(&inst, &item).unwrap();
        assert_eq!(m.payload, "https://example.test/a");
        assert_eq!(origin_of(&inst, &m), "https://example.test/a");
    }

    #[test]
    fn empty_text_items_are_rejected_not_ingested() {
        let inst = inst_with(sample_connector());
        let item = json!({ "key": "E1", "data": { "title": "t", "body": "  " } });
        assert!(map_item(&inst, &item).is_err());
    }

    #[test]
    fn body_cursor_fill_removes_empty_and_keeps_next() {
        // Each page builds a FRESH body from the manifest template (same as
        // fetch_items does), so the second fill needs its own copy — after the
        // first call removed the field, there is no {cursor} left to fill.
        let mut body = json!({ "page_size": 50, "start_cursor": "{cursor}" });
        fill_body_cursor(&mut body, "");
        assert!(body.get("start_cursor").is_none()); // first page: field dropped
        let mut body2 = json!({ "page_size": 50, "start_cursor": "{cursor}" });
        fill_body_cursor(&mut body2, "abc");
        assert_eq!(body2.get("start_cursor").unwrap(), "abc");
    }

    #[test]
    fn origin_dedup_queries_by_subject() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let sid = crate::repo::insert_subject(&c, "信息检索", None, None, None).unwrap();
        assert!(!origin_exists(&c, &sid, "connector:z:1").unwrap());
        crate::repo::insert_source(&c, &sid, None, "IR Basics", "txt", Some("connector:z:1")).unwrap();
        assert!(origin_exists(&c, &sid, "connector:z:1").unwrap());
        assert!(!origin_exists(&c, &sid, "connector:z:2").unwrap());
    }

    #[test]
    fn find_resolves_across_loaded_plugins() {
        // plugins::all() is global static state; only assert the miss path here
        // (the hit path is covered by plugins.rs tests that load manifests).
        assert!(find("no-such-connector").is_none());
    }
}
