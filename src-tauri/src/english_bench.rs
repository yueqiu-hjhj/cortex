//! English bench — 知源's 应试英语: read ONE passage six ways (close reading),
//! then train the two skills exams actually score (culture-loaded translation
//! and high-scoring phrases).
//!
//! What Cortex already had: word-level translation on selection (SourceViewer)
//! and paragraph bilingual view — "ask as you read". What was missing: "read
//! it through", the six-dimension pass a 考研 reader does over a passage:
//! a faithful full translation, the gist + attitude radar, long-sentence
//! surgery, synonym upgrades, familiar words used strangely, and the trap
//! questions the exam would ask. Plus translation training's three drills.
//!
//! Shape: one run per (kind, source); each dimension is its own item with its
//! own status. Six independent calls, six independent failures — a broken
//! 长难句拆解 must not take the translation down with it (same rule as
//! slides: a bad page degrades to a stub, it does not burn the deck).

use crate::db::{new_id, now_ms, AppState};
use crate::error::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

/// How much of the source text goes to the model. A 考研 passage is ~400
/// words; an imported book chapter is not. Clamping is honest as long as the
/// UI SAYS it happened — `truncated` rides on the run for exactly that.
pub const TEXT_BUDGET: usize = 12000;

#[derive(Debug, Clone, Serialize)]
pub struct BenchRun {
    pub id: String,
    /// close | translate
    pub kind: String,
    pub source_id: String,
    pub source_name: String,
    pub status: String, // running | done | error
    pub error: Option<String>,
    pub model: Option<String>,
    pub truncated: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchItem {
    pub id: String,
    pub dimension: String,
    pub ord: i64,
    /// pending | done | error
    pub status: String,
    pub error: Option<String>,
    /// Parsed JSON for this dimension (None while pending / on error).
    pub payload: Option<serde_json::Value>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchResult {
    pub run: BenchRun,
    pub items: Vec<BenchItem>,
}

/// The six close-reading dimensions, in display order.
pub const CLOSE_DIMENSIONS: &[(&str, &str)] = &[
    ("translation", "全文精译"),
    ("gist", "主旨与态度"),
    ("syntax", "长难句拆解"),
    ("synonyms", "同义替换"),
    ("familiar", "熟词生义"),
    ("traps", "命题陷阱"),
];

/// The three translation-training drills.
pub const TRANSLATE_DIMENSIONS: &[(&str, &str)] = &[
    ("culture_words", "文化负载词"),
    ("phrases", "高分短语"),
    ("checklist", "评分自查"),
];

pub fn dimensions_of(kind: &str) -> &'static [(&'static str, &'static str)] {
    match kind {
        "translate" => TRANSLATE_DIMENSIONS,
        _ => CLOSE_DIMENSIONS,
    }
}

pub fn dimension_label<'a>(kind: &str, dimension: &'a str) -> &'a str {
    dimensions_of(kind)
        .iter()
        .find(|(d, _)| *d == dimension)
        .map(|(_, l)| *l)
        .unwrap_or(dimension)
}

// ---- run plumbing -----------------------------------------------------------

pub fn create_run(conn: &Connection, kind: &str, source_id: &str) -> Result<String> {
    if !matches!(kind, "close" | "translate") {
        return Err(Error::Other(format!("未知精读类型：{kind}")));
    }
    let name: Option<String> = conn
        .query_row("SELECT name FROM sources WHERE id=?1", [source_id], |r| {
            r.get(0)
        })
        .optional()?;
    if name.is_none() {
        return Err(Error::NotFound(format!("source {source_id}")));
    }
    let id = new_id();
    let now = now_ms();
    conn.execute(
        "INSERT INTO bench_runs (id, kind, source_id, status, created_at, updated_at)
         VALUES (?1,?2,?3,'running',?4,?4)",
        params![id, kind, source_id, now],
    )?;
    for (ord, (dim, _)) in dimensions_of(kind).iter().enumerate() {
        conn.execute(
            "INSERT INTO bench_items (id, run_id, dimension, ord, status, created_at, updated_at)
             VALUES (?1,?2,?3,?4,'pending',?5,?5)",
            params![new_id(), id, dim, ord as i64, now],
        )?;
    }
    Ok(id)
}

