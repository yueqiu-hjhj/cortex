//! Extraction templates — the Elicit-style workflow B4 was missing: instead of
//! a free-row-label comparison, a template fixes the field list (样本量 / 方法 /
//! 效应量 …) once, and a run applies it to N papers, one LLM call per paper.
//! Every answer lands in `extract_cells` as its own row, so each value is
//! individually reviewable: model cells can be corrected (source flips to
//! 'manual' and later re-runs never overwrite them), single papers can be
//! retried, and the whole table exports to CSV.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::db::{new_id, now_ms};
use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractField {
    pub key: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtractTemplate {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub fields: Vec<ExtractField>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtractRun {
    pub id: String,
    pub template_id: String,
    pub template_name: String,
    pub title: String,
    /// Field-list snapshot taken when the run was created — the table shape of
    /// THIS run, independent of later template edits.
    pub fields: Vec<ExtractField>,
    pub item_ids: Vec<String>,
    pub model: Option<String>,
    pub status: String, // running | done | error
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtractCell {
    pub item_id: String,
    pub field_key: String,
    pub value: String,
    /// Verbatim supporting sentence from the paper card (2.4: PRISMA-grade
    /// provenance for the user-defined-schema path too).
    pub evidence: Option<String>,
    /// Back-link to excerpts — set when the evidence matched or became a card.
    pub excerpt_id: Option<String>,
    pub source: String, // model | manual
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtractItem {
    pub id: String,
    pub title: String,
    pub authors: Option<String>,
    pub year: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtractResult {
    pub run: ExtractRun,
    pub items: Vec<ExtractItem>,
    pub cells: Vec<ExtractCell>,
}

// ---- templates ---------------------------------------------------------

fn parse_fields(raw: &str) -> Vec<ExtractField> {
    serde_json::from_str(raw).unwrap_or_default()
}

pub fn list_templates(conn: &Connection) -> Result<Vec<ExtractTemplate>> {
    let mut st = conn.prepare(
        "SELECT id, name, description, fields, created_at, updated_at
         FROM extract_templates ORDER BY updated_at DESC",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, name, description, fields, created_at, updated_at) = row?;
        out.push(ExtractTemplate {
            id,
            name,
            description,
            fields: parse_fields(&fields),
            created_at,
            updated_at,
        });
    }
    Ok(out)
}

fn validate_fields(fields: &[ExtractField]) -> Result<()> {
    if fields.is_empty() {
        return Err(Error::Other("模板至少需要一个字段".into()));
    }
    let mut seen = std::collections::HashSet::new();
    for f in fields {
        let key = f.key.trim();
        let prompt = f.prompt.trim();
        if key.is_empty() || prompt.is_empty() {
            return Err(Error::Other("字段的 key 和提取要求都不能为空".into()));
        }
        if key.len() > 64 {
            return Err(Error::Other(format!("字段 key 过长：{key}")));
        }
        if !seen.insert(key.to_string()) {
            return Err(Error::Other(format!("字段 key 重复：{key}")));
        }
    }
    Ok(())
}

pub fn save_template(
    conn: &Connection,
    id: Option<&str>,
    name: &str,
    description: Option<&str>,
    fields: &[ExtractField],
) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::Other("模板名称不能为空".into()));
    }
    validate_fields(fields)?;
    let normalized: Vec<ExtractField> = fields
        .iter()
        .map(|f| ExtractField {
            key: f.key.trim().to_string(),
            prompt: f.prompt.trim().to_string(),
        })
        .collect();
    let fields_json = serde_json::to_string(&normalized).unwrap_or_else(|_| "[]".into());
    let now = now_ms();
    match id.filter(|s| !s.trim().is_empty()) {
        Some(id) => {
            let n = conn.execute(
                "UPDATE extract_templates SET name=?2, description=?3, fields=?4, updated_at=?5 WHERE id=?1",
                params![id, name, description, fields_json, now],
            )?;
            if n == 0 {
                return Err(Error::NotFound(format!("template {id}")));
            }
            Ok(id.to_string())
        }
        None => {
            let id = new_id();
            conn.execute(
                "INSERT INTO extract_templates (id, name, description, fields, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, name, description, fields_json, now, now],
            )?;
            Ok(id)
        }
    }
}

pub fn delete_template(conn: &Connection, id: &str) -> Result<()> {
    let n = conn.execute("DELETE FROM extract_templates WHERE id = ?1", params![id])?;
    if n == 0 {
        return Err(Error::NotFound(format!("template {id}")));
    }
    Ok(())
}

// ---- runs --------------------------------------------------------------

fn row_to_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<ExtractRun> {
    let rev: String = r.get(3)?;
    let ids: String = r.get(8)?;
    Ok(ExtractRun {
        id: r.get(0)?,
        template_id: r.get(1)?,
        template_name: r.get(2)?,
        title: r.get(4)?,
        fields: parse_fields(&rev),
        model: r.get(5)?,
        status: r.get(6)?,
        error: r.get(7)?,
        item_ids: serde_json::from_str(&ids).unwrap_or_default(),
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
    })
}

const RUN_SELECT: &str = "SELECT r.id, r.template_id, COALESCE(t.name, '（已删模板）'), r.template_rev, \
                          r.title, r.model, r.status, r.error, r.item_ids, r.created_at, r.updated_at \
                          FROM extract_runs r LEFT JOIN extract_templates t ON t.id = r.template_id";

pub fn create_run(
    conn: &Connection,
    template_id: &str,
    item_ids: &[String],
    title: Option<&str>,
) -> Result<String> {
    if item_ids.is_empty() {
        return Err(Error::Other("请至少勾选一篇论文".into()));
    }
    let row = conn
        .query_row(
            "SELECT name, fields FROM extract_templates WHERE id = ?1",
            params![template_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("template {template_id}")))?;
    validate_fields(&parse_fields(&row.1))?;
    let id = new_id();
    let now = now_ms();
    let title = match title.map(str::trim).filter(|s| !s.is_empty()) {
        Some(t) => t.to_string(),
        None => format!("{} · {} 篇", row.0, item_ids.len()),
    };
    conn.execute(
        "INSERT INTO extract_runs (id, template_id, template_rev, title, status, item_ids, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6, ?7)",
        params![id, template_id, row.1, title, serde_json::to_string(item_ids).unwrap_or_else(|_| "[]".into()), now, now],
    )?;
    Ok(id)
}

pub fn get_run(conn: &Connection, id: &str) -> Result<ExtractRun> {
    conn.query_row(&format!("{RUN_SELECT} WHERE r.id = ?1"), params![id], row_to_run)
        .map_err(|_| Error::NotFound(format!("run {id}")))
}

pub fn list_runs(conn: &Connection) -> Result<Vec<ExtractRun>> {
    let mut st = conn.prepare(&format!("{RUN_SELECT} ORDER BY r.created_at DESC LIMIT 100"))?;
    let rows = st.query_map([], row_to_run)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn delete_run(conn: &Connection, id: &str) -> Result<()> {
    let n = conn.execute("DELETE FROM extract_runs WHERE id = ?1", params![id])?;
    if n == 0 {
        return Err(Error::NotFound(format!("run {id}")));
    }
    Ok(())
}

pub fn get_result(conn: &Connection, run_id: &str) -> Result<ExtractResult> {
    let run = get_run(conn, run_id)?;
    let mut items = Vec::with_capacity(run.item_ids.len());
    for pid in &run.item_ids {
        let item = conn
            .query_row(
                "SELECT id, title, authors, year FROM papers WHERE id = ?1",
                params![pid],
                |r| {
                    Ok(ExtractItem {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        authors: r.get(2)?,
                        year: r.get(3)?,
                    })
                },
            )
            .optional()?
            .unwrap_or(ExtractItem {
                id: pid.clone(),
                title: "（论文已删除）".into(),
                authors: None,
                year: None,
            });
        items.push(item);
    }
    let mut st = conn.prepare(
        "SELECT item_id, field_key, value, evidence, excerpt_id, source, updated_at
         FROM extract_cells WHERE run_id = ?1 ORDER BY item_id, field_key",
    )?;
    let cells = st
        .query_map(params![run_id], |r| {
            Ok(ExtractCell {
                item_id: r.get(0)?,
                field_key: r.get(1)?,
                value: r.get(2)?,
                evidence: r.get(3)?,
                excerpt_id: r.get(4)?,
                source: r.get(5)?,
                updated_at: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ExtractResult { run, items, cells })
}

/// Store one human correction. Always writes source='manual' — the flag is what
/// keeps re-runs and retries from clobbering reviewed values. Evidence is
/// cleared with the edit: the old proof sentence no longer backs the new text.
pub fn set_cell(
    conn: &Connection,
    run_id: &str,
    item_id: &str,
    field_key: &str,
    value: &str,
) -> Result<()> {
    let run = get_run(conn, run_id)?;
    if !run.fields.iter().any(|f| f.key == field_key) {
        return Err(Error::Other(format!("本次提取没有字段 {field_key}")));
    }
    if !run.item_ids.iter().any(|i| i == item_id) {
        return Err(Error::Other("该论文不在本次提取范围内".into()));
    }
    conn.execute(
        "INSERT INTO extract_cells (run_id, item_id, field_key, value, evidence, excerpt_id, source, updated_at)
         VALUES (?1, ?2, ?3, ?4, NULL, NULL, 'manual', ?5)
         ON CONFLICT(run_id, item_id, field_key)
         DO UPDATE SET value=excluded.value, evidence=NULL, excerpt_id=NULL,
           source='manual', updated_at=excluded.updated_at",
        params![run_id, item_id, field_key, value, now_ms()],
    )?;
    Ok(())
}

// ---- LLM execution ------------------------------------------------------

fn field_block(fields: &[ExtractField]) -> String {
    fields
        .iter()
        .map(|f| format!("- {}: {}", f.key, f.prompt))
        .collect::<Vec<_>>()
        .join("\n")
}

fn paper_prompt(fields: &[ExtractField], card: &str) -> (String, String) {
    let system = "You are a systematic-review data-extraction assistant. You get ONE paper card \
        (title, metadata, abstract, verbatim excerpts) and a list of extraction fields keyed by \
        `key`. Fill EVERY field for THIS paper using ONLY the card. Never invent facts, numbers, \
        or section names; if the card does not answer a field, output an empty string for both \
        its value and its evidence. For every field you CAN fill, also output `evidence`: ONE \
        sentence copied VERBATIM, character-for-character, from the card that best supports the \
        value. Quote exact terms, dataset/metric/algorithm names and figures from the card when \
        they answer the field. Keep each value to 1-3 sentences (under 60 words). Write each \
        value in the language of that field's prompt. Output ONLY a JSON object of the exact \
        shape {\"values\":{\"<key>\":\"...\"},\"evidence\":{\"<key>\":\"...\"}} with every key \
        present in BOTH maps — no markdown, no commentary.";
    let user = format!(
        "FIELDS:\n{}\n\nPAPER CARD:\n{}",
        field_block(fields),
        card
    );
    (system.to_string(), user)
}

fn cell_upsert(
    conn: &Connection,
    run_id: &str,
    item_id: &str,
    field_key: &str,
    value: &str,
    evidence: Option<&str>,
    excerpt_id: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO extract_cells (run_id, item_id, field_key, value, evidence, excerpt_id, source, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'model', ?7)
         ON CONFLICT(run_id, item_id, field_key) DO UPDATE SET
           value=excluded.value, evidence=excluded.evidence, excerpt_id=excluded.excerpt_id,
           source=excluded.source, updated_at=excluded.updated_at
         WHERE extract_cells.source != 'manual'",
        params![run_id, item_id, field_key, value, evidence, excerpt_id, now_ms()],
    )?;
    Ok(())
}

fn model_spec(conn: &Connection) -> Result<String> {
    let spec = crate::repo::get_setting(conn, "model_cheatsheet")?
        .unwrap_or_else(|| "openrouter:deepseek/deepseek-v4-flash".into());
    crate::commands::guard_offline_llm(conn, &spec)?;
    Ok(spec)
}

fn finish_run(conn: &Connection, run_id: &str, model: &str, warn: Option<String>) -> Result<()> {
    conn.execute(
        "UPDATE extract_runs SET status='done', model=?2, error=?3, updated_at=?4 WHERE id=?1",
        params![run_id, model, warn, now_ms()],
    )?;
    Ok(())
}

fn fail_run(conn: &Connection, run_id: &str, err: &str) {
    let _ = conn.execute(
        "UPDATE extract_runs SET status='error', error=?2, updated_at=?3 WHERE id=?1",
        params![run_id, err, now_ms()],
    );
}

/// Extract one paper's cells + evidence links with the loaded model. Errors
/// bubble to the caller, which decides per-paper whether to warn-and-continue
/// or abort.
fn extract_one_paper(
    model: &mut Box<dyn crate::llm::Llm>,
    conn: &Connection,
    run_id: &str,
    item_id: &str,
    fields: &[ExtractField],
    card: &str,
) -> Result<()> {
    let excerpts = crate::papers::list_excerpts(conn, Some(item_id))?;
    let source_id: Option<String> = conn
        .query_row(
            "SELECT source_id FROM papers WHERE id = ?1",
            params![item_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let (system, user) = paper_prompt(fields, card);
    let n_fields = fields.len() as u32;
    model.set_max_tokens((1024 + 520 * n_fields).max(2048)); // values + evidence quotes
    let raw = model.complete(&system, &user)?;
    crate::tokens::record(conn, "extraction", &model.name(), &format!("{system}\n{user}"), &raw, None);
    let parsed = match crate::llm::extract_json(&raw) {
        Ok(v) => v,
        Err(_) => {
            let raw2 = model.complete(
                &system,
                &format!("{user}\n\nIMPORTANT: the previous reply could not be parsed. Respond with ONLY the raw JSON object {{\"values\":{{…}},\"evidence\":{{…}}}}."),
            )?;
            crate::llm::extract_json(&raw2)?
        }
    };
    let values = parsed
        .get("values")
        .and_then(|v| v.as_object())
        .ok_or_else(|| Error::Other("模型输出缺少 values 对象".into()))?;
    let evidences = parsed.get("evidence").and_then(|v| v.as_object());
    for f in fields {
        let val = values
            .get(&f.key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or_default();
        let ev = evidences
            .and_then(|m| m.get(&f.key))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or_default();
        // Same review-grade provenance as PRISMA: reuse a matching excerpt,
        // else create one only if the sentence is verbatim in the card.
        let excerpt_id = if val.is_empty() || ev.is_empty() {
            None
        } else {
            crate::prisma::link_evidence(
                conn,
                item_id,
                source_id.as_deref(),
                &f.key,
                card,
                ev,
                &excerpts,
            )?
        };
        cell_upsert(
            conn,
            run_id,
            item_id,
            &f.key,
            val,
            if ev.is_empty() { None } else { Some(ev) },
            excerpt_id.as_deref(),
        )?;
    }
    Ok(())
}

/// Run the whole extraction: one LLM call per paper, cells saved incrementally
/// so a crash mid-run keeps everything extracted so far. A paper whose reply is
/// unusable becomes a per-paper warning on the finished run — one bad reply
/// must not kill a 30-paper run.
pub fn execute_run(app: &AppHandle, run_id: &str) -> Result<()> {
    let state = app.state::<crate::db::AppState>();
    let (fields, item_ids, titles, cards, mut model) = {
        let c = state.db.lock().unwrap();
        let run = get_run(&c, run_id)?;
        if run.status != "running" {
            return Err(Error::Other("该次提取已结束，请重新发起".into()));
        }
        let mut titles = Vec::with_capacity(run.item_ids.len());
        let mut cards = Vec::with_capacity(run.item_ids.len());
        for pid in &run.item_ids {
            let (p, card) = crate::papers::collect_paper_card(&c, pid)?;
            titles.push(p.title);
            cards.push(card);
        }
        let spec = model_spec(&c)?;
        let keys = crate::commands::read_keys(&c)?;
        let mut model = crate::llm::from_spec_or_any(&spec, &keys).ok_or_else(|| {
            Error::Other("No model configured - add an API key in Settings -> API keys".into())
        })?;
        crate::commands::apply_budget(&mut model, &c, "cheatsheet");
        (run.fields, run.item_ids, titles, cards, model)
    };

    let mut warnings: Vec<String> = Vec::new();
    let mut model_name = String::from("unknown");
    for (i, item_id) in item_ids.iter().enumerate() {
        let res = {
            let c = state.db.lock().unwrap();
            model_name = model.name();
            extract_one_paper(&mut model, &c, run_id, item_id, &fields, &cards[i])
        };
        if let Err(e) = res {
            warnings.push(format!("《{}》：{e}", titles[i]));
        }
    }
    let warn = if warnings.is_empty() {
        None
    } else {
        Some(format!(
            "{}/{} 篇未成功（可对单行重试）：{}",
            warnings.len(),
            titles.len(),
            warnings.join("；")
        ))
    };
    let outcome = {
        let c = state.db.lock().unwrap();
        finish_run(&c, run_id, &model_name, warn)
    };
    if let Err(e) = outcome {
        let c = state.db.lock().unwrap();
        fail_run(&c, run_id, &e.to_string());
    }
    Ok(())
}

/// Re-run extraction for ONE paper of an existing run — a row that failed, or
/// one whose excerpts changed after the run. Manual corrections on other cells
/// survive (cell_upsert never touches source='manual').
pub fn retry_item(app: &AppHandle, run_id: &str, item_id: &str) -> Result<()> {
    let state = app.state::<crate::db::AppState>();
    let (fields, card, mut model) = {
        let c = state.db.lock().unwrap();
        let run = get_run(&c, run_id)?;
        if !run.item_ids.iter().any(|i| i == item_id) {
            return Err(Error::Other("该论文不在本次提取范围内".into()));
        }
        let (_, card) = crate::papers::collect_paper_card(&c, item_id)?;
        let spec = model_spec(&c)?;
        let keys = crate::commands::read_keys(&c)?;
        let mut model = crate::llm::from_spec_or_any(&spec, &keys).ok_or_else(|| {
            Error::Other("No model configured - add an API key in Settings -> API keys".into())
        })?;
        crate::commands::apply_budget(&mut model, &c, "cheatsheet");
        (run.fields, card, model)
    };
    let c = state.db.lock().unwrap();
    extract_one_paper(&mut model, &c, run_id, item_id, &fields, &card)
}

/// RFC-4180 CSV with a UTF-8 BOM so Excel opens CJK content correctly. The
/// header uses field keys; values come from cells (manual edits included).
/// After the value columns comes one `{key}_证据` column per field — a table
/// others will check must stay checkable (2.4).
pub fn to_csv(result: &ExtractResult) -> String {
    fn esc(s: &str) -> String {
        if s.contains([',', '"', '\n', '\r']) {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.to_string()
        }
    }
    let cell = |item_id: &str, key: &str| {
        result
            .cells
            .iter()
            .find(|c| c.item_id == item_id && c.field_key == key)
    };
    let mut out = String::from("\u{feff}");
    let mut header = vec!["标题".to_string(), "作者".to_string(), "年份".to_string()];
    header.extend(result.run.fields.iter().map(|f| f.key.clone()));
    header.extend(result.run.fields.iter().map(|f| format!("{}_证据", f.key)));
    out.push_str(&header.iter().map(|h| esc(h)).collect::<Vec<_>>().join(","));
    out.push('\n');
    for item in &result.items {
        let mut row = vec![
            item.title.clone(),
            item.authors.clone().unwrap_or_default(),
            item.year.map(|y| y.to_string()).unwrap_or_default(),
        ];
        for f in &result.run.fields {
            row.push(cell(&item.id, &f.key)
                .map(|c| c.value.clone())
                .unwrap_or_default());
        }
        for f in &result.run.fields {
            row.push(cell(&item.id, &f.key)
                .and_then(|c| c.evidence.clone())
                .unwrap_or_default());
        }
        out.push_str(&row.iter().map(|h| esc(h)).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    out
}

/// One sheet carrying the whole run: values, then per-field evidence columns —
/// the same shape as `to_csv`, styled through the shared xlsx writer.
pub fn xlsx_sheets(result: &ExtractResult) -> Vec<crate::office::xlsx::SheetSpec> {
    use crate::office::xlsx::{Cell, SheetSpec};
    let cell = |item_id: &str, key: &str| {
        result
            .cells
            .iter()
            .find(|c| c.item_id == item_id && c.field_key == key)
    };
    let mut headers = vec!["论文".to_string(), "年份".to_string()];
    headers.extend(result.run.fields.iter().map(|f| f.key.clone()));
    headers.extend(result.run.fields.iter().map(|f| format!("{}_证据", f.key)));
    let mut widths = vec![34.0, 8.0];
    widths.extend(result.run.fields.iter().map(|_| 26.0));
    widths.extend(result.run.fields.iter().map(|_| 34.0));
    let rows = result
        .items
        .iter()
        .map(|item| {
            let mut row = vec![
                Cell::Text(item.title.clone()),
                match item.year {
                    Some(y) => Cell::Num(y as f64),
                    None => Cell::Text(String::new()),
                },
            ];
            for f in &result.run.fields {
                row.push(Cell::Text(
                    cell(&item.id, &f.key).map(|c| c.value.clone()).unwrap_or_default(),
                ));
            }
            for f in &result.run.fields {
                row.push(Cell::Text(
                    cell(&item.id, &f.key)
                        .and_then(|c| c.evidence.clone())
                        .unwrap_or_default(),
                ));
            }
            row
        })
        .collect();
    vec![SheetSpec {
        name: "提取表".into(),
        title: Some(result.run.title.clone()),
        headers,
        widths,
        rows,
        chart: None,
    }]
}

// ---- Tauri command surface ----------------------------------------------

#[tauri::command]
pub fn extract_template_list(state: State<crate::db::AppState>) -> Result<Vec<ExtractTemplate>> {
    let c = state.db.lock().unwrap();
    list_templates(&c)
}

#[tauri::command]
pub fn extract_template_save(
    state: State<crate::db::AppState>,
    id: Option<String>,
    name: String,
    description: Option<String>,
    fields: Vec<ExtractField>,
) -> Result<String> {
    let c = state.db.lock().unwrap();
    save_template(&c, id.as_deref(), &name, description.as_deref(), &fields)
}

#[tauri::command]
pub fn extract_template_delete(state: State<crate::db::AppState>, id: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    delete_template(&c, &id)
}

#[tauri::command]
pub fn extract_run_create(
    state: State<crate::db::AppState>,
    template_id: String,
    item_ids: Vec<String>,
    title: Option<String>,
) -> Result<String> {
    let c = state.db.lock().unwrap();
    create_run(&c, &template_id, &item_ids, title.as_deref())
}

#[tauri::command]
pub fn extract_run_list(state: State<crate::db::AppState>) -> Result<Vec<ExtractRun>> {
    let c = state.db.lock().unwrap();
    list_runs(&c)
}

#[tauri::command]
pub fn extract_run_delete(state: State<crate::db::AppState>, id: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    delete_run(&c, &id)
}

#[tauri::command]
pub fn extract_result_get(
    state: State<crate::db::AppState>,
    run_id: String,
) -> Result<ExtractResult> {
    let c = state.db.lock().unwrap();
    get_result(&c, &run_id)
}

#[tauri::command]
pub fn extract_cell_set(
    state: State<crate::db::AppState>,
    run_id: String,
    item_id: String,
    field_key: String,
    value: String,
) -> Result<()> {
    let c = state.db.lock().unwrap();
    set_cell(&c, &run_id, &item_id, &field_key, &value)
}

#[tauri::command]
pub fn extract_result_csv(state: State<crate::db::AppState>, run_id: String) -> Result<String> {
    let c = state.db.lock().unwrap();
    let result = get_result(&c, &run_id)?;
    Ok(to_csv(&result))
}

/// Write the run's table as a CSV file (RFC-4180 + BOM) at `dest` — chosen via
/// the frontend's save dialog. The backend writes it because the app has no fs
/// plugin; every other file export follows the same shape.
#[tauri::command]
pub fn extract_result_csv_file(
    state: State<crate::db::AppState>,
    run_id: String,
    dest: String,
) -> Result<()> {
    let csv = {
        let c = state.db.lock().unwrap();
        to_csv(&get_result(&c, &run_id)?)
    };
    std::fs::write(&dest, csv.as_bytes())
        .map_err(|e| Error::Other(format!("写入 CSV 失败：{e}")))?;
    Ok(())
}

/// Write the run's table as a styled .xlsx workbook (values + evidence cols).
#[tauri::command]
pub fn extract_result_xlsx_file(
    state: State<crate::db::AppState>,
    run_id: String,
    dest: String,
) -> Result<()> {
    let sheets = {
        let c = state.db.lock().unwrap();
        xlsx_sheets(&get_result(&c, &run_id)?)
    };
    crate::office::xlsx::build_workbook(std::path::Path::new(&dest), &sheets)
        .map_err(|e| Error::Other(format!("写入 Excel 失败：{e}")))?;
    Ok(())
}

/// Long-running: one LLM call per paper. Spawned on the blocking pool so the
/// UI thread never waits; cells land incrementally as each paper finishes.
#[tauri::command]
pub async fn extract_run_execute(app: AppHandle, run_id: String) -> Result<()> {
    tauri::async_runtime::spawn_blocking(move || execute_run(&app, &run_id))
        .await
        .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

/// Long-running: re-extract a single paper of an existing run.
#[tauri::command]
pub async fn extract_run_retry(
    app: AppHandle,
    run_id: String,
    item_id: String,
) -> Result<()> {
    tauri::async_runtime::spawn_blocking(move || retry_item(&app, &run_id, &item_id))
        .await
        .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::AppState;
    use crate::papers::PaperInput;

    fn state() -> AppState {
        AppState::in_memory().unwrap()
    }

    fn seed_paper(conn: &Connection, title: &str) -> String {
        crate::papers::import_paper(
            conn,
            &PaperInput {
                title: title.into(),
                authors: Some("A. Author".into()),
                year: Some(2024),
                venue: None,
                doi: None,
                url: None,
                abstract_text: Some("We study RAG.".into()),
                subject_id: None,
                source_id: None,
            },
        )
        .unwrap()
        .id
    }

    fn fields() -> Vec<ExtractField> {
        vec![
            ExtractField { key: "sample".into(), prompt: "样本量与数据集".into() },
            ExtractField { key: "method".into(), prompt: "核心方法".into() },
        ]
    }

    #[test]
    fn template_roundtrip_and_validation() {
        let st = state();
        let c = st.db.lock().unwrap();
        let id = save_template(&c, None, "RCT 提取", Some("随机对照试验"), &fields()).unwrap();
        let list = list_templates(&c).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].fields.len(), 2);
        assert_eq!(list[0].description.as_deref(), Some("随机对照试验"));

        // update in place
        let mut f2 = fields();
        f2.push(ExtractField { key: "effect".into(), prompt: "效应量".into() });
        save_template(&c, Some(&id), "RCT 提取 v2", None, &f2).unwrap();
        assert_eq!(list_templates(&c).unwrap()[0].fields.len(), 3);

        // validation: duplicate keys and empty name are rejected
        let dup = vec![
            ExtractField { key: "sample".into(), prompt: "a".into() },
            ExtractField { key: "sample".into(), prompt: "b".into() },
        ];
        assert!(save_template(&c, None, "dup", None, &dup).is_err());
        assert!(save_template(&c, None, "  ", None, &fields()).is_err());
        assert!(save_template(&c, None, "no fields", None, &[]).is_err());

        delete_template(&c, &id).unwrap();
        assert!(list_templates(&c).unwrap().is_empty());
    }

    #[test]
    fn run_snapshots_fields_and_cells_are_reviewable() {
        let st = state();
        let c = st.db.lock().unwrap();
        let pid = seed_paper(&c, "Paper One");
        let tid = save_template(&c, None, "T", None, &fields()).unwrap();
        let run_id = create_run(&c, &tid, &[pid.clone()], None).unwrap();
        let run = get_run(&c, &run_id).unwrap();
        assert_eq!(run.status, "running");
        assert_eq!(run.fields.len(), 2);
        assert_eq!(run.item_ids, vec![pid.clone()]);

        // model writes a cell, human corrects it — source flips to manual
        cell_upsert(&c, &run_id, &pid, "sample", "n=128 (model)", Some("the sample comprised 128 participants"), None).unwrap();
        set_cell(&c, &run_id, &pid, "sample", "n=132 (fixed)").unwrap();
        let res = get_result(&c, &run_id).unwrap();
        assert_eq!(res.items[0].title, "Paper One");
        let cell = res.cells.iter().find(|c| c.field_key == "sample").unwrap();
        assert_eq!(cell.value, "n=132 (fixed)");
        assert_eq!(cell.source, "manual");
        // a manual edit clears the stale model evidence
        assert!(cell.evidence.is_none());

        // re-run upsert must NOT clobber the manual correction
        cell_upsert(&c, &run_id, &pid, "sample", "n=999 (rerun)", None, None).unwrap();
        let res = get_result(&c, &run_id).unwrap();
        let cell = res.cells.iter().find(|c| c.field_key == "sample").unwrap();
        assert_eq!(cell.value, "n=132 (fixed)");

        // set_cell rejects unknown fields / foreign papers
        assert!(set_cell(&c, &run_id, &pid, "nope", "x").is_err());
        assert!(set_cell(&c, &run_id, "other-paper", "sample", "x").is_err());
    }

    #[test]
    fn csv_export_escapes_and_keeps_column_order() {
        let st = state();
        let c = st.db.lock().unwrap();
        let pid = seed_paper(&c, "A, \"tricky\" title");
        let tid = save_template(&c, None, "T", None, &fields()).unwrap();
        let run_id = create_run(&c, &tid, &[pid], Some("我的提取")).unwrap();
        cell_upsert(&c, &run_id, &get_run(&c, &run_id).unwrap().item_ids[0], "sample", "n=42", Some("n = 42"), None).unwrap();
        let res = get_result(&c, &run_id).unwrap();
        let csv = to_csv(&res);
        assert!(csv.starts_with('\u{feff}'));
        assert!(csv.contains("标题,作者,年份,sample,method,sample_证据,method_证据"));
        assert!(csv.contains("\"A, \"\"tricky\"\" title\""));
        assert!(csv.contains("n=42"));
        assert!(get_run(&c, &run_id).unwrap().title == "我的提取");
    }
}
