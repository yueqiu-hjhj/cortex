//! Drafts — the last leg of the research pipeline: a selection of excerpts
//! becomes a citable paragraph. The excerpt ids (in citation order) are stored
//! alongside the payload, so [1] always resolves to `excerpt_ids[0]` even if the
//! model rearranges or drops citations; the reference list is built HERE, never
//! taken from the model.

use std::collections::HashSet;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::db::{new_id, now_ms};
use crate::error::{Error, Result};

/// One numbered citation slot, resolved server-side from the picked excerpts.
#[derive(Debug, Clone, Serialize)]
pub struct DraftRef {
    pub n: i64,
    pub excerpt_id: String,
    pub paper_id: Option<String>,
    pub title: String,
    pub authors: Option<String>,
    pub year: Option<i64>,
    pub page: Option<i64>,
    pub quote: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Draft {
    pub id: String,
    pub title: String,
    pub excerpt_ids: Vec<String>,
    pub meta: Option<String>,
    pub payload: Option<serde_json::Value>,
    pub created_at: i64,
}

/// What the model returns: {thesis, paragraphs:[{text, cites}]}.
#[derive(Debug, Clone, Deserialize)]
pub struct DraftBody {
    #[serde(default)]
    pub thesis: String,
    #[serde(default)]
    pub paragraphs: Vec<DraftPara>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DraftPara {
    pub text: String,
    #[serde(default)]
    pub cites: Vec<i64>,
}

/// Resolve the picked excerpts into ordered citation slots, preserving the
/// caller's order and rejecting duplicates. Errors if any excerpt has been
/// deleted since it was picked — better a loud failure than a silent [3] that
/// points at nothing.
pub fn load_refs(conn: &Connection, excerpt_ids: &[String]) -> Result<Vec<DraftRef>> {
    let mut seen = HashSet::new();
    let mut ordered: Vec<&String> = Vec::new();
    for id in excerpt_ids {
        if !id.is_empty() && seen.insert(id.as_str()) {
            ordered.push(id);
        }
    }
    if ordered.is_empty() {
        return Err(Error::Other("No excerpts selected.".into()));
    }
    let mut refs = Vec::with_capacity(ordered.len());
    for (i, id) in ordered.into_iter().enumerate() {
        let row = conn
            .query_row(
                "SELECT e.id, e.paper_id, COALESCE(p.title, ''), p.authors, p.year, e.page, e.quote, e.note
                 FROM excerpts e LEFT JOIN papers p ON p.id = e.paper_id
                 WHERE e.id = ?1",
                params![id],
                |r| {
                    Ok(DraftRef {
                        n: (i + 1) as i64,
                        excerpt_id: r.get(0)?,
                        paper_id: r.get(1)?,
                        title: r.get(2)?,
                        authors: r.get(3)?,
                        year: r.get(4)?,
                        page: r.get(5)?,
                        quote: r.get(6)?,
                        note: r.get(7)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("excerpt {id} (deleted since it was picked)")))?;
        refs.push(row);
    }
    Ok(refs)
}

/// The numbered excerpt blocks handed to the model as the user message.
pub fn build_user_prompt(refs: &[DraftRef]) -> String {
    let mut blocks = String::new();
    for r in refs {
        let mut head = format!("[{}] Paper: {}", r.n, r.title);
        let mut meta: Vec<String> = Vec::new();
        if let Some(a) = r.authors.as_deref().filter(|s| !s.trim().is_empty()) {
            meta.push(a.to_string());
        }
        if let Some(y) = r.year {
            meta.push(y.to_string());
        }
        if !meta.is_empty() {
            head.push_str(&format!(" ({})", meta.join(", ")));
        }
        if let Some(p) = r.page {
            head.push_str(&format!(", p.{p}"));
        }
        head.push_str("\n    Quote: ");
        blocks.push_str(&head);
        blocks.push_str(r.quote.trim());
        blocks.push('\n');
        if let Some(note) = r.note.as_deref().filter(|s| !s.trim().is_empty()) {
            blocks.push_str(&format!("    Note: {}\n", note.trim()));
        }
        blocks.push('\n');
    }
    blocks.trim_end().to_string()
}

pub fn save(
    conn: &Connection,
    title: &str,
    excerpt_ids: &[String],
    meta: &str,
    payload: &serde_json::Value,
) -> Result<String> {
    let id = new_id();
    conn.execute(
        "INSERT INTO drafts (id, title, excerpt_ids, meta, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id,
            title,
            serde_json::to_string(excerpt_ids).unwrap_or_else(|_| "[]".into()),
            meta,
            payload.to_string(),
            now_ms(),
        ],
    )?;
    Ok(id)
}

pub fn get(conn: &Connection, id: &str) -> Result<Draft> {
    conn.query_row(
        "SELECT id, title, excerpt_ids, meta, payload, created_at FROM drafts WHERE id = ?1",
        params![id],
        row_to_draft,
    )
    .map_err(|_| Error::NotFound(format!("draft {id}")))
}

pub fn list(conn: &Connection) -> Result<Vec<Draft>> {
    let mut st = conn.prepare(
        "SELECT id, title, excerpt_ids, meta, payload, created_at FROM drafts ORDER BY created_at DESC LIMIT 100",
    )?;
    let rows = st.query_map([], row_to_draft)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    let n = conn.execute("DELETE FROM drafts WHERE id = ?1", params![id])?;
    if n == 0 {
        return Err(Error::NotFound(format!("draft {id}")));
    }
    Ok(())
}

fn row_to_draft(r: &rusqlite::Row<'_>) -> rusqlite::Result<Draft> {
    let ids_raw: String = r.get(2)?;
    let payload_raw: Option<String> = r.get(4)?;
    Ok(Draft {
        id: r.get(0)?,
        title: r.get(1)?,
        excerpt_ids: serde_json::from_str(&ids_raw).unwrap_or_default(),
        meta: r.get(3)?,
        payload: payload_raw.and_then(|p| serde_json::from_str(&p).ok()),
        created_at: r.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::AppState;
    use crate::papers;

    fn state() -> AppState {
        AppState::in_memory().unwrap()
    }

    fn seed(conn: &Connection) -> Vec<String> {
        let p = papers::import_paper(
            conn,
            &papers::PaperInput {
                title: "Attention Is All You Need".into(),
                authors: Some("Ashish Vaswani, Noam Shazeer".into()),
                year: Some(2017),
                venue: Some("NeurIPS".into()),
                doi: None,
                url: None,
                abstract_text: None,
                subject_id: None,
                source_id: None,
            },
        )
        .unwrap();
        let e1 = papers::add_excerpt(
            conn,
            Some(&p.id),
            None,
            "Self-attention beats recurrence.",
            Some(3),
            Some("core claim"),
            None,
        )
        .unwrap();
        let e2 = papers::add_excerpt(
            conn,
            Some(&p.id),
            None,
            "Multi-head attention captures subspaces.",
            Some(4),
            None,
            None,
        )
        .unwrap();
        vec![e1.id, e2.id]
    }

    #[test]
    fn refs_keep_pick_order_and_dedupe() {
        let st = state();
        let c = st.db.lock().unwrap();
        let ids = seed(&c);
        let refs = load_refs(&c, &[ids[1].clone(), ids[0].clone(), ids[1].clone()]).unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].n, 1);
        assert_eq!(refs[0].excerpt_id, ids[1]);
        assert_eq!(refs[1].n, 2);
        assert_eq!(refs[1].title, "Attention Is All You Need");
        assert_eq!(refs[1].page, Some(3));
    }

    #[test]
    fn missing_excerpt_is_a_loud_error() {
        let st = state();
        let c = st.db.lock().unwrap();
        let ids = seed(&c);
        let err = load_refs(&c, &[ids[0].clone(), "gone".into()]).unwrap_err();
        assert!(err.to_string().contains("gone"));
    }

    #[test]
    fn empty_selection_is_rejected() {
        let st = state();
        let c = st.db.lock().unwrap();
        assert!(load_refs(&c, &[]).is_err());
    }

    #[test]
    fn user_prompt_numbers_excerpts() {
        let st = state();
        let c = st.db.lock().unwrap();
        let ids = seed(&c);
        let refs = load_refs(&c, &[ids[0].clone()]).unwrap();
        let p = build_user_prompt(&refs);
        assert!(p.contains("[1] Paper: Attention Is All You Need (Ashish Vaswani, Noam Shazeer, 2017), p.3"));
        assert!(p.contains("Quote: Self-attention beats recurrence."));
        assert!(p.contains("Note: core claim"));
    }

    #[test]
    fn save_get_list_roundtrip() {
        let st = state();
        let c = st.db.lock().unwrap();
        let ids = seed(&c);
        let payload = serde_json::json!({
            "thesis": "t",
            "paragraphs": [{"text": "x [1]", "cites": [1]}],
            "references": []
        });
        let id = save(&c, "成稿", &ids, "2 条摘录 · 1 段", &payload).unwrap();
        let d = get(&c, &id).unwrap();
        assert_eq!(d.excerpt_ids, ids);
        assert_eq!(d.payload.unwrap()["paragraphs"][0]["cites"][0], 1);
        assert_eq!(list(&c).unwrap().len(), 1);
        delete(&c, &id).unwrap();
        assert!(list(&c).unwrap().is_empty());
    }
}
