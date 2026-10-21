//! Knowledge point docs — one point, one readable document.
//!
//! The idea worth stealing from 知源: a doc's *coordinates* (which layer a
//! point sits in, what must be read first) are computed **locally** from the
//! framework's own prerequisite edges; the model only writes the prose and a
//! short "why this order". A model that invented its own layering would
//! silently reorder the curriculum, so layer numbers go INTO the prompt and
//! are never parsed back OUT of the reply.
//!
//! Layering is a deterministic peel over `knowledge_edges(kind='prerequisite')`
//! (edge from a point to the point it depends on). A cycle cannot hang it:
//! whatever survives the peel lands one layer above the rest, which is wrong
//! in an interesting way rather than fatal.
//!
//! Grounding comes from what you already have: the node's own summary and the
//! notes that mention it (0064's note_concepts index). No notes yet is a
//! first-class state — the doc says so instead of inventing material.

use crate::db::AppState;
use crate::error::{Error, Result};
use crate::llm::Llm;
use crate::models::KnowledgeNode;
use crate::repo;
use rusqlite::{params, Connection};
use serde::Serialize;
use tauri::Manager;

/// System prompt for doc generation. Registered in `prompts` so the lab can
/// run and tune it like every other model call.
pub const SYSTEM: &str = r#"你是知识点讲解作者。给你一个知识点的标题、摘要、它在框架里的层号、前置知识点，以及用户自己笔记里提到它的原文。写一篇"能从头读到尾"的讲解材料。

规则：
1. 只能使用给定的材料。材料没提到的事实不要写；需要背景知识才能懂的地方，明确标「（背景：…，你的材料里没有，建议补）」
2. 层号和前置顺序是本地算好的事实，不是建议——按这个顺序讲，不要自己发明学习路径
3. 结构：一句话定义 → 为什么重要 → 材料里的关键点（引用原文时保留原句）→ 常见混淆或易错点 → 三行自测问题（不给答案）
4. 400–800 字，中文，Markdown，不要代码围栏
5. 写完材料后另起一行写 `---WHY---`，然后用 1–3 句话说明"为什么这个点排在第 N 层、要放在这些前置之后"——只解释顺序，不重复内容"#;

/// Separator between the doc body and the ordering rationale. The model is
/// asked for it explicitly; its absence is tolerated (empty `why`), its
/// presence is split out deterministically.
pub const WHY_MARKER: &str = "---WHY---";

/// One node's place in the local layering.
#[derive(Debug, Clone, Serialize)]
pub struct NodeLayer {
    pub node_id: String,
    pub title: String,
    pub kind: String,
    pub layer: i64,
}

/// What the UI shows: the cached doc (if any) plus freshly computed
/// coordinates. `has_doc` false renders the empty state, not an error.
#[derive(Debug, Clone, Serialize)]
pub struct NodeDoc {
    pub node_id: String,
    pub node_title: String,
    pub subject_id: String,
    pub layer: i64,
    pub layers: Vec<NodeLayer>,
    pub prereqs: Vec<String>,
    pub has_doc: bool,
    pub body: String,
    pub why: String,
    pub model: String,
    pub updated_at: i64,
}

/// Split a model reply into (body, why). A missing marker is not an error —
/// the doc is still useful without the ordering note.
pub fn split_why(raw: &str) -> (String, String) {
    let body = strip_fence(raw);
    match body.find(WHY_MARKER) {
        Some(i) => {
            let doc = body[..i].trim().to_string();
            let why = body[i + WHY_MARKER.len()..].trim().to_string();
            (doc, why)
        }
        None => (body, String::new()),
    }
}

fn strip_fence(raw: &str) -> String {
    let s = raw.trim();
    let s = match s.strip_prefix("```") {
        Some(rest) => match rest.find('\n') {
            Some(nl) => &rest[nl + 1..],
            None => rest,
        },
        None => s,
    };
    s.strip_suffix("```").unwrap_or(s).trim().to_string()
}

