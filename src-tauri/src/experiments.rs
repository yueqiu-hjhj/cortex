//! Experiment cards — the lightweight 复现实验 layer (科研一体化's last hole).
//!
//! OpenScience/AIPOCH bring full notebook + SSH/Slurm runtimes; that is a
//! product of its own. ResearchClaw proved a lighter shape works: a CARD that
//! rides along with the paper and records the four things reproducibility
//! actually needs — hypothesis, code, environment, observed result. The card
//! is data (livesync-mergeable, attached to the paper, searchable alongside
//! excerpts); EXECUTION is delegated to the agent platform (agent_exec tools
//! can read/write experiment cards via the DB like any other row), so the app
//! never shells out itself.
//!
//! Status flow mirrors the reading pipeline: planned → running → done|failed.

use crate::db::{new_id, now_ms};
use crate::error::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

const STATUSES: [&str; 4] = ["planned", "running", "done", "failed"];

#[derive(Debug, Clone, Serialize)]
pub struct Experiment {
    pub id: String,
    pub paper_id: Option<String>,
    pub subject_id: Option<String>,
    pub title: String,
    pub hypothesis: Option<String>,
    pub code: Option<String>,
    pub environment: Option<String>,
    pub result: Option<String>,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
}

const COLS: &str = "id, paper_id, subject_id, title, hypothesis, code, environment, result, \
                    status, created_at, updated_at";

fn row_to_exp(r: &rusqlite::Row) -> rusqlite::Result<Experiment> {
    Ok(Experiment {
        id: r.get(0)?,
        paper_id: r.get(1)?,
        subject_id: r.get(2)?,
        title: r.get(3)?,
        hypothesis: r.get(4)?,
        code: r.get(5)?,
        environment: r.get(6)?,
        result: r.get(7)?,
        status: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
    })
}

fn validate_status(status: &str) -> Result<()> {
    if STATUSES.contains(&status) {
        Ok(())
    } else {
        Err(Error::Other(format!(
            "实验状态只能是 planned/running/done/failed，收到 '{status}'"
        )))
    }
}

pub fn get(conn: &Connection, id: &str) -> Result<Experiment> {
    conn.query_row(
        &format!("SELECT {COLS} FROM experiments WHERE id = ?1"),
        [id],
        row_to_exp,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("experiment {id}")))
}

/// List experiments for a paper, a subject, or everything (both filters optional).
pub fn list(
    conn: &Connection,
    paper_id: Option<&str>,
    subject_id: Option<&str>,
) -> Result<Vec<Experiment>> {
    let mut sql = format!("SELECT {COLS} FROM experiments WHERE 1=1");
    let mut bound: Vec<String> = Vec::new();
    if let Some(p) = paper_id.filter(|s| !s.is_empty()) {
        sql.push_str(" AND paper_id = ?");
        bound.push(p.to_string());
    }
    if let Some(s) = subject_id.filter(|s| !s.is_empty()) {
        sql.push_str(" AND subject_id = ?");
        bound.push(s.to_string());
    }
    sql.push_str(" ORDER BY updated_at DESC");
    let mut st = conn.prepare(&sql)?;
    let rows = st
        .query_map(rusqlite::params_from_iter(bound.iter()), row_to_exp)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn create(
    conn: &Connection,
    paper_id: Option<&str>,
    subject_id: Option<&str>,
    title: &str,
    hypothesis: Option<&str>,
    environment: Option<&str>,
) -> Result<Experiment> {
    if title.trim().is_empty() {
        return Err(Error::Other("实验标题不能为空".into()));
    }
    let id = new_id();
    let now = now_ms();
    conn.execute(
        "INSERT INTO experiments (id, paper_id, subject_id, title, hypothesis, environment, \
            status, created_at, updated_at) \
         VALUES (?1,?2,?3,?4,?5,?6,'planned',?7,?7)",
        params![id, paper_id, subject_id, title.trim(), hypothesis, environment, now],
    )?;
    get(conn, &id)
}

/// Update any subset of fields; None leaves the column untouched.
pub fn update(
    conn: &Connection,
    id: &str,
    title: Option<&str>,
    hypothesis: Option<&str>,
    code: Option<&str>,
    environment: Option<&str>,
    result: Option<&str>,
    status: Option<&str>,
) -> Result<Experiment> {
    get(conn, id)?;
    if let Some(s) = status {
        validate_status(s)?;
    }
    let now = now_ms();
    conn.execute(
        "UPDATE experiments SET \
            title = COALESCE(?2, title), \
            hypothesis = COALESCE(?3, hypothesis), \
            code = COALESCE(?4, code), \
            environment = COALESCE(?5, environment), \
            result = COALESCE(?6, result), \
            status = COALESCE(?7, status), \
            updated_at = ?8 \
          WHERE id = ?1",
        params![id, title, hypothesis, code, environment, result, status, now],
    )?;
    get(conn, id)
}

pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    let n = conn.execute("DELETE FROM experiments WHERE id = ?1", [id])?;
    if n == 0 {
        return Err(Error::NotFound(format!("experiment {id}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::papers::PaperInput;

    #[test]
    fn crud_roundtrip_and_status_validation() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let e = create(&c, None, None, "复现 Table 2", Some("同条件应得 88.2 acc"), Some("torch 2.4, seed 42")).unwrap();
        assert_eq!(e.status, "planned");

        let e = update(&c, &e.id, None, None, Some("print(train())"), None, Some("acc 87.9, 差 0.3"), Some("done")).unwrap();
        assert_eq!(e.status, "done");
        assert!(e.code.as_deref().unwrap().contains("train"));
        assert_eq!(e.result.as_deref(), Some("acc 87.9, 差 0.3"));

        assert!(update(&c, &e.id, None, None, None, None, None, Some("exploded")).is_err());
        assert!(create(&c, None, None, "  ", None, None).is_err());

        let by_sub = list(&c, None, None).unwrap();
        assert_eq!(by_sub.len(), 1);
        delete(&c, &e.id).unwrap();
        assert!(list(&c, None, None).unwrap().is_empty());
        assert!(delete(&c, &e.id).is_err());
    }

    #[test]
    fn list_filters_are_independent() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        // Real parent rows — FK enforcement is on.
        let sub1 = crate::repo::insert_subject(&c, "线代", None, None, None).unwrap();
        let sub2 = crate::repo::insert_subject(&c, "优化", None, None, None).unwrap();
        let paper = crate::papers::import_paper(
            &c,
            &PaperInput {
                title: "Attention".into(),
                authors: None, year: None, venue: None, doi: None, url: None,
                abstract_text: None, subject_id: None, source_id: None,
            },
        )
        .unwrap();
        let a = create(&c, Some(&paper.id), Some(&sub1), "A", None, None).unwrap();
        let b = create(&c, None, Some(&sub1), "B", None, None).unwrap();
        create(&c, None, Some(&sub2), "C", None, None).unwrap();
        assert_eq!(list(&c, Some(&paper.id), None).unwrap().len(), 1);
        assert_eq!(list(&c, None, Some(&sub1)).unwrap().len(), 2);
        assert_eq!(list(&c, None, None).unwrap().len(), 3);
        assert_eq!(list(&c, Some(&paper.id), Some(&sub1)).unwrap()[0].id, a.id);
        let _ = b;
    }
}
