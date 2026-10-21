//! Notes → knowledge coverage.
//!
//! The knowledge tree used to describe the *library*: a topic was covered iff
//! its sources had been imported and chunked. Your own writing was invisible in
//! it — so a subject could look 0% covered while you had written five notes on
//! it, which is the wrong answer to "have I covered this?".
//!
//! Two numbers now exist, and they mean different things:
//!
//!   source_coverage  what the imported library gives you (set by sync)
//!   coverage         source_coverage ∪ notes  ← the number the UI shows
//!
//! `coverage` is derived, so it can be recomputed from scratch at any time.
//! Editing or deleting a note therefore *lowers* the number again — nothing is
//! a one-way increment that leaves the tree lying.
//!
//! Matching is deliberately dumb and local: a note covers a knowledge node if
//! its title or body mentions that node's title (case-insensitive, word-bounded
//! for Latin scripts, plain substring for CJK). No embeddings, no model calls:
//! this runs on every save and must be instant and explainable. `hits` is stored
//! so a future weighting has the evidence available.

use crate::db::now_ms;
use crate::error::Result;
use crate::repo;
use rusqlite::{params, Connection};

/// Latin titles shorter than this ("AI", "OS") match too much prose to be
/// evidence; CJK titles are dense enough at two characters.
const MIN_ASCII_CHARS: usize = 3;
const MIN_CJK_CHARS: usize = 2;

/// A note covers a point once. The first note is worth a lot (you engaged with
/// it), every further note less (diminishing), and notes alone top out at 1.0
/// only after five — mastery is a separate BKT estimate and should not be
/// implied by coverage.
fn note_coverage_for(notes: i64) -> f64 {
    if notes <= 0 {
        return 0.0;
    }
    (0.4 + 0.15 * (notes - 1) as f64).min(1.0)
}

