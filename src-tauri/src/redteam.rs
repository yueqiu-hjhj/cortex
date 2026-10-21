//! Red-team review — 知源's 顶刊红队: before you submit, read your paper the
//! way a hostile top-venue reviewer will. One run over one paper's card
//! produces Soundness / Novelty / Rigor scores, the flaws that alone
//! justify rejection, the rescue experiments that would answer them, and
//! the rebuttal strategy for the ones you decide to fight.
//!
//! The honesty rules, all tested:
//!
//!   * every fatal flaw / weakness MUST quote the card verbatim — a
//!     critique without a quote is an assertion, and this module refuses
//!     to store those (same discipline as tensions)
//!   * scores are clamped into 1-10; a model that answers "11/10" is
//!     corrected, not believed
//!   * the verdict comes from the reviewer, but a run with fatal flaws
//!     can never be stored as "accept" — the scores and the verdict have
//!     to agree, or the row lies about itself
//!   * findings the model invents about OTHER papers are dropped: the
//!     quote has to exist in THIS paper's card
//!
//! Same run-shape as PRISMA (0048) and tensions (0050): status run + rows,
//! live polling, delete and re-run.

use crate::db::{new_id, now_ms, AppState};
use crate::error::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

/// The reviewer's brief. Hostile but fair: score against the stated venue's
/// bar, and every accusation carries its quote — a review that cannot point
/// at the sentence it condemns is just an opinion with a scoresheet.
pub const SYSTEM: &str = "You are a hostile but fair reviewer for the stated top venue. You are given \
ONE paper's card (title + extracted content). Review it the way a program committee member who wants \
to find reasons to REJECT would — then be fair about what survives. Return ONLY a raw JSON object \
of the shape {\"soundness\":int(1-10),\"novelty\":int(1-10),\"rigor\":int(1-10),\
\"verdict\":\"accept\"|\"minor\"|\"major\"|\"reject\",\"summary\":string,\"findings\":[{\"kind\":\"fatal\"|\
\"weakness\"|\"rescue\"|\"rebuttal\",\"title\":string,\"detail\":string,\"quote\":string}]}. Rules: \
soundness = are the claims actually supported by the evidence shown; novelty = what is new \
relative to what a reader of this venue already knows; rigor = methodology, baselines, ablations, \
statistics, reproducibility. Score against the STATED venue's bar — the same paper is a reject at \
one venue and an accept at another. kind: fatal = a flaw that ALONE justifies rejection (if you \
cannot name one, do not invent one); weakness = a real problem that does not kill the paper; \
rescue = the cheapest decisive experiment/analysis that would answer a fatal flaw; rebuttal = how \
the authors could defend a point they choose to keep. For fatal and weakness, quote MUST be a SHORT \
verbatim fragment (<=200 chars) copied FROM THE CARD — if you cannot quote it, you cannot claim \
it; downgrade the point to a strategy or drop it. Never invent quotes; never quote a sentence the \
card does not contain. title: one line, specific (\"no baseline comparison\", not \"weak \
evaluation\"). Prefer few high-signal findings over many weak ones (max 12). summary: 2-4 \
sentences, the overall read a PC member would write. If the paper is genuinely strong, say so — a \
review that manufactures flaws is as useless as one that finds none.";

