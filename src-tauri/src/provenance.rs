//! Source view — 知源's 来源视图: the bipartite graph of materials × knowledge
//! points, where a point backed by exactly ONE material is called out as a
//! 孤证 (single-source claim) instead of being quietly presented as settled.
//!
//! The whole graph is DERIVED — no table, no migration, nothing to go stale.
//! Three kinds of support, all deterministic and explainable:
//!
//!   topic × source   structural: the source was filed under that topic
//!   point × note     `note_concepts` (the same matcher notes→coverage uses)
//!   concept × source text: the concept title occurs in the source's chunks
//!                    (the SAME matcher, so "counts as evidence" means one
//!                    thing everywhere in the app)
//!
//! Verdicts are computed on LIBRARY SOURCES only — two of your own notes are
//! not independent confirmation of anything:
//!
//!   cross      ≥2 sources back it      → 交叉验证
//!   single     exactly 1 source        → 孤证, prompt to add sources
//!   notes_only 0 sources but ≥1 note   → still only your own writing
//!   orphan     nothing at all
//!
//! Why derive instead of storing: `sync_knowledge_graph` runs on every
//! Subject Overview mount and concept nodes appear whenever a node doc or a
//! tutor session creates one — any stored copy of this graph would need a
//! write hook on all of those paths and would still drift. The computation is
//! one pass over the subject's chunk text, which is milliseconds.

use crate::db::AppState;
use crate::error::Result;
use crate::note_knowledge::match_hits;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use tauri::State;

/// One left-hand node: an imported source or one of your notes.
#[derive(Debug, Clone, Serialize)]
pub struct MaterialNode {
    pub id: String,
    pub name: String,
    /// "source" | "note"
    pub kind: String,
    /// For sources: whether it parsed into chunks (None for notes — a note
    /// IS its own text, there is nothing to parse).
    pub parsed: Option<bool>,
    /// How many points this material supports (edges out of it).
    pub supports: usize,
}

/// One right-hand node: a topic or concept of the subject.
#[derive(Debug, Clone, Serialize)]
pub struct PointNode {
    pub id: String,
    pub title: String,
    /// "topic" | "concept"
    pub kind: String,
    /// Distinct library sources backing this point.
    pub source_count: usize,
    /// Distinct notes mentioning this point.
    pub note_count: usize,
    /// cross | single | notes_only | orphan
    pub verdict: String,
}