/// Rebuild one note's links, then refresh its subject's coverage.
/// Returns how many knowledge nodes the note mentions.
pub fn rebuild(conn: &Connection, note_id: &str) -> Result<usize> {
    let note = repo::get_note(conn, note_id)?;
    conn.execute(
        "DELETE FROM note_concepts WHERE note_id=?1",
        params![note_id],
    )?;
    let Some(subject_id) = note.subject_id.clone() else {
        return Ok(0);
    };
    // Title counts: a note called "BBR: 带宽探测" covers BBR even if the body
    // never repeats the word.
    let hay = format!("{}\n{}", note.title, note.body).to_lowercase();
    let candidates: Vec<(String, String)> = {
        let mut st = conn.prepare(
            "SELECT id, title FROM knowledge_nodes
              WHERE subject_id=?1 AND kind IN ('topic','concept')",
        )?;
        let rows = st.query_map(params![subject_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let now = now_ms();
    let mut linked = 0;
    for (node_id, title) in candidates {
        let needle = title.trim().to_lowercase();
        if !matchable(&needle) {
            continue;
        }
        let hits = count_hits(&hay, &needle);
        if hits == 0 {
            continue;
        }
        conn.execute(
            "INSERT OR REPLACE INTO note_concepts
                (note_id, node_id, hits, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![note_id, node_id, hits as i64, now],
        )?;
        linked += 1;
    }
    apply_coverage(conn, &subject_id)?;
    Ok(linked)
}

fn matchable(needle: &str) -> bool {
    let len = needle.chars().count();
    if needle.is_ascii() {
        len >= MIN_ASCII_CHARS
    } else {
        len >= MIN_CJK_CHARS
    }
}

/// Occurrences of `needle` in `hay`. Latin matches must sit on word boundaries
/// ("AI" should not be found inside "said"); CJK has no spaces, so a plain
/// substring is the only sensible unit.
fn count_hits(hay: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    if !needle.is_ascii() {
        return hay.matches(needle).count();
    }
    let mut hits = 0;
    let mut from = 0;
    while let Some(offset) = hay[from..].find(needle) {
        let at = from + offset;
        let end = at + needle.len();
        let before_ok = !hay[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric());
        let after_ok = !hay[end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric());
        if before_ok && after_ok {
            hits += 1;
        }
        from = end;
    }
    hits
}

/// Recompute `coverage` for every node of a subject from its two inputs.
/// Idempotent, and self-healing: links whose note disappeared are pruned first,
/// so any delete path (including restore-from-trash) converges on the next run.
pub fn apply_coverage(conn: &Connection, subject_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM note_concepts WHERE note_id NOT IN (SELECT id FROM notes)",
        [],
    )?;
    let now = now_ms();
    // Two passes instead of one clever UPDATE: the formula lives in Rust above,
    // and a subject has tens of nodes, not thousands.
    let nodes: Vec<String> = {
        let mut st = conn.prepare("SELECT id FROM knowledge_nodes WHERE subject_id=?1")?;
        let rows = st.query_map(params![subject_id], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for node_id in nodes {
        let notes: i64 = conn.query_row(
            "SELECT COUNT(*) FROM note_concepts WHERE node_id=?1",
            params![&node_id],
            |r| r.get(0),
        )?;
        conn.execute(
            "UPDATE knowledge_nodes
                SET coverage = MAX(source_coverage, ?2), updated_at=?3
              WHERE id=?1",
            params![&node_id, note_coverage_for(notes), now],
        )?;
    }
    Ok(())
}

/// Drop a deleted note's links and refresh the subject it lived in. Called after
/// the row is gone; the FK cascade usually got the links already, but the
/// coverage number still has to be recomputed.
pub fn note_removed(conn: &Connection, note_id: &str, subject_id: Option<&str>) -> Result<()> {
    conn.execute(
        "DELETE FROM note_concepts WHERE note_id=?1",
        params![note_id],
    )?;
    if let Some(sid) = subject_id {
        apply_coverage(conn, sid)?;
    }
    Ok(())
}

/// Backfill / repair: rebuild every note (optionally within one subject).
/// Returns the total number of links written.
pub fn rebuild_all(conn: &Connection, subject_id: Option<&str>) -> Result<usize> {
    let ids: Vec<String> = {
        let mut st = conn.prepare(
            "SELECT id FROM notes WHERE (?1 IS NULL OR subject_id=?1)",
        )?;
        let rows = st.query_map(params![subject_id], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut total = 0;
    for id in ids {
        total += rebuild(conn, &id)?;
    }
    if let Some(sid) = subject_id {
        apply_coverage(conn, sid)?;
    }
    Ok(total)
}

#[tauri::command]
pub fn note_knowledge_resync(
    state: tauri::State<crate::db::AppState>,
    subject_id: Option<String>,
) -> Result<usize> {
    let c = state.db.lock().unwrap();
    rebuild_all(&c, subject_id.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::AppState;

    /// Subject + topic node with no sources: the "0% covered although I wrote
    /// notes" case this module exists to fix.
    fn seeded(topic: &str) -> (AppState, String, String) {
        let st = AppState::in_memory().unwrap();
        let sid = {
            let c = st.db.lock().unwrap();
            let sid = repo::insert_subject(&c, "网络", None, None, None).unwrap();
            let tid = repo::insert_topic(&c, &sid, topic, None, &[]).unwrap();
            repo::sync_knowledge_graph(&c, &sid).unwrap();
            let _ = tid;
            sid
        };
        let node = {
            let c = st.db.lock().unwrap();
            c.query_row(
                "SELECT id FROM knowledge_nodes WHERE subject_id=?1 AND kind='topic'",
                params![&sid],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
        };
        (st, sid, node)
    }

    fn coverage(conn: &Connection, node_id: &str) -> f64 {
        conn.query_row(
            "SELECT coverage FROM knowledge_nodes WHERE id=?1",
            params![node_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn a_note_raises_the_coverage_of_the_point_it_names() {
        let (st, sid, node) = seeded("拥塞控制");
        let c = st.db.lock().unwrap();
        assert_eq!(coverage(&c, &node), 0.0, "no sources yet");
        let nid = repo::insert_note(&c, Some(&sid), None, "BBR 笔记", "拥塞控制里 BBR 靠探测带宽。").unwrap();
        let linked = rebuild(&c, &nid).unwrap();
        assert_eq!(linked, 1);
        assert!((coverage(&c, &node) - 0.4).abs() < 1e-6);
    }

    #[test]
    fn a_second_note_from_the_same_point_adds_less() {
        let (st, sid, node) = seeded("拥塞控制");
        let c = st.db.lock().unwrap();
        for (i, title) in ["一", "二", "三"].iter().enumerate() {
            let nid = repo::insert_note(&c, Some(&sid), None, title, "拥塞控制").unwrap();
            rebuild(&c, &nid).unwrap();
            let expected = note_coverage_for(i as i64 + 1);
            assert!((coverage(&c, &node) - expected).abs() < 1e-6, "note {}", i + 1);
        }
    }

    #[test]
    fn a_note_in_another_subject_does_not_move_this_one() {
        let (st, sid, node) = seeded("拥塞控制");
        let c = st.db.lock().unwrap();
        let other = repo::insert_subject(&c, "数据库", None, None, None).unwrap();
        let nid = repo::insert_note(&c, Some(&other), None, "无关", "拥塞控制").unwrap();
        rebuild(&c, &nid).unwrap();
        assert_eq!(coverage(&c, &node), 0.0);
        let _ = sid;
    }

    #[test]
    fn deleting_a_note_lowers_coverage_again() {
        let (st, sid, node) = seeded("拥塞控制");
        let c = st.db.lock().unwrap();
        let nid = repo::insert_note(&c, Some(&sid), None, "笔记", "拥塞控制").unwrap();
        rebuild(&c, &nid).unwrap();
        assert!(coverage(&c, &node) > 0.3);
        c.execute("DELETE FROM notes WHERE id=?1", params![&nid]).unwrap();
        apply_coverage(&c, &sid).unwrap();
        assert_eq!(coverage(&c, &node), 0.0, "derived, so it can fall back");
    }

    #[test]
    fn latin_titles_respect_word_boundaries() {
        // `count_hits` receives already-lowercased text from `rebuild`; the
        // boundary rule is what keeps "ai" out of "said".
        assert_eq!(count_hits("he said nothing", "ai"), 0);
        assert_eq!(count_hits("an ai model, ai again", "ai"), 2);
        assert_eq!(count_hits("bbr 与 bbr", "bbr"), 2);
    }

    #[test]
    fn short_latin_titles_are_not_evidence() {
        assert!(!matchable("ai"));
        assert!(matchable("bbr"));
        assert!(matchable("网络"));
        assert!(!matchable("网"));
    }

    #[test]
    fn sync_keeps_what_notes_added() {
        let (st, sid, node) = seeded("拥塞控制");
        let c = st.db.lock().unwrap();
        let nid = repo::insert_note(&c, Some(&sid), None, "笔记", "拥塞控制").unwrap();
        rebuild(&c, &nid).unwrap();
        assert!((coverage(&c, &node) - 0.4).abs() < 1e-6);
        // Every Subject Overview mount runs this; it must not undo your writing.
        repo::sync_knowledge_graph(&c, &sid).unwrap();
        assert!(
            (coverage(&c, &node) - 0.4).abs() < 1e-6,
            "a sync must not zero what notes added"
        );
    }

    #[test]
    fn imported_material_is_the_floor_for_notes() {
        let (st, sid, node) = seeded("拥塞控制");
        let c = st.db.lock().unwrap();
        c.execute(
            "UPDATE knowledge_nodes SET source_coverage=0.9, coverage=0.9 WHERE id=?1",
            params![&node],
        )
        .unwrap();
        let nid = repo::insert_note(&c, Some(&sid), None, "笔记", "拥塞控制").unwrap();
        rebuild(&c, &nid).unwrap();
        assert!((coverage(&c, &node) - 0.9).abs() < 1e-6, "one note cannot lower a covered topic");
    }

    #[test]
    fn backfill_links_notes_that_existed_before_the_feature() {
        let (st, sid, _node) = seeded("拥塞控制");
        let c = st.db.lock().unwrap();
        repo::insert_note(&c, Some(&sid), None, "旧笔记", "拥塞控制的公平性").unwrap();
        let n = rebuild_all(&c, Some(&sid)).unwrap();
        assert_eq!(n, 1);
    }
}