#[derive(Debug, Clone, Serialize)]
pub struct RedteamRun {
    pub id: String,
    pub title: String,
    pub status: String, // running | done | error
    pub error: Option<String>,
    pub paper_id: String,
    pub paper_title: String,
    pub venue: String,
    pub soundness: Option<i64>,
    pub novelty: Option<i64>,
    pub rigor: Option<i64>,
    /// accept | minor | major | reject
    pub verdict: Option<String>,
    pub summary: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RedteamFinding {
    pub id: String,
    /// fatal | weakness | rescue | rebuttal
    pub kind: String,
    pub title: String,
    pub detail: String,
    pub quote: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RedteamResult {
    pub run: RedteamRun,
    pub findings: Vec<RedteamFinding>,
}

pub const DEFAULT_VENUE: &str = "顶刊（NeurIPS / CVPR 级）";

/// Findings that must be quoted to be stored — the ones that accuse the
/// paper of something. Strategies (rescue/rebuttal) are the reviewer's
/// advice and need no quote.
fn quote_required(kind: &str) -> bool {
    matches!(kind, "fatal" | "weakness")
}

/// A run with fatal flaws can never read "accept". The model is allowed to
/// be harsh, not to contradict itself.
fn reconcile_verdict(verdict: &str, fatal_count: usize) -> String {
    let v = verdict.trim().to_lowercase();
    let v = match v.as_str() {
        "accept" | "minor" | "major" | "reject" => v,
        _ => "major".to_string(), // an unparseable verdict defaults to caution
    };
    if fatal_count > 0 && v == "accept" {
        return "major".into();
    }
    v
}

pub fn create_run(conn: &Connection, paper_id: &str, venue: Option<&str>) -> Result<String> {
    let paper_title: Option<String> = conn
        .query_row("SELECT title FROM papers WHERE id=?1", [paper_id], |r| {
            r.get(0)
        })
        .optional()?;
    let Some(paper_title) = paper_title else {
        return Err(Error::NotFound(format!("paper {paper_id}")));
    };
    let id = new_id();
    let now = now_ms();
    conn.execute(
        "INSERT INTO redteam_runs (id, title, status, paper_id, venue, created_at, updated_at)
         VALUES (?1,?2,'running',?3,?4,?5,?5)",
        params![
            id,
            format!("红队评审 · {paper_title}"),
            paper_id,
            venue.unwrap_or(DEFAULT_VENUE).trim(),
            now
        ],
    )?;
    Ok(id)
}

pub fn list_runs(conn: &Connection) -> Result<Vec<RedteamRun>> {
    let mut st = conn.prepare(
        "SELECT r.id, r.title, r.status, r.error, r.paper_id, p.title, r.venue,
                r.soundness, r.novelty, r.rigor, r.verdict, r.summary, r.created_at, r.updated_at
           FROM redteam_runs r JOIN papers p ON p.id = r.paper_id
          ORDER BY r.created_at DESC LIMIT 30",
    )?;
    let rows = st
        .query_map([], map_run)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn map_run(r: &rusqlite::Row) -> rusqlite::Result<RedteamRun> {
    Ok(RedteamRun {
        id: r.get(0)?,
        title: r.get(1)?,
        status: r.get(2)?,
        error: r.get(3)?,
        paper_id: r.get(4)?,
        paper_title: r.get(5)?,
        venue: r.get(6)?,
        soundness: r.get(7)?,
        novelty: r.get(8)?,
        rigor: r.get(9)?,
        verdict: r.get(10)?,
        summary: r.get(11)?,
        created_at: r.get(12)?,
        updated_at: r.get(13)?,
    })
}

pub fn get_run(conn: &Connection, id: &str) -> Result<RedteamRun> {
    conn.query_row(
        "SELECT r.id, r.title, r.status, r.error, r.paper_id, p.title, r.venue,
                r.soundness, r.novelty, r.rigor, r.verdict, r.summary, r.created_at, r.updated_at
           FROM redteam_runs r JOIN papers p ON p.id = r.paper_id WHERE r.id=?1",
        [id],
        map_run,
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("redteam run {id}")),
        other => Error::Db(other),
    })
}

pub fn get_result(conn: &Connection, run_id: &str) -> Result<RedteamResult> {
    let run = get_run(conn, run_id)?;
    let mut st = conn.prepare(
        "SELECT id, kind, title, detail, quote, created_at
           FROM redteam_findings WHERE run_id=?1 ORDER BY created_at ASC",
    )?;
    let findings = st
        .query_map(params![run_id], |r| {
            Ok(RedteamFinding {
                id: r.get(0)?,
                kind: r.get(1)?,
                title: r.get(2)?,
                detail: r.get(3)?,
                quote: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(RedteamResult { run, findings })
}

pub fn delete_run(conn: &Connection, run_id: &str) -> Result<()> {
    conn.execute("DELETE FROM redteam_findings WHERE run_id=?1", [run_id])?;
    conn.execute("DELETE FROM redteam_runs WHERE id=?1", [run_id])?;
    Ok(())
}

/// Store one model payload as findings. Returns (stored, dropped).
///
/// Split out of `execute` so the validation rules are testable without a
/// model: quote requirements, score clamping, verdict reconciliation, and
/// the quote-must-exist-in-the-card check all live here.
pub fn store_findings(
    conn: &Connection,
    run_id: &str,
    payload: &serde_json::Value,
) -> Result<(usize, usize)> {
    let card_lower = crate::papers::collect_paper_card(conn, &paper_id_of(conn, run_id)?)?.1.to_lowercase();
    let mut stored = 0usize;
    let mut dropped = 0usize;
    let clamp = |s: &str, n: usize| s.chars().take(n).collect::<String>();

    if let Some(list) = payload.get("findings").and_then(|v| v.as_array()) {
        for f in list.iter().take(24) {
            let kind = f
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_lowercase();
            if !matches!(kind.as_str(), "fatal" | "weakness" | "rescue" | "rebuttal") {
                dropped += 1;
                continue;
            }
            let title = clamp(f.get("title").and_then(|v| v.as_str()).unwrap_or("").trim(), 120);
            let detail = clamp(
                f.get("detail").and_then(|v| v.as_str()).unwrap_or("").trim(),
                2000,
            );
            if title.is_empty() || detail.is_empty() {
                dropped += 1;
                continue;
            }
            let quote_raw = f.get("quote").and_then(|v| v.as_str()).unwrap_or("").trim();
            let quote = if quote_raw.is_empty() {
                None
            } else {
                Some(clamp(quote_raw, 400))
            };
            // An accusation without its evidence is an assertion. And the
            // quote must actually be in THIS paper's card — a reviewer who
            // "quotes" a sentence the paper never wrote is fabricating.
            if quote_required(&kind) {
                match &quote {
                    Some(q) if card_lower.contains(&q.to_lowercase()) => {}
                    _ => {
                        dropped += 1;
                        continue;
                    }
                }
            }
            conn.execute(
                "INSERT INTO redteam_findings (id, run_id, kind, title, detail, quote, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    new_id(),
                    run_id,
                    kind,
                    title,
                    detail,
                    quote,
                    now_ms()
                ],
            )?;
            stored += 1;
        }
    }

    // Scores + verdict land on the run row.
    let score = |key: &str| -> Option<i64> {
        payload
            .get(key)
            .and_then(|v| v.as_i64())
            .map(|n| n.clamp(1, 10))
    };
    let summary = payload
        .get("summary")
        .and_then(|v| v.as_str())
        .map(|s| clamp(s.trim(), 2000));
    let fatal_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM redteam_findings WHERE run_id=?1 AND kind='fatal'",
        params![run_id],
        |r| r.get(0),
    )?;
    let verdict = reconcile_verdict(
        payload.get("verdict").and_then(|v| v.as_str()).unwrap_or(""),
        fatal_count as usize,
    );
    conn.execute(
        "UPDATE redteam_runs
            SET status='done', error=NULL, soundness=?2, novelty=?3, rigor=?4,
                verdict=?5, summary=?6, updated_at=?7
          WHERE id=?1",
        params![
            run_id,
            score("soundness"),
            score("novelty"),
            score("rigor"),
            verdict,
            summary,
            now_ms()
        ],
    )?;
    Ok((stored, dropped))
}

fn paper_id_of(conn: &Connection, run_id: &str) -> Result<String> {
    conn.query_row(
        "SELECT paper_id FROM redteam_runs WHERE id=?1",
        [run_id],
        |r| r.get(0),
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("redteam run {run_id}")),
        other => Error::Db(other),
    })
}

/// Execute a run: one LLM call over the paper card. Records token usage.
pub fn execute(app: &AppHandle, run_id: &str) -> Result<()> {
    let state = app.state::<AppState>();
    let (paper_title, card, mut model) = {
        let c = state.db.lock().unwrap();
        let run = get_run(&c, run_id)?;
        if run.status != "running" {
            return Err(Error::Other("该次评审已结束，请重新发起".into()));
        }
        let card = crate::papers::collect_paper_card(&c, &run.paper_id)?.1;
        let spec = crate::commands::model_spec_for(&c, "cheatsheet");
        let keys = crate::commands::read_keys(&c)?;
        let mut model = crate::llm::from_spec_or_any(&spec, &keys).ok_or_else(|| {
            Error::Other("No model configured - add an API key in Settings -> API keys".into())
        })?;
        crate::commands::apply_budget(&mut model, &c, "cheatsheet");
        (run.paper_title, card, model)
    };

    let sys = crate::prompts::text(crate::prompts::REDTEAM_REVIEW);
    let user = format!(
        "VENUE: {}\nPAPER TITLE: {}\nPAPER CARD:\n{}\nReturn the review JSON now.",
        venue_of(app, run_id)?,
        paper_title,
        card
    );
    model.set_max_tokens(6000);
    let raw = model.complete(&sys, &user);
    let parsed = match raw {
        Ok(r) => {
            {
                let c = state.db.lock().unwrap();
                crate::tokens::record(
                    &c,
                    "redteam",
                    &model.name(),
                    &format!("{sys}\n{user}"),
                    &r,
                    None,
                );
            }
            crate::llm::extract_json(&r).ok()
        }
        Err(e) => {
            let c = state.db.lock().unwrap();
            c.execute(
                "UPDATE redteam_runs SET status='error', error=?2, updated_at=?3 WHERE id=?1",
                params![run_id, format!("模型调用失败：{e}"), now_ms()],
            )?;
            return Err(Error::Other(format!("模型调用失败：{e}")));
        }
    };
    let Some(payload) = parsed else {
        let c = state.db.lock().unwrap();
        c.execute(
            "UPDATE redteam_runs SET status='error', error='模型输出无法解析为 JSON', updated_at=?2 WHERE id=?1",
            params![run_id, now_ms()],
        )?;
        return Err(Error::Other("模型输出无法解析为 JSON".into()));
    };

    {
        let c = state.db.lock().unwrap();
        store_findings(&c, run_id, &payload)?;
    }
    Ok(())
}

fn venue_of(app: &AppHandle, run_id: &str) -> Result<String> {
    let state = app.state::<AppState>();
    let c = state.db.lock().unwrap();
    conn_venue(&c, run_id)
}

fn conn_venue(conn: &Connection, run_id: &str) -> Result<String> {
    conn.query_row(
        "SELECT venue FROM redteam_runs WHERE id=?1",
        [run_id],
        |r| r.get(0),
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("redteam run {run_id}")),
        other => Error::Db(other),
    })
}

