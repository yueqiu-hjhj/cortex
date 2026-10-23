//! Summary tree (D6) — hierarchical digests that let a small-context local
//! model answer library-scale questions. NotebookLM buys reach with a 2M-token
//! window; here the reach comes from structure instead: one subject overview,
//! one digest per topic, one digest per source, all cached in `summary_nodes`
//! and assembled top-down (overview → matching topic digests → matching source
//! digests → raw chunks). Raw text only enters as the last, smallest layer, so
//! the same prompt budget that used to hold 18k chars of chunks now covers the
//! shape of the whole library plus the specific passages that matched.
//!
//! Freshness is read-time: a node whose `updated_at` is older than its
//! underlying row is stale and gets skipped — a stale digest is never shown.
//! Nodes are rebuildable cache, not user content.

use std::collections::{HashMap, HashSet};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::db::{new_id, now_ms};
use crate::error::{Error, Result};
use crate::models::ChunkHit;
use crate::repo;

#[derive(Debug, Clone, Serialize)]
pub struct SummaryNode {
    pub node_kind: String,
    pub node_id: String,
    pub summary: String,
    pub keywords: Vec<String>,
    pub model: Option<String>,
    pub chars_in: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SummaryTreeStatus {
    pub subject_ok: bool,
    pub subject_missing: bool,
    pub topic_fresh: i64,
    pub topic_stale: i64,
    pub topic_missing: i64,
    pub source_fresh: i64,
    pub source_stale: i64,
    pub source_missing: i64,
    /// Source-text chars covered by fresh source digests — the "reach" number.
    pub total_chars: i64,
    pub model_configured: bool,
}

pub(crate) const PART_SUM_SYS: &str = "你是学术资料摘要助手。把下面的资料片段压缩为不超过 120 词的要点摘要：保留关键术语、定义、数字、方法名与结论；不要评论，不要开场白，只输出摘要文本。语言与原文一致。";
pub(crate) const SOURCE_SUM_SYS: &str = "你是学术资料摘要助手。基于给定内容输出 JSON：{\"summary\":\"…\",\"keywords\":[\"…\"]}。summary 是不超过 160 词的忠实要点摘要（保留关键术语、数字、方法名、结论）；keywords 是 5-10 个适合检索的关键词。语言与原文一致。只输出 JSON，不要其他文字。";
pub(crate) const REDUCE_SUM_SYS: &str = "你是学术资料摘要助手。下面是同一份材料各片段的要点摘要，把它们合并为一份不超过 160 词的整体摘要，输出 JSON：{\"summary\":\"…\",\"keywords\":[\"…\"]}。keywords 为 5-10 个检索关键词。语言与原文一致。只输出 JSON。";
pub(crate) const GROUP_SUM_SYS: &str = "你是学术资料摘要助手。下面是同一主题下若干材料的摘要（每条以「名称」开头）。综合为不超过 130 词的主题级摘要：覆盖共同主线、关键差异与重要结论，并给出 5-10 个检索关键词。输出 JSON：{\"summary\":\"…\",\"keywords\":[\"…\"]}。语言与输入一致。只输出 JSON。";
pub(crate) const SUBJECT_SUM_SYS: &str = "你是学术资料摘要助手。下面是一个科目下各主题的摘要（可能含少量未分组材料的摘要）。综合为不超过 170 词的科目级总览：这个知识库讲什么、主线结构、最重要的结论。输出 JSON：{\"summary\":\"…\",\"keywords\":[\"…\"]}。keywords 为 5-10 个检索关键词。语言与输入一致。只输出 JSON。";

const SOURCE_WINDOW: usize = 11000;

// ---- nodes & freshness ---------------------------------------------------

fn row_updated_at(conn: &Connection, table: &str, id: &str) -> Result<i64> {
    // table names come only from our own call sites (never user input).
    let sql = format!("SELECT updated_at FROM {table} WHERE id = ?1");
    conn.query_row(&sql, params![id], |r| r.get(0))
        .map_err(|_| Error::NotFound(format!("{table} {id}")))
}

pub fn get_node(conn: &Connection, kind: &str, node_id: &str) -> Result<Option<SummaryNode>> {
    let row = conn
        .query_row(
            "SELECT node_kind, node_id, summary, keywords, model, chars_in, updated_at
             FROM summary_nodes WHERE node_kind = ?1 AND node_id = ?2",
            params![kind, node_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                ))
            },
        )
        .optional()?;
    Ok(row.map(
        |(node_kind, node_id, summary, keywords, model, chars_in, updated_at)| SummaryNode {
            node_kind,
            node_id,
            summary,
            keywords: keywords
                .and_then(|k| serde_json::from_str(&k).ok())
                .unwrap_or_default(),
            model,
            chars_in,
            updated_at,
        },
    ))
}

