//! Lightweight visual generators — mind maps and charts produced from source
//! material, rendered as *deterministic* SVG (no third rendering library).
//!
//! Why its own module: Reviva ships mind-map / chart creation tools and Cortex
//! had neither. The rule that keeps this honest is the same one the rest of the
//! app lives by — a model proposes structured JSON, we validate it against hard
//! limits (depth, branching, label length, counts) and only then render. The
//! output is reproducible: the same payload always yields the same SVG, so a
//! generated visual can be re-rendered from the stored `materials` row forever.
//!
//! Three chart shapes are supported (flow / comparison / matrix); mind map is a
//! fourth. Every generator reads the source's extracted text, asks the model for
//! the matching JSON, validates, renders, and stores the result as a material.

use crate::error::{Error, Result};
use crate::repo;
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::Value;

const MAX_DEPTH: usize = 5;
const MAX_CHILDREN: usize = 8;
const MAX_NODES: usize = 60;
const MAX_LABEL: usize = 40;
const TEXT_BUDGET: usize = 30_000;

#[derive(Debug, Clone, serde::Serialize)]
pub struct VisualResult {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub svg: String,
}

// ---- source text -----------------------------------------------------------

fn source_text(conn: &Connection, source_id: &str) -> Result<String> {
    let content: Option<String> = conn
        .query_row("SELECT content FROM sources WHERE id=?1", [source_id], |r| r.get(0))
        .ok();
    match content {
        Some(c) if !c.trim().is_empty() => {
            if c.len() > TEXT_BUDGET {
                Ok(c.chars().take(TEXT_BUDGET).collect())
            } else {
                Ok(c)
            }
        }
        _ => Err(Error::Other(
            "这个来源还没有可解析的正文（先等解析完成，或换一个文本来源）".into(),
        )),
    }
}

// ---- mind map -----------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
struct RawMindNode {
    label: String,
    #[serde(default)]
    children: Vec<RawMindNode>,
}

fn validate_mindmap(node: &RawMindNode, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Other(format!("导图层数超过上限 {MAX_DEPTH}")));
    }
    if node.label.trim().is_empty() {
        return Err(Error::Other("导图节点标签不能为空".into()));
    }
    if node.label.chars().count() > MAX_LABEL {
        return Err(Error::Other("导图节点标签过长".into()));
    }
    *count += 1;
    if *count > MAX_NODES {
        return Err(Error::Other(format!("导图节点数超过上限 {MAX_NODES}")));
    }
    if node.children.len() > MAX_CHILDREN {
        return Err(Error::Other(format!("单个节点的子节点超过上限 {MAX_CHILDREN}")));
    }
    for c in &node.children {
        validate_mindmap(c, depth + 1, count)?;
    }
    Ok(())
}

struct Pos {
    x: f64,
    y: f64,
    label: String,
}

/// Left-to-right tree: leaves stack vertically, parents center on their children.
fn layout_mindmap(root: &RawMindNode) -> (Vec<Pos>, Vec<(usize, usize)>) {
    let mut nodes: Vec<Pos> = Vec::new();
    let mut edges: Vec<(usize, usize)> = Vec::new();
    let xgap = 170.0;
    let ygap = 46.0;
    let mut next_y = 0.0;

    fn walk(
        n: &RawMindNode,
        depth: usize,
        yref: &mut f64,
        nodes: &mut Vec<Pos>,
        edges: &mut Vec<(usize, usize)>,
        xgap: f64,
        ygap: f64,
    ) -> usize {
        let id = nodes.len();
        let x = depth as f64 * xgap + 20.0;
        let my_y = if n.children.is_empty() {
            *yref += ygap;
            *yref - ygap / 2.0
        } else {
            let mut child_ys: Vec<f64> = Vec::new();
            for c in &n.children {
                let cid = walk(c, depth + 1, yref, nodes, edges, xgap, ygap);
                child_ys.push(nodes[cid].y);
                edges.push((id, cid));
            }
            child_ys.iter().sum::<f64>() / child_ys.len() as f64
        };
        nodes.push(Pos { x, y: my_y, label: n.label.clone() });
        id
    }

    walk(root, 0, &mut next_y, &mut nodes, &mut edges, xgap, ygap);
    (nodes, edges)
}