/// Markdown report: scores, verdict, then findings grouped by kind.
pub fn to_markdown(result: &RedteamResult) -> String {
    let r = &result.run;
    let mut out = format!("# {} · {}\n\n", r.title, r.venue);
    let scores = match (r.soundness, r.novelty, r.rigor) {
        (Some(s), Some(n), Some(g)) => format!("Soundness {s}/10 · Novelty {n}/10 · Rigor {g}/10"),
        _ => "评分缺失".to_string(),
    };
    out.push_str(&format!(
        "**结论：{}**（{}）\n\n",
        r.verdict.as_deref().unwrap_or("未给出"),
        scores
    ));
    if let Some(s) = &r.summary {
        out.push_str(&format!("{s}\n\n"));
    }
    let sections = [
        ("fatal", "致命问题（单独任何一条都足以拒稿）"),
        ("weakness", "次要问题"),
        ("rescue", "抢救性实验"),
        ("rebuttal", "反驳防守策略"),
    ];
    for (kind, label) in sections {
        let items: Vec<&RedteamFinding> =
            result.findings.iter().filter(|f| f.kind == kind).collect();
        if items.is_empty() {
            continue;
        }
        out.push_str(&format!("## {label}\n\n"));
        for (i, f) in items.iter().enumerate() {
            out.push_str(&format!("{}. **{}** — {}\n", i + 1, f.title, f.detail));
            if let Some(q) = &f.quote {
                out.push_str(&format!("   > 「{q}」\n"));
            }
        }
        out.push('\n');
    }
    out
}

