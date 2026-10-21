//! XMind export — take the knowledge framework somewhere else.
//!
//! 知源 lets you export the framework as XMind with one click; Cortex only had
//! the G6 star-map, which dies with the app. This writes a real `.xmind`
//! (XMind 2020+ "content.json" format, a zip with three members):
//!
//! ```text
//! sheet.xmind
//! ├── content.json   [{"rootTopic": {...tree...}}]
//! ├── metadata.json
//! └── manifest.json
//! ```
//!
//! Nodes map by parent_id; mastery becomes a label ("掌握 62%") so the
//! recipient sees how well each concept is known without a legend.

use crate::error::{Error, Result};
use crate::models::KnowledgeGraph;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;

fn topic(node: &crate::models::KnowledgeNode, children: Vec<Value>) -> Value {
    let mut t = json!({
        "id": node.id,
        "class": "topic",
        "title": if node.title.trim().is_empty() { node.node_key.clone() } else { node.title.clone() },
    });
    // Mastery as a label — XMind renders labels as small pills.
    if node.observations > 0 {
        t["labels"] = json!([format!("掌握 {:.0}%", node.mastery * 100.0)]);
    }
    if let Some(s) = node.summary.as_deref().filter(|s| !s.trim().is_empty()) {
        t["notes"] = json!({ "plain": { "content": s.trim() } });
    }
    if !children.is_empty() {
        t["children"] = json!({ "attached": children });
    }
    t
}

/// Build the topic tree (roots → children by parent_id, position-ordered).
fn build_tree(graph: &KnowledgeGraph) -> Vec<Value> {
    let mut by_parent: HashMap<Option<&str>, Vec<&crate::models::KnowledgeNode>> = HashMap::new();
    for n in &graph.nodes {
        by_parent.entry(n.parent_id.as_deref()).or_default().push(n);
    }
    for v in by_parent.values_mut() {
        v.sort_by_key(|n| n.position);
    }
    // Recursive build; cycle-safe by depth cap (frameworks are shallow, but a
    // corrupted parent chain must not hang the export).
    fn build(
        parent: Option<&str>,
        map: &HashMap<Option<&str>, Vec<&crate::models::KnowledgeNode>>,
        depth: usize,
    ) -> Vec<Value> {
        if depth > 32 {
            return Vec::new();
        }
        map.get(&parent)
            .map(|kids| {
                kids.iter()
                    .map(|n| {
                        let children = build(Some(n.id.as_str()), map, depth + 1);
                        topic(n, children)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    build(None, &by_parent, 0)
}

/// Render a knowledge graph as a .xmind (zip bytes).
pub fn to_xmind(graph: &KnowledgeGraph, sheet_title: &str) -> Result<Vec<u8>> {
    if graph.nodes.is_empty() {
        return Err(Error::Other(
            "知识框架还是空的 —— 先生成/同步框架再导出".into(),
        ));
    }
    let roots = build_tree(graph);
    // Exactly one root is the XMind convention: wrap multiples in a subject node.
    let root = if roots.len() == 1 {
        roots.into_iter().next().unwrap()
    } else {
        json!({
            "id": "root",
            "class": "topic",
            "title": sheet_title,
            "children": { "attached": roots }
        })
    };
    let content = json!([{
        "id": "sheet-1",
        "class": "sheet",
        "title": sheet_title,
        "rootTopic": root
    }]);
    let metadata = json!({
        "creator": { "name": "Cortex", "version": env!("CARGO_PKG_VERSION") },
        "activeSheetId": "sheet-1"
    });
    let manifest = json!({ "file-entries": { "content.json": {}, "metadata.json": {} } });

    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let opts: zip::write::SimpleFileOptions =
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, val) in [
            ("content.json", &content),
            ("metadata.json", &metadata),
            ("manifest.json", &manifest),
        ] {
            zip.start_file(name, opts)
                .map_err(|e| Error::Other(format!("zip {name}: {e}")))?;
            zip.write_all(&serde_json::to_vec_pretty(val).map_err(|e| Error::Other(e.to_string()))?)
                .map_err(|e| Error::Other(format!("write {name}: {e}")))?;
        }
        zip.finish()
            .map_err(|e| Error::Other(format!("zip finish: {e}")))?;
    }
    Ok(buf.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{CalibrationSummary, KnowledgeNode};

    fn node(id: &str, parent: Option<&str>, title: &str, pos: i64, mastery: f64) -> KnowledgeNode {
        KnowledgeNode {
            id: id.into(),
            subject_id: "s1".into(),
            topic_id: None,
            source_id: None,
            parent_id: parent.map(|p| p.to_string()),
            node_key: id.into(),
            kind: "concept".into(),
            title: title.into(),
            summary: None,
            position: pos,
            coverage: 1.0,
            mastery,
            importance: 1.0,
            confidence: 1.0,
            observations: if mastery > 0.0 { 3 } else { 0 },
            note_count: 0,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn graph(nodes: Vec<KnowledgeNode>) -> KnowledgeGraph {
        KnowledgeGraph {
            subject_id: "s1".into(),
            nodes,
            edges: Vec::new(),
            calibration: CalibrationSummary {
                observations: 0,
                correct: 0,
                mean_confidence: 0.0,
                accuracy: 0.0,
                brier_score: 0.0,
                buckets: Vec::new(),
            },
        }
    }

    #[test]
    fn hierarchy_maps_parent_ids_in_position_order() {
        let g = graph(vec![
            node("root1", None, "线性代数", 0, 0.8),
            node("c2", Some("root1"), "特征值", 2, 0.6),
            node("c1", Some("root1"), "行列式", 1, 0.9),
        ]);
        let bytes = to_xmind(&g, "线代框架").unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut content = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("content.json").unwrap(), &mut content).unwrap();
        let v: Value = serde_json::from_str(&content).unwrap();
        let root = &v[0]["rootTopic"];
        assert_eq!(root["title"], "线性代数");
        let kids = root["children"]["attached"].as_array().unwrap();
        assert_eq!(kids.len(), 2);
        // position order: 行列式(1) before 特征值(2)
        assert_eq!(kids[0]["title"], "行列式");
        assert_eq!(kids[1]["title"], "特征值");
        // mastery label present when observations > 0
        assert!(kids[1]["labels"][0].as_str().unwrap().contains("60%"));
    }

    #[test]
    fn multiple_roots_wrap_in_sheet_topic() {
        let g = graph(vec![
            node("a", None, "A", 0, 0.0),
            node("b", None, "B", 1, 0.0),
        ]);
        let bytes = to_xmind(&g, "框架").unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut content = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("content.json").unwrap(), &mut content).unwrap();
        let v: Value = serde_json::from_str(&content).unwrap();
        let root = &v[0]["rootTopic"];
        assert_eq!(root["title"], "框架");
        assert_eq!(root["children"]["attached"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn empty_graph_is_an_honest_error() {
        let g = graph(vec![]);
        assert!(to_xmind(&g, "x").unwrap_err().to_string().contains("空"));
    }
}
