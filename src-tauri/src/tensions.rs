//! Cross-paper tension detection — 知源's 整理助手 reads the knowledge base and
//! surfaces where the literature DISAGREES; a systematic review that only
//! summarizes consensus is half a review.
//!
//! One run takes N papers (the Research page's cross-paper selection), sends
//! their cards to the model ONCE, and stores every detected tension as a row:
//! topic, the two papers, the conflicting claims, the kind of conflict, a
//! severity, and the VERBATIM quotes that evidence each side — a tension without
//! quotes is just an assertion, and this table refuses to store those.
//!
//! Same run-shape as PRISMA (0048): status run + rows, live polling, delete and
//! retry the whole run; `source` is always "model" here — tensions are
//! hypotheses to check, not data, so nothing is silently user-editable.

use crate::db::{new_id, now_ms};
use crate::error::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use crate::db::AppState;
use tauri::{AppHandle, Manager, State};

#[derive(Debug, Clone, Serialize)]
pub struct TensionRun {
    pub id: String,
    pub title: String,
    pub status: String, // running | done | error
    pub error: Option<String>,
    pub item_ids: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Tension {
    pub id: String,
    pub run_id: String,
    pub topic: String,
    pub paper_a_id: String,
    pub paper_b_id: String,
    pub claim_a: String,
    pub claim_b: String,
    /// contradiction | complementary | evidence | scope
    pub conflict: String,
    /// high | medium | low
    pub severity: String,
    pub quote_a: String,
    pub quote_b: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TensionResult {
    pub run: TensionRun,
    /// (paper_id, title) for every source paper, so cards can name sides.
    pub papers: Vec<(String, String)>,
    pub tensions: Vec<Tension>,
}

pub(crate) const SYSTEM: &str = "You are a systematic-review analyst. You are given several academic \
papers (title + extracted card). Your job: find GENUINE TENSIONS between them — places where \
the literature disagrees, not where it agrees. Return ONLY a raw JSON object of the shape \
{\"tensions\":[{\"topic\":string,\"paper_a\":string,\"paper_b\":string,\"claim_a\":string,\"claim_b\":string,\
\"conflict\":\"contradiction\"|\"complementary\"|\"evidence\"|\"scope\",\"severity\":\"high\"|\"medium\"|\"low\",\
\"quote_a\":string,\"quote_b\":string}]}. Rules: paper_a/paper_b MUST be exact paper titles from the \
input. claim_a/claim_b: one sentence each, stating what paper_a / paper_b holds about the topic. \
conflict: contradiction = they assert incompatible things; complementary = they cover different \
pieces; evidence = one's evidence undercuts the other; scope = same claim under different \
conditions/ populations. quote_a/quote_b: a SHORT verbatim fragment (<=200 chars) FROM THE CARD \
that evidences each side — if you cannot quote it, do not report the tension. Prefer few, \
high-signal tensions over many weak ones (max 10). If there are no genuine tensions, return \
{\"tensions\":[]}. Never invent quotes.";

pub fn create_run(conn: &Connection, item_ids: &[String], title: Option<&str>) -> Result<String> {
    if item_ids.len() < 2 {
        return Err(Error::Other("张力分析至少需要两篇论文".into()));
    }
    for pid in item_ids {
        let exists: bool = conn
            .query_row("SELECT 1 FROM papers WHERE id=?1", [pid], |_| Ok(true))
            .optional()?
            .unwrap_or(false);
        if !exists {
            return Err(Error::NotFound(format!("paper {pid}")));
        }
    }
    let id = new_id();
    let now = now_ms();
    conn.execute(
        "INSERT INTO tension_runs (id, title, status, item_ids, created_at, updated_at)
         VALUES (?1,?2,'running',?3,?4,?4)",
        params![
            id,
            title.unwrap_or(if item_ids.len() == 2 { "两文献张力" } else { "多文献张力分析" }),
            serde_json::to_string(item_ids)?,
            now
        ],
    )?;
    Ok(id)
}

pub fn list_runs(conn: &Connection) -> Result<Vec<TensionRun>> {
    let mut st = conn.prepare(
        "SELECT id, title, status, error, item_ids, created_at, updated_at
           FROM tension_runs ORDER BY created_at DESC LIMIT 30",
    )?;
    let rows = st
        .query_map([], |r| {
            let ids: String = r.get(4)?;
            Ok(TensionRun {
                id: r.get(0)?,
                title: r.get(1)?,
                status: r.get(2)?,
                error: r.get(3)?,
                item_ids: serde_json::from_str(&ids).unwrap_or_default(),
                created_at: r.get(5)?,
                updated_at: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn get_result(conn: &Connection, run_id: &str) -> Result<TensionResult> {
    let run = get_run(conn, run_id)?;
    let mut papers = Vec::with_capacity(run.item_ids.len());
    for pid in &run.item_ids {
        let title: Option<String> = conn
            .query_row("SELECT title FROM papers WHERE id=?1", [pid], |r| r.get(0))
            .optional()?;
        papers.push((pid.clone(), title.unwrap_or_else(|| "(已删除)".into())));
    }
    let mut st = conn.prepare(
        "SELECT id, run_id, topic, paper_a_id, paper_b_id, claim_a, claim_b, conflict, severity,
                quote_a, quote_b, created_at
           FROM tensions WHERE run_id=?1 ORDER BY created_at ASC",
    )?;
    let tensions = st
        .query_map(params![run_id], |r| {
            Ok(Tension {
                id: r.get(0)?,
                run_id: r.get(1)?,
                topic: r.get(2)?,
                paper_a_id: r.get(3)?,
                paper_b_id: r.get(4)?,
                claim_a: r.get(5)?,
                claim_b: r.get(6)?,
                conflict: r.get(7)?,
                severity: r.get(8)?,
                quote_a: r.get(9)?,
                quote_b: r.get(10)?,
                created_at: r.get(11)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(TensionResult { run, papers, tensions })
}

pub fn get_run(conn: &Connection, id: &str) -> Result<TensionRun> {
    conn.query_row(
        "SELECT id, title, status, error, item_ids, created_at, updated_at FROM tension_runs WHERE id=?1",
        [id],
        |r| {
            let ids: String = r.get(4)?;
            Ok(TensionRun {
                id: r.get(0)?,
                title: r.get(1)?,
                status: r.get(2)?,
                error: r.get(3)?,
                item_ids: serde_json::from_str(&ids).unwrap_or_default(),
                created_at: r.get(5)?,
                updated_at: r.get(6)?,
            })
        },
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("tension run {id}")),
        other => Error::Db(other),
    })
}

pub fn delete_run(conn: &Connection, run_id: &str) -> Result<()> {
    conn.execute("DELETE FROM tensions WHERE run_id=?1", [run_id])?;
    conn.execute("DELETE FROM tension_runs WHERE id=?1", [run_id])?;
    Ok(())
}

/// Execute a run: one LLM call over all paper cards. Records token usage.
pub fn execute(app: &AppHandle, run_id: &str) -> Result<()> {
    let state = app.state::<crate::db::AppState>();
    let (item_ids, cards, mut model) = {
        let c = state.db.lock().unwrap();
        let run = get_run(&c, run_id)?;
        if run.status != "running" {
            return Err(Error::Other("该次分析已结束，请重新发起".into()));
        }
        let mut cards = Vec::with_capacity(run.item_ids.len());
        for pid in &run.item_ids {
            let (p, card) = crate::papers::collect_paper_card(&c, pid)?;
            cards.push((p.title, card));
        }
        let spec = model_spec(&c)?;
        let keys = crate::commands::read_keys(&c)?;
        let mut model = crate::llm::from_spec_or_any(&spec, &keys).ok_or_else(|| {
            Error::Other("No model configured - add an API key in Settings -> API keys".into())
        })?;
        crate::commands::apply_budget(&mut model, &c, "cheatsheet");
        (run.item_ids, cards, model)
    };

    let mut blocks = String::new();
    for (i, (title, card)) in cards.iter().enumerate() {
        blocks.push_str(&format!("### PAPER {} (title: {})\n{}\n\n", i + 1, title, card));
    }
    let user = format!(
        "PAPERS:\n{blocks}Return the tensions JSON now. If none are genuine, return {{\"tensions\":[]}}."
    );
    model.set_max_tokens(4096);
    let sys = crate::prompts::text(crate::prompts::TENSION_ANALYZE);
    let raw = model.complete(&sys, &user);
    let parsed = match raw {
        Ok(r) => {
            { let c = state.db.lock().unwrap(); crate::tokens::record(&c, "tension", &model.name(), &format!("{sys}\n{user}"), &r, None); }
            crate::llm::extract_json(&r).ok()
        }
        Err(e) => {
            let c = state.db.lock().unwrap();
            c.execute(
                "UPDATE tension_runs SET status='error', error=?2, updated_at=?3 WHERE id=?1",
                params![run_id, format!("模型调用失败：{e}"), now_ms()],
            )?;
            return Err(Error::Other(format!("模型调用失败：{e}")));
        }
    };
    let Some(payload) = parsed else {
        let c = state.db.lock().unwrap();
        c.execute(
            "UPDATE tension_runs SET status='error', error='模型输出无法解析为 JSON', updated_at=?2 WHERE id=?1",
            params![run_id, now_ms()],
        )?;
        return Err(Error::Other("模型输出无法解析为 JSON".into()));
    };

    // Title lookup for validating paper_a/paper_b (must be exact titles).
    let title_to_id: std::collections::HashMap<String, String> = {
        let c = state.db.lock().unwrap();
        let mut m = std::collections::HashMap::new();
        for pid in &item_ids {
            if let Ok(t) = c.query_row("SELECT title FROM papers WHERE id=?1", [pid], |r| r.get::<_, String>(0)) {
                m.insert(t, pid.clone());
            }
        }
        m
    };

    let mut stored = 0usize;
    let mut skipped = 0usize;
    {
        let c = state.db.lock().unwrap();
        if let Some(list) = payload.get("tensions").and_then(|v| v.as_array()) {
            for t in list.iter().take(10) {
                let topic = t.get("topic").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                let pa = t.get("paper_a").and_then(|v| v.as_str()).unwrap_or("").trim();
                let pb = t.get("paper_b").and_then(|v| v.as_str()).unwrap_or("").trim();
                // Both sides must resolve to real papers in the run — no
                // hallucinated literature.
                let (Some(pid_a), Some(pid_b)) = (title_to_id.get(pa), title_to_id.get(pb)) else {
                    skipped += 1;
                    continue;
                };
                let quote_a = t.get("quote_a").and_then(|v| v.as_str()).unwrap_or("").trim();
                let quote_b = t.get("quote_b").and_then(|v| v.as_str()).unwrap_or("").trim();
                if topic.is_empty() || quote_a.is_empty() || quote_b.is_empty() {
                    skipped += 1;
                    continue;
                }
                let clamp = |s: &str, n: usize| s.chars().take(n).collect::<String>();
                c.execute(
                    "INSERT INTO tensions (id, run_id, topic, paper_a_id, paper_b_id, claim_a, claim_b,
                                           conflict, severity, quote_a, quote_b, created_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                    params![
                        new_id(),
                        run_id,
                        clamp(&topic, 80),
                        pid_a,
                        pid_b,
                        clamp(t.get("claim_a").and_then(|v| v.as_str()).unwrap_or(""), 300),
                        clamp(t.get("claim_b").and_then(|v| v.as_str()).unwrap_or(""), 300),
                        clamp(t.get("conflict").and_then(|v| v.as_str()).unwrap_or("contradiction"), 20),
                        clamp(t.get("severity").and_then(|v| v.as_str()).unwrap_or("medium"), 10),
                        clamp(quote_a, 240),
                        clamp(quote_b, 240),
                        now_ms()
                    ],
                )?;
                stored += 1;
            }
        }
        c.execute(
            "UPDATE tension_runs SET status='done', error=NULL, updated_at=?2 WHERE id=?1",
            params![run_id, now_ms()],
        )?;
    }
    let _ = (stored, skipped);
    Ok(())
}

fn model_spec(conn: &Connection) -> Result<String> {
    Ok(crate::commands::model_spec_for(conn, "cheatsheet"))
}

/// Markdown report: one section per tension, both quotes, source titles.
pub fn to_markdown(result: &TensionResult) -> String {
    let mut out = format!("# 文献张力分析 · {}\n\n", result.run.title);
    let title_of = |id: &str| {
        result
            .papers
            .iter()
            .find(|(pid, _)| pid == id)
            .map(|(_, t)| t.clone())
            .unwrap_or_else(|| id.to_string())
    };
    if result.tensions.is_empty() {
        out.push_str("未检测到显著张力（或文献间高度一致）。\n");
        return out;
    }
    for (i, t) in result.tensions.iter().enumerate() {
        out.push_str(&format!(
            "## {}. {}（{} · {}）\n\n- **{}**: {}\n- **{}**: {}\n\n> 「{}」\n>\n> 「{}」\n\n",
            i + 1,
            t.topic,
            t.conflict,
            t.severity,
            title_of(&t.paper_a_id),
            t.claim_a,
            title_of(&t.paper_b_id),
            t.claim_b,
            t.quote_a,
            t.quote_b
        ));
    }
    out
}

// ---- commands -------------------------------------------------------------

#[tauri::command]
pub fn tension_run_create(
    state: State<AppState>,
    item_ids: Vec<String>,
    title: Option<String>,
) -> Result<String> {
    let c = state.db.lock().unwrap();
    create_run(&c, &item_ids, title.as_deref())
}

#[tauri::command]
pub fn tension_run_list(state: State<AppState>) -> Result<Vec<TensionRun>> {
    let c = state.db.lock().unwrap();
    list_runs(&c)
}

#[tauri::command]
pub fn tension_run_result(state: State<AppState>, run_id: String) -> Result<TensionResult> {
    let c = state.db.lock().unwrap();
    get_result(&c, &run_id)
}

#[tauri::command]
pub fn tension_run_delete(state: State<AppState>, run_id: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    delete_run(&c, &run_id)
}

#[tauri::command]
pub async fn tension_run_execute(app: AppHandle, run_id: String) -> Result<()> {
    tauri::async_runtime::spawn_blocking(move || execute(&app, &run_id))
        .await
        .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

#[tauri::command]
pub fn tension_export_md(state: State<AppState>, run_id: String) -> Result<String> {
    let c = state.db.lock().unwrap();
    Ok(to_markdown(&get_result(&c, &run_id)?))
}

/// Write the Markdown report to a user-chosen path (the export button).
#[tauri::command]
pub fn tension_md_file(state: State<AppState>, run_id: String, dest: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    let md = to_markdown(&get_result(&c, &run_id)?);
    std::fs::write(std::path::Path::new(&dest), md)
        .map_err(|e| Error::Other(format!("write {dest}: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::papers::PaperInput;

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
    fn create_requires_two_real_papers() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let p1 = seed_paper(&c, "Paper A", "x");
        assert!(create_run(&c, &[p1.clone()], None).is_err());
        assert!(create_run(&c, &[p1.clone(), "nope".into()], None).is_err());
        let p2 = seed_paper(&c, "Paper B", "y");
        let id = create_run(&c, &[p1, p2], Some("对比")).unwrap();
        let run = get_run(&c, &id).unwrap();
        assert_eq!(run.status, "running");
        assert_eq!(run.item_ids.len(), 2);
        assert_eq!(run.title, "对比");
    }

    #[test]
    fn result_and_markdown_roundtrip() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let p1 = seed_paper(&c, "Paper A", "We find X increases Y.");
        let p2 = seed_paper(&c, "Paper B", "We find X decreases Y.");
        let run_id = create_run(&c, &[p1.clone(), p2.clone()], None).unwrap();
        // Simulate what execute() stores after the model call.
        c.execute(
            "INSERT INTO tensions (id, run_id, topic, paper_a_id, paper_b_id, claim_a, claim_b,
                                   conflict, severity, quote_a, quote_b, created_at)
             VALUES ('t1',?1,'X 对 Y 的影响',?2,?3,'X 提升 Y','X 降低 Y','contradiction','high',
                     'X increases Y','X decreases Y',1)",
            params![run_id, p1, p2],
        )
        .unwrap();
        c.execute(
            "UPDATE tension_runs SET status='done' WHERE id=?1",
            [&run_id],
        )
        .unwrap();
        let r = get_result(&c, &run_id).unwrap();
        assert_eq!(r.tensions.len(), 1);
        assert_eq!(r.tensions[0].topic, "X 对 Y 的影响");
        assert_eq!(r.papers.len(), 2);
        let md = to_markdown(&r);
        assert!(md.contains("X 对 Y 的影响"));
        assert!(md.contains("Paper A"));
        assert!(md.contains("Paper B"));
        assert!(md.contains("X increases Y"));
        delete_run(&c, &run_id).unwrap();
        assert!(get_result(&c, &run_id).is_err());
    }
}