// ---- commands -------------------------------------------------------------

#[tauri::command]
pub fn redteam_run_create(
    state: State<'_, AppState>,
    paper_id: String,
    venue: Option<String>,
) -> Result<String> {
    let c = state.db.lock().unwrap();
    create_run(&c, &paper_id, venue.as_deref())
}

#[tauri::command]
pub fn redteam_run_list(state: State<'_, AppState>) -> Result<Vec<RedteamRun>> {
    let c = state.db.lock().unwrap();
    list_runs(&c)
}

#[tauri::command]
pub fn redteam_run_result(state: State<'_, AppState>, run_id: String) -> Result<RedteamResult> {
    let c = state.db.lock().unwrap();
    get_result(&c, &run_id)
}

#[tauri::command]
pub fn redteam_run_delete(state: State<'_, AppState>, run_id: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    delete_run(&c, &run_id)
}

#[tauri::command]
pub async fn redteam_run_execute(app: AppHandle, run_id: String) -> Result<()> {
    tauri::async_runtime::spawn_blocking(move || execute(&app, &run_id))
        .await
        .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

#[tauri::command]
pub fn redteam_export_md(state: State<'_, AppState>, run_id: String) -> Result<String> {
    let c = state.db.lock().unwrap();
    Ok(to_markdown(&get_result(&c, &run_id)?))
}

#[tauri::command]
pub fn redteam_md_file(state: State<'_, AppState>, run_id: String, dest: String) -> Result<()> {
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
    fn create_requires_a_real_paper() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        assert!(create_run(&c, "nope", None).is_err());
        let p = seed_paper(&c, "Paper A", "We train on 12 examples.");
        let id = create_run(&c, &p, None).unwrap();
        let run = get_run(&c, &id).unwrap();
        assert_eq!(run.status, "running");
        assert_eq!(run.venue, DEFAULT_VENUE);
        assert_eq!(run.paper_title, "Paper A");
    }

    #[test]
    fn an_accusation_without_a_real_quote_is_dropped() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let p = seed_paper(&c, "Paper A", "We train on 12 examples and claim SOTA.");
        let run_id = create_run(&c, &p, None).unwrap();
        let (stored, dropped) = store_findings(
            &c,
            &run_id,
            &serde_json::json!({
                "soundness": 4, "novelty": 6, "rigor": 3, "verdict": "major",
                "findings": [
                    {"kind":"fatal","title":"样本太小","detail":"n=12 撑不起结论","quote":"We train on 12 examples"},
                    {"kind":"fatal","title":"引用了不存在的话","detail":"卡片里没有这句","quote":"this sentence is not in the card at all"},
                    {"kind":"weakness","title":"没有引用就指责","detail":"缺引用","quote":""}
                ]
            }),
        )
        .unwrap();
        assert_eq!(stored, 1, "only the finding whose quote exists may land");
        assert_eq!(dropped, 2);
        let r = get_result(&c, &run_id).unwrap();
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].kind, "fatal");
        assert_eq!(r.findings[0].quote.as_deref(), Some("We train on 12 examples"));
    }

    #[test]
    fn strategies_may_go_unquoted_but_accusations_may_not() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let p = seed_paper(&c, "Paper A", "We train on 12 examples.");
        let run_id = create_run(&c, &p, None).unwrap();
        let (stored, _) = store_findings(
            &c,
            &run_id,
            &serde_json::json!({
                "findings": [
                    {"kind":"rescue","title":"补 10 倍样本","detail":"重跑全表"},
                    {"kind":"rebuttal","title":"小样本是本领域常态","detail":"引同期工作"}
                ]
            }),
        )
        .unwrap();
        assert_eq!(stored, 2);
    }

    #[test]
    fn scores_are_clamped_and_accept_cannot_survive_a_fatal_flaw() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let p = seed_paper(&c, "Paper A", "We train on 12 examples.");
        let run_id = create_run(&c, &p, None).unwrap();
        store_findings(
            &c,
            &run_id,
            &serde_json::json!({
                "soundness": 11, "novelty": 0, "rigor": 7, "verdict": "accept",
                "findings": [
                    {"kind":"fatal","title":"样本太小","detail":"n=12","quote":"We train on 12 examples"}
                ]
            }),
        )
        .unwrap();
        let r = get_result(&c, &run_id).unwrap();
        assert_eq!(r.run.soundness, Some(10), "11/10 is corrected to 10");
        assert_eq!(r.run.novelty, Some(1), "0 is corrected to 1");
        assert_eq!(r.run.rigor, Some(7));
        assert_eq!(r.run.verdict.as_deref(), Some("major"));
        assert_eq!(r.run.status, "done");
    }

    #[test]
    fn an_unknown_verdict_falls_back_to_caution() {
        assert_eq!(reconcile_verdict("accept", 0), "accept");
        assert_eq!(reconcile_verdict("banana", 0), "major");
        assert_eq!(reconcile_verdict("Reject", 0), "reject");
        assert_eq!(reconcile_verdict("accept", 2), "major");
    }

    #[test]
    fn markdown_groups_findings_by_kind() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let p = seed_paper(&c, "Paper A", "We train on 12 examples.");
        let run_id = create_run(&c, &p, Some("ICML")).unwrap();
        store_findings(
            &c,
            &run_id,
            &serde_json::json!({
                "soundness": 5, "novelty": 5, "rigor": 5, "verdict": "major",
                "summary": "想法有趣，证据不足。",
                "findings": [
                    {"kind":"fatal","title":"样本太小","detail":"n=12","quote":"We train on 12 examples"},
                    {"kind":"rescue","title":"补样本","detail":"重跑"}
                ]
            }),
        )
        .unwrap();
        let md = to_markdown(&get_result(&c, &run_id).unwrap());
        assert!(md.contains("ICML"));
        assert!(md.contains("致命问题"));
        assert!(md.contains("抢救性实验"));
        assert!(md.contains("「We train on 12 examples」"));
        assert!(md.contains("想法有趣，证据不足"));
        delete_run(&c, &run_id).unwrap();
        assert!(get_result(&c, &run_id).is_err());
    }
}