/// A node is usable only when it exists AND is at least as new as its
/// underlying row.
fn fresh_node(
    conn: &Connection,
    kind: &str,
    node_id: &str,
    table: &str,
) -> Result<Option<SummaryNode>> {
    let node = match get_node(conn, kind, node_id)? {
        Some(n) => n,
        None => return Ok(None),
    };
    let parent = row_updated_at(conn, table, node_id).unwrap_or(i64::MAX);
    if node.updated_at >= parent {
        Ok(Some(node))
    } else {
        Ok(None)
    }
}

pub fn status(conn: &Connection, subject_id: &str) -> Result<SummaryTreeStatus> {
    let mut st = SummaryTreeStatus::default();

    match fresh_node(conn, "subject", subject_id, "subjects")? {
        Some(_) => st.subject_ok = true,
        None => st.subject_missing = true,
    }

    let mut q = conn.prepare(
        "SELECT t.id, n.updated_at, t.updated_at FROM topics t
         LEFT JOIN summary_nodes n ON n.node_kind='topic' AND n.node_id=t.id
         WHERE t.subject_id = ?1",
    )?;
    let rows = q.query_map(params![subject_id], |r| {
        Ok((r.get::<_, Option<i64>>(1)?, r.get::<_, i64>(2)?))
    })?;
    for row in rows {
        let (node_at, topic_at) = row?;
        match node_at {
            Some(at) if at >= topic_at => st.topic_fresh += 1,
            Some(_) => st.topic_stale += 1,
            None => st.topic_missing += 1,
        }
    }

    // Sources with no text need no digest and are not counted at all.
    let mut q = conn.prepare(
        "SELECT s.content, n.updated_at, s.updated_at FROM sources s
         LEFT JOIN summary_nodes n ON n.node_kind='source' AND n.node_id=s.id
         WHERE s.subject_id = ?1",
    )?;
    let rows = q.query_map(params![subject_id], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<i64>>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (content, node_at, src_at) = row?;
        if content.as_deref().map(str::trim).unwrap_or("").is_empty() {
            continue;
        }
        match node_at {
            Some(at) if at >= src_at => st.source_fresh += 1,
            Some(_) => st.source_stale += 1,
            None => st.source_missing += 1,
        }
    }
    st.total_chars = conn.query_row(
        "SELECT COALESCE(SUM(chars_in),0) FROM summary_nodes WHERE subject_id=?1 AND node_kind='source'",
        params![subject_id],
        |r| r.get(0),
    )?;

    let spec = crate::commands::model_spec_for(conn, "cheatsheet");
    st.model_configured = crate::commands::guard_offline_llm(conn, &spec).is_ok();
    Ok(st)
}

// ---- building ------------------------------------------------------------

fn upsert_node(
    conn: &Connection,
    subject_id: &str,
    kind: &str,
    node_id: &str,
    summary: &str,
    keywords: &[String],
    model: &str,
    chars_in: i64,
) -> Result<()> {
    let now = now_ms();
    conn.execute(
        "INSERT INTO summary_nodes (id, subject_id, node_kind, node_id, summary, keywords, model, chars_in, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
         ON CONFLICT(node_kind, node_id) DO UPDATE SET
           summary=excluded.summary, keywords=excluded.keywords, model=excluded.model,
           chars_in=excluded.chars_in, updated_at=excluded.updated_at",
        params![
            new_id(),
            subject_id,
            kind,
            node_id,
            summary,
            serde_json::to_string(keywords).unwrap_or_else(|_| "[]".into()),
            model,
            chars_in,
            now
        ],
    )?;
    Ok(())
}