/// Latest run for (kind, source), items included. None if never run.
pub fn get_latest(conn: &Connection, kind: &str, source_id: &str) -> Result<Option<BenchResult>> {
    let run_id: Option<String> = conn
        .query_row(
            "SELECT id FROM bench_runs WHERE kind=?1 AND source_id=?2 ORDER BY created_at DESC LIMIT 1",
            params![kind, source_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(run_id) = run_id else { return Ok(None) };
    Ok(Some(get_result(conn, &run_id)?))
}

pub fn get_result(conn: &Connection, run_id: &str) -> Result<BenchResult> {
    let run = conn
        .query_row(
            "SELECT r.id, r.kind, r.source_id, s.name, r.status, r.error, r.model, r.truncated,
                    r.created_at, r.updated_at
               FROM bench_runs r JOIN sources s ON s.id = r.source_id WHERE r.id=?1",
            [run_id],
            |r| {
                Ok(BenchRun {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    source_id: r.get(2)?,
                    source_name: r.get(3)?,
                    status: r.get(4)?,
                    error: r.get(5)?,
                    model: r.get(6)?,
                    truncated: r.get::<_, i64>(7)? != 0,
                    created_at: r.get(8)?,
                    updated_at: r.get(9)?,
                })
            },
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("bench run {run_id}")),
            other => Error::Db(other),
        })?;
    let mut st = conn.prepare(
        "SELECT id, dimension, ord, status, error, payload, created_at
           FROM bench_items WHERE run_id=?1 ORDER BY ord",
    )?;
    let items = st
        .query_map(params![run_id], |r| {
            let raw: Option<String> = r.get(5)?;
            Ok(BenchItem {
                id: r.get(0)?,
                dimension: r.get(1)?,
                ord: r.get(2)?,
                status: r.get(3)?,
                error: r.get(4)?,
                payload: raw.and_then(|s| serde_json::from_str(&s).ok()),
                created_at: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(BenchResult { run, items })
}

pub fn delete_run(conn: &Connection, run_id: &str) -> Result<()> {
    conn.execute("DELETE FROM bench_items WHERE run_id=?1", [run_id])?;
    conn.execute("DELETE FROM bench_runs WHERE id=?1", [run_id])?;
    Ok(())
}

// ---- the source text ---------------------------------------------------------

/// Clamp a source's text to the model budget. Returns (text, truncated).
/// Splits on a char boundary so a CJK source cannot be cut mid-codepoint.
pub fn clamp_text(content: &str) -> (String, bool) {
    let n = content.chars().count();
    if n <= TEXT_BUDGET {
        return (content.to_string(), false);
    }
    let head: String = content.chars().take(TEXT_BUDGET).collect();
    (head, true)
}

fn source_text(conn: &Connection, source_id: &str) -> Result<(String, bool)> {
    let content: Option<String> = conn
        .query_row("SELECT content FROM sources WHERE id=?1", [source_id], |r| {
            r.get(0)
        })
        .optional()?
        .flatten();
    match content {
        Some(c) if !c.trim().is_empty() => Ok(clamp_text(&c)),
        _ => Err(Error::Other(
            "这个来源还没有可解析的正文（先等解析完成，或换一个文本来源）".into(),
        )),
    }
}

// ---- prompts -----------------------------------------------------------------

/// One SYSTEM per kind: the user message names the dimension, the response is
/// that dimension's JSON only. Splitting per dimension keeps every call
/// small enough to finish and lets one bad dimension fail alone.
pub const CLOSE_SYS: &str = "You are a 考研英语 reading coach. You are given ONE English passage. \
The user names ONE dimension of a six-dimension close reading; you return ONLY that dimension's \
raw JSON, no prose. Shapes: \
translation: {\"text\":string} — a faithful, fluent Chinese translation of the WHOLE passage. \
gist: {\"main_idea\":string,\"attitude\":\"support\"|\"neutral\"|\"critical\"|\"mixed\",\"confidence\":number 0-1,\"points\":[string]} — \
main idea in one sentence, the author's attitude, 2-4 key points. \
syntax: {\"sentences\":[{\"original\":string,\"structure\":string,\"translation\":string}]} — the 2-4 \
hardest sentences: original VERBATIM, structure = 主干与修饰层次拆解 (e.g. \"主句 X；which 定从修 Y；\
句末介词短语作状语\"), translation = 地道汉语. Never invent a sentence that is not in the passage. \
synonyms: {\"pairs\":[{\"original\":string,\"upgrade\":string,\"context\":string}]} — 2-5 phrases in the \
passage that a stronger writer would upgrade; original and context VERBATIM from the passage, upgrade \
= the more formal/preccise alternative. \
familiar: {\"words\":[{\"word\":string,\"common\":string,\"here\":string,\"sentence\":string}]} — 2-4 \
words whose meaning HERE differs from the common one; sentence VERBATIM. \
traps: {\"questions\":[{\"question\":string,\"options\":[string,string,string,string],\"answer\":0-3,\
\"trap\":string,\"explain\":string}]} — 3-5 exam-style questions a test writer would build from this \
passage, each naming the trap it sets (偷换概念/过度推断/张冠李戴/以偏概全/正反颠倒). \
If the passage does not support a dimension, return the empty array for it — never invent content.";

pub const TRANSLATE_SYS: &str = "You are a 考研翻译 trainer. You are given ONE English passage \
(the user's own material, possibly mixed with Chinese). The user names ONE drill; you return ONLY \
that drill's raw JSON, no prose. Shapes: \
culture_words: {\"words\":[{\"phrase\":string,\"literal\":string,\"proper\":string,\"example\":string}]} — \
2-5 culture-loaded expressions FROM or suggested BY the passage: literal = the literal-but-wrong \
translation a learner would produce, proper = the idiomatic one, example = a short bilingual example. \
phrases: {\"items\":[{\"scene\":string,\"plain\":string,\"better\":string}]} — 2-5 writing upgrades \
relevant to the passage's register: scene = when to use, plain = the mediocre phrasing, better = the \
high-scoring one. \
checklist: {\"dimensions\":[{\"name\":string,\"what\":string}]} — the 5-6 dimensions a translation \
self-score should cover (信/达/雅 and the concrete things that lose marks), each with a one-line \
\"what to check\". Base them on the passage's actual difficulty. \
If the passage does not support a drill, return the empty array — never invent content.";

fn sys_for(kind: &str) -> &'static str {
    match kind {
        "translate" => TRANSLATE_SYS,
        _ => CLOSE_SYS,
    }
}

fn user_for(kind: &str, dimension: &str, text: &str) -> String {
    let label = dimension_label(kind, dimension);
    let shape_hint = match (kind, dimension) {
        ("close", "translation") => "Return {\"text\": …} now.",
        ("close", "gist") => "Return {\"main_idea\":…,\"attitude\":…,\"confidence\":…,\"points\":[…]} now.",
        ("close", "syntax") => "Return {\"sentences\":[…]} now.",
        ("close", "synonyms") => "Return {\"pairs\":[…]} now.",
        ("close", "familiar") => "Return {\"words\":[…]} now.",
        ("close", "traps") => "Return {\"questions\":[…]} now.",
        ("translate", "culture_words") => "Return {\"words\":[…]} now.",
        ("translate", "phrases") => "Return {\"items\":[…]} now.",
        ("translate", "checklist") => "Return {\"dimensions\":[…]} now.",
        _ => "Return the JSON now.",
    };
    format!(
        "PASSAGE:\n{text}\n\nDIMENSION: {label} ({dimension})\n{shape_hint}"
    )
}

// ---- per-dimension validation -------------------------------------------------

/// Validate one dimension's payload. Returns the cleaned payload (or an error
/// naming what was wrong). Every string that claims to quote the passage is
/// checked against it — a fabricated quote is worse than a missing row.
fn validate_dimension(
    kind: &str,
    dimension: &str,
    payload: &serde_json::Value,
    passage_lower: &str,
) -> Result<serde_json::Value> {
    let s = |v: &serde_json::Value| v.as_str().unwrap_or("").trim().to_string();
    let arr = |v: &serde_json::Value| v.as_array().cloned().unwrap_or_default();
    match (kind, dimension) {
        ("close", "translation") => {
            let text = s(payload.get("text").unwrap_or(&serde_json::Value::Null));
            if text.chars().count() < 20 {
                return Err(Error::Other("译文太短，不像全文精译".into()));
            }
            Ok(serde_json::json!({ "text": text }))
        }
        ("close", "gist") => {
            let main = s(payload.get("main_idea").unwrap_or(&serde_json::Value::Null));
            if main.is_empty() {
                return Err(Error::Other("主旨为空".into()));
            }
            let attitude = s(payload.get("attitude").unwrap_or(&serde_json::Value::Null));
            let attitude = if matches!(attitude.as_str(), "support" | "neutral" | "critical" | "mixed") {
                attitude
            } else {
                "neutral".into()
            };
            let points: Vec<String> = arr(payload.get("points").unwrap_or(&serde_json::Value::Null))
                .iter()
                .map(&s)
                .filter(|p| !p.is_empty())
                .take(6)
                .collect();
            Ok(serde_json::json!({
                "main_idea": main,
                "attitude": attitude,
                "confidence": payload.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.5).clamp(0.0, 1.0),
                "points": points,
            }))
        }
        ("close", "syntax") => {
            let mut out = Vec::new();
            for item in arr(payload.get("sentences").unwrap_or(&serde_json::Value::Null)) {
                let original = s(item.get("original").unwrap_or(&serde_json::Value::Null));
                let structure = s(item.get("structure").unwrap_or(&serde_json::Value::Null));
                let translation = s(item.get("translation").unwrap_or(&serde_json::Value::Null));
                if original.is_empty() || structure.is_empty() || translation.is_empty() {
                    continue;
                }
                // The sentence must exist in the passage — a "long sentence"
                // the model composed itself teaches the wrong structure.
                if !passage_lower.contains(&original.to_lowercase()) {
                    continue;
                }
                out.push(serde_json::json!({
                    "original": original, "structure": structure, "translation": translation,
                }));
                if out.len() >= 5 {
                    break;
                }
            }
            if out.is_empty() {
                return Err(Error::Other("没有可验证的长难句".into()));
            }
            Ok(serde_json::json!({ "sentences": out }))
        }
        ("close", "synonyms") => {
            let mut out = Vec::new();
            for item in arr(payload.get("pairs").unwrap_or(&serde_json::Value::Null)) {
                let original = s(item.get("original").unwrap_or(&serde_json::Value::Null));
                let upgrade = s(item.get("upgrade").unwrap_or(&serde_json::Value::Null));
                if original.is_empty() || upgrade.is_empty() {
                    continue;
                }
                // original must be in the passage (the thing being upgraded);
                // the upgrade is the model's contribution and needs no quote.
                if !passage_lower.contains(&original.to_lowercase()) {
                    continue;
                }
                out.push(serde_json::json!({
                    "original": original, "upgrade": upgrade,
                    "context": s(item.get("context").unwrap_or(&serde_json::Value::Null)),
                }));
                if out.len() >= 6 {
                    break;
                }
            }
            if out.is_empty() {
                return Err(Error::Other("没有可验证的替换对".into()));
            }
            Ok(serde_json::json!({ "pairs": out }))
        }
        ("close", "familiar") => {
            let mut out = Vec::new();
            for item in arr(payload.get("words").unwrap_or(&serde_json::Value::Null)) {
                let word = s(item.get("word").unwrap_or(&serde_json::Value::Null));
                let here = s(item.get("here").unwrap_or(&serde_json::Value::Null));
                let sentence = s(item.get("sentence").unwrap_or(&serde_json::Value::Null));
                if word.is_empty() || here.is_empty() {
                    continue;
                }
                if !sentence.is_empty() && !passage_lower.contains(&sentence.to_lowercase()) {
                    continue;
                }
                out.push(serde_json::json!({
                    "word": word, "common": s(item.get("common").unwrap_or(&serde_json::Value::Null)),
                    "here": here, "sentence": sentence,
                }));
                if out.len() >= 6 {
                    break;
                }
            }
            if out.is_empty() {
                return Err(Error::Other("没有熟词生义".into()));
            }
            Ok(serde_json::json!({ "words": out }))
        }
        ("close", "traps") => {
            let mut out = Vec::new();
            for item in arr(payload.get("questions").unwrap_or(&serde_json::Value::Null)) {
                let question = s(item.get("question").unwrap_or(&serde_json::Value::Null));
                let options: Vec<String> = arr(item.get("options").unwrap_or(&serde_json::Value::Null))
                    .iter()
                    .map(&s)
                    .collect();
                let answer = item.get("answer").and_then(|v| v.as_i64()).unwrap_or(-1);
                if question.is_empty() || options.len() != 4 || options.iter().any(|o| o.is_empty()) {
                    continue;
                }
                if !(0..=3).contains(&answer) {
                    continue; // an answer key that points nowhere is not a question
                }
                out.push(serde_json::json!({
                    "question": question, "options": options, "answer": answer,
                    "trap": s(item.get("trap").unwrap_or(&serde_json::Value::Null)),
                    "explain": s(item.get("explain").unwrap_or(&serde_json::Value::Null)),
                }));
                if out.len() >= 6 {
                    break;
                }
            }
            if out.is_empty() {
                return Err(Error::Other("没有有效的题目".into()));
            }
            Ok(serde_json::json!({ "questions": out }))
        }
        ("translate", "culture_words") => {
            let mut out = Vec::new();
            for item in arr(payload.get("words").unwrap_or(&serde_json::Value::Null)) {
                let phrase = s(item.get("phrase").unwrap_or(&serde_json::Value::Null));
                let proper = s(item.get("proper").unwrap_or(&serde_json::Value::Null));
                if phrase.is_empty() || proper.is_empty() {
                    continue;
                }
                out.push(serde_json::json!({
                    "phrase": phrase,
                    "literal": s(item.get("literal").unwrap_or(&serde_json::Value::Null)),
                    "proper": proper,
                    "example": s(item.get("example").unwrap_or(&serde_json::Value::Null)),
                }));
                if out.len() >= 6 {
                    break;
                }
            }
            if out.is_empty() {
                return Err(Error::Other("没有文化负载词".into()));
            }
            Ok(serde_json::json!({ "words": out }))
        }
        ("translate", "phrases") => {
            let mut out = Vec::new();
            for item in arr(payload.get("items").unwrap_or(&serde_json::Value::Null)) {
                let plain = s(item.get("plain").unwrap_or(&serde_json::Value::Null));
                let better = s(item.get("better").unwrap_or(&serde_json::Value::Null));
                if plain.is_empty() || better.is_empty() {
                    continue;
                }
                out.push(serde_json::json!({
                    "scene": s(item.get("scene").unwrap_or(&serde_json::Value::Null)),
                    "plain": plain, "better": better,
                }));
                if out.len() >= 6 {
                    break;
                }
            }
            if out.is_empty() {
                return Err(Error::Other("没有高分短语".into()));
            }
            Ok(serde_json::json!({ "items": out }))
        }
        ("translate", "checklist") => {
            let mut out = Vec::new();
            for item in arr(payload.get("dimensions").unwrap_or(&serde_json::Value::Null)) {
                let name = s(item.get("name").unwrap_or(&serde_json::Value::Null));
                if name.is_empty() {
                    continue;
                }
                out.push(serde_json::json!({
                    "name": name,
                    "what": s(item.get("what").unwrap_or(&serde_json::Value::Null)),
                }));
                if out.len() >= 8 {
                    break;
                }
            }
            if out.is_empty() {
                return Err(Error::Other("评分自查为空".into()));
            }
            Ok(serde_json::json!({ "dimensions": out }))
        }
        _ => Err(Error::Other(format!("未知维度：{kind}/{dimension}"))),
    }
}

// ---- execution ----------------------------------------------------------------

/// Run every pending dimension of a run, one call each, sequentially. Each
/// dimension's success/failure is recorded on its own item; the run status is
/// the honest sum afterwards (error if ALL failed, done otherwise — partial
/// results are results, and the UI labels the failed dimensions).
pub fn execute(app: &AppHandle, run_id: &str) -> Result<()> {
    let state = app.state::<AppState>();
    let (kind, source_id) = {
        let c = state.db.lock().unwrap();
        let run = get_result(&c, run_id)?;
        if run.run.status != "running" {
            return Err(Error::Other("该次精读已结束，请重新发起".into()));
        }
        (run.run.kind.clone(), run.run.source_id.clone())
    };
    let (text, truncated) = {
        let c = state.db.lock().unwrap();
        source_text(&c, &source_id)?
    };
    {
        let c = state.db.lock().unwrap();
        c.execute(
            "UPDATE bench_runs SET truncated=?2, updated_at=?3 WHERE id=?1",
            params![run_id, truncated as i64, now_ms()],
        )?;
    }
    let passage_lower = text.to_lowercase();

    let (spec, keys) = {
        let c = state.db.lock().unwrap();
        (
            crate::commands::model_spec_for(&c, "chat"),
            crate::commands::read_keys(&c)?,
        )
    };
    let sys = sys_for(&kind).to_string();

    let dims: Vec<(String, i64)> = {
        let c = state.db.lock().unwrap();
        let mut st = c.prepare(
            "SELECT dimension, ord FROM bench_items WHERE run_id=?1 AND status='pending' ORDER BY ord",
        )?;
        let rows = st
            .query_map(params![run_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };

    let mut model_name = String::new();
    for (dimension, _ord) in &dims {
        let mut model = match crate::llm::from_spec_or_any(&spec, &keys) {
            Some(m) => m,
            None => {
                let c = state.db.lock().unwrap();
                c.execute(
                    "UPDATE bench_runs SET status='error', error='没有配置模型 — 请在设置里添加 API key', updated_at=?2 WHERE id=?1",
                    params![run_id, now_ms()],
                )?;
                return Err(Error::Other(
                    "No model configured - add an API key in Settings -> API keys".into(),
                ));
            }
        };
        crate::commands::apply_budget(&mut model, &state.db.lock().unwrap(), "chat");
        model_name = model.name();
        let user = user_for(&kind, dimension, &text);
        model.set_max_tokens(6000);
        let raw = model.complete(&sys, &user);
        {
            let c = state.db.lock().unwrap();
            crate::tokens::record(&c, "bench", &model.name(), &format!("{sys}\n{user}"), raw.as_deref().unwrap_or(""), None);
        }
        match raw {
            Ok(r) => match crate::llm::extract_json(&r) {
                Ok(payload) => match validate_dimension(&kind, dimension, &payload, &passage_lower) {
                    Ok(clean) => {
                        let c = state.db.lock().unwrap();
                        c.execute(
                            "UPDATE bench_items SET status='done', error=NULL, payload=?2, updated_at=?3
                              WHERE run_id=?1 AND dimension=?4",
                            params![run_id, clean.to_string(), now_ms(), dimension],
                        )?;
                    }
                    Err(e) => {
                        let c = state.db.lock().unwrap();
                        c.execute(
                            "UPDATE bench_items SET status='error', error=?2, updated_at=?3
                              WHERE run_id=?1 AND dimension=?4",
                            params![run_id, format!("校验失败：{e}"), now_ms(), dimension],
                        )?;
                    }
                },
                Err(_) => {
                    let c = state.db.lock().unwrap();
                    c.execute(
                        "UPDATE bench_items SET status='error', error='模型输出无法解析为 JSON', updated_at=?3
                          WHERE run_id=?1 AND dimension=?2",
                        params![run_id, dimension, now_ms()],
                    )?;
                }
            },
            Err(e) => {
                let c = state.db.lock().unwrap();
                c.execute(
                    "UPDATE bench_items SET status='error', error=?2, updated_at=?3
                      WHERE run_id=?1 AND dimension=?4",
                    params![run_id, format!("模型调用失败：{e}"), now_ms(), dimension],
                )?;
            }
        }
    }

    {
        let c = state.db.lock().unwrap();
        let (done, total): (i64, i64) = c.query_row(
            "SELECT SUM(status='done'), COUNT(*) FROM bench_items WHERE run_id=?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let status = if done == 0 { "error" } else { "done" };
        let err = if done == 0 {
            Some("所有维度都失败了——检查模型配置后重试".into())
        } else if done < total {
            Some(format!("{}/{} 个维度完成，其余可单独重试", done, total))
        } else {
            None
        };
        c.execute(
            "UPDATE bench_runs SET status=?2, error=?3, model=?4, updated_at=?5 WHERE id=?1",
            params![run_id, status, err, model_name, now_ms()],
        )?;
    }
    Ok(())
}

/// Re-run ONE failed dimension of a run (the retry button on a broken card).
pub fn retry_item(app: &AppHandle, run_id: &str, dimension: &str) -> Result<()> {
    let state = app.state::<AppState>();
    {
        let c = state.db.lock().unwrap();
        c.execute(
            "UPDATE bench_items SET status='pending', error=NULL, updated_at=?2
              WHERE run_id=?1 AND dimension=?3",
            params![run_id, now_ms(), dimension],
        )?;
        c.execute(
            "UPDATE bench_runs SET status='running', updated_at=?2 WHERE id=?1",
            params![run_id, now_ms()],
        )?;
    }
    execute(app, run_id)
}

// ---- synonym → vocab deck ------------------------------------------------------

/// Push one synonym pair into the subject's 词汇本 (the same deck 语料采词
/// writes to), so it enters the SRS queue like any other term. `original` is
/// the card front (what the passage said), the upgrade is the definition.
pub fn save_synonym(conn: &Connection, source_id: &str, original: &str, upgrade: &str, context: &str) -> Result<()> {
    let subject_id: Option<String> = conn
        .query_row("SELECT subject_id FROM sources WHERE id=?1", [source_id], |r| {
            r.get(0)
        })?;
    let Some(subject_id) = subject_id else {
        return Err(Error::Other("这个来源没有归属学科，无法进词汇本".into()));
    };
    if original.trim().is_empty() || upgrade.trim().is_empty() {
        return Err(Error::Other("替换对不完整".into()));
    }
    let cand = crate::vocab::MineCandidate {
        word: original.trim().to_string(),
        normalized: original.trim().to_lowercase(),
        definition: format!("升级表达：{}", upgrade.trim()),
        sentence: if context.trim().is_empty() {
            original.trim().to_string()
        } else {
            context.trim().to_string()
        },
    };
    crate::vocab::save_mined(conn, &subject_id, std::slice::from_ref(&cand))?;
    Ok(())
}

// ---- markdown ------------------------------------------------------------------

pub fn to_markdown(result: &BenchResult) -> String {
    let r = &result.run;
    let mut out = format!(
        "# {} · {}\n\n",
        if r.kind == "translate" { "翻译训练" } else { "应试精读" },
        r.source_name
    );
    if r.truncated {
        out.push_str(&format!(
            "> 注：原文超过 {} 字，本次只解析了前段。\n\n",
            TEXT_BUDGET
        ));
    }
    for item in &result.items {
        let label = dimension_label(&r.kind, &item.dimension);
        if item.status != "done" {
            out.push_str(&format!(
                "## {label}\n\n（未完成：{}）\n\n",
                item.error.as_deref().unwrap_or("尚未生成")
            ));
            continue;
        }
        let Some(p) = &item.payload else { continue };
        out.push_str(&format!("## {label}\n\n"));
        match (r.kind.as_str(), item.dimension.as_str()) {
            ("close", "translation") => {
                out.push_str(p.get("text").and_then(|v| v.as_str()).unwrap_or(""));
                out.push_str("\n\n");
            }
            ("close", "gist") => {
                out.push_str(&format!(
                    "**主旨**：{}\n\n**态度**：{}（置信度 {}）\n\n",
                    p.get("main_idea").and_then(|v| v.as_str()).unwrap_or(""),
                    p.get("attitude").and_then(|v| v.as_str()).unwrap_or(""),
                    p.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.0),
                ));
                if let Some(pts) = p.get("points").and_then(|v| v.as_array()) {
                    for pt in pts {
                        out.push_str(&format!("- {}\n", pt.as_str().unwrap_or("")));
                    }
                    out.push('\n');
                }
            }
            ("close", "syntax") => {
                if let Some(ss) = p.get("sentences").and_then(|v| v.as_array()) {
                    for (i, s) in ss.iter().enumerate() {
                        out.push_str(&format!(
                            "{}. **{}**\n   - 结构：{}\n   - 译文：{}\n",
                            i + 1,
                            s.get("original").and_then(|v| v.as_str()).unwrap_or(""),
                            s.get("structure").and_then(|v| v.as_str()).unwrap_or(""),
                            s.get("translation").and_then(|v| v.as_str()).unwrap_or(""),
                        ));
                    }
                    out.push('\n');
                }
            }
            ("close", "synonyms") => {
                if let Some(ps) = p.get("pairs").and_then(|v| v.as_array()) {
                    for pr in ps {
                        out.push_str(&format!(
                            "- {} → **{}**\n",
                            pr.get("original").and_then(|v| v.as_str()).unwrap_or(""),
                            pr.get("upgrade").and_then(|v| v.as_str()).unwrap_or(""),
                        ));
                    }
                    out.push('\n');
                }
            }
            ("close", "familiar") => {
                if let Some(ws) = p.get("words").and_then(|v| v.as_array()) {
                    for w in ws {
                        out.push_str(&format!(
                            "- **{}**：常义「{}」，此处「{}」\n",
                            w.get("word").and_then(|v| v.as_str()).unwrap_or(""),
                            w.get("common").and_then(|v| v.as_str()).unwrap_or(""),
                            w.get("here").and_then(|v| v.as_str()).unwrap_or(""),
                        ));
                    }
                    out.push('\n');
                }
            }
            ("close", "traps") => {
                if let Some(qs) = p.get("questions").and_then(|v| v.as_array()) {
                    for (i, q) in qs.iter().enumerate() {
                        out.push_str(&format!(
                            "{}. {}（陷阱：{}）\n",
                            i + 1,
                            q.get("question").and_then(|v| v.as_str()).unwrap_or(""),
                            q.get("trap").and_then(|v| v.as_str()).unwrap_or(""),
                        ));
                        if let Some(opts) = q.get("options").and_then(|v| v.as_array()) {
                            for (oi, o) in opts.iter().enumerate() {
                                out.push_str(&format!(
                                    "   {}. {}{}\n",
                                    (b'A' + oi as u8) as char,
                                    o.as_str().unwrap_or(""),
                                    if oi as i64 == q.get("answer").and_then(|v| v.as_i64()).unwrap_or(-1) {
                                        " ← 答案"
                                    } else {
                                        ""
                                    }
                                ));
                            }
                        }
                        let explain = q.get("explain").and_then(|v| v.as_str()).unwrap_or("");
                        if !explain.is_empty() {
                            out.push_str(&format!("   解析：{explain}\n"));
                        }
                    }
                    out.push('\n');
                }
            }
            ("translate", "culture_words") => {
                if let Some(ws) = p.get("words").and_then(|v| v.as_array()) {
                    for w in ws {
                        out.push_str(&format!(
                            "- **{}**：直译「{}」→ 宜译「{}」{}\n",
                            w.get("phrase").and_then(|v| v.as_str()).unwrap_or(""),
                            w.get("literal").and_then(|v| v.as_str()).unwrap_or(""),
                            w.get("proper").and_then(|v| v.as_str()).unwrap_or(""),
                            {
                                let e = w.get("example").and_then(|v| v.as_str()).unwrap_or("");
                                if e.is_empty() { String::new() } else { format!("（{e}）") }
                            }
                        ));
                    }
                    out.push('\n');
                }
            }
            ("translate", "phrases") => {
                if let Some(is) = p.get("items").and_then(|v| v.as_array()) {
                    for it in is {
                        out.push_str(&format!(
                            "- {}：{} → **{}**\n",
                            it.get("scene").and_then(|v| v.as_str()).unwrap_or(""),
                            it.get("plain").and_then(|v| v.as_str()).unwrap_or(""),
                            it.get("better").and_then(|v| v.as_str()).unwrap_or(""),
                        ));
                    }
                    out.push('\n');
                }
            }
            ("translate", "checklist") => {
                if let Some(ds) = p.get("dimensions").and_then(|v| v.as_array()) {
                    for d in ds {
                        out.push_str(&format!(
                            "- [ ] **{}**：{}\n",
                            d.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                            d.get("what").and_then(|v| v.as_str()).unwrap_or(""),
                        ));
                    }
                    out.push('\n');
                }
            }
            _ => {}
        }
    }
    out
}

// ---- commands -------------------------------------------------------------------

#[tauri::command]
pub fn bench_run_create(
    state: State<'_, AppState>,
    kind: String,
    source_id: String,
) -> Result<String> {
    let c = state.db.lock().unwrap();
    create_run(&c, &kind, &source_id)
}

#[tauri::command]
pub fn bench_run_get(
    state: State<'_, AppState>,
    kind: String,
    source_id: String,
) -> Result<Option<BenchResult>> {
    let c = state.db.lock().unwrap();
    get_latest(&c, &kind, &source_id)
}

#[tauri::command]
pub fn bench_run_delete(state: State<'_, AppState>, run_id: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    delete_run(&c, &run_id)
}

#[tauri::command]
pub async fn bench_run_execute(app: AppHandle, run_id: String) -> Result<()> {
    tauri::async_runtime::spawn_blocking(move || execute(&app, &run_id))
        .await
        .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

#[tauri::command]
pub async fn bench_item_retry(app: AppHandle, run_id: String, dimension: String) -> Result<()> {
    tauri::async_runtime::spawn_blocking(move || retry_item(&app, &run_id, &dimension))
        .await
        .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

#[tauri::command]
pub fn bench_export_md(state: State<'_, AppState>, run_id: String) -> Result<String> {
    let c = state.db.lock().unwrap();
    Ok(to_markdown(&get_result(&c, &run_id)?))
}

#[tauri::command]
pub fn bench_md_file(state: State<'_, AppState>, run_id: String, dest: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    let md = to_markdown(&get_result(&c, &run_id)?);
    std::fs::write(std::path::Path::new(&dest), md)
        .map_err(|e| Error::Other(format!("write {dest}: {e}")))?;
    Ok(())
}

#[tauri::command]
pub fn bench_synonym_save(
    state: State<'_, AppState>,
    source_id: String,
    original: String,
    upgrade: String,
    context: String,
) -> Result<()> {
    let c = state.db.lock().unwrap();
    save_synonym(&c, &source_id, &original, &upgrade, &context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo;

    fn seed() -> (AppState, String) {
        let st = AppState::in_memory().unwrap();
        let sid = {
            let c = st.db.lock().unwrap();
            let sid = repo::insert_subject(&c, "英语", None, None, None).unwrap();
            let src = repo::insert_source(&c, &sid, None, "Passage 1", "txt", Some("test")).unwrap();
            repo::finalize_source(
                &c,
                &src,
                "ready",
                None,
                Some("The committee approved the plan. It was a decision that many had opposed, yet the evidence eventually compelled agreement. Costs fell by half within a year."),
                None,
            )
            .unwrap();
            sid
        };
        (st, sid)
    }

    fn source_of(st: &AppState) -> String {
        let c = st.db.lock().unwrap();
        c.query_row("SELECT id FROM sources LIMIT 1", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn create_lays_out_every_dimension_pending() {
        let (st, _sid) = seed();
        let src = source_of(&st);
        let run_id = {
            let c = st.db.lock().unwrap();
            create_run(&c, "close", &src).unwrap()
        };
        let r = {
            let c = st.db.lock().unwrap();
            get_result(&c, &run_id).unwrap()
        };
        assert_eq!(r.items.len(), 6);
        assert!(r.items.iter().all(|i| i.status == "pending"));
        assert_eq!(r.items[0].dimension, "translation");
        assert_eq!(r.items[5].dimension, "traps");
        assert_eq!(r.run.status, "running");
    }

    #[test]
    fn create_refuses_an_unknown_kind_or_source() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        assert!(create_run(&c, "nope", "x").is_err());
        assert!(create_run(&c, "close", "missing").is_err());
    }

    #[test]
    fn a_trap_with_a_dangling_answer_is_dropped() {
        let passage = "The committee approved the plan. Costs fell by half within a year.";
        let payload = serde_json::json!({
            "questions": [
                {"question":"Why did costs fall?","options":["A","B","C","D"],"answer":0,"trap":"x","explain":"y"},
                {"question":"Who opposed?","options":["A","B","C"],"answer":0,"trap":"x","explain":"y"},
                {"question":"When?","options":["A","B","C","D"],"answer":9,"trap":"x","explain":"y"},
                {"question":"How much?","options":["A","","C","D"],"answer":1,"trap":"x","explain":"y"},
            ]
        });
        let clean = validate_dimension("close", "traps", &payload, &passage.to_lowercase()).unwrap();
        let qs = clean.get("questions").unwrap().as_array().unwrap();
        assert_eq!(qs.len(), 1, "only the well-formed question survives");
    }

    #[test]
    fn a_long_sentence_the_passage_never_had_is_dropped() {
        let passage = "The committee approved the plan.";
        let payload = serde_json::json!({
            "sentences": [
                {"original":"The committee approved the plan.","structure":"主 + 谓 + 宾","translation":"委员会批准了计划。"},
                {"original":"This sentence is not in the passage at all.","structure":"x","translation":"y"},
            ]
        });
        let clean = validate_dimension("close", "syntax", &payload, &passage.to_lowercase()).unwrap();
        let ss = clean.get("sentences").unwrap().as_array().unwrap();
        assert_eq!(ss.len(), 1);
        assert_eq!(ss[0].get("original").unwrap().as_str().unwrap(), "The committee approved the plan.");
    }

    #[test]
    fn a_synonym_upgrade_keeps_the_pair_but_checks_the_original() {
        let passage = "Costs fell by half within a year. The plan was very good.";
        let payload = serde_json::json!({
            "pairs": [
                {"original":"very good","upgrade":"remarkably effective","context":"The plan was very good."},
                {"original":"fell","upgrade":"declined","context":"Costs fell by half within a year."},
                {"original":"totally absent phrase","upgrade":"x","context":"y"},
            ]
        });
        let clean = validate_dimension("close", "synonyms", &payload, &passage.to_lowercase()).unwrap();
        let ps = clean.get("pairs").unwrap().as_array().unwrap();
        assert_eq!(ps.len(), 2, "the pair whose original is not in the passage is dropped");
        assert!(ps.iter().any(|p| p.get("upgrade").unwrap().as_str() == Some("remarkably effective")));
    }

    #[test]
    fn a_too_short_translation_is_refused_rather_than_cached() {
        let payload = serde_json::json!({ "text": "太短" });
        assert!(validate_dimension("close", "translation", &payload, "x").is_err());
    }

    #[test]
    fn clamp_keeps_char_boundaries_and_flags_the_cut() {
        let (full, truncated) = clamp_text("短文本");
        assert!(!truncated);
        assert_eq!(full, "短文本");
        let long: String = "啊".repeat(TEXT_BUDGET + 500);
        let (cut, truncated) = clamp_text(&long);
        assert!(truncated);
        assert_eq!(cut.chars().count(), TEXT_BUDGET);
    }

    #[test]
    fn markdown_renders_every_dimension() {
        let (st, _sid) = seed();
        let src = source_of(&st);
        let run_id = {
            let c = st.db.lock().unwrap();
            let run_id = create_run(&c, "close", &src).unwrap();
            c.execute(
                "UPDATE bench_items SET status='done', payload=?2 WHERE run_id=?1 AND dimension='translation'",
                params![run_id, serde_json::json!({"text":"委员会批准了该计划。"}).to_string()],
            )
            .unwrap();
            c.execute(
                "UPDATE bench_items SET status='done', payload=?2 WHERE run_id=?1 AND dimension='traps'",
                params![run_id, serde_json::json!({"questions":[{"question":"Why?","options":["A","B","C","D"],"answer":2,"trap":"以偏概全","explain":"因为"}]}).to_string()],
            )
            .unwrap();
            c.execute(
                "UPDATE bench_items SET status='error', error='模型调用失败' WHERE run_id=?1 AND dimension='gist'",
                params![run_id],
            )
            .unwrap();
            c.execute(
                "UPDATE bench_runs SET status='done', truncated=1 WHERE id=?1",
                params![run_id],
            )
            .unwrap();
            run_id
        };
        let md = {
            let c = st.db.lock().unwrap();
            to_markdown(&get_result(&c, &run_id).unwrap())
        };
        assert!(md.contains("委员会批准了该计划"));
        assert!(md.contains("命题陷阱"));
        assert!(md.contains("以偏概全"));
        assert!(md.contains("C ← 答案"));
        assert!(md.contains("未完成：模型调用失败"));
        assert!(md.contains("只解析了前段"));
    }

    #[test]
    fn a_synonym_lands_in_the_vocab_deck() {
        let (st, _sid) = seed();
        let src = source_of(&st);
        {
            let c = st.db.lock().unwrap();
            save_synonym(&c, &src, "very good", "remarkably effective", "The plan was very good.").unwrap();
        }
        let c = st.db.lock().unwrap();
        let terms: i64 = c
            .query_row("SELECT COUNT(*) FROM vocab_terms", [], |r| r.get(0))
            .unwrap();
        assert_eq!(terms, 1);
        let due: i64 = c
            .query_row("SELECT COUNT(*) FROM srs_cards", [], |r| r.get(0))
            .unwrap();
        assert_eq!(due, 1, "the term is seeded due-now like any mined word");
    }

    #[test]
    fn an_empty_synonym_pair_is_refused() {
        let (st, _sid) = seed();
        let src = source_of(&st);
        let c = st.db.lock().unwrap();
        assert!(save_synonym(&c, &src, "", "b", "c").is_err());
        assert!(save_synonym(&c, &src, "a", "  ", "c").is_err());
        // (subject_id is NOT NULL on sources, so the no-subject branch is
        // defensive only — a source always has a deck to land in.)
    }

    #[test]
    fn a_culture_word_without_a_proper_translation_is_dropped() {
        let payload = serde_json::json!({
            "words": [
                {"phrase":" individualism ","literal":"个人主义","proper":"individualism 所强调的个体自主","example":"He is a rugged individualist."},
                {"phrase":"","literal":"x","proper":"y"},
                {"phrase":"z","literal":"x","proper":""},
            ]
        });
        let clean = validate_dimension("translate", "culture_words", &payload, "x").unwrap();
        let ws = clean.get("words").unwrap().as_array().unwrap();
        assert_eq!(ws.len(), 1, "a word with no proper translation teaches the literal trap it warns about");
        let blank = serde_json::json!({ "words": [ {"phrase":"","literal":"x","proper":""} ] });
        assert!(validate_dimension("translate", "culture_words", &blank, "x").is_err());
    }

    #[test]
    fn a_scoring_phrase_needs_both_sides() {
        let payload = serde_json::json!({
            "items": [
                {"scene":"观点","plain":"I think","better":"I would argue that"},
                {"scene":"","plain":"","better":"y"},
                {"scene":"x","plain":"a lot of","better":""},
            ]
        });
        let clean = validate_dimension("translate", "phrases", &payload, "x").unwrap();
        let is = clean.get("items").unwrap().as_array().unwrap();
        assert_eq!(is.len(), 1);
        assert_eq!(is[0].get("better").unwrap().as_str().unwrap(), "I would argue that");
        let blank = serde_json::json!({ "items": [ {"scene":"x","plain":"","better":""} ] });
        assert!(validate_dimension("translate", "phrases", &blank, "x").is_err());
    }

    #[test]
    fn a_checklist_row_without_a_name_is_not_a_check() {
        let payload = serde_json::json!({
            "dimensions": [
                {"name":" 忠实 ","what":"译文是否遗漏信息"},
                {"name":"","what":"x"},
            ]
        });
        let clean = validate_dimension("translate", "checklist", &payload, "x").unwrap();
        let ds = clean.get("dimensions").unwrap().as_array().unwrap();
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].get("name").unwrap().as_str().unwrap(), "忠实");
        let blank = serde_json::json!({ "dimensions": [ {"name":"","what":"x"} ] });
        assert!(validate_dimension("translate", "checklist", &blank, "x").is_err());
    }

    #[test]
    fn markdown_renders_the_translation_drills() {
        let (st, _sid) = seed();
        let src = source_of(&st);
        let run_id = {
            let c = st.db.lock().unwrap();
            let run_id = create_run(&c, "translate", &src).unwrap();
            c.execute(
                "UPDATE bench_items SET status='done', payload=?2 WHERE run_id=?1 AND dimension='culture_words'",
                params![run_id, serde_json::json!({"words":[{"phrase":"individualism","literal":"个人主义","proper":"个体自主意识","example":"He is a rugged individualist."}]}).to_string()],
            )
            .unwrap();
            c.execute(
                "UPDATE bench_items SET status='done', payload=?2 WHERE run_id=?1 AND dimension='phrases'",
                params![run_id, serde_json::json!({"items":[{"scene":"观点","plain":"I think","better":"I would argue that"}]}).to_string()],
            )
            .unwrap();
            c.execute(
                "UPDATE bench_items SET status='error', error='评分维度生成失败' WHERE run_id=?1 AND dimension='checklist'",
                params![run_id],
            )
            .unwrap();
            c.execute(
                "UPDATE bench_runs SET status='done', truncated=1 WHERE id=?1",
                params![run_id],
            )
            .unwrap();
            run_id
        };
        let md = {
            let c = st.db.lock().unwrap();
            to_markdown(&get_result(&c, &run_id).unwrap())
        };
        assert!(md.starts_with("# 翻译训练 · "), "the drills are titled 翻译训练, not 应试精读");
        assert!(md.contains("只解析了前段"), "the truncation note is kind-agnostic");
        assert!(md.contains("- **individualism**：直译「个人主义」→ 宜译「个体自主意识」（He is a rugged individualist.）"));
        assert!(md.contains("- 观点：I think → **I would argue that**"));
        assert!(md.contains("## 评分自查\n\n（未完成：评分维度生成失败）"));
    }
}
