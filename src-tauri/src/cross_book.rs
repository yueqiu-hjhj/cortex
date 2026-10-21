//! Cross-book knowledge network: shared concept detection and bridges across
//! subjects. A concept is "shared" when two subjects have knowledge nodes whose
//! normalized title (or node_key) match exactly. The network endpoint returns
//! the concept as a node plus the per-subject mastery estimate so the UI can
//! render a bridge graph ("Mitochondria" appears in Biology and Cell Biology,
//! mastered 90% in one and 40% in the other).

use crate::error::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossConcept {
    pub concept: String,
    pub nodes: Vec<CrossNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossNode {
    pub subject_id: String,
    pub subject_name: String,
    pub node_id: String,
    pub title: String,
    pub mastery: f64,
    pub coverage: f64,
}

pub fn detect(conn: &Connection) -> Result<Vec<CrossConcept>> {
    let mut stmt = conn.prepare(
        "SELECT LOWER(kn.title) AS concept, kn.id, kn.subject_id, s.name, kn.title, kn.mastery, kn.coverage
           FROM knowledge_nodes kn
           JOIN subjects s ON s.id = kn.subject_id
          WHERE kn.kind IN ('concept','topic')
          ORDER BY concept"
    )?;
    let mut by_concept: std::collections::BTreeMap<String, Vec<CrossNode>> = std::collections::BTreeMap::new();
    let rows = stmt.query_map([], |r| {
        let concept: String = r.get(0)?;
        let node_id: String = r.get(1)?;
        let subject_id: String = r.get(2)?;
        let subject_name: String = r.get(3)?;
        let title: String = r.get(4)?;
        let mastery: f64 = r.get(5)?;
        let coverage: f64 = r.get(6)?;
        Ok((concept, CrossNode { subject_id, subject_name, node_id, title, mastery, coverage }))
    })?;
    for row in rows {
        let (concept, node) = row?;
        by_concept.entry(concept).or_default().push(node);
    }
    let mut out = Vec::new();
    for (concept, mut nodes) in by_concept {
        let mut seen = std::collections::HashSet::new();
        nodes.retain(|n| seen.insert(n.subject_id.clone()));
        if nodes.len() >= 2 {
            out.push(CrossConcept { concept, nodes });
        }
    }
    Ok(out)
}

pub fn bridges_for_subject(conn: &Connection, subject_id: &str) -> Result<Vec<CrossConcept>> {
    let mut out = Vec::new();
    let all = detect(conn)?;
    for concept in all {
        if concept.nodes.iter().any(|n| n.subject_id == subject_id) {
            out.push(concept);
        }
    }
    Ok(out)
}

pub fn apply_cross_links(conn: &Connection) -> Result<usize> {
    let concepts = detect(conn)?;
    let mut n = 0;
    let tx = conn.unchecked_transaction()?;
    for concept in concepts {
        let count = (concept.nodes.len() - 1) as i64;
        for node in &concept.nodes {
            tx.execute(
                // Bump the *source* half too: `coverage` is now derived from
                // source_coverage ∪ notes on every sync, so raising only the
                // displayed number would be undone at the next mount.
                "UPDATE knowledge_nodes
                    SET source_coverage = MIN(1.0, source_coverage + ?1 * 0.05),
                        coverage = MIN(1.0, coverage + ?1 * 0.05)
                  WHERE id=?2",
                params![count, node.node_id],
            )?;
        }
        for i in 0..concept.nodes.len() {
            for j in (i + 1)..concept.nodes.len() {
                let a = &concept.nodes[i];
                let b = &concept.nodes[j];
                tx.execute(
                    "INSERT OR REPLACE INTO knowledge_edges
                        (id, subject_id, from_id, to_id, kind, weight, created_at)
                     VALUES (?1,?2,?3,?4,'cross',?5,?6)",
                    params![crate::db::new_id(), a.subject_id, a.node_id, b.node_id, 0.4, crate::db::now_ms()],
                )?;
                n += 1;
            }
        }
    }
    tx.commit()?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_concepts_form_bridges() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let s1 = crate::repo::insert_subject(&c, "Bio", None, None, None).unwrap();
        let s2 = crate::repo::insert_subject(&c, "Cell Bio", None, None, None).unwrap();
        crate::repo::sync_knowledge_graph(&c, &s1).unwrap();
        crate::repo::sync_knowledge_graph(&c, &s2).unwrap();
        // Add a shared concept node to each subject.
        let s1_root: String = c.query_row("SELECT id FROM knowledge_nodes WHERE subject_id=?1", params![&s1], |r| r.get(0)).unwrap();
        let s2_root: String = c.query_row("SELECT id FROM knowledge_nodes WHERE subject_id=?1", params![&s2], |r| r.get(0)).unwrap();
        let insert = |id: &str, sid: &str, parent: &str| {
            c.execute(
                "INSERT INTO knowledge_nodes (id, subject_id, topic_id, source_id, parent_id, node_key, kind, title, summary, position, coverage, mastery, importance, confidence, observations, created_at, updated_at) VALUES (?1,?2,NULL,NULL,?3,'concept:Mitochondria','concept','Mitochondria','organelle',1,0.5,0.4,0.5,0.3,0,1,1)",
                params![id, sid, parent],
            ).unwrap();
        };
        insert("kn-bio-mito", &s1, &s1_root);
        insert("kn-cell-mito", &s2, &s2_root);
        let bridges = bridges_for_subject(&c, &s1).unwrap();
        assert!(bridges.iter().any(|b| b.concept == "mitochondria"), "bridges: {:?}", bridges);
        let n = apply_cross_links(&c).unwrap();
        assert!(n > 0);
        let cross_count: i64 = c.query_row("SELECT count(*) FROM knowledge_edges WHERE kind='cross'", [], |r| r.get(0)).unwrap();
        assert!(cross_count > 0);
    }
}