fn render_mindmap(root: &RawMindNode) -> String {
    let (nodes, edges) = layout_mindmap(root);
    let max_x = nodes.iter().map(|n| n.x).fold(0.0_f64, f64::max);
    let max_y = nodes.iter().map(|n| n.y).fold(0.0_f64, f64::max);
    let w = (max_x + 170.0) as usize;
    let h = (max_y + 50.0) as usize;
    let mut s = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w} {h}\" width=\"{w}\" height=\"{h}\" font-family=\"system-ui, sans-serif\">"
    );
    s.push_str("<style>.mm-node{fill:#1f2937;stroke:#3b82f6;stroke-width:1.5;} .mm-text{fill:#e5e7eb;font-size:13px;} .mm-edge{stroke:#64748b;stroke-width:1.5;fill:none;}</style>");
    for (a, b) in &edges {
        let pa = &nodes[*a];
        let pb = &nodes[*b];
        let x1 = pa.x + 150.0;
        let y1 = pa.y;
        let x2 = pb.x;
        let y2 = pb.y;
        let mx = (x1 + x2) / 2.0;
        s.push_str(&format!(
            "<path class=\"mm-edge\" d=\"M{ax},{ay} C {mx2},{ay} {mx2},{by} {bx},{by}\" />",
            ax = fmt1(x1), ay = fmt1(y1), mx2 = fmt1(mx), by = fmt1(y2), bx = fmt1(x2)
        ));
    }
    for n in &nodes {
        let label = escape_xml(&trunc(&n.label, MAX_LABEL));
        let tw = (label.chars().count() as f64 * 7.5 + 16.0).max(40.0);
        let x = n.x;
        let y = n.y - 13.0;
        s.push_str(&format!(
            "<g><rect class=\"mm-node\" x=\"{x}\" y=\"{y}\" width=\"{tw}\" height=\"26\" rx=\"6\" />\
             <text class=\"mm-text\" x=\"{cx}\" y=\"{cy}\" text-anchor=\"middle\">{label}</text></g>",
            x = fmt1(x), y = fmt1(y), tw = fmt1(tw), cx = fmt1(x + tw / 2.0), cy = fmt1(n.y + 4.0), label = label
        ));
    }
    s.push_str("</svg>");
    s
}