/// One edge: this material supports this point, this strongly, this way.
#[derive(Debug, Clone, Serialize)]
pub struct SupportEdge {
    pub material_id: String,
    pub point_id: String,
    /// Occurrences of the point's title in the material (1 for structural
    /// and note edges, where the count means something different).
    pub hits: usize,
    /// "topic" (structural) | "text" (found in chunks) | "mention" (note link)
    pub via: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Bipartite {
    pub materials: Vec<MaterialNode>,
    pub points: Vec<PointNode>,
    pub edges: Vec<SupportEdge>,
}

/// Build the bipartite view for one subject. Pure read: no writes, no model.
pub fn bipartite(conn: &Connection, subject_id: &str) -> Result<Bipartite> {
    // ---- left side: materials -------------------------------------------
    let mut materials: Vec<MaterialNode> = Vec::new();
    {
        let mut st = conn.prepare(
            "SELECT id, name, status FROM sources WHERE subject_id=?1 ORDER BY created_at DESC",
        )?;
        let rows = st.query_map(params![subject_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for (id, name, status) in rows.collect::<rusqlite::Result<Vec<_>>>()? {
            let chunks: i64 = conn.query_row(
                "SELECT COUNT(*) FROM chunks WHERE source_id=?1",
                params![&id],
                |r| r.get(0),
            )?;
            materials.push(MaterialNode {
                id,
                name,
                kind: "source".into(),
                // "ready" with zero chunks is a parse that produced nothing —
                // it can ground nothing, and saying otherwise would be a lie.
                parsed: Some(status == "ready" && chunks > 0),
                supports: 0,
            });
        }
    }
    {
        let mut st = conn.prepare(
            "SELECT id, title FROM notes WHERE subject_id=?1 ORDER BY updated_at DESC",
        )?;
        let rows = st.query_map(params![subject_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for (id, title) in rows.collect::<rusqlite::Result<Vec<_>>>()? {
            materials.push(MaterialNode {
                id,
                name: title,
                kind: "note".into(),
                parsed: None,
                supports: 0,
            });
        }
    }

    // ---- right side: points ----------------------------------------------
    let mut points: Vec<PointNode> = Vec::new();
    {
        let mut st = conn.prepare(
            "SELECT id, title, kind FROM knowledge_nodes
              WHERE subject_id=?1 AND kind IN ('topic','concept')
              ORDER BY position, title",
        )?;
        let rows = st.query_map(params![subject_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for (id, title, kind) in rows.collect::<rusqlite::Result<Vec<_>>>()? {
            points.push(PointNode {
                id,
                title,
                kind,
                source_count: 0,
                note_count: 0,
                verdict: "orphan".into(),
            });
        }
    }

    // Position indexes, built once: edges and verdicts are resolved through
    // these instead of closures, so the final counting pass can mutate the
    // node vectors without fighting the borrow checker.
    let mat_pos: std::collections::HashMap<&str, usize> = materials
        .iter()
        .enumerate()
        .map(|(i, m)| (m.id.as_str(), i))
        .collect();
    let pt_pos: std::collections::HashMap<&str, usize> = points
        .iter()
        .enumerate()
        .map(|(i, p)| (p.id.as_str(), i))
        .collect();

    let mut edges: Vec<SupportEdge> = Vec::new();

    // ---- edge kind 1: topic × source (structural) -------------------------
    // A source filed under a topic supports that topic — this is the filing
    // system, not a text match, so hits stays 1.
    {
        let mut st = conn.prepare(
            "SELECT s.id, s.topic_id FROM sources s
              WHERE s.subject_id=?1 AND s.topic_id IS NOT NULL",
        )?;
        let rows = st.query_map(params![subject_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for (source_id, topic_id) in rows.collect::<rusqlite::Result<Vec<_>>>()? {
            let node_key = format!("topic:{topic_id}");
            let point_id: Option<String> = conn
                .query_row(
                    "SELECT id FROM knowledge_nodes WHERE subject_id=?1 AND node_key=?2",
                    params![subject_id, node_key],
                    |r| r.get(0),
                )
                .optional()?;
            if let (Some(&mi), Some(pid)) = (
                mat_pos.get(source_id.as_str()),
                point_id.as_deref().and_then(|id| pt_pos.get(id)),
            ) {
                edges.push(SupportEdge {
                    material_id: materials[mi].id.clone(),
                    point_id: points[*pid].id.clone(),
                    hits: 1,
                    via: "topic".into(),
                });
            }
        }
    }

    // ---- edge kind 2: point × note (already materialized) ------------------
    {
        let mut st = conn.prepare(
            "SELECT nc.note_id, nc.node_id, nc.hits FROM note_concepts nc
              JOIN notes n ON n.id = nc.note_id
             WHERE n.subject_id=?1",
        )?;
        let rows = st.query_map(params![subject_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)? as usize,
            ))
        })?;
        for (note_id, node_id, hits) in rows.collect::<rusqlite::Result<Vec<_>>>()? {
            if let (Some(&mi), Some(&pi)) = (
                mat_pos.get(note_id.as_str()),
                pt_pos.get(node_id.as_str()),
            ) {
                edges.push(SupportEdge {
                    material_id: materials[mi].id.clone(),
                    point_id: points[pi].id.clone(),
                    hits: hits.max(1),
                    via: "mention".into(),
                });
            }
        }
    }

    // ---- edge kind 3: concept × source (text match) -------------------------
    // Only for parsed sources (no chunks → nothing to match) and only for
    // concepts (topics already have their structural edge). One pass over
    // each source's text, reused across every concept.
    let concept_ids: Vec<String> = points
        .iter()
        .filter(|p| p.kind == "concept")
        .map(|p| p.id.clone())
        .collect();
    if !concept_ids.is_empty() {
        // Load each parsed source's full text once.
        let mut corpus: Vec<(String, String)> = Vec::new(); // (source_id, lowercased text)
        for m in &materials {
            if m.kind != "source" || m.parsed != Some(true) {
                continue;
            }
            let mut text = String::new();
            let mut st = conn.prepare("SELECT text FROM chunks WHERE source_id=?1 ORDER BY ord")?;
            let rows = st.query_map(params![&m.id], |r| r.get::<_, String>(0))?;
            for t in rows {
                text.push_str(&t?);
                text.push('\n');
            }
            corpus.push((m.id.clone(), text.to_lowercase()));
        }
        for pid in &concept_ids {
            let title: String = conn.query_row(
                "SELECT title FROM knowledge_nodes WHERE id=?1",
                params![pid],
                |r| r.get(0),
            )?;
            for (source_id, hay) in &corpus {
                let hits = match_hits(hay, &title);
                if hits == 0 {
                    continue;
                }
                if let (Some(&mi), Some(&pi)) = (
                    mat_pos.get(source_id.as_str()),
                    pt_pos.get(pid.as_str()),
                ) {
                    edges.push(SupportEdge {
                        material_id: materials[mi].id.clone(),
                        point_id: points[pi].id.clone(),
                        hits,
                        via: "text".into(),
                    });
                }
            }
        }
    }

    // ---- verdicts + support counts -----------------------------------------
    // Computed into locals first, applied after: the index maps borrow the
    // node vectors immutably, and the final pass has to write into them.
    // Materials are counted DISTINCT per point, so duplicate edges (the same
    // source reaching a point two ways) can never inflate a verdict.
    let note_ids: std::collections::HashSet<&str> = materials
        .iter()
        .filter(|m| m.kind == "note")
        .map(|m| m.id.as_str())
        .collect();
    let mut verdicts: Vec<(usize, String, usize, usize)> = Vec::new(); // (point idx, verdict, sources, notes)
    for (pi, p) in points.iter().enumerate() {
        let mut sources: Vec<&str> = Vec::new();
        let mut notes: Vec<&str> = Vec::new();
        for e in &edges {
            if e.point_id != p.id {
                continue;
            }
            let bucket = if note_ids.contains(e.material_id.as_str()) {
                &mut notes
            } else {
                &mut sources
            };
            if !bucket.contains(&e.material_id.as_str()) {
                bucket.push(&e.material_id);
            }
        }
        let verdict = match (sources.len(), notes.len()) {
            (0, 0) => "orphan",
            (0, _) => "notes_only",
            (1, _) => "single",
            _ => "cross",
        }
        .to_string();
        verdicts.push((pi, verdict, sources.len(), notes.len()));
    }
    let mut support_counts = vec![0usize; materials.len()];
    for e in &edges {
        if let Some(&mi) = mat_pos.get(e.material_id.as_str()) {
            support_counts[mi] += 1;
        }
    }
    for (pi, verdict, source_count, note_count) in verdicts {
        points[pi].verdict = verdict;
        points[pi].source_count = source_count;
        points[pi].note_count = note_count;
    }
    for (m, n) in materials.iter_mut().zip(support_counts) {
        m.supports = n;
    }

    Ok(Bipartite {
        materials,
        points,
        edges,
    })
}

#[tauri::command]
pub fn provenance_bipartite(
    state: State<'_, AppState>,
    subject_id: String,
) -> Result<Bipartite> {
    let c = state.db.lock().unwrap();
    bipartite(&c, &subject_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo;

    fn seed() -> (AppState, String, Vec<String>) {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let sid = repo::insert_subject(&c, "网络", None, None, None).unwrap();
        let mut ids = Vec::new();
        for name in ["课件A", "课件B", "课件C"] {
            ids.push(
                repo::insert_source(&c, &sid, None, name, "txt", Some("test")).unwrap(),
            );
        }
        drop(c);
        (st, sid, ids)
    }

    fn concept(c: &Connection, subject: &str, key: &str, title: &str) -> String {
        let id = format!("kn-{key}");
        let now = crate::db::now_ms();
        c.execute(
            "INSERT INTO knowledge_nodes (id, subject_id, node_key, kind, title, created_at, updated_at)
             VALUES (?1,?2,?3,'concept',?4,?5,?5)",
            params![id, subject, key, title, now],
        )
        .unwrap();
        id
    }

    #[test]
    fn a_point_two_sources_back_is_cross_validated() {
        let (st, sid, srcs) = seed();
        let c = st.db.lock().unwrap();
        repo::sync_knowledge_graph(&c, &sid).unwrap();
        let bbr = concept(&c, &sid, "bbr", "BBR");
        // Both parsed sources mention it.
        for s in &srcs[..2] {
            repo::finalize_source(&c, s, "ready", None, None, None).unwrap();
            repo::insert_chunk(&c, s, &sid, None, 0, "BBR 是谷歌的拥塞控制算法。", None, 2, &[0u8; 8], "").unwrap();
        }
        let b = bipartite(&c, &sid).unwrap();
        let p = b.points.iter().find(|p| p.id == bbr).unwrap();
        assert_eq!(p.verdict, "cross", "{p:?}");
        assert_eq!(p.source_count, 2);
        let text_edges = b
            .edges
            .iter()
            .filter(|e| e.point_id == bbr && e.via == "text")
            .count();
        assert_eq!(text_edges, 2);
    }

    #[test]
    fn a_point_one_source_backs_is_a_single_source_claim() {
        let (st, sid, srcs) = seed();
        let c = st.db.lock().unwrap();
        repo::sync_knowledge_graph(&c, &sid).unwrap();
        let bbr = concept(&c, &sid, "bbr", "BBR");
        repo::finalize_source(&c, &srcs[0], "ready", None, None, None).unwrap();
        repo::insert_chunk(&c, &srcs[0], &sid, None, 0, "BBR 是谷歌的拥塞控制算法。", None, 2, &[0u8; 8], "").unwrap();
        let b = bipartite(&c, &sid).unwrap();
        let p = b.points.iter().find(|p| p.id == bbr).unwrap();
        assert_eq!(p.verdict, "single", "one source is a 孤证, not consensus: {p:?}");
        assert_eq!(p.source_count, 1);
    }

    #[test]
    fn notes_alone_are_not_independent_confirmation() {
        let (st, sid, _srcs) = seed();
        let c = st.db.lock().unwrap();
        repo::sync_knowledge_graph(&c, &sid).unwrap();
        let bbr = concept(&c, &sid, "bbr", "BBR");
        // Two of the user's own notes mention the point; no library source does.
        for i in 0..2 {
            let nid = repo::insert_note(&c, Some(&sid), None, &format!("笔记{i}"), "BBR 笔记正文").unwrap();
            c.execute(
                "INSERT INTO note_concepts (note_id, node_id, hits, created_at, updated_at)
                 VALUES (?1,?2,3,1,1)",
                params![nid, bbr],
            )
            .unwrap();
        }
        let b = bipartite(&c, &sid).unwrap();
        let p = b.points.iter().find(|p| p.id == bbr).unwrap();
        assert_eq!(p.verdict, "notes_only", "{p:?}");
        assert_eq!(p.note_count, 2);
        assert_eq!(p.source_count, 0);
    }

    #[test]
    fn a_point_nothing_supports_is_an_orphan() {
        let (st, sid, _srcs) = seed();
        let c = st.db.lock().unwrap();
        repo::sync_knowledge_graph(&c, &sid).unwrap();
        let ghost = concept(&c, &sid, "ghost", "幽灵概念");
        let b = bipartite(&c, &sid).unwrap();
        let p = b.points.iter().find(|p| p.id == ghost).unwrap();
        assert_eq!(p.verdict, "orphan");
    }

    #[test]
    fn an_unparsed_source_grounds_nothing_by_text() {
        let (st, sid, srcs) = seed();
        let c = st.db.lock().unwrap();
        repo::sync_knowledge_graph(&c, &sid).unwrap();
        let bbr = concept(&c, &sid, "bbr", "BBR");
        // Chunks exist but the source never finished: "draft" is not evidence.
        repo::insert_chunk(&c, &srcs[0], &sid, None, 0, "BBR 是谷歌的拥塞控制算法。", None, 2, &[0u8; 8], "").unwrap();
        let b = bipartite(&c, &sid).unwrap();
        let p = b.points.iter().find(|p| p.id == bbr).unwrap();
        assert_eq!(p.verdict, "orphan", "a draft source must not ground anything: {p:?}");
        let m = b.materials.iter().find(|m| m.id == srcs[0]).unwrap();
        assert_eq!(m.parsed, Some(false));
    }

    #[test]
    fn a_source_filed_under_a_topic_supports_that_topic() {
        let (st, sid, srcs) = seed();
        let c = st.db.lock().unwrap();
        let topic = repo::insert_topic(&c, &sid, "传输层", None, &[]).unwrap();
        c.execute(
            "UPDATE sources SET topic_id=?2 WHERE id=?1",
            params![&srcs[0], &topic],
        )
        .unwrap();
        repo::sync_knowledge_graph(&c, &sid).unwrap();
        let b = bipartite(&c, &sid).unwrap();
        let topic_point = b
            .points
            .iter()
            .find(|p| p.kind == "topic" && p.title == "传输层")
            .unwrap();
        assert_eq!(topic_point.source_count, 1);
        assert!(
            b.edges
                .iter()
                .any(|e| e.point_id == topic_point.id && e.via == "topic" && e.material_id == srcs[0])
        );
    }

    #[test]
    fn materials_count_their_supports() {
        let (st, sid, srcs) = seed();
        let c = st.db.lock().unwrap();
        repo::sync_knowledge_graph(&c, &sid).unwrap();
        let bbr = concept(&c, &sid, "bbr", "BBR");
        let vegas = concept(&c, &sid, "vegas", "Vegas");
        repo::finalize_source(&c, &srcs[0], "ready", None, None, None).unwrap();
        repo::insert_chunk(&c, &srcs[0], &sid, None, 0, "BBR 与 Vegas 都是拥塞控制算法。", None, 2, &[0u8; 8], "").unwrap();
        let b = bipartite(&c, &sid).unwrap();
        let m = b.materials.iter().find(|m| m.id == srcs[0]).unwrap();
        assert_eq!(m.supports, 2, "one source backing two points: {m:?}");
        assert!(b.points.iter().any(|p| p.id == vegas && p.source_count == 1));
        assert!(b.points.iter().any(|p| p.id == bbr && p.source_count == 1));
    }
}