fn parse_sum_json(raw: &str) -> Result<(String, Vec<String>)> {
    let v = crate::llm::extract_json(raw)?;
    let summary = v
        .get("summary")
        .and_then(|s| s.as_str())
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if summary.is_empty() {
        return Err(Error::Other("模型摘要为空".into()));
    }
    let keywords = v
        .get("keywords")
        .and_then(|k| k.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok((summary, keywords))
}

/// Split long content into ~window-char parts at paragraph boundaries so each
/// map call sees a coherent slice instead of a mid-sentence cut.
fn split_parts(content: &str, window: usize) -> Vec<String> {
    let content = content.trim();
    if content.len() <= window {
        return vec![content.to_string()];
    }
    let mut parts = Vec::new();
    let mut start = 0usize;
    while start < content.len() {
        let mut end = (start + window).min(content.len());
        if end < content.len() {
            if let Some(pos) = content[start..end].rfind("\n\n") {
                if pos > window / 3 {
                    end = start + pos;
                }
            }
        }
        while !content.is_char_boundary(end) {
            end -= 1;
        }
        let slice = content[start..end].trim();
        if !slice.is_empty() {
            parts.push(slice.to_string());
        }
        start = end;
    }
    parts
}

fn summarize_source(model: &mut Box<dyn crate::llm::Llm>, content: &str) -> Result<(String, Vec<String>, i64)> {
    let parts = split_parts(content, SOURCE_WINDOW);
    let chars_in = content.chars().count() as i64;
    if parts.len() == 1 {
        model.set_max_tokens(1400);
        let raw = model.complete(&crate::prompts::text(crate::prompts::SUMMARY_SOURCE), parts[0].as_str())?;
        let (summary, keywords) = parse_sum_json(&raw)?;
        return Ok((summary, keywords, chars_in));
    }
    let mut piece_summaries = Vec::with_capacity(parts.len());
    for part in &parts {
        model.set_max_tokens(800);
        piece_summaries.push(model.complete(&crate::prompts::text(crate::prompts::SUMMARY_PART), part)?);
    }
    model.set_max_tokens(1400);
    let joined = piece_summaries
        .iter()
        .enumerate()
        .map(|(i, s)| format!("片段{}：\n{}", i + 1, s.trim()))
        .collect::<Vec<_>>()
        .join("\n\n");
    let raw = model.complete(&crate::prompts::text(crate::prompts::SUMMARY_REDUCE), &joined)?;
    let (summary, keywords) = parse_sum_json(&raw)?;
    Ok((summary, keywords, chars_in))
}

fn summarize_group(
    model: &mut Box<dyn crate::llm::Llm>,
    system: &str,
    items: &[(String, String)],
) -> Result<(String, Vec<String>)> {
    let input = items
        .iter()
        .map(|(name, s)| format!("「{name}」\n{}", s.trim()))
        .collect::<Vec<_>>()
        .join("\n\n");
    model.set_max_tokens(1400);
    let raw = model.complete(system, &input)?;
    parse_sum_json(&raw)
}

/// Build / refresh the tree for a subject, bottom-up: source digests (map for
/// long content, one call per ~11k-char part + one reduce), topic rollups from
/// their sources' digests, then the subject overview. Fresh nodes are kept
/// unless `force`; sources without text are skipped.
pub fn build(app: &AppHandle, subject_id: &str, force: bool) -> Result<SummaryTreeStatus> {
    let state = app.state::<crate::db::AppState>();
    let (_subject_name, topics, sources, mut model) = {
        let c = state.db.lock().unwrap();
        let subject = repo::get_subject(&c, subject_id)?;
        let mut q = c.prepare("SELECT id, name FROM topics WHERE subject_id=?1 ORDER BY position, created_at")?;
        let topics = q
            .query_map(params![subject_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut q = c.prepare(
            "SELECT id, name, topic_id, content, updated_at FROM sources WHERE subject_id=?1 ORDER BY created_at",
        )?;
        let sources = q
            .query_map(params![subject_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let spec = crate::commands::model_spec_for(&c, "cheatsheet");
        crate::commands::guard_offline_llm(&c, &spec)?;
        let keys = crate::commands::read_keys(&c)?;
        let mut model = crate::llm::from_spec_or_any(&spec, &keys).ok_or_else(|| {
            Error::Other("No model configured - add an API key in Settings -> API keys".into())
        })?;
        crate::commands::apply_budget(&mut model, &c, "cheatsheet");
        (subject.name, topics, sources, model)
    };
    let model_name = model.name();

    // L2 — per-source digests.
    let mut digests: HashMap<String, String> = HashMap::new();
    for (sid, _name, _tid, content, updated_at) in &sources {
        let text = content.as_deref().unwrap_or("").trim();
        if text.is_empty() {
            continue;
        }
        {
            let c = state.db.lock().unwrap();
            if !force {
                if let Some(node) = get_node(&c, "source", sid)? {
                    if node.updated_at >= *updated_at {
                        digests.insert(sid.clone(), node.summary);
                        continue;
                    }
                }
            }
        }
        let (summary, keywords, chars_in) = summarize_source(&mut model, text)?;
        let c = state.db.lock().unwrap();
        upsert_node(&c, subject_id, "source", sid, &summary, &keywords, &model_name, chars_in)?;
        digests.insert(sid.clone(), summary);
    }

    // L1 — per-topic rollups from member digests.
    for (tid, _tname) in &topics {
        let items: Vec<(String, String)> = sources
            .iter()
            .filter(|(sid, _, topic, content, _)| {
                topic.as_deref() == Some(tid.as_str())
                    && !content.as_deref().unwrap_or("").trim().is_empty()
                    && digests.contains_key(sid)
            })
            .filter_map(|(sid, name, ..)| digests.get(sid).map(|d| (name.clone(), d.clone())))
            .collect();
        if items.is_empty() {
            continue;
        }
        let topic_at = row_updated_at(&state.db.lock().unwrap(), "topics", tid).unwrap_or(i64::MAX);
        {
            let c = state.db.lock().unwrap();
            if !force {
                if let Some(node) = get_node(&c, "topic", tid)? {
                    if node.updated_at >= topic_at {
                        continue;
                    }
                }
            }
        }
        let (summary, keywords) = summarize_group(&mut model, &crate::prompts::text(crate::prompts::SUMMARY_GROUP), &items)?;
        let c = state.db.lock().unwrap();
        upsert_node(&c, subject_id, "topic", tid, &summary, &keywords, &model_name, 0)?;
    }

    // L0 — subject overview from fresh topic rollups + ungrouped source digests.
    let subj_at = row_updated_at(&state.db.lock().unwrap(), "subjects", subject_id).unwrap_or(i64::MAX);
    let mut items: Vec<(String, String)> = Vec::new();
    {
        let c = state.db.lock().unwrap();
        for (tid, tname) in &topics {
            if let Some(node) = fresh_node(&c, "topic", tid, "topics")? {
                items.push((tname.clone(), node.summary));
            }
        }
        for (sid, name, topic, content, _) in &sources {
            if topic.is_none() && !content.as_deref().unwrap_or("").trim().is_empty() {
                if let Some(node) = fresh_node(&c, "source", sid, "sources")? {
                    items.push((name.clone(), node.summary));
                }
            }
        }
    }
    let need = force
        || match get_node(&state.db.lock().unwrap(), "subject", subject_id)? {
            None => true,
            Some(n) => n.updated_at < subj_at,
        };
    if need && !items.is_empty() {
        let (summary, keywords) = summarize_group(&mut model, &crate::prompts::text(crate::prompts::SUMMARY_SUBJECT), &items)?;
        let c = state.db.lock().unwrap();
        upsert_node(&c, subject_id, "subject", subject_id, &summary, &keywords, &model_name, 0)?;
    }

    let c = state.db.lock().unwrap();
    status(&c, subject_id)
}

// ---- context assembly ------------------------------------------------------

/// Byte-budgeted clip that never splits a char.
fn clip(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\u{2026}", &s[..end])
}

fn keyword_score(node: &SummaryNode, terms: &[String]) -> i64 {
    if terms.is_empty() {
        return 0;
    }
    let hay = format!("{} {}", node.summary, node.keywords.join(" ")).to_lowercase();
    terms.iter().filter(|t| hay.contains(t.as_str())).count() as i64
}

// ---- context intent: what shape of context does this question want? --------

/// Which layer of the library a question is asking about. The context block is
/// one budget split four ways (overview → topics → sources → raw text), and
/// the right split depends on the question: "梳理一下这学期的框架" wants the
/// digests, "这个公式为什么成立" wants the raw passage. A fixed 10/25/25/40
/// spent 60% of the budget on layers a pinpoint question never reads, and
/// starved the digests a synthesis question is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextIntent {
    /// Wants the exact passage: definition, derivation, why/how.
    Pinpoint,
    /// Ordinary question — the historical 10/25/25/40 split.
    Balanced,
    /// Wants the shape of the material: summary, comparison, whole-topic view.
    Synthesis,
}

impl ContextIntent {
    pub fn as_str(&self) -> &'static str {
        match self {
            ContextIntent::Pinpoint => "pinpoint",
            ContextIntent::Balanced => "balanced",
            ContextIntent::Synthesis => "synthesis",
        }
    }
}

/// Intent words. Deliberately two small closed vocabularies, bilingual: the
/// cost of a miss is only a suboptimal split, and every branch still includes
/// both digests and raw text — precision here is not worth an LLM call on the
/// hot path of every question.
const SYNTHESIS_WORDS: &[&str] = &[
    "总结", "概括", "梳理", "总览", "概览", "整体", "全貌", "框架", "大纲", "脉络",
    "对比", "比较", "区别", "联系", "关系", "哪些", "这章", "本章", "本课", "本学期",
    "整个", "全部", "综合",
    "summary", "summarize", "overview", "outline", "compare", "comparison",
    "difference", "whole", "entire", "recap", "synthesize", "review",
];
const PINPOINT_WORDS: &[&str] = &[
    "什么是", "什么意思", "是什么", "定义", "为什么", "为何", "怎么算", "如何",
    "公式", "证明", "推导", "定理", "第几页", "页码", "原文",
    "what is", "why", "how", "define", "definition", "formula", "prove",
    "proof", "derive", "page",
];

/// Classify the question, then (only if it carries no intent words) let the
/// retrieval shape speak: hits spread across many sources means the question
/// pulls threads together (synthesis), hits all from one source mean it is
/// about that passage (pinpoint). This second signal is what makes the split
/// work for questions that are not phrased like textbook prompts.
pub fn context_intent(query: Option<&str>, raw_hits: &[ChunkHit]) -> ContextIntent {
    if let Some(q) = query {
        let lower = q.to_lowercase();
        let syn = SYNTHESIS_WORDS.iter().filter(|w| lower.contains(*w)).count();
        let pin = PINPOINT_WORDS.iter().filter(|w| lower.contains(*w)).count();
        // Majority wins; a tie leans synthesis — an overview answer still
        // carries the raw passages, a pinpoint answer cannot reconstruct a
        // whole-topic picture.
        if syn > pin {
            return ContextIntent::Synthesis;
        }
        if pin > syn {
            return ContextIntent::Pinpoint;
        }
        if syn > 0 {
            // Tie (e.g. "总结一下为什么"): intent words were present at all.
            return ContextIntent::Synthesis;
        }
    }
    let mut sources: Vec<&str> = raw_hits.iter().map(|h| h.source_id.as_str()).collect();
    sources.sort_unstable();
    sources.dedup();
    if raw_hits.len() >= 4 && sources.len() >= 3 {
        return ContextIntent::Synthesis;
    }
    if !raw_hits.is_empty() && sources.len() == 1 {
        return ContextIntent::Pinpoint;
    }
    ContextIntent::Balanced
}

/// Split `total` bytes across (overview, topics, sources, raw). The four
/// always add up to `total`; only the proportions move:
///   synthesis 15/35/30/20 — the digests ARE the answer material
///   balanced  10/25/25/40 — the historical split
///   pinpoint  5/10/15/70 — the passage is the answer material
pub fn budget_split(intent: ContextIntent, total: usize) -> (usize, usize, usize, usize) {
    let (ov_pct, ov_cap, tp_pct, sr_pct) = match intent {
        ContextIntent::Synthesis => (15, 2_400, 35, 30),
        ContextIntent::Balanced => (10, 1_400, 25, 25),
        ContextIntent::Pinpoint => (5, 800, 10, 15),
    };
    let overview = (total * ov_pct / 100).min(ov_cap);
    let topics = total * tp_pct / 100;
    let sources = total * sr_pct / 100;
    let raw = total.saturating_sub(overview + topics + sources);
    (overview, topics, sources, raw)
}

/// Assemble hierarchical context within `max_chars` bytes, with the split
/// chosen by `context_intent` (see `budget_split` for the proportions).
/// `topic_ids` empty = whole subject; `source_ids` (when non-empty) pins the
/// scope to exactly those sources; `raw_hits` are pre-retrieved chunks (chat
/// mode). When `raw_hits` is empty (generation mode) the raw layer is filled by
/// the existing fair-allocation `context_text`. Returns the text and the names
/// of the sources represented in it (digests + raw hits) — callers use that as
/// the allowed-citation whitelist.
pub fn context_block(
    conn: &Connection,
    subject_id: &str,
    topic_ids: &[String],
    source_ids: Option<&[String]>,
    query: Option<&str>,
    raw_hits: &[ChunkHit],
    max_chars: usize,
) -> Result<(String, Vec<String>)> {
    let total = max_chars.max(4000);
    let intent = context_intent(query, raw_hits);
    let (overview_budget, topics_budget, sources_budget, raw_budget) = budget_split(intent, total);

    let terms = query.map(repo::query_terms).unwrap_or_default();
    let mut out = String::new();
    let mut covered: Vec<String> = Vec::new();

    // Scope resolution: explicit source selection > explicit topics > whole subject.
    let all_sources: Vec<(String, String, Option<String>)> = {
        let mut q =
            conn.prepare("SELECT id, name, topic_id FROM sources WHERE subject_id=?1 ORDER BY created_at")?;
        let rows = q.query_map(params![subject_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let in_scope: Vec<(String, String)> = if let Some(ids) = source_ids.filter(|v| !v.is_empty()) {
        all_sources
            .iter()
            .filter(|(id, ..)| ids.iter().any(|x| x == id))
            .map(|(id, name, _)| (id.clone(), name.clone()))
            .collect()
    } else if !topic_ids.is_empty() {
        all_sources
            .iter()
            .filter(|(_, _, t)| t.as_deref().is_some_and(|t| topic_ids.iter().any(|x| x == t)))
            .map(|(id, name, _)| (id.clone(), name.clone()))
            .collect()
    } else {
        all_sources
            .iter()
            .map(|(id, name, _)| (id.clone(), name.clone()))
            .collect()
    };

    // L0 — subject overview.
    if overview_budget > 0 {
        if let Some(node) = fresh_node(conn, "subject", subject_id, "subjects")? {
            out.push_str(&format!("[知识库总览]\n{}", clip(&node.summary, overview_budget)));
        }
    }

    // L1 — topic digests: explicit topics, else the topics of the scoped
    // sources (or every topic when unscoped). Ranked by query-term overlap.
    if topics_budget > 0 {
        let mut topic_scope: Vec<String> = Vec::new();
        if !topic_ids.is_empty() {
            topic_scope = topic_ids.to_vec();
        } else {
            let mut seen = HashSet::new();
            for (id, _) in &in_scope {
                if let Some(Some(t)) = all_sources.iter().find(|(sid, _, _)| sid == id).map(|(_, _, t)| t) {
                    if seen.insert(t.clone()) {
                        topic_scope.push(t.clone());
                    }
                }
            }
        }
        if !topic_scope.is_empty() {
            let mut named: Vec<(String, String)> = Vec::new();
            let mut q = conn.prepare("SELECT id, name FROM topics WHERE subject_id=?1")?;
            let rows = q.query_map(params![subject_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (tid, name) = row?;
                if topic_scope.contains(&tid) {
                    named.push((tid, name));
                }
            }
            let mut entries: Vec<(String, SummaryNode)> = Vec::new();
            for (tid, name) in named {
                if let Some(node) = fresh_node(conn, "topic", &tid, "topics")? {
                    entries.push((name, node));
                }
            }
            if !terms.is_empty() {
                entries.sort_by_key(|(_, n)| -keyword_score(n, &terms));
            }
            let mut used = 0usize;
            for (name, node) in entries {
                if used >= topics_budget {
                    break;
                }
                let piece = clip(&node.summary, (topics_budget - used).saturating_sub(48).min(900));
                if piece.trim().is_empty() {
                    continue;
                }
                let block = format!("\n\n[主题摘要 · {name}]\n{piece}");
                used += block.len();
                out.push_str(&block);
            }
        }
    }

    // L2 — source digests for the scoped sources (raw-hit sources are already
    // in scope in chat mode; their digests ride along as the "expansion"
    // preview). Ranked by query-term overlap when there is a query.
    if sources_budget > 0 && !in_scope.is_empty() {
        let mut entries: Vec<(String, String, SummaryNode)> = Vec::new();
        for (sid, name) in &in_scope {
            if let Some(node) = fresh_node(conn, "source", sid, "sources")? {
                entries.push((sid.clone(), name.clone(), node));
            }
        }
        if !terms.is_empty() {
            entries.sort_by_key(|(_, _, n)| -keyword_score(n, &terms));
        }
        let mut used = 0usize;
        for (_sid, name, node) in entries {
            if used >= sources_budget {
                break;
            }
            let piece = clip(&node.summary, (sources_budget - used).saturating_sub(48).min(700));
            if piece.trim().is_empty() {
                continue;
            }
            let block = format!("\n\n[材料摘要 · {name}]\n{piece}");
            used += block.len();
            if !covered.contains(&name) {
                covered.push(name);
            }
            out.push_str(&block);
        }
    }

    // L3 — raw text: chat mode uses the retrieved chunks; generation mode falls
    // back to the fair-allocation filler over the remaining budget.
    if raw_budget > 0 {
        if !raw_hits.is_empty() {
            let mut used = 0usize;
            for h in raw_hits {
                let remain = raw_budget.saturating_sub(used);
                if remain < 200 {
                    break;
                }
                let loc = h.loc.as_deref().map(|l| format!(" · {l}")).unwrap_or_default();
                let text = clip(&h.text, remain.saturating_sub(40));
                let block = format!("\n\n[原文 · {}{}]\n{}", h.source_name, loc, text);
                used += block.len();
                out.push_str(&block);
                if !covered.contains(&h.source_name) {
                    covered.push(h.source_name.clone());
                }
            }
        } else {
            let has_sel = source_ids.is_some_and(|v| !v.is_empty());
            let (text, _) = if has_sel {
                repo::context_text(conn, subject_id, None, source_ids, raw_budget)?
            } else if topic_ids.len() == 1 {
                repo::context_text(conn, subject_id, Some(&topic_ids[0]), None, raw_budget)?
            } else if topic_ids.len() > 1 {
                let per = raw_budget / topic_ids.len().max(1);
                let mut acc = String::new();
                for tid in topic_ids {
                    let (t, _) = repo::context_text(conn, subject_id, Some(tid), None, per)?;
                    if !t.trim().is_empty() {
                        acc.push_str(&t);
                    }
                }
                (acc, 0)
            } else {
                repo::context_text(conn, subject_id, None, None, raw_budget)?
            };
            if !text.trim().is_empty() {
                out.push_str("\n\n[原文]\n");
                out.push_str(&text);
            }
        }
    }

    Ok((out, covered))
}

// ---- Tauri command surface ----------------------------------------------

#[tauri::command]
pub fn summary_tree_status(
    state: State<crate::db::AppState>,
    subject_id: String,
) -> Result<SummaryTreeStatus> {
    let c = state.db.lock().unwrap();
    status(&c, &subject_id)
}

/// Long-running: builds/refreshes the digest tree (many LLM calls). Spawned on
/// the blocking pool so the UI thread never waits.
#[tauri::command]
pub async fn summary_tree_build(
    app: AppHandle,
    subject_id: String,
    force: bool,
) -> Result<SummaryTreeStatus> {
    tauri::async_runtime::spawn_blocking(move || build(&app, &subject_id, force))
        .await
        .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::AppState;
    use crate::vector::f32s_to_blob;

    fn state() -> AppState {
        AppState::in_memory().unwrap()
    }

    fn seed_library(c: &Connection) -> (String, String, String) {
        let sid = repo::insert_subject(c, "信息检索", None, None, None).unwrap();
        let t1 = repo::insert_topic(c, &sid, "向量检索", None, &[]).unwrap();
        let s1 = repo::insert_source(c, &sid, Some(&t1), "lecture1.md", "md", None).unwrap();
        repo::insert_chunk(
            c,
            &s1,
            &sid,
            Some(&t1),
            0,
            "BM25 是经典的词频检索基线。",
            Some("p.1"),
            4,
            &f32s_to_blob(&vec![0.0; 4]),
            "",
        )
        .unwrap();
        c.execute(
            "UPDATE sources SET content = ?1 WHERE id = ?2",
            params!["第一章 BM25 与词频检索的基线方法。", s1],
        )
        .unwrap();
        (sid, t1, s1)
    }

    fn node(kind: &str, id: &str, summary: &str, at: i64) -> SummaryNode {
        SummaryNode {
            node_kind: kind.into(),
            node_id: id.into(),
            summary: summary.into(),
            keywords: vec!["检索".into()],
            model: Some("test".into()),
            chars_in: 100,
            updated_at: at,
        }
    }

    fn upsert_raw(c: &Connection, subject_id: &str, n: &SummaryNode) {
        upsert_node(
            c,
            subject_id,
            &n.node_kind,
            &n.node_id,
            &n.summary,
            &n.keywords,
            n.model.as_deref().unwrap_or("test"),
            n.chars_in,
        )
        .unwrap();
        // Force the node's updated_at to the exact timestamp we want.
        c.execute(
            "UPDATE summary_nodes SET updated_at = ?1 WHERE node_kind=?2 AND node_id=?3",
            params![n.updated_at, n.node_kind, n.node_id],
        )
        .unwrap();
    }

    #[test]
    fn stale_nodes_are_skipped_fresh_ones_shown() {
        let st = state();
        let c = st.db.lock().unwrap();
        let (sid, t1, s1) = seed_library(&c);
        let now = now_ms();

        // Fresh nodes: at == now >= row.updated_at.
        upsert_raw(&c, &sid, &node("subject", &sid, "总览：信息检索课。", now));
        upsert_raw(&c, &sid, &node("topic", &t1, "主题：向量检索基线。", now));
        upsert_raw(&c, &sid, &node("source", &s1, "材料：BM25 讲义摘要。", now));

        let (text, names) = context_block(&c, &sid, &[], None, None, &[], 24000).unwrap();
        assert!(text.contains("[知识库总览]"));
        assert!(text.contains("[主题摘要"));
        assert!(text.contains("[材料摘要"));
        assert!(text.contains("BM25 是经典的词频检索基线"));
        assert!(names.contains(&"lecture1.md".to_string()));

        // Make the source node stale (older than the source row) — its digest
        // must vanish while the overview survives.
        c.execute(
            "UPDATE summary_nodes SET updated_at = ?1 WHERE node_kind='source'",
            params![now - 10_000],
        )
        .unwrap();
        c.execute(
            "UPDATE sources SET updated_at = ?1 WHERE id = ?2",
            params![now + 1, s1],
        )
        .unwrap();
        let (text2, _) = context_block(&c, &sid, &[], None, None, &[], 24000).unwrap();
        assert!(!text2.contains("材料摘要"));
        assert!(text2.contains("[知识库总览]"));
    }

    #[test]
    fn query_ranks_and_raw_hits_fill_the_last_layer() {
        let st = state();
        let c = st.db.lock().unwrap();
        let (sid, t1, _s1) = seed_library(&c);
        let now = now_ms();
        upsert_raw(&c, &sid, &node("topic", &t1, "主题：BM25 与词频。", now));
        upsert_raw(&c, &sid, &node("source", &_s1, "材料：BM25 摘要。", now));

        let hit = ChunkHit {
            id: "ch1".into(),
            source_id: _s1.clone(),
            source_name: "lecture1.md".into(),
            text: "BM25 的得分函数由词频与逆文档频率组成。".into(),
            loc: Some("p.2".into()),
            char_start: None,
            char_end: None,
            score: 0.9,
        };
        let (text, names) =
            context_block(&c, &sid, &[], None, Some("BM25 词频怎么算"), &[hit], 24000).unwrap();
        assert!(
            text.contains("[原文 · lecture1.md · p.2]"),
            "raw hit missing; text = {text:?}"
        );
        assert!(text.contains("[材料摘要"));
        assert!(names.contains(&"lecture1.md".to_string()));
        assert!(text.len() <= 24000 + 2000);
    }

    #[test]
    fn clip_respects_char_boundaries() {
        let s = "中文内容测试".repeat(100);
        let cut = clip(&s, 50);
        assert!(cut.chars().count() < s.chars().count());
        assert!(cut.ends_with('\u{2026}'));
    }

    fn hit(id: &str, source_id: &str, text: &str) -> ChunkHit {
        ChunkHit {
            id: id.into(),
            source_id: source_id.into(),
            source_name: format!("{source_id}.md"),
            text: text.into(),
            loc: None,
            char_start: None,
            char_end: None,
            score: 0.5,
        }
    }

    /// Intent words win over retrieval shape; the shape decides only when the
    /// question carries none. A tie leans synthesis (an overview answer still
    /// carries raw passages; a pinpoint answer cannot reconstruct a topic).
    #[test]
    fn intent_words_win_then_retrieval_shape_decides() {
        use ContextIntent::*;
        let spread: Vec<ChunkHit> = (0..4)
            .map(|i| hit(&format!("h{i}"), &format!("s{i}"), "text"))
            .collect();
        let single = vec![hit("h1", "s1", "text"), hit("h2", "s1", "text")];

        // Words.
        assert_eq!(context_intent(Some("梳理一下这学期的框架"), &[]), Synthesis);
        assert_eq!(context_intent(Some("这个公式为什么成立"), &[]), Pinpoint);
        assert_eq!(context_intent(Some("对比 A 和 B 的区别"), &[]), Synthesis);
        // Majority: two synthesis words vs one pinpoint word.
        assert_eq!(context_intent(Some("总结并对比这两章的框架，为什么"), &[]), Synthesis);
        // Tie leans synthesis.
        assert_eq!(context_intent(Some("总结一下为什么"), &[]), Synthesis);

        // No words: the retrieval shape speaks.
        assert_eq!(context_intent(None, &spread), Synthesis, "hit spread across 3+ sources pulls threads together");
        assert_eq!(context_intent(Some("特征值分解"), &single), Pinpoint, "one source = it is about that passage");
        assert_eq!(context_intent(Some("特征值分解"), &[]), Balanced);
    }

    /// The four layers always add up to the whole budget; only the proportions
    /// move, and the raw share is ordered pinpoint > balanced > synthesis.
    #[test]
    fn budget_split_adds_up_and_moves_the_raw_share() {
        for total in [4_000usize, 18_000, 24_000, 60_000] {
            let (ov, tp, sr, raw) = budget_split(ContextIntent::Balanced, total);
            assert_eq!(ov + tp + sr + raw, total, "total {total}");
        }
        let raw_of = |i| budget_split(i, 24_000).3;
        assert!(raw_of(ContextIntent::Pinpoint) > raw_of(ContextIntent::Balanced));
        assert!(raw_of(ContextIntent::Balanced) > raw_of(ContextIntent::Synthesis));
        // The overview cap tracks the intent: a synthesis question may spend
        // more of the budget on the whole-library picture.
        assert_eq!(budget_split(ContextIntent::Synthesis, 24_000).0, 2_400);
        assert_eq!(budget_split(ContextIntent::Pinpoint, 24_000).0, 800);
    }

    /// End to end: the same library, the same hits, two questions — the
    /// synthesis question gets more digest bytes, the pinpoint question gets
    /// more passage bytes. Small budget on purpose: the per-block caps (900
    /// bytes) only bite above it, and the point is that the LAYERS move, not
    /// that the total grows.
    #[test]
    fn the_split_follows_the_question_not_a_fixed_ratio() {
        let st = state();
        let c = st.db.lock().unwrap();
        let (sid, t1, s1) = seed_library(&c);
        let now = now_ms();
        upsert_raw(&c, &sid, &node("topic", &t1, &format!("主题脉络：{}", "向量检索".repeat(300)), now));
        let hits = vec![hit("ch1", &s1, &"BM25 的得分函数由词频与逆文档频率组成。".repeat(40))];

        let (syn, _) = context_block(&c, &sid, &[], None, Some("总结本章脉络"), &hits, 4_000).unwrap();
        let (pin, _) = context_block(&c, &sid, &[], None, Some("这个公式为什么成立"), &hits, 4_000).unwrap();
        let split_at = |t: &str| t.find("\n\n[原文").unwrap_or(t.len());
        let (syn_digest, syn_raw) = (split_at(&syn), split_at(&pin));
        assert!(
            syn_digest > syn_raw,
            "the synthesis question spends more on digests ({syn_digest} vs {syn_raw})"
        );
        assert!(
            pin[split_at(&pin)..].len() > syn[syn_digest..].len(),
            "the pinpoint question spends more on the passage ({} vs {})",
            pin.len() - split_at(&pin),
            syn.len() - syn_digest
        );
        assert!(syn.contains("主题脉络"), "the synthesis block carries the topic digest");
    }
}
