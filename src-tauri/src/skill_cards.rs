//! Cross-book skill cards — methodology precipitation.
//!
//! cross_book::detect already finds concepts that recur across books
//! (subjects). What it does NOT do is turn that into a usable artifact: a
//! recurring method in 3 books is a SKILL worth owning, and this module
//! precipitates it into a card: what the method is, when to reach for it,
//! concrete steps, and which books demonstrate it. The derivation is honest
//! data (the recurring concept + its book appearances); the prose is the
//! model's, labeled `source: "model"` — same reviewability contract as
//! PRISMA/D8.
//!
//! One run (`precipitate`) generates cards for up to N top candidates
//! (most-recurring concepts first), skipping concepts that already have a
//! card unless forced.

use crate::db::{new_id, now_ms};
use crate::error::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use crate::db::AppState;
use tauri::{AppHandle, Manager, State};

#[derive(Debug, Clone, Serialize)]
pub struct SkillCandidate {
    /// The recurring concept (lowercased match key).
    pub concept: String,
    /// Display title (first occurrence's title).
    pub title: String,
    /// (subject_id, subject_name) per book the concept appears in.
    pub books: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillCard {
    pub id: String,
    pub concept: String,
    pub title: String,
    pub summary: String,
    pub when_to_use: String,
    /// 3-5 concrete steps (model output).
    pub steps: Vec<String>,
    /// (subject_id, subject_name) appearances the card was derived from.
    pub appearances: Vec<(String, String)>,
    pub model: String,
    pub created_at: i64,
}

const SYSTEM: &str = "You are a study-methodology distiller. You are given a CONCEPT that \
recurs across several books/courses, plus the book names it appears in. Produce a reusable \
SKILL CARD for a student: how to actually apply this method. Return ONLY raw JSON of the shape \
{\"title\":string,\"summary\":string,\"when_to_use\":string,\"steps\":[string,...]}. Rules: \
summary = 1-2 sentences on what the method is. when_to_use = the concrete triggers (\"when you \
see …\", \"before you …\"). steps = 3-5 action steps, each one sentence, imperative, specific — \
no fluff, no \"understand the material\". Write in the SAME language as the concept.";

/// Top recurring-concepts candidates for cards (most books first, then mastery).
pub fn candidates(conn: &Connection, limit: usize) -> Result<Vec<SkillCandidate>> {
    let detected = crate::cross_book::detect(conn)?;
    let mut out: Vec<SkillCandidate> = Vec::new();
    for c in detected {
        let title = c
            .nodes
            .iter()
            .max_by(|a, b| a.mastery.partial_cmp(&b.mastery).unwrap_or(std::cmp::Ordering::Equal))
            .map(|n| n.title.clone())
            .unwrap_or_else(|| c.concept.clone());
        let books: Vec<(String, String)> = c
            .nodes
            .iter()
            .map(|n| (n.subject_id.clone(), n.subject_name.clone()))
            .collect();
        out.push(SkillCandidate {
            concept: c.concept,
            title,
            books,
        });
    }
    out.sort_by(|a, b| b.books.len().cmp(&a.books.len()).then(a.title.cmp(&b.title)));
    out.truncate(limit);
    Ok(out)
}

pub fn list_cards(conn: &Connection) -> Result<Vec<SkillCard>> {
    let mut st = conn.prepare(
        "SELECT id, concept, title, summary, when_to_use, steps, appearances, model, created_at
           FROM skill_cards ORDER BY created_at DESC LIMIT 50",
    )?;
    let rows = st
        .query_map([], |r| {
            let steps: String = r.get(5)?;
            let apps: String = r.get(6)?;
            Ok(SkillCard {
                id: r.get(0)?,
                concept: r.get(1)?,
                title: r.get(2)?,
                summary: r.get(3)?,
                when_to_use: r.get(4)?,
                steps: serde_json::from_str(&steps).unwrap_or_default(),
                appearances: serde_json::from_str(&apps).unwrap_or_default(),
                model: r.get(7)?,
                created_at: r.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn delete_card(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM skill_cards WHERE id=?1", [id])?;
    Ok(())
}

/// Generate up to `limit` cards from the top recurring concepts. Skips
/// concepts that already have a card. Returns (generated, skipped_existing).
pub fn precipitate(app: &AppHandle, limit: usize) -> Result<(usize, usize)> {
    let state = app.state::<crate::db::AppState>();
    let (cands, mut model) = {
        let c = state.db.lock().unwrap();
        let cands = candidates(&c, limit * 3)?;
        let spec: String = c
            .query_row("SELECT value FROM settings WHERE key='model_cheatsheet'", [], |r| r.get(0))
            .unwrap_or_else(|_| "openrouter:deepseek/deepseek-v4-flash".to_string());
        let keys = crate::commands::read_keys(&c)?;
        let mut model = crate::llm::from_spec_or_any(&spec, &keys).ok_or_else(|| {
            Error::Other("No model configured - add an API key in Settings -> API keys".into())
        })?;
        crate::commands::apply_budget(&mut model, &c, "cheatsheet");
        (cands, model)
    };

    let mut generated = 0usize;
    let mut skipped = 0usize;
    for cand in cands.iter().take(limit * 2) {
        if generated >= limit {
            break;
        }
        {
            let c = state.db.lock().unwrap();
            let exists: bool = c
                .query_row(
                    "SELECT 1 FROM skill_cards WHERE concept=?1",
                    [&cand.concept],
                    |_| Ok(true),
                )
                .optional()?
                .unwrap_or(false);
            if exists {
                skipped += 1;
                continue;
            }
        }
        let book_list = cand
            .books
            .iter()
            .map(|(_, n)| n.clone())
            .collect::<Vec<_>>()
            .join("、");
        let user = format!(
            "CONCEPT: {}\nAPPEARS IN ({} books): {}\nReturn the skill card JSON now.",
            cand.title,
            cand.books.len(),
            book_list
        );
        model.set_max_tokens(1024);
        let raw = match model.complete(SYSTEM, &user) {
            Ok(r) => r,
            Err(_) => continue, // one failed concept must not abort the run
        };
        {
            let c = state.db.lock().unwrap();
            crate::tokens::record(&c, "skill-card", &model.name(), &format!("{SYSTEM}\\n{user}"), &raw, None);
        }
        let Some(payload) = crate::llm::extract_json(&raw).ok() else { continue };
        let clamp = |s: &str, n: usize| s.trim().chars().take(n).collect::<String>();
        let title = clamp(payload.get("title").and_then(|v| v.as_str()).unwrap_or(&cand.title), 60);
        let summary = clamp(payload.get("summary").and_then(|v| v.as_str()).unwrap_or(""), 300);
        let when = clamp(payload.get("when_to_use").and_then(|v| v.as_str()).unwrap_or(""), 300);
        let steps: Vec<String> = payload
            .get("steps")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str())
                    .map(|s| clamp(s, 200))
                    .collect()
            })
            .unwrap_or_default();
        if summary.is_empty() || steps.is_empty() {
            continue;
        }
        let c = state.db.lock().unwrap();
        c.execute(
            "INSERT INTO skill_cards (id, concept, title, summary, when_to_use, steps, appearances, model, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                new_id(),
                cand.concept,
                title,
                summary,
                when,
                serde_json::to_string(&steps)?,
                serde_json::to_string(&cand.books)?,
                model.name(),
                now_ms()
            ],
        )?;
        generated += 1;
    }
    Ok((generated, skipped))
}

// ---- commands -------------------------------------------------------------

#[tauri::command]
pub fn skill_card_candidates(state: State<AppState>, limit: Option<usize>) -> Result<Vec<SkillCandidate>> {
    let c = state.db.lock().unwrap();
    candidates(&c, limit.unwrap_or(8).clamp(1, 20))
}

#[tauri::command]
pub fn skill_cards_list(state: State<AppState>) -> Result<Vec<SkillCard>> {
    let c = state.db.lock().unwrap();
    list_cards(&c)
}

#[tauri::command]
pub fn skill_card_delete(state: State<AppState>, id: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    delete_card(&c, &id)
}

/// Generate up to `limit` skill cards from recurring cross-book concepts.
/// One concept failing must not abort the run.
#[tauri::command]
pub async fn skill_cards_precipitate(app: AppHandle, limit: Option<usize>) -> Result<(usize, usize)> {
    tauri::async_runtime::spawn_blocking(move || precipitate(&app, limit.unwrap_or(5).clamp(1, 12)))
        .await
        .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_require_cross_book_recurrence() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        // Two books each with the same concept title → a candidate.
        let s1 = crate::repo::insert_subject(&c, "概率论", None, None, None).unwrap();
        let s2 = crate::repo::insert_subject(&c, "线代", None, None, None).unwrap();
        let mk = |sid: &str, title: &str| {
            crate::repo::upsert_knowledge_node(&c, sid, None, None, None, title, "concept", title, None, 1, 1.0).unwrap();
        };
        let _ = s1;
        let _ = s2;
        mk(&crate::repo::list_subjects(&c).unwrap()[0].id, "贝叶斯定理");
        mk(&crate::repo::list_subjects(&c).unwrap()[1].id, "贝叶斯定理");
        // Same-book repeat does NOT count (dedup by subject inside detect).
        mk(&crate::repo::list_subjects(&c).unwrap()[0].id, "条件概率");
        mk(&crate::repo::list_subjects(&c).unwrap()[0].id, "条件概率");
        let cands = candidates(&c, 10).unwrap();
        let titles: Vec<&str> = cands.iter().map(|c| c.title.as_str()).collect();
        assert!(titles.contains(&"贝叶斯定理"), "cross-book concept must surface");
        assert!(!titles.contains(&"条件概率"), "single-book repeats are not cross-book");
        let cd = cands.iter().find(|c| c.title == "贝叶斯定理").unwrap();
        assert_eq!(cd.books.len(), 2);
    }

    #[test]
    fn card_list_and_delete() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        c.execute(
            "INSERT INTO skill_cards (id, concept, title, summary, when_to_use, steps, appearances, model, created_at)
             VALUES ('k1','bayes','贝叶斯定理','一种更新信念的方法','看到先验证据时', '[\"写出先验\",\"乘似然\",\"归一化\"]',
                     '[[\"s1\",\"概率论\"],[\"s2\",\"线代\"]]','m',1)",
            [],
        )
        .unwrap();
        let cards = list_cards(&c).unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].steps.len(), 3);
        assert_eq!(cards[0].appearances.len(), 2);
        delete_card(&c, "k1").unwrap();
        assert!(list_cards(&c).unwrap().is_empty());
    }
}