// ---- charts --------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
struct RawFlow {
    nodes: Vec<String>,
    edges: Vec<(usize, usize)>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
struct RawComparison {
    groups: Vec<RawGroup>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
struct RawGroup {
    name: String,
    items: Vec<RawBar>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
struct RawBar {
    label: String,
    value: f64,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
struct RawMatrix {
    rows: Vec<String>,
    cols: Vec<String>,
    cells: Vec<Vec<String>>,
}

fn validate_flow(f: &RawFlow) -> Result<()> {
    if f.nodes.is_empty() {
        return Err(Error::Other("流程图至少需要一个节点".into()));
    }
    if f.nodes.len() > 12 {
        return Err(Error::Other("流程图节点数超过 12".into()));
    }
    for (i, n) in f.nodes.iter().enumerate() {
        if n.trim().is_empty() {
            return Err(Error::Other(format!("流程图第 {} 个节点标签为空", i + 1)));
        }
    }
    for (a, b) in &f.edges {
        if *a >= f.nodes.len() || *b >= f.nodes.len() {
            return Err(Error::Other("流程图的边指向了不存在的节点".into()));
        }
    }
    Ok(())
}

fn validate_comparison(c: &RawComparison) -> Result<()> {
    if c.groups.is_empty() {
        return Err(Error::Other("对比图至少需要一组".into()));
    }
    if c.groups.len() > 6 {
        return Err(Error::Other("对比图组数超过 6".into()));
    }
    for g in &c.groups {
        if g.items.is_empty() {
            return Err(Error::Other(format!("对比图「{}」没有条目", g.name)));
        }
        for it in &g.items {
            if it.label.trim().is_empty() {
                return Err(Error::Other("对比图条目标签不能为空".into()));
            }
            if !(0.0..=1.0).contains(&it.value) {
                return Err(Error::Other("对比图数值必须在 0–1 之间".into()));
            }
        }
    }
    Ok(())
}

fn validate_matrix(m: &RawMatrix) -> Result<()> {
    if m.rows.is_empty() || m.cols.is_empty() {
        return Err(Error::Other("矩阵必须有行和列".into()));
    }
    if m.rows.len() > 10 || m.cols.len() > 10 {
        return Err(Error::Other("矩阵行列数超过 10".into()));
    }
    if m.cells.len() != m.rows.len() {
        return Err(Error::Other("矩阵单元格行数与行标签数不一致".into()));
    }
    for (i, row) in m.cells.iter().enumerate() {
        if row.len() != m.cols.len() {
            return Err(Error::Other(format!("矩阵第 {} 行的单元格数与列数不一致", i + 1)));
        }
    }
    Ok(())
}

fn render_flow(f: &RawFlow) -> String {
    let n = f.nodes.len();
    let bw = 120.0;
    let bh = 40.0;
    let gap = 70.0;
    let w = (n as f64 * (bw + gap) + 40.0) as usize;
    let h = 120;
    let mut s = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w} {h}\" width=\"{w}\" height=\"{h}\" font-family=\"system-ui, sans-serif\">"
    );
    s.push_str("<style>.fl-box{fill:#0f766e;stroke:#14b8a6;stroke-width:1.5;} .fl-text{fill:#ecfeff;font-size:13px;} .fl-edge{stroke:#64748b;stroke-width:2;marker-end:url(#ar);}</style>");
    s.push_str("<defs><marker id=\"ar\" markerWidth=\"10\" markerHeight=\"10\" refX=\"8\" refY=\"3\" orient=\"auto\"><path d=\"M0,0 L8,3 L0,6 Z\" fill=\"#64748b\"/></marker></defs>");
    let xs: Vec<f64> = (0..n).map(|i| 20.0 + i as f64 * (bw + gap)).collect();
    for (i, label) in f.nodes.iter().enumerate() {
        let x = xs[i];
        s.push_str(&format!(
            "<g><rect class=\"fl-box\" x=\"{x}\" y=\"40\" width=\"{bw}\" height=\"{bh}\" rx=\"8\" />\
             <text class=\"fl-text\" x=\"{cx}\" y=\"64\" text-anchor=\"middle\">{label}</text></g>",
            x = fmt1(x), bw = fmt1(bw), bh = fmt1(bh), cx = fmt1(x + bw / 2.0),
            label = escape_xml(&trunc(label, 14))
        ));
    }
    for (a, b) in &f.edges {
        let x1 = xs[*a] + bw;
        let x2 = xs[*b];
        s.push_str(&format!("<line class=\"fl-edge\" x1=\"{x1}\" y1=\"60\" x2=\"{x2}\" y2=\"60\" />", x1 = fmt1(x1), x2 = fmt1(x2)));
    }
    s.push_str("</svg>");
    s
}

fn render_comparison(c: &RawComparison) -> String {
    let row_h = 26.0;
    let group_pad = 14.0;
    let label_w = 150.0;
    let bar_max = 320.0;
    let total_rows: usize = c.groups.iter().map(|g| g.items.len()).sum::<usize>() + c.groups.len();
    let h = (total_rows as f64 * row_h + group_pad * c.groups.len() as f64 + 30.0) as usize;
    let w = (label_w + bar_max + 40.0) as usize;
    let mut s = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w} {h}\" width=\"{w}\" height=\"{h}\" font-family=\"system-ui, sans-serif\">"
    );
    s.push_str("<style>.cmp-label{fill:#e5e7eb;font-size:12px;} .cmp-bar{fill:#6366f1;} .cmp-grp{fill:#94a3b8;font-size:12px;font-weight:600;} .cmp-val{fill:#cbd5e1;font-size:11px;}</style>");
    let mut y = 16.0;
    for g in &c.groups {
        s.push_str(&format!(
            "<text class=\"cmp-grp\" x=\"16\" y=\"{y}\">{name}</text>",
            y = fmt1(y), name = escape_xml(&trunc(&g.name, 18))
        ));
        y += row_h;
        for it in &g.items {
            let bw = (it.value * bar_max).max(2.0);
            let pct = format!("{}%", (it.value * 100.0).round() as i32);
            s.push_str(&format!(
                "<text class=\"cmp-label\" x=\"16\" y=\"{ly}\">{label}</text>\
                 <rect class=\"cmp-bar\" x=\"{label_w}\" y=\"{ry}\" width=\"{bw}\" height=\"14\" rx=\"3\" />\
                 <text class=\"cmp-val\" x=\"{vx}\" y=\"{ly}\">{val}</text>",
                label = escape_xml(&trunc(&it.label, 16)),
                ly = fmt1(y),
                ry = fmt1(y - 12.0),
                label_w = fmt1(label_w),
                bw = fmt1(bw),
                vx = fmt1(label_w + bw + 6.0),
                val = pct
            ));
            y += row_h;
        }
        y += group_pad;
    }
    s.push_str("</svg>");
    s
}

