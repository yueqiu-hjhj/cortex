//! Deep Research — 课题 → 多源文献发现 → AI 筛选 → 阅读清单 → 有据综述初稿。
//!
//! NotebookLM 在 2025-11 把 Deep Research 集成进产品（给一个题目，自动搜网、
//! 筛选、建库）；SurfSense 用 LangGraph 做了同类 Researcher。Cortex 的元件
//! 其实全齐：四路学术检索（arXiv/Crossref/OpenAlex/Semantic Scholar）+ 可选
//! SearXNG、RRF 融合、查询改写、papers 表、以及刚落地的引用校验。这个模块把
//! 它们串成一条命令：
//!
//!   1. DISCOVER  literature_search 改写课题为 2-3 个变体，五源并发，RRF 融合
//!   2. SCREEN    LLM 读候选（标题/年份/摘要）选出最相关的 N 篇，每篇给一行理由；
//!                模型不可用或输出不合规时退回按融合排序取前 N（并明说）
//!   3. IMPORT    经 papers::import_paper 入库（DOI 去重，已在库的标记 imported=false）
//!   4. DRAFT     以入选论文摘要为上下文写综述初稿（背景/现状/分歧/缺口/下一步，
//!                行内 [n] 引用），过 citecheck::verify_numbered 剔除幽灵引用
//!   5. SAVE      综述存为科目下的笔记，历史可查
//!
//! 每一步都发进度事件（复用 ingest:progress 通道），前端可以实时显示阶段。

use crate::error::Result;
use crate::llm::Llm;
use crate::literature::{self, LiteratureHit};
use rusqlite::Connection;
use serde::Serialize;

#[allow(dead_code)]
const DEFAULT_MAX_SOURCES: usize = 6;
const MAX_CANDIDATES_SHOWN: usize = 24;
const ABSTRACT_CHARS: usize = 400;

