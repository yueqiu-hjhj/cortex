//! PRISMA 8-dimension extraction — the systematic-review twin of D8's
//! extraction templates. The 8 fields are FIXED in code (研究问题 / 方法 /
//! 贡献 / 结果 / 局限 / 适用人群 / 使用前提 / 复现成本), so starting a review
//! is one click: no template to configure. MatrixView stays the free-form
//! comparison; this is the review-grade table where every value must be
//! traceable. Traceability is the point of `evidence` + `excerpt_id`: the
//! model must quote one verbatim card sentence per field, and that sentence is
//! either linked to an EXISTING excerpt card or — when it really appears in
//! the card — stored as a new excerpt (note "PRISMA · <维度>"), so the review
//! table and the excerpt pipeline stay the same corpus.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::db::{new_id, now_ms};
use crate::error::{Error, Result};
use crate::extraction::{ExtractField, ExtractItem};

#[derive(Debug, Clone, Serialize)]
pub struct PrismaRun {
    pub id: String,
    pub title: String,
    /// Fixed 8-field list, snapshotted at run time — an old run's table shape
    /// survives future prompt edits to the built-in template.
    pub fields: Vec<ExtractField>,
    pub model: Option<String>,
    pub status: String, // running | done | error
    pub error: Option<String>,
    pub item_ids: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrismaCell {
    pub item_id: String,
    pub field_key: String,
    pub value: String,
    /// Verbatim supporting sentence from the card (model output).
    pub evidence: Option<String>,
    /// Back-link into excerpts — set when the evidence matched an existing
    /// excerpt or was verbatim enough to become one.
    pub excerpt_id: Option<String>,
    pub source: String, // model | manual
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrismaResult {
    pub run: PrismaRun,
    pub items: Vec<ExtractItem>,
    pub cells: Vec<PrismaCell>,
}

// ---- the fixed 8-field template -------------------------------------------

/// (key, 导出列名 / 展示名, 问模型的提取要求). The list is code-owned on
/// purpose: PRISMA 抽取的意义就在于所有综述共用同一张表的形状。
pub fn prisma_fields() -> Vec<ExtractField> {
    [
        ("research_question", "研究问题", "这篇论文要回答的核心研究问题是什么？"),
        ("method", "方法", "用了什么方法 / 模型 / 技术路线？"),
        ("contribution", "贡献", "相对已有工作的主要贡献或创新点是什么？"),
        ("results", "结果", "关键结果是什么？尽量引用具体数字、数据集与指标名称。"),
        ("limitations", "局限", "作者自述或显而易见的局限与失效场景是什么？"),
        ("audience", "适用人群", "什么样的研究者 / 工程师 / 应用场景适合采用这项工作？"),
        ("constraints", "使用前提", "采用它依赖哪些数据、算力、许可证或部署条件？"),
        ("repro_cost", "复现成本", "代码与数据是否公开？估计需要多少工程量与算力才能复现？"),
    ]
    .into_iter()
    .map(|(key, _label, prompt)| ExtractField {
        key: key.to_string(),
        prompt: prompt.to_string(),
    })
    .collect()
}

/// 导出表头（CSV / Markdown）用的中文列名，与 prisma_fields 一一对应。
pub fn prisma_labels() -> Vec<&'static str> {
    vec![
        "研究问题",
        "方法",
        "贡献",
        "结果",
        "局限",
        "适用人群",
        "使用前提",
        "复现成本",
    ]
}

// ---- runs ------------------------------------------------------------------

fn row_to_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<PrismaRun> {
    let rev: String = r.get(1)?;
    Ok(PrismaRun {
        id: r.get(0)?,
        fields: serde_json::from_str(&rev).unwrap_or_default(),
        title: r.get(2)?,
        model: r.get(3)?,
        status: r.get(4)?,
        error: r.get(5)?,
        item_ids: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

const RUN_SELECT: &str =
    "SELECT id, fields_rev, title, model, status, error, item_ids, created_at, updated_at FROM prisma_runs";

pub fn create_run(conn: &Connection, item_ids: &[String], title: Option<&str>) -> Result<String> {
    if item_ids.is_empty() {
        return Err(Error::Other("请至少勾选一篇论文".into()));
    }
    let fields = prisma_fields();
    let id = new_id();
    let now = now_ms();
    let title = match title.map(str::trim).filter(|s| !s.is_empty()) {
        Some(t) => t.to_string(),
        None => format!("PRISMA 8 维 · {} 篇", item_ids.len()),
    };
    conn.execute(
        "INSERT INTO prisma_runs (id, fields_rev, title, status, item_ids, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'running', ?4, ?5, ?6)",
        params![
            id,
            serde_json::to_string(&fields).unwrap_or_else(|_| "[]".into()),
            title,
            serde_json::to_string(item_ids).unwrap_or_else(|_| "[]".into()),
            now,
            now
        ],
    )?;
    Ok(id)
}

pub fn get_run(conn: &Connection, id: &str) -> Result<PrismaRun> {
    conn.query_row(&format!("{RUN_SELECT} WHERE id = ?1"), params![id], row_to_run)
        .map_err(|_| Error::NotFound(format!("prisma run {id}")))
}

pub fn list_runs(conn: &Connection) -> Result<Vec<PrismaRun>> {
    let mut st = conn.prepare(&format!("{RUN_SELECT} ORDER BY created_at DESC LIMIT 100"))?;
    let rows = st.query_map([], row_to_run)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn delete_run(conn: &Connection, id: &str) -> Result<()> {
    let n = conn.execute("DELETE FROM prisma_runs WHERE id = ?1", params![id])?;
    if n == 0 {
        return Err(Error::NotFound(format!("prisma run {id}")));
    }
    Ok(())
}

pub fn get_result(conn: &Connection, run_id: &str) -> Result<PrismaResult> {
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
         FROM prisma_cells WHERE run_id = ?1 ORDER BY item_id, field_key",
    )?;
    let cells = st
        .query_map(params![run_id], |r| {
            Ok(PrismaCell {
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
    Ok(PrismaResult { run, items, cells })
}

/// Store one human correction. Always writes source='manual' — re-runs and
/// retries never overwrite manual cells. The model's evidence/excerpt link is
/// kept: it still records where the original reading came from.
pub fn set_cell(
    conn: &Connection,
    run_id: &str,
    item_id: &str,
    field_key: &str,
    value: &str,
) -> Result<()> {
    let run = get_run(conn, run_id)?;
    if !run.fields.iter().any(|f| f.key == field_key) {
        return Err(Error::Other(format!("PRISMA 模板没有字段 {field_key}")));
    }
    if !run.item_ids.iter().any(|i| i == item_id) {
        return Err(Error::Other("该论文不在本次提取范围内".into()));
    }
    conn.execute(
        "INSERT INTO prisma_cells (run_id, item_id, field_key, value, evidence, excerpt_id, source, updated_at)
         VALUES (?1, ?2, ?3, ?4, NULL, NULL, 'manual', ?5)
         ON CONFLICT(run_id, item_id, field_key)
         DO UPDATE SET value=excluded.value, source='manual', updated_at=excluded.updated_at",
        params![run_id, item_id, field_key, value, now_ms()],
    )?;
    Ok(())
}

// ---- evidence back-linking -------------------------------------------------

/// Collapse all whitespace runs to single spaces — the model may re-wrap lines
/// it quotes from the card.
fn norm_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Resolve one evidence sentence to an excerpt id, preferring an EXISTING
/// excerpt: if the quote matches (either contains the other), the cell links
/// to that card instead of duplicating it. Otherwise the evidence must be
/// verbatim in the card to become a NEW excerpt — a quote the card doesn't
/// contain stays unlinked rather than fabricating provenance.
fn link_evidence(
    conn: &Connection,
    paper_id: &str,
    source_id: Option<&str>,
    field_label: &str,
    card: &str,
    evidence: &str,
    excerpts: &[crate::papers::Excerpt],
) -> Result<Option<String>> {
    let ev = norm_ws(evidence);
    if ev.chars().count() < 8 {
        return Ok(None); // too short to be meaningful provenance
    }
    for e in excerpts {
        let q = norm_ws(&e.quote);
        if q.is_empty() {
            continue;
        }
        if q.contains(&ev) || ev.contains(&q) {
            return Ok(Some(e.id.clone()));
        }
    }
    if !norm_ws(card).contains(&ev) {
        return Ok(None);
    }
    let ex = crate::papers::add_excerpt(
        conn,
        Some(paper_id),
        source_id,
        evidence,
        None,
        Some(&format!("PRISMA · {field_label}")),
        Some("prisma"),
    )?;
    Ok(Some(ex.id))
}

fn field_label(key: &str) -> &'static str {
    match key {
        "research_question" => "研究问题",
        "method" => "方法",
        "contribution" => "贡献",
        "results" => "结果",
        "limitations" => "局限",
        "audience" => "适用人群",
        "constraints" => "使用前提",
        "repro_cost" => "复现成本",
        _ => "PRISMA",
    }
}

// ---- LLM execution ---------------------------------------------------------

fn field_block(fields: &[ExtractField]) -> String {
    fields
        .iter()
        .map(|f| format!("- {}: {}", f.key, f.prompt))
        .collect::<Vec<_>>()
        .join("\n")
}

fn paper_prompt(fields: &[ExtractField], card: &str) -> (String, String) {
    let system = "You are a systematic-review (PRISMA-style) data-extraction assistant. You get \
        ONE paper card (title, metadata, abstract, verbatim excerpts) and a FIXED list of 8 \
        extraction fields. Fill EVERY field for THIS paper using ONLY the card. Never invent \
        facts, numbers, or section names; if the card does not answer a field, output an empty \
        string for both its value and its evidence. For every field you CAN fill, also output \
        `evidence`: ONE sentence copied VERBATIM, character-for-character, from the card that \
        best supports the value. Quote exact terms, dataset/metric/algorithm names and figures. \
        Keep each value to 1-3 sentences (under 60 words). Write each value in the language of \
        that field's prompt. Output ONLY a JSON object of the exact shape \
        {\"values\":{\"<key>\":\"...\"},\"evidence\":{\"<key>\":\"...\"}} with every key present \
        in BOTH maps — no markdown, no commentary.";
    let user = format!("FIELDS:\n{}\n\nPAPER CARD:\n{}", field_block(fields), card);
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
        "INSERT INTO prisma_cells (run_id, item_id, field_key, value, evidence, excerpt_id, source, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'model', ?7)
         ON CONFLICT(run_id, item_id, field_key) DO UPDATE SET
           value=excluded.value, evidence=excluded.evidence, excerpt_id=excluded.excerpt_id,
           source=excluded.source, updated_at=excluded.updated_at
         WHERE prisma_cells.source != 'manual'",
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
        "UPDATE prisma_runs SET status='done', model=?2, error=?3, updated_at=?4 WHERE id=?1",
        params![run_id, model, warn, now_ms()],
    )?;
    Ok(())
}

fn fail_run(conn: &Connection, run_id: &str, err: &str) {
    let _ = conn.execute(
        "UPDATE prisma_runs SET status='error', error=?2, updated_at=?3 WHERE id=?1",
        params![run_id, err, now_ms()],
    );
}

/// Extract one paper's 8 cells + evidence links with the loaded model.
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
        let excerpt_id = if val.is_empty() {
            None
        } else {
            link_evidence(
                conn,
                item_id,
                source_id.as_deref(),
                field_label(&f.key),
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

/// Run the whole extraction: one LLM call per paper, cells land incrementally.
/// A paper whose reply is unusable becomes a per-paper warning on the finished
/// run — one bad reply must not kill a 30-paper review table.
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

/// Re-run ONE paper of an existing run. Manual corrections survive
/// (cell_upsert never touches source='manual').
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

// ---- export ----------------------------------------------------------------

fn md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', "<br>").replace('\r', "")
}

/// Summary table as Markdown — the shape a review paper's appendix expects.
pub fn to_markdown(result: &PrismaResult) -> String {
    let mut header: Vec<String> = vec!["论文".into(), "年份".into()];
    header.extend(prisma_labels().into_iter().map(String::from));
    let mut out = String::new();
    out.push_str(&format!("## {}\n\n", result.run.title));
    out.push_str(&format!("| {} |\n", header.join(" | ")));
    out.push_str(&format!(
        "|{}|\n",
        header.iter().map(|_| " --- ".to_string()).collect::<Vec<_>>().join("|")
    ));
    for item in &result.items {
        let mut row = vec![md_cell(&item.title), item.year.map(|y| y.to_string()).unwrap_or_default()];
        for f in &result.run.fields {
            let v = result
                .cells
                .iter()
                .find(|c| c.item_id == item.id && c.field_key == f.key)
                .map(|c| c.value.clone())
                .unwrap_or_default();
            row.push(md_cell(&v));
        }
        out.push_str(&format!("| {} |\n", row.join(" | ")));
    }
    out
}

/// RFC-4180 CSV with a UTF-8 BOM so Excel opens CJK content correctly.
pub fn to_csv(result: &PrismaResult) -> String {
    fn esc(s: &str) -> String {
        if s.contains([',', '"', '\n', '\r']) {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.to_string()
        }
    }
    let mut out = String::from("\u{feff}");
    let mut header = vec!["标题".to_string(), "作者".to_string(), "年份".to_string()];
    header.extend(prisma_labels().into_iter().map(String::from));
    out.push_str(&header.iter().map(|h| esc(h)).collect::<Vec<_>>().join(","));
    out.push('\n');
    for item in &result.items {
        let mut row = vec![
            item.title.clone(),
            item.authors.clone().unwrap_or_default(),
            item.year.map(|y| y.to_string()).unwrap_or_default(),
        ];
        for f in &result.run.fields {
            let v = result
                .cells
                .iter()
                .find(|c| c.item_id == item.id && c.field_key == f.key)
                .map(|c| c.value.clone())
                .unwrap_or_default();
            row.push(v);
        }
        out.push_str(&row.iter().map(|h| esc(h)).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    out
}

// ---- Tauri command surface -------------------------------------------------

#[tauri::command]
pub fn prisma_run_create(
    state: State<crate::db::AppState>,
    item_ids: Vec<String>,
    title: Option<String>,
) -> Result<String> {
    let c = state.db.lock().unwrap();
    create_run(&c, &item_ids, title.as_deref())
}

#[tauri::command]
pub fn prisma_run_list(state: State<crate::db::AppState>) -> Result<Vec<PrismaRun>> {
    let c = state.db.lock().unwrap();
    list_runs(&c)
}

#[tauri::command]
pub fn prisma_run_delete(state: State<crate::db::AppState>, id: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    delete_run(&c, &id)
}

#[tauri::command]
pub fn prisma_result_get(state: State<crate::db::AppState>, run_id: String) -> Result<PrismaResult> {
    let c = state.db.lock().unwrap();
    get_result(&c, &run_id)
}

#[tauri::command]
pub fn prisma_cell_set(
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
pub fn prisma_result_csv_file(
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

#[tauri::command]
pub fn prisma_result_md_file(
    state: State<crate::db::AppState>,
    run_id: String,
    dest: String,
) -> Result<()> {
    let md = {
        let c = state.db.lock().unwrap();
        to_markdown(&get_result(&c, &run_id)?)
    };
    std::fs::write(&dest, md.as_bytes())
        .map_err(|e| Error::Other(format!("写入 Markdown 失败：{e}")))?;
    Ok(())
}

/// Long-running: one LLM call per paper. Spawned on the blocking pool so the
/// UI thread never waits; cells land incrementally as each paper finishes.
#[tauri::command]
pub async fn prisma_run_execute(app: AppHandle, run_id: String) -> Result<()> {
    tauri::async_runtime::spawn_blocking(move || execute_run(&app, &run_id))
        .await
        .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

/// Long-running: re-extract a single paper of an existing run.
#[tauri::command]
pub async fn prisma_run_retry(app: AppHandle, run_id: String, item_id: String) -> Result<()> {
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

    fn seed_paper(conn: &Connection, title: &str, abstract_text: &str) -> String {
        crate::papers::import_paper(
            conn,
            &PaperInput {
                title: title.into(),
                authors: Some("A. Author".into()),
                year: Some(2024),
                venue: None,
                doi: None,
                url: None,
                abstract_text: Some(abstract_text.into()),
                subject_id: None,
                source_id: None,
            },
        )
        .unwrap()
        .id
    }

    #[test]
    fn labels_pair_with_field_keys() {
        let keys = prisma_fields();
        let labels = prisma_labels();
        assert_eq!(keys.len(), labels.len());
        for (f, label) in keys.iter().zip(labels.iter()) {
            assert_eq!(field_label(&f.key), *label, "key {} 与导出列名错位", f.key);
        }
    }

    #[test]
    fn run_snapshot_and_manual_cells_survive_reruns() {
        let st = state();
        let c = st.db.lock().unwrap();
        let pid = seed_paper(&c, "Paper One", "We study RAG.");
        let run_id = create_run(&c, &[pid.clone()], None).unwrap();
        let run = get_run(&c, &run_id).unwrap();
        assert_eq!(run.status, "running");
        assert_eq!(run.fields.len(), 8);
        assert_eq!(run.item_ids, vec![pid.clone()]);
        assert_eq!(run.fields[0].key, "research_question");

        // model writes, human corrects, model re-runs — manual survives
        cell_upsert(&c, &run_id, &pid, "method", "RAG (model)", None, None).unwrap();
        set_cell(&c, &run_id, &pid, "method", "RAG + rerank (fixed)").unwrap();
        cell_upsert(&c, &run_id, &pid, "method", "overwritten?", None, None).unwrap();
        let res = get_result(&c, &run_id).unwrap();
        let cell = res.cells.iter().find(|c| c.field_key == "method").unwrap();
        assert_eq!(cell.value, "RAG + rerank (fixed)");
        assert_eq!(cell.source, "manual");

        // validation: foreign paper / unknown field
        assert!(set_cell(&c, &run_id, "other", "method", "x").is_err());
        assert!(set_cell(&c, &run_id, &pid, "nope", "x").is_err());
        assert!(create_run(&c, &[], None).is_err());

        delete_run(&c, &run_id).unwrap();
        assert!(list_runs(&c).unwrap().is_empty());
    }

    #[test]
    fn evidence_links_existing_excerpt_then_creates_verbatim_ones() {
        let st = state();
        let c = st.db.lock().unwrap();
        let pid = seed_paper(&c, "Paper Two", "BM25 beats dense retrieval on this benchmark.");
        let existing = crate::papers::add_excerpt(
            &c,
            Some(&pid),
            None,
            "BM25 beats dense retrieval on this benchmark.",
            Some(3),
            Some("用户划的重点"),
            None,
        )
        .unwrap();

        // 1) evidence matching the existing excerpt links to IT (no duplicate)
        let excerpts = crate::papers::list_excerpts(&c, Some(&pid)).unwrap();
        let card = "PAPER CARD\nBM25 beats dense retrieval on this benchmark.";
        let id1 = link_evidence(
            &c,
            &pid,
            None,
            "结果",
            card,
            "BM25 beats dense retrieval on this benchmark.",
            &excerpts,
        )
        .unwrap();
        assert_eq!(id1.as_deref(), Some(existing.id.as_str()));
        assert_eq!(crate::papers::list_excerpts(&c, Some(&pid)).unwrap().len(), 1);

        // 2) evidence verbatim in the card but not an excerpt yet -> new excerpt
        let card2 = "PAPER CARD\nThe corpus has 8.8M passages and evaluation uses MRR@10.";
        let id2 = link_evidence(
            &c,
            &pid,
            None,
            "结果",
            card2,
            "The corpus has 8.8M passages and evaluation uses MRR@10.",
            &excerpts,
        )
        .unwrap();
        assert!(id2.is_some());
        let after = crate::papers::list_excerpts(&c, Some(&pid)).unwrap();
        assert_eq!(after.len(), 2);
        let created = after.iter().find(|e| Some(&e.id) == id2.as_ref()).unwrap();
        assert_eq!(created.note.as_deref(), Some("PRISMA · 结果"));
        assert_eq!(created.tags.as_deref(), Some("prisma"));

        // 2b) re-linking the SAME evidence reuses the created card (no dup)
        let excerpts_now = crate::papers::list_excerpts(&c, Some(&pid)).unwrap();
        let id2b = link_evidence(
            &c,
            &pid,
            None,
            "结果",
            card2,
            "The corpus has 8.8M passages and evaluation uses MRR@10.",
            &excerpts_now,
        )
        .unwrap();
        assert_eq!(id2b, id2);
        assert_eq!(crate::papers::list_excerpts(&c, Some(&pid)).unwrap().len(), 2);

        // 3) hallucinated evidence (not in card) stays unlinked
        let id3 = link_evidence(
            &c,
            &pid,
            None,
            "方法",
            card2,
            "The model was trained on 512 TPUv4 chips for three weeks.",
            &excerpts_now,
        )
        .unwrap();
        assert!(id3.is_none());

        // 4) tiny evidence is not provenance
        let id4 = link_evidence(&c, &pid, None, "方法", card2, "BM25", &excerpts_now).unwrap();
        assert!(id4.is_none());
    }

    #[test]
    fn exports_escape_and_keep_column_order() {
        let st = state();
        let c = st.db.lock().unwrap();
        let pid = seed_paper(&c, "A, \"tricky\" | title", "We study RAG.");
        let run_id = create_run(&c, &[pid], Some("综述表")).unwrap();
        let run = get_run(&c, &run_id).unwrap();
        cell_upsert(&c, &run_id, &run.item_ids[0], "method", "RAG\nwith | pipe", None, None)
            .unwrap();
        let res = get_result(&c, &run_id).unwrap();

        let csv = to_csv(&res);
        assert!(csv.starts_with('\u{feff}'));
        assert!(csv.contains("标题,作者,年份,研究问题,方法,贡献,结果,局限,适用人群,使用前提,复现成本"));
        assert!(csv.contains("\"A, \"\"tricky\"\" | title\""));
        assert!(csv.contains("RAG\nwith | pipe"));

        let md = to_markdown(&res);
        assert!(md.starts_with("## 综述表"));
        assert!(md.contains("| 论文 | 年份 | 研究问题 |"));
        assert!(md.contains("A, \"tricky\" \\| title"));
        assert!(md.contains("RAG<br>with \\| pipe"));
    }
}