fn render_matrix(m: &RawMatrix) -> String {
    let cell = 90.0;
    let chead = 30.0;
    let rhead = 110.0;
    let w = (rhead + m.cols.len() as f64 * cell + 10.0) as usize;
    let h = (chead + m.rows.len() as f64 * cell + 10.0) as usize;
    let mut s = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w} {h}\" width=\"{w}\" height=\"{h}\" font-family=\"system-ui, sans-serif\">"
    );
    s.push_str("<style>.mx-h{fill:#334155;font-size:11px;font-weight:600;} .mx-cell{fill:#0f172a;stroke:#334155;} .mx-t{fill:#cbd5e1;font-size:10px;}</style>");
    // column headers
    for (j, c) in m.cols.iter().enumerate() {
        let x = rhead + j as f64 * cell;
        s.push_str(&format!(
            "<text class=\"mx-h\" x=\"{x}\" y=\"18\" text-anchor=\"middle\">{c}</text>",
            x = fmt1(x), c = escape_xml(&trunc(c, 10))
        ));
    }
    for (i, r) in m.rows.iter().enumerate() {
        let y = chead + i as f64 * cell;
        s.push_str(&format!(
            "<text class=\"mx-h\" x=\"6\" y=\"{ty}\">{r}</text>",
            ty = fmt1(y + cell / 2.0),
            r = escape_xml(&trunc(r, 14))
        ));
        for (j, celltext) in m.cells[i].iter().enumerate() {
            let x = rhead + j as f64 * cell;
            s.push_str(&format!(
                "<rect class=\"mx-cell\" x=\"{x}\" y=\"{y}\" width=\"{cw}\" height=\"{ch}\" />\
                 <text class=\"mx-t\" x=\"{tx}\" y=\"{ty}\">{t}</text>",
                x = fmt1(x), y = fmt1(y), cw = fmt1(cell - 2.0),
                ch = fmt1(cell - 2.0),
                tx = fmt1(x + 5.0),
                ty = fmt1(y + cell / 2.0),
                t = escape_xml(&trunc(celltext, 12))
            ));
        }
    }
    s.push_str("</svg>");
    s
}

// ---- shared helpers -------------------------------------------------------------