#[derive(Debug, Clone, Serialize)]
pub struct DeepResearchPick {
    pub paper_id: String,
    pub title: String,
    pub url: Option<String>,
    pub doi: Option<String>,
    pub year: Option<i64>,
    pub venue: Option<String>,
    /// Which discovery source(s) surfaced it (arxiv / crossref / …).
    pub found_by: String,
    /// The screener's one-line reason, or the fallback "融合排序最高" note.
    pub why: String,
    /// false = the DOI was already in the library; nothing new was added.
    pub imported: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeepResearchResult {
    pub topic: String,
    pub rewritten: Vec<String>,
    pub candidates: usize,
    pub sources_attempted: Vec<String>,
    pub sources_succeeded: Vec<String>,
    pub picks: Vec<DeepResearchPick>,
    pub overview: String,
    pub note_id: String,
    pub took_ms: i64,
    pub warnings: Vec<String>,
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

/// Parse the screener's reply: a JSON array of `{"i": n, "why": "…"}`. Lenient
/// — models wrap JSON in prose/fences constantly. Falls back to None so the
/// caller can use the rank-order fallback.
fn parse_picks(reply: &str, max: usize) -> Option<Vec<(usize, String)>> {
    let start = reply.find('[')?;
    let end = reply.rfind(']')? + 1;
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&reply[start..end]).ok()?;
    let mut out = Vec::new();
    for v in parsed {
        let i = v.get("i")?.as_i64()? as usize;
        let why = v
            .get("why")
            .and_then(|w| w.as_str())
            .unwrap_or("与课题相关")
            .trim()
            .to_string();
        if i >= 1 && i <= MAX_CANDIDATES_SHOWN && out.len() < max {
            out.push((i, why));
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Step 2: AI screening. Returns (pick indices into `hits`, reasons) — or a
/// rank-order fallback with an honest warning when the model path fails.
fn screen(
    model: Option<&dyn Llm>,
    topic: &str,
    hits: &[LiteratureHit],
    max_sources: usize,
) -> (Vec<(usize, String)>, Vec<String>) {
    let mut warnings = Vec::new();
    let Some(model) = model else {
        warnings.push("未配置可用模型 —— 按检索融合排序直接取前 N 篇，未经 AI 筛选。".into());
        let picks = (0..max_sources.min(hits.len()))
            .map(|i| (i + 1, "检索融合排序最高（未经 AI 筛选）".into()))
            .collect();
        return (picks, warnings);
    };
    let list: String = hits
        .iter()
        .take(MAX_CANDIDATES_SHOWN)
        .enumerate()
        .map(|(i, h)| {
            format!(
                "[{}] {}（{}，{}）\n摘要：{}",
                i + 1,
                h.title,
                h.venue.as_deref().unwrap_or("未知来源"),
                h.year.map(|y| y.to_string()).unwrap_or_else(|| "n.d.".into()),
                truncate(h.abstract_text.as_deref().unwrap_or("（无摘要）"), ABSTRACT_CHARS),
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let sys = "You are a research screener. Given a research TOPIC and numbered candidate \
        papers (title, venue, year, abstract), pick the ones genuinely relevant to the topic — \
        prefer quality and direct relevance over marginal matches, and keep the set focused. \
        Output ONLY a JSON array like [{\"i\": 3, \"why\": \"直接研究该问题的随机对照实验\"}, …], \
        at most the requested number, ordered most-relevant first. The 'why' must be one short \
        sentence in Chinese.";
    let user = format!(
        "TOPIC: {topic}\n\n请从以下 {} 篇候选中选出最多 {} 篇：\n\n{list}",
        hits.len().min(MAX_CANDIDATES_SHOWN),
        max_sources
    );
    match model.complete(sys, &user) {
        Ok(reply) => match parse_picks(&reply, max_sources) {
            Some(picks) => (picks, warnings),
            None => {
                warnings.push("模型筛选输出不合规 —— 回退到检索融合排序。".into());
                let picks = (0..max_sources.min(hits.len()))
                    .map(|i| (i + 1, "检索融合排序最高（模型输出未解析成功）".into()))
                    .collect();
                (picks, warnings)
            }
        },
        Err(e) => {
            warnings.push(format!("模型筛选失败（{e}）—— 回退到检索融合排序。"));
            let picks = (0..max_sources.min(hits.len()))
                .map(|i| (i + 1, "检索融合排序最高（模型调用失败）".into()))
                .collect();
            (picks, warnings)
        }
    }
}

/// Step 4: the grounded overview draft, then ghost-citation stripping.
fn draft_overview(model: &dyn Llm, topic: &str, picked: &[(&LiteratureHit, usize)]) -> Result<String> {
    let corpus: String = picked
        .iter()
        .map(|(h, n)| {
            format!(
                "[{n}] {}（{}）\n{}",
                h.title,
                h.year.map(|y| y.to_string()).unwrap_or_else(|| "n.d.".into()),
                truncate(h.abstract_text.as_deref().unwrap_or("（无摘要）"), ABSTRACT_CHARS),
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let sys = "You write the FIRST DRAFT of a research overview (综述初稿) from numbered \
        paper abstracts. Structure: 背景与问题 → 已有工作与现状 → 分歧与不足 → 研究缺口 → \
        下一步。Cite claims inline as [n] matching the numbered abstracts; do not invent \
        citations or facts beyond the abstracts. 600–900 characters, light Markdown, Chinese.";
    let user = format!("课题：{topic}\n\n候选摘要：\n\n{corpus}");
    let raw = model.complete(sys, &user)?;
    // Ghost citations out — same standard the chat path enforces.
    let (cleaned, _cited) = crate::citecheck::verify_numbered(&raw, picked.len());
    Ok(cleaned)
}

/// Run the whole pipeline. `progress` receives (stage, detail, percent) for the
/// UI; the DB writes happen in the caller's connection. Network-heavy steps run
/// in the COMMAND layer's blocking hop — this function itself is sync.
pub fn run(
    conn: &Connection,
    model: Option<&dyn Llm>,
    searxng: Option<&str>,
    subject_id: &str,
    topic: &str,
    max_sources: usize,
    progress: &mut dyn FnMut(&str, &str, u8),
) -> Result<DeepResearchResult> {
    let started = std::time::Instant::now();
    let topic = topic.trim();
    if topic.is_empty() {
        return Err(crate::error::Error::Other("研究课题不能为空".into()));
    }
    let max_sources = max_sources.clamp(3, 12);
    let mut warnings = Vec::new();

    // 1. DISCOVER — multi-source scholarly search, RRF-fused, query rewritten.
    progress("search", "正在多源检索文献…", 5);
    let keywords = literature::subject_keywords(conn, subject_id);
    let lit = literature::literature_search(model, topic, &keywords, searxng, 30)?;
    if lit.hits.is_empty() {
        return Err(crate::error::Error::Other(
            "检索没有返回任何候选 —— 换个表述（或配置 SearXNG 扩大覆盖）再试。".into(),
        ));
    }
    progress("screen", &format!("检索到 {} 篇候选，正在 AI 筛选…", lit.hits.len()), 40);

    // 2. SCREEN
    let (pick_idx, mut screen_warnings) = screen(model, topic, &lit.hits, max_sources);
    warnings.append(&mut screen_warnings);

    // 3. IMPORT — DOI dedupe against the whole library first.
    progress("import", "正在导入阅读清单…", 65);
    let known_dois: std::collections::HashSet<String> = crate::papers::list_papers(conn, None, None, None)?
        .into_iter()
        .filter_map(|p| p.doi)
        .collect();
    let mut picks: Vec<DeepResearchPick> = Vec::new();
    let mut picked_hits: Vec<(&LiteratureHit, usize)> = Vec::new();
    let mut used_dois: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (i, why) in pick_idx {
        let Some(hit) = lit.hits.get(i.saturating_sub(1)) else { continue };
        let n = picked_hits.len() + 1;
        picked_hits.push((hit, n));
        // Normalize + dedupe: same DOI within this run, or already in the
        // library, still gets listed (it IS relevant) but not re-imported.
        let doi_norm = hit.doi.as_deref().map(|d| d.trim_start_matches("https://doi.org/").to_lowercase());
        let importable = match &doi_norm {
            Some(d) => !known_dois.contains(d) && used_dois.insert(d.clone()),
            None => true,
        };
        let paper_id = if importable {
            let paper = crate::papers::import_paper(
                conn,
                &crate::papers::PaperInput {
                    title: hit.title.clone(),
                    authors: Some(hit.authors.join(", ")),
                    year: hit.year,
                    venue: hit.venue.clone(),
                    doi: hit.doi.clone(),
                    url: hit.url.clone(),
                    abstract_text: hit.abstract_text.clone(),
                    subject_id: Some(subject_id.to_string()),
                    source_id: None,
                },
            )?;
            paper.id
        } else {
            String::new()
        };
        picks.push(DeepResearchPick {
            paper_id,
            title: hit.title.clone(),
            url: hit.url.clone(),
            doi: hit.doi.clone(),
            year: hit.year,
            venue: hit.venue.clone(),
            found_by: hit.sources.join("/"),
            why,
            imported: importable,
        });
    }

    // 4. DRAFT — grounded in the picked abstracts, ghost citations stripped.
    progress("draft", "正在撰写综述初稿…", 85);
    let overview = match model {
        Some(m) if !picked_hits.is_empty() => draft_overview(m, topic, &picked_hits)?,
        _ => String::new(),
    };

    // 5. SAVE — the overview is a subject note; history and sync come free.
    progress("save", "正在保存…", 95);
    let note_id = if overview.trim().is_empty() {
        String::new()
    } else {
        crate::repo::insert_note(
            conn,
            Some(subject_id),
            None,
            &format!("综述初稿：{topic}"),
            &overview,
        )?
    };

    progress("done", "完成", 100);
    Ok(DeepResearchResult {
        topic: topic.to_string(),
        rewritten: lit.rewritten,
        candidates: lit.hits.len(),
        sources_attempted: lit.sources_attempted,
        sources_succeeded: lit.sources_succeeded,
        picks,
        overview,
        note_id,
        took_ms: started.elapsed().as_millis() as i64,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_picks_tolerates_prose_and_fences() {
        let reply = r#"好的，这是我的选择：
        ```json
        [{"i": 3, "why": "直接相关"}, {"i": 1, "why": "方法同源"}, {"i": 99, "why": "越界"}]
        ```"#;
        let picks = parse_picks(reply, 5).unwrap();
        assert_eq!(picks.len(), 2, "out-of-range index 99 must be dropped");
        assert_eq!(picks[0].0, 3);
        assert_eq!(picks[1].1, "方法同源");
        assert!(parse_picks("没有 JSON 在这里", 5).is_none());
    }

    #[test]
    fn parse_picks_caps_at_max() {
        let reply = r#"[{"i":1,"why":"a"},{"i":2,"why":"b"},{"i":3,"why":"c"},{"i":4,"why":"d"}]"#;
        let picks = parse_picks(reply, 2).unwrap();
        assert_eq!(picks.len(), 2);
    }

    #[test]
    fn screen_falls_back_to_rank_order_without_a_model() {
        let hit = |t: &str| LiteratureHit {
            title: t.into(),
            authors: vec![],
            year: Some(2024),
            venue: None,
            doi: None,
            url: None,
            abstract_text: Some("abstract".into()),
            citation_count: None,
            source: "arxiv".into(),
            sources: vec!["arxiv".into()],
            rrf_score: 0.0,
            final_score: 0.0,
            reasons: vec![],
        };
        let hits = vec![hit("a"), hit("b")];
        let (picks, warnings) = screen(None, "topic", &hits, 3);
        assert_eq!(picks.len(), 2, "capped at the number of hits");
        assert!(picks[0].1.contains("未经 AI 筛选"));
        assert!(warnings[0].contains("未配置可用模型"));
    }
}