/// Kahn peel over prerequisite edges. Nodes nothing depends on are layer 0;
/// a node's layer is 1 + max(layer of its prerequisites). Edge direction:
/// from_id depends on to_id.
pub fn compute_layers(conn: &Connection, subject_id: &str) -> Result<Vec<NodeLayer>> {
    let mut nodes: Vec<(String, String, String)> = Vec::new(); // id, title, kind
    {
        let mut st = conn.prepare(
            "SELECT id, title, kind FROM knowledge_nodes WHERE subject_id=?1 AND kind!='subject'",
        )?;
        let rows = st.query_map(params![subject_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?;
        for row in rows {
            nodes.push(row?);
        }
    }
    if nodes.is_empty() {
        return Ok(Vec::new());
    }
    let known: std::collections::HashSet<&str> = nodes.iter().map(|(id, _, _)| id.as_str()).collect();

    // deps[node] = prerequisites. Readiness (not dependents) is what gates the
    // peel: a node is ready when every prerequisite already has a layer.
    let mut deps: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    {
        let mut st = conn.prepare(
            "SELECT from_id, to_id FROM knowledge_edges
              WHERE subject_id=?1 AND kind='prerequisite'",
        )?;
        let rows = st.query_map(params![subject_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (from, to) = row?;
            // An edge pointing outside this subject is ignored: the layer
            // computation is per-framework, not global.
            if !known.contains(from.as_str()) || !known.contains(to.as_str()) {
                continue;
            }
            deps.entry(from.clone()).or_default().push(to);
        }
    }

    let mut layer_of: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut remaining: Vec<String> = nodes.iter().map(|(id, _, _)| id.clone()).collect();
    let mut current: i64 = 0;
    while !remaining.is_empty() {
        let ready: Vec<String> = remaining
            .iter()
            .filter(|id| {
                deps.get(*id)
                    .map(|ps| ps.iter().all(|p| layer_of.contains_key(p)))
                    .unwrap_or(true)
            })
            .cloned()
            .collect();
        if ready.is_empty() {
            // Cycle: everything left goes one layer above the resolved part
            // rather than looping forever. Deterministic and visible.
            let top = layer_of.values().copied().max().map(|m| m + 1).unwrap_or(0);
            for id in &remaining {
                layer_of.insert(id.clone(), top);
            }
            break;
        }
        for id in &ready {
            let own = deps
                .get(id)
                .map(|ps| ps.iter().filter_map(|p| layer_of.get(p)).max().copied().unwrap_or(0) + 1)
                .unwrap_or(0);
            layer_of.insert(id.clone(), own.max(current));
        }
        remaining.retain(|id| !layer_of.contains_key(id));
        current += 1;
    }

    let mut out: Vec<NodeLayer> = nodes
        .into_iter()
        .map(|(id, title, kind)| {
            let layer = layer_of.get(&id).copied().unwrap_or(0);
            NodeLayer { node_id: id, title, kind, layer }
        })
        .collect();
    out.sort_by(|a, b| a.layer.cmp(&b.layer).then(a.title.cmp(&b.title)));
    Ok(out)
}

/// Transitive prerequisite titles for one node, nearest first. Used to tell
/// the model what the reader is assumed to already know.
pub fn prereq_titles(conn: &Connection, node_id: &str) -> Result<Vec<String>> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut frontier = vec![node_id.to_string()];
    let mut out: Vec<String> = Vec::new();
    while !frontier.is_empty() {
        let mut next: Vec<String> = Vec::new();
        for id in &frontier {
            let mut st = conn.prepare(
                "SELECT e.to_id, n.title FROM knowledge_edges e
                   JOIN knowledge_nodes n ON n.id = e.to_id
                  WHERE e.from_id=?1 AND e.kind='prerequisite'",
            )?;
            let rows = st.query_map(params![id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (to, title) = row?;
                if seen.insert(to.clone()) {
                    out.push(title);
                    next.push(to);
                }
            }
        }
        frontier = next;
    }
    Ok(out)
}

/// Notes that mention this node, with a preview each — the doc's only
/// grounding besides the node's own summary.
fn grounding(conn: &Connection, node: &KnowledgeNode) -> Result<Vec<(String, String)>> {
    let mut st = conn.prepare(
        "SELECT n.title, n.body FROM note_concepts nc
           JOIN notes n ON n.id = nc.note_id
          WHERE nc.node_id=?1
          ORDER BY nc.hits DESC, n.updated_at DESC
          LIMIT 6",
    )?;
    let rows = st.query_map(params![node.id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (title, body) = row?;
        let preview: String = body.chars().take(600).collect();
        out.push((title, preview));
    }
    Ok(out)
}

fn build_user_prompt(
    node: &KnowledgeNode,
    layer: i64,
    prereqs: &[String],
    grounding: &[(String, String)],
) -> String {
    let mut p = format!(
        "知识点：{}（{}）\n层号：第 {} 层（0 为基础层，没有前置）\n",
        node.title, node.kind, layer
    );
    if let Some(s) = node.summary.as_deref().filter(|s| !s.trim().is_empty()) {
        p.push_str(&format!("框架里的摘要：{}\n", s));
    }
    if prereqs.is_empty() {
        p.push_str("前置知识点：无（这是基础层）\n");
    } else {
        p.push_str(&format!("前置知识点（按依赖顺序）：{}\n", prereqs.join(" → ")));
    }
    if grounding.is_empty() {
        p.push_str("\n用户笔记：还没有笔记提到这个知识点。\n");
    } else {
        p.push_str("\n用户笔记里提到它的原文：\n");
        for (title, body) in grounding {
            p.push_str(&format!("—— 《{}》\n{}\n\n", title, body));
        }
    }
    p.push_str("\n按系统提示的规则写讲解材料，结尾用 ---WHY--- 分隔顺序说明。");
    p
}

/// Read the cached doc plus fresh coordinates. Never fails just because no
/// doc exists yet.
pub fn get(conn: &Connection, node_id: &str) -> Result<NodeDoc> {
    let node = repo::get_knowledge_node(conn, node_id)?;
    let layers = compute_layers(conn, &node.subject_id)?;
    let layer = layers
        .iter()
        .find(|l| l.node_id == node_id)
        .map(|l| l.layer)
        .unwrap_or(0);
    let prereqs = prereq_titles(conn, node_id)?;
    let cached = conn
        .query_row(
            "SELECT body, why, model, updated_at FROM node_docs WHERE node_id=?1",
            params![node_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )
        .ok();
    Ok(NodeDoc {
        node_id: node.id.clone(),
        node_title: node.title.clone(),
        subject_id: node.subject_id.clone(),
        layer,
        layers,
        prereqs,
        has_doc: cached.is_some(),
        body: cached.as_ref().map(|c| c.0.clone()).unwrap_or_default(),
        why: cached.as_ref().map(|c| c.1.clone()).unwrap_or_default(),
        model: cached.as_ref().map(|c| c.2.clone()).unwrap_or_default(),
        updated_at: cached.map(|c| c.3).unwrap_or(0),
    })
}

/// Generate (or regenerate) the doc. The layer injected into the prompt is
/// the locally computed one; any layer claim in the reply is ignored.
pub fn generate(conn: &Connection, node_id: &str, model: &dyn Llm) -> Result<NodeDoc> {
    let node = repo::get_knowledge_node(conn, node_id)?;
    let layers = compute_layers(conn, &node.subject_id)?;
    let layer = layers
        .iter()
        .find(|l| l.node_id == node_id)
        .map(|l| l.layer)
        .unwrap_or(0);
    let prereqs = prereq_titles(conn, node_id)?;
    let grounding = grounding(conn, &node)?;
    let user = build_user_prompt(&node, layer, &prereqs, &grounding);
    let raw = model.complete(&crate::prompts::text(crate::prompts::NODE_DOC), &user)?;
    let (body, why) = split_why(&raw);
    if body.chars().count() < 40 {
        return Err(Error::Other("模型回复太短，不像一篇讲解材料 —— 重试或换个模型".into()));
    }
    let now = crate::db::now_ms();
    conn.execute(
        "INSERT INTO node_docs (node_id, subject_id, body, why, model, created_at, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?6)
         ON CONFLICT(node_id) DO UPDATE SET
           body=excluded.body, why=excluded.why, model=excluded.model, updated_at=excluded.updated_at",
        params![node_id, node.subject_id, body, why, model.name(), now],
    )?;
    let subject_id = node.subject_id.clone();
    crate::tokens::record(conn, "chat", &model.name(), &user, &body, Some(&subject_id));
    get(conn, node_id)
}

pub fn delete(conn: &Connection, node_id: &str) -> Result<()> {
    conn.execute("DELETE FROM node_docs WHERE node_id=?1", params![node_id])?;
    Ok(())
}

#[tauri::command]
pub fn node_doc_get(state: tauri::State<'_, AppState>, node_id: String) -> Result<NodeDoc> {
    let c = state.db.lock().unwrap();
    get(&c, &node_id)
}

#[tauri::command]
pub fn node_doc_delete(state: tauri::State<'_, AppState>, node_id: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    delete(&c, &node_id)
}

#[tauri::command]
pub async fn node_doc_generate(app: tauri::AppHandle, node_id: String) -> Result<NodeDoc> {
    tauri::async_runtime::spawn_blocking(move || -> Result<NodeDoc> {
        let state = app.state::<AppState>();
        // Model loaded under the lock, called outside it — same shape as moc.
        let model = {
            let c = state.db.lock().unwrap();
            let keys = crate::commands::read_keys(&c)?;
            crate::commands::load_model(&c, &keys, "chat")?
        };
        let c = state.db.lock().unwrap();
        generate(&c, &node_id, model.as_ref())
    })
    .await
    .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::AppState;

    struct Canned(&'static str);

    impl Llm for Canned {
        fn complete(&self, _system: &str, _user: &str) -> Result<String> {
            Ok(self.0.to_string())
        }
        fn name(&self) -> String {
            "canned".into()
        }
    }

    fn node(c: &Connection, subject: &str, key: &str, title: &str, kind: &str) -> String {
        let id = format!("n-{key}");
        let now = crate::db::now_ms();
        c.execute(
            "INSERT INTO knowledge_nodes (id, subject_id, node_key, kind, title, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?6)",
            params![id, subject, key, kind, title, now],
        )
        .unwrap();
        id
    }

    fn edge(c: &Connection, subject: &str, from: &str, to: &str) {
        c.execute(
            "INSERT OR REPLACE INTO knowledge_edges (id, subject_id, from_id, to_id, kind, weight, created_at)
             VALUES (?1,?2,?3,?4,'prerequisite',1.0,?5)",
            params![format!("e-{from}-{to}"), subject, from, to, crate::db::now_ms()],
        )
        .unwrap();
    }

    fn seeded() -> (AppState, String, Vec<String>) {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let sid = repo::insert_subject(&c, "网络", None, None, None).unwrap();
        let a = node(&c, &sid, "tcp", "TCP 握手", "topic");
        let b = node(&c, &sid, "flow", "拥塞控制", "topic");
        let d = node(&c, &sid, "bbr", "BBR", "concept");
        edge(&c, &sid, &b, &a); // 拥塞控制 depends on TCP 握手
        edge(&c, &sid, &d, &b); // BBR depends on 拥塞控制
        drop(c);
        (st, sid, vec![a, b, d])
    }

    #[test]
    fn layers_peel_the_prerequisite_chain() {
        let (st, sid, ids) = seeded();
        let c = st.db.lock().unwrap();
        let layers = compute_layers(&c, &sid).unwrap();
        let layer_of = |id: &str| layers.iter().find(|l| l.node_id == id).unwrap().layer;
        assert_eq!(layer_of(&ids[0]), 0, "TCP 握手 has no prereqs");
        assert_eq!(layer_of(&ids[1]), 1);
        assert_eq!(layer_of(&ids[2]), 2, "BBR sits above its whole chain");
    }

    #[test]
    fn a_cycle_lands_one_layer_above_instead_of_hanging() {
        // Mixed graph: A is resolvable, C↔D form a cycle above it. The cycle
        // must not hang the peel, and must not pretend to be layer 0.
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let sid = repo::insert_subject(&c, "网络", None, None, None).unwrap();
        let a = node(&c, &sid, "a", "基础", "topic");
        let b = node(&c, &sid, "b", "进阶", "topic");
        let cc = node(&c, &sid, "c", "环一", "concept");
        let d = node(&c, &sid, "d", "环二", "concept");
        edge(&c, &sid, &b, &a);
        edge(&c, &sid, &cc, &d);
        edge(&c, &sid, &d, &cc);
        let layers = compute_layers(&c, &sid).unwrap();
        let layer_of = |id: &str| layers.iter().find(|l| l.node_id == id).unwrap().layer;
        assert_eq!(layer_of(&a), 0);
        assert_eq!(layer_of(&b), 1);
        assert_eq!(layer_of(&cc), layer_of(&d), "cycle members share one layer");
        assert!(layer_of(&cc) > layer_of(&b), "the cycle sits above everything resolved");
    }

    #[test]
    fn a_graph_that_is_one_big_cycle_still_terminates() {
        let (st, sid, ids) = seeded();
        {
            let c = st.db.lock().unwrap();
            edge(&c, &sid, &ids[0], &ids[2]); // close the loop
        }
        let c = st.db.lock().unwrap();
        let layers = compute_layers(&c, &sid).unwrap();
        let tops: Vec<i64> = ids.iter().map(|id| layers.iter().find(|l| &l.node_id == id).unwrap().layer).collect();
        assert!(tops.iter().all(|t| *t == tops[0]), "cycle members share one layer: {tops:?}");
    }

    #[test]
    fn nodes_without_prereqs_are_layer_zero() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let sid = repo::insert_subject(&c, "数学", None, None, None).unwrap();
        node(&c, &sid, "x", "孤立点", "concept");
        let layers = compute_layers(&c, &sid).unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].layer, 0);
    }

    #[test]
    fn prereq_titles_walk_the_chain_transitively() {
        let (st, _sid, ids) = seeded();
        let c = st.db.lock().unwrap();
        let chain = prereq_titles(&c, &ids[2]).unwrap();
        assert_eq!(chain, vec!["拥塞控制".to_string(), "TCP 握手".to_string()]);
    }

    #[test]
    fn why_is_split_out_and_its_absence_is_tolerated() {
        let (body, why) = split_why("一篇讲解。\n\n---WHY---\n因为前置是 TCP 握手。");
        assert_eq!(body, "一篇讲解。");
        assert_eq!(why, "因为前置是 TCP 握手。");
        let (body, why) = split_why("```markdown\n只有正文\n```");
        assert_eq!(body, "只有正文");
        assert_eq!(why, "");
    }

    #[test]
    fn the_prompt_carries_the_computed_layer() {
        let (st, sid, ids) = seeded();
        let c = st.db.lock().unwrap();
        let node = repo::get_knowledge_node(&c, &ids[1]).unwrap();
        let layers = compute_layers(&c, &sid).unwrap();
        let layer = layers.iter().find(|l| l.node_id == ids[1]).unwrap().layer;
        let p = build_user_prompt(&node, layer, &["TCP 握手".into()], &[]);
        assert!(p.contains("第 1 层"), "{p}");
        assert!(p.contains("前置知识点（按依赖顺序）：TCP 握手"), "{p}");
        assert!(p.contains("还没有笔记"), "no grounding is stated, not hidden");
    }

    #[test]
    fn a_model_claim_about_layers_never_overwrites_the_local_one() {
        let (st, _sid, ids) = seeded();
        let c = st.db.lock().unwrap();
        // The reply insists the point is layer 99; the stored coordinates must
        // still come from the prerequisite peel.
        let llm = Canned("这是一篇足够长的讲解材料正文，专门用来骗过长度检查，内容其实不重要，凑够四十个字才行。\n\n---WHY---\n我认为它应该排在第 99 层。");
        let doc = generate(&c, &ids[1], &llm).unwrap();
        assert_eq!(doc.layer, 1, "local computation wins over the model's claim");
        assert!(doc.why.contains("第 99 层"), "the claim is kept as prose, not as data");
        assert!(doc.has_doc);
    }

    #[test]
    fn a_too_short_reply_is_refused_rather_than_cached() {
        let (st, _sid, ids) = seeded();
        let c = st.db.lock().unwrap();
        let llm = Canned("太短。");
        assert!(generate(&c, &ids[1], &llm).is_err());
        let doc = get(&c, &ids[1]).unwrap();
        assert!(!doc.has_doc, "a failed generation leaves no half-doc behind");
    }

    #[test]
    fn regenerate_overwrites_and_delete_clears() {
        let (st, _sid, ids) = seeded();
        let c = st.db.lock().unwrap();
        let llm = Canned("第一版讲解材料正文，长度足够通过检查，内容随意，专门用来凑够四十个字的版本，现在真的够了。");
        generate(&c, &ids[1], &llm).unwrap();
        let llm2 = Canned("第二版讲解材料正文，长度也足够通过检查，内容更新，同样是凑够四十个字的版本，现在也够了。");
        let doc = generate(&c, &ids[1], &llm2).unwrap();
        assert!(doc.body.starts_with("第二版"));
        let count: i64 = c
            .query_row("SELECT COUNT(*) FROM node_docs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "regeneration is an upsert, not a second row");
        delete(&c, &ids[1]).unwrap();
        assert!(!get(&c, &ids[1]).unwrap().has_doc);
    }

    #[test]
    fn grounding_quotes_the_notes_that_mention_the_point() {
        let (st, sid, ids) = seeded();
        {
            let c = st.db.lock().unwrap();
            let nid = repo::insert_note(&c, Some(&sid), None, "BBR 笔记", "BBR 的瓶颈带宽估计很关键。").unwrap();
            c.execute(
                "INSERT INTO note_concepts (note_id, node_id, hits, created_at, updated_at)
                 VALUES (?1,?2,3,?3,?3)",
                params![nid, ids[2], crate::db::now_ms()],
            )
            .unwrap();
        }
        let c = st.db.lock().unwrap();
        let node = repo::get_knowledge_node(&c, &ids[2]).unwrap();
        let g = grounding(&c, &node).unwrap();
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].0, "BBR 笔记");
        assert!(g[0].1.contains("瓶颈带宽"));
        let p = build_user_prompt(&node, 2, &["拥塞控制".into()], &g);
        assert!(p.contains("《BBR 笔记》"), "{p}");
    }

    /// Manual end-to-end against a COPY of the real library: migrations
    /// 0066–0069 must apply to a 60-migration database, and the layer peel
    /// must survive a framework built by sync (contains-edges only, no
    /// prerequisites yet — everything is layer 0 by definition).
    ///
    /// ```text
    /// CORTEX_REAL_DB="$APPDATA/app.study.cortex/cortex.db" \
    ///   cargo test --lib node_docs::tests::real_library_layers -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "manual: set CORTEX_REAL_DB to the real cortex.db"]
    fn real_library_layers() {
        let Ok(src) = std::env::var("CORTEX_REAL_DB") else {
            eprintln!("CORTEX_REAL_DB not set — skipping");
            return;
        };
        let dir = std::env::temp_dir().join(format!("cortex-doc-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let copy = dir.join("cortex.db");
        std::fs::copy(&src, &copy).unwrap();
        for ext in ["-wal", "-shm"] {
            let from = std::path::PathBuf::from(format!("{src}{ext}"));
            if from.exists() {
                std::fs::copy(&from, dir.join(format!("cortex.db{ext}"))).unwrap();
            }
        }
        // AppState::new runs every migration, 0066 included.
        let st = AppState::new(&copy).unwrap();
        let c = st.db.lock().unwrap();
        let version: i64 = c.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        eprintln!("schema version after migrate: {version}");
        assert!(version >= 66, "the new migrations must have run");

        let subjects = repo::list_subjects(&c).unwrap();
        let mut total_nodes = 0usize;
        for s in subjects.iter().take(8) {
            // Sync first — a library that never opened the subject overview
            // has no nodes at all (same precondition as the MOC e2e).
            repo::sync_knowledge_graph(&c, &s.id).unwrap();
            let layers = compute_layers(&c, &s.id).unwrap();
            total_nodes += layers.len();
            // Every layer must be a small non-negative integer; a framework
            // without prerequisite edges is all zeros.
            for l in &layers {
                assert!(l.layer >= 0 && l.layer < 50, "{}: layer {}", l.title, l.layer);
            }
        }
        eprintln!("layers computed over {total_nodes} nodes in {} subjects", subjects.len().min(8));
        assert!(total_nodes > 0, "sync must have produced nodes on the real library");
        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