fn trunc(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else {
        chars.iter().take(max).collect::<String>() + "…"
    }
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Format an f64 to one decimal WITHOUT the `f` format trait (this toolchain's
/// rustc does not implement fixed-point formatting). Produces e.g. "12.3".
fn fmt1(x: f64) -> String {
    let neg = x < 0.0;
    let scaled = (x.abs() * 10.0).round() as i64;
    let intp = scaled / 10;
    let frac = scaled.rem_euclid(10);
    format!("{}{}.{}", if neg { "-" } else { "" }, intp, frac)
}

fn strip_fences(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if s.starts_with("```") {
        if let Some(end) = s.rfind("```") {
            if end > 3 {
                s = s[3..end].to_string();
            }
        }
        if let Some(nl) = s.find('\n') {
            s = s[nl + 1..].to_string();
        }
        s = s.trim().to_string();
    }
    s
}

fn load_model(state: &tauri::State<'_, crate::AppState>) -> Result<Box<dyn crate::llm::Llm>> {
    let (spec, keys) = {
        let c = state.db.lock().unwrap();
        (
            crate::commands::model_spec_for(&c, "default"),
            crate::commands::read_keys(&c)?,
        )
    };
    let model = crate::llm::from_spec_or_any(&spec, &keys)
        .ok_or_else(|| Error::Other("未配置模型，请先在设置里添加 API key".into()))?;
    Ok(model)
}

const MIND_SYS: &str = "You are a study visualizer. Output strict JSON only, no prose, no markdown fences.";

// ---- commands -------------------------------------------------------------------

#[tauri::command]
pub async fn mindmap_generate(
    state: tauri::State<'_, crate::AppState>,
    source_id: String,
) -> Result<VisualResult> {
    let text = {
        let c = state.db.lock().unwrap();
        source_text(&c, &source_id)?
    };
    let mut model = load_model(&state)?;
    crate::commands::apply_budget(&mut model, &state.db.lock().unwrap(), "default");
    let prompt = format!(
        "为下面的学习材料生成一张思维导图，抓住核心概念与它们的从属关系。\n\n要求：\n- 根节点是材料主题\n- 每个节点 label 简短（≤{MAX_LABEL} 字）\n- 最多 {MAX_DEPTH} 层、每节点最多 {MAX_CHILDREN} 个子节点、总节点 ≤ {MAX_NODES}\n- 严格只输出 JSON：{{\"root\":{{\"label\":\"...\",\"children\":[{{\"label\":\"...\",\"children\":[]}}]}}}}，不要任何额外文字\n\n材料：\n{text}"
    );
    let raw = model.complete(MIND_SYS, &prompt).map_err(|e| Error::Other(e.to_string()))?;
    let parsed: RawMindNode = serde_json::from_str(&strip_fences(&raw))
        .map_err(|e| Error::Other(format!("AI 返回的 JSON 无法解析为导图：{e}")))?;
    let mut count = 0;
    validate_mindmap(&parsed, 0, &mut count)?;
    let svg = render_mindmap(&parsed);
    let payload = serde_json::to_value(&parsed).unwrap_or(Value::Null);
    let (id, title) = {
        let c = state.db.lock().unwrap();
        let subject_id: String = c
            .query_row("SELECT subject_id FROM sources WHERE id=?1", [source_id], |r| r.get(0))
            .map_err(|_| Error::Other("来源不存在".into()))?;
        let title = parsed.label.clone();
        let id = repo::save_material(&c, &subject_id, None, "mindmap", &title, "mind map", &payload)?;
        (id, title)
    };
    Ok(VisualResult { id, title, kind: "mindmap".into(), svg })
}

#[tauri::command]
pub async fn chart_generate(
    state: tauri::State<'_, crate::AppState>,
    source_id: String,
    chart_type: String,
) -> Result<VisualResult> {
    let text = {
        let c = state.db.lock().unwrap();
        source_text(&c, &source_id)?
    };
    let (shape_desc, example) = match chart_type.as_str() {
        "flow" => (
            "流程图：按顺序的步骤",
            "{\"nodes\":[\"提出问题\",\"文献检索\",\"筛选\"],\"edges\":[[0,1],[1,2]]}",
        ),
        "comparison" => (
            "对比图：若干组，每组若干条目带 0–1 的数值",
            "{\"groups\":[{\"name\":\"方法A\",\"items\":[{\"label\":\"准确率\",\"value\":0.82},{\"label\":\"速度\",\"value\":0.6}]}]}",
        ),
        "matrix" => (
            "矩阵：行与列的交叉对比",
            "{\"rows\":[\"维度1\",\"维度2\"],\"cols\":[\"方案X\",\"方案Y\"],\"cells\":[[\"强\",\"弱\"],[\"弱\",\"强\"]]}",
        ),
        _ => return Err(Error::Other("未知图表类型（应为 flow/comparison/matrix）".into())),
    };
    let mut model = load_model(&state)?;
    crate::commands::apply_budget(&mut model, &state.db.lock().unwrap(), "default");
    let prompt = format!(
        "根据材料生成一张「{chart_type}」图（{shape_desc}）。\n\n要求：\n- 只输出严格 JSON，形状如：{example}\n- 标签简短，不要任何额外文字或 markdown 围栏\n\n材料：\n{text}"
    );
    let raw = model.complete(MIND_SYS, &prompt).map_err(|e| Error::Other(e.to_string()))?;
    let cleaned = strip_fences(&raw);
    let (svg, payload, title) = match chart_type.as_str() {
        "flow" => {
            let v: RawFlow = serde_json::from_str(&cleaned)
                .map_err(|e| Error::Other(format!("AI 返回的流程图 JSON 无法解析：{e}")))?;
            validate_flow(&v)?;
            (render_flow(&v), serde_json::to_value(&v).unwrap_or(Value::Null), v.nodes.first().cloned().unwrap_or_else(|| "流程图".into()))
        }
        "comparison" => {
            let v: RawComparison = serde_json::from_str(&cleaned)
                .map_err(|e| Error::Other(format!("AI 返回的对比图 JSON 无法解析：{e}")))?;
            validate_comparison(&v)?;
            (
                render_comparison(&v),
                serde_json::to_value(&v).unwrap_or(Value::Null),
                v.groups.first().map(|g| g.name.clone()).unwrap_or_else(|| "对比图".into()),
            )
        }
        "matrix" => {
            let v: RawMatrix = serde_json::from_str(&cleaned)
                .map_err(|e| Error::Other(format!("AI 返回的矩阵 JSON 无法解析：{e}")))?;
            validate_matrix(&v)?;
            (
                render_matrix(&v),
                serde_json::to_value(&v).unwrap_or(Value::Null),
                format!("矩阵（{}×{}）", v.rows.len(), v.cols.len()),
            )
        }
        _ => return Err(Error::Other("未知图表类型".into())),
    };
    let id = {
        let c = state.db.lock().unwrap();
        let subject_id: String = c
            .query_row("SELECT subject_id FROM sources WHERE id=?1", [source_id], |r| r.get(0))
            .map_err(|_| Error::Other("来源不存在".into()))?;
        repo::save_material(&c, &subject_id, None, "chart", &title, &chart_type, &payload)?
    };
    Ok(VisualResult { id, title, kind: "chart".into(), svg })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo;

    fn seed() -> (crate::AppState, String) {
        let st = crate::AppState::in_memory().unwrap();
        let sid = {
            let c = st.db.lock().unwrap();
            repo::insert_subject(&c, "英语", None, None, None).unwrap()
        };
        (st, sid)
    }

    fn src_with(c: &Connection, sid: &str, content: &str) -> String {
        let id = repo::insert_source(&c, sid, None, "Passage", "txt", Some("test")).unwrap();
        c.execute(
            "UPDATE sources SET content=?1 WHERE id=?2",
            rusqlite::params![content, id],
        )
        .unwrap();
        id
    }

    #[test]
    fn mindmap_validation_rejects_too_deep_and_empty_label() {
        let deep = RawMindNode {
            label: "root".into(),
            children: vec![RawMindNode {
                label: "a".into(),
                children: vec![RawMindNode {
                    label: "b".into(),
                    children: vec![RawMindNode {
                        label: "c".into(),
                        children: vec![RawMindNode {
                            label: "d".into(),
                            children: vec![RawMindNode {
                                label: "e".into(),
                                children: vec![RawMindNode {
                                    label: "f".into(),
                                    children: vec![RawMindNode { label: "g".into(), children: vec![] }],
                                }],
                            }],
                        }],
                    }],
                }],
            }],
        };
        let mut count = 0;
        assert!(validate_mindmap(&deep, 0, &mut count).is_err());

        let empty = RawMindNode { label: "".into(), children: vec![] };
        let mut c2 = 0;
        assert!(validate_mindmap(&empty, 0, &mut c2).is_err());
    }

    #[test]
    fn mindmap_svg_is_well_formed() {
        let tree = RawMindNode {
            label: "主题".into(),
            children: vec![
                RawMindNode { label: "概念A".into(), children: vec![] },
                RawMindNode {
                    label: "概念B".into(),
                    children: vec![RawMindNode { label: "子点".into(), children: vec![] }],
                },
            ],
        };
        let svg = render_mindmap(&tree);
        assert!(svg.starts_with("<svg"));
        assert!(svg.trim_end().ends_with("</svg>"));
        assert!(svg.contains("概念A"));
        // deterministic: same input → same output
        assert_eq!(svg, render_mindmap(&tree));
    }

    #[test]
    fn flow_rejects_bad_edge_and_empty() {
        let empty = RawFlow { nodes: vec![], edges: vec![] };
        assert!(validate_flow(&empty).is_err());
        let bad_edge = RawFlow { nodes: vec!["a".into(), "b".into()], edges: vec![(0, 5)] };
        assert!(validate_flow(&bad_edge).is_err());
        let ok = RawFlow { nodes: vec!["a".into(), "b".into()], edges: vec![(0, 1)] };
        assert!(validate_flow(&ok).is_ok());
    }

    #[test]
    fn comparison_rejects_out_of_range_value() {
        let bad = RawComparison {
            groups: vec![RawGroup {
                name: "g".into(),
                items: vec![RawBar { label: "x".into(), value: 1.5 }],
            }],
        };
        assert!(validate_comparison(&bad).is_err());
        let ok = RawComparison {
            groups: vec![RawGroup {
                name: "g".into(),
                items: vec![RawBar { label: "x".into(), value: 0.5 }],
            }],
        };
        assert!(validate_comparison(&ok).is_ok());
    }

    #[test]
    fn matrix_rejects_mismatched_cells() {
        let bad = RawMatrix {
            rows: vec!["r1".into(), "r2".into()],
            cols: vec!["c1".into()],
            cells: vec![vec!["a".into()], vec!["b".into(), "c".into()]],
        };
        assert!(validate_matrix(&bad).is_err());
        let ok = RawMatrix {
            rows: vec!["r1".into()],
            cols: vec!["c1".into(), "c2".into()],
            cells: vec![vec!["a".into(), "b".into()]],
        };
        assert!(validate_matrix(&ok).is_ok());
    }

    #[test]
    fn charts_render_to_svg() {
        let flow = RawFlow { nodes: vec!["a".into(), "b".into(), "c".into()], edges: vec![(0, 1), (1, 2)] };
        assert!(render_flow(&flow).starts_with("<svg"));
        let cmp = RawComparison {
            groups: vec![RawGroup {
                name: "方法".into(),
                items: vec![RawBar { label: "准".into(), value: 0.8 }, RawBar { label: "快".into(), value: 0.4 }],
            }],
        };
        assert!(render_comparison(&cmp).contains("80%"));
        let mat = RawMatrix {
            rows: vec!["维度".into()],
            cols: vec!["X".into(), "Y".into()],
            cells: vec![vec!["强".into(), "弱".into()]],
        };
        assert!(render_matrix(&mat).starts_with("<svg"));
    }

    #[test]
    fn escape_xml_neutralizes_markup() {
        assert_eq!(escape_xml("a<b>&c"), "a&lt;b&gt;&amp;c");
    }

    #[test]
    fn mindmap_stores_as_material() {
        let (st, sid) = seed();
        let c = st.db.lock().unwrap();
        // The command itself needs an LLM key at runtime, so we exercise the
        // storage path it uses: save_material roundtrips a mindmap payload.
        let id = repo::save_material(
            &c,
            &sid,
            None,
            "mindmap",
            "主题",
            "mind map",
            &serde_json::json!({"label":"主题","children":[]}),
        )
        .unwrap();
        let got = repo::get_material(&c, &id).unwrap();
        assert_eq!(got.kind, "mindmap");
        assert_eq!(got.title, "主题");
    }
}
