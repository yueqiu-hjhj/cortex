//! Growth profile — the human-owned object that reasons over Cortex's
//! learning signals.
//!
//! Cortex already captures the raw material a "learner profile" needs:
//! per-node BKT mastery, confidence calibration, and tutor misjudgements.
//! This module turns those into *items* a person reads, pauses, resolves,
//! and exports. The design rule that separates it from a black box:
//!
//!   - every item is traceable (its `evidence` points back to the row that
//!     justified it),
//!   - the model only ever *proposes* (`derive`); a row exists only after the
//!     user confirms it (`add` with origin `'derived'`),
//!   - no "the agent grew itself" — status is the only axis the UI flips.
//!
//! `derive` is deterministic and needs no LLM, so the whole feature is
//! unit-testable without a network.

use crate::error::{Error, Result};
use crate::repo;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

/// The four lenses a profile item can take. Stored as a `TEXT` kind column.
pub const KINDS: &[&str] = &["goal", "misconception", "review", "strength"];

/// Allowed lifecycle states. `active` is the only one the tutor prompt reads.
pub const STATUSES: &[&str] = &["active", "paused", "resolved", "dismissed"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    /// Where the claim came from: knowledge_node | tutor_misjudged | calibration | manual.
    pub source: String,
    /// The row id (or subject id for calibration) that backs the claim.
    pub r#ref: String,
    /// A traceable snippet — the exact text that justified the item.
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GrowthItem {
    pub id: String,
    pub subject_id: String,
    pub subject_name: String,
    pub kind: String,
    pub title: String,
    pub detail: String,
    pub status: String,
    pub origin: String,
    pub evidence: Vec<Evidence>,
    pub created_at: i64,
    pub updated_at: i64,
    pub resolved_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GrowthProposal {
    pub subject_id: String,
    pub subject_name: String,
    pub kind: String,
    pub title: String,
    pub detail: String,
    pub evidence: Vec<Evidence>,
}

fn parse_evidence(s: &str) -> Result<Vec<Evidence>> {
    if s.trim().is_empty() {
        return Ok(vec![]);
    }
    serde_json::from_str::<Vec<Evidence>>(s)
        .map_err(|e| Error::Other(format!("evidence JSON 无效：{e}")))
}

fn evidence_to_text(ev: &[Evidence]) -> String {
    serde_json::to_string(ev).unwrap_or_else(|_| "[]".to_string())
}

fn row_to_item(
    r: &rusqlite::Row<'_>,
) -> rusqlite::Result<GrowthItem> {
    let evidence_text: String = r.get(8)?;
    let resolved_at: Option<i64> = r.get(11)?;
    Ok(GrowthItem {
        id: r.get(0)?,
        subject_id: r.get(1)?,
        subject_name: r.get(2)?,
        kind: r.get(3)?,
        title: r.get(4)?,
        detail: r.get(5)?,
        status: r.get(6)?,
        origin: r.get(7)?,
        evidence: parse_evidence(&evidence_text).unwrap_or_default(),
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
        resolved_at,
    })
}

/// Propose items from real learning data. Pure read — inserts nothing.
/// `subject_id = None` scans every subject.
pub fn derive(conn: &Connection, subject_id: Option<&str>) -> Result<Vec<GrowthProposal>> {
    let mut st = conn.prepare(
        "SELECT id, name FROM subjects WHERE (?1 IS NULL OR id = ?1) ORDER BY name",
    )?;
    let rows = st.query_map(params![subject_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let subjects = rows.collect::<rusqlite::Result<Vec<(String, String)>>>()?;
    let mut out = Vec::new();
    for (sid, name) in subjects {
        derive_subject(conn, &sid, &name, &mut out);
    }
    Ok(out)
}

fn derive_subject(conn: &Connection, sid: &str, name: &str, out: &mut Vec<GrowthProposal>) {
    // 1) knowledge nodes → review (low mastery) / strength (high mastery)
    let mut low_nodes: Vec<(String, String)> = Vec::new();
    if let Ok(mut st) = conn.prepare(
        "SELECT id, title, mastery, observations FROM knowledge_nodes WHERE subject_id=?1",
    ) {
        if let Ok(rows) = st.query_map(params![sid], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?, r.get::<_, i64>(3)?))
        }) {
            if let Ok(nodes) = rows.collect::<rusqlite::Result<Vec<(String, String, f64, i64)>>>() {
                for (nid, title, mastery, obs) in nodes {
                    if obs < 2 {
                        continue; // no signal yet
                    }
                    let pct = (mastery * 100.0).round() as i64;
                    if mastery < 0.4 {
                        low_nodes.push((nid.clone(), title.clone()));
                        out.push(GrowthProposal {
                            subject_id: sid.to_string(),
                            subject_name: name.to_string(),
                            kind: "review".into(),
                            title: format!("复习：{title}"),
                            detail: format!("掌握度约 {pct}%，已练习 {obs} 次仍偏低。"),
                            evidence: vec![Evidence {
                                source: "knowledge_node".into(),
                                r#ref: nid,
                                excerpt: title,
                            }],
                        });
                    } else if mastery >= 0.8 {
                        out.push(GrowthProposal {
                            subject_id: sid.to_string(),
                            subject_name: name.to_string(),
                            kind: "strength".into(),
                            title: format!("优势：{title}"),
                            detail: format!("掌握度约 {pct}%，已稳定掌握。"),
                            evidence: vec![Evidence {
                                source: "knowledge_node".into(),
                                r#ref: nid,
                                excerpt: title,
                            }],
                        });
                    }
                }
            }
        }
    }
    // subject-level goal from accumulated weak points
    if !low_nodes.is_empty() {
        let ev: Vec<Evidence> = low_nodes
            .iter()
            .take(5)
            .map(|(id, t)| Evidence {
                source: "knowledge_node".into(),
                r#ref: id.clone(),
                excerpt: t.clone(),
            })
            .collect();
        out.push(GrowthProposal {
            subject_id: sid.to_string(),
            subject_name: name.to_string(),
            kind: "goal".into(),
            title: format!("目标：巩固 {} 的薄弱点", name),
            detail: format!("有 {} 个知识点掌握度低于 40%。", low_nodes.len()),
            evidence: ev,
        });
    }

    // 2) unresolved tutor misjudgements → misconception
    if let Ok(mut st) = conn.prepare(
        "SELECT id, question, answer FROM tutor_misjudged WHERE subject_id=?1 AND resolved=0",
    ) {
        if let Ok(rows) = st.query_map(params![sid], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        }) {
            if let Ok(m) = rows.collect::<rusqlite::Result<Vec<(String, String, String)>>>() {
                for (mid, q, a) in m {
                    let qc: String = q.chars().take(48).collect();
                    let ac: String = a.chars().take(80).collect();
                    out.push(GrowthProposal {
                        subject_id: sid.to_string(),
                        subject_name: name.to_string(),
                        kind: "misconception".into(),
                        title: format!("误区：{qc}…"),
                        detail: format!("你当时自信但答错了。原答：{ac}"),
                        evidence: vec![Evidence {
                            source: "tutor_misjudged".into(),
                            r#ref: mid,
                            excerpt: q,
                        }],
                    });
                }
            }
        }
    }

    // 3) overconfidence in calibration → misconception
    if let Ok(cal) = repo::calibration_summary(conn, sid) {
        if cal.observations >= 4
            && cal.mean_confidence >= 0.7
            && cal.accuracy < cal.mean_confidence - 0.2
        {
            let pct_conf = (cal.mean_confidence * 100.0).round() as i64;
            let pct_acc = (cal.accuracy * 100.0).round() as i64;
            out.push(GrowthProposal {
                subject_id: sid.to_string(),
                subject_name: name.to_string(),
                kind: "misconception".into(),
                title: "校准偏差：高信心却常错".into(),
                detail: format!(
                    "高信心答题 {} 次，平均信心 {}%，实际正确率 {}%。",
                    cal.observations, pct_conf, pct_acc
                ),
                evidence: vec![Evidence {
                    source: "calibration".into(),
                    r#ref: sid.to_string(),
                    excerpt: format!("{} 次高信心，正确率 {}%", cal.observations, pct_acc),
                }],
            });
        }
    }
}

/// List items. `subject_id = None` spans every subject (cross-conversation
/// cockpit view); `status = None` returns all lifecycles.
pub fn list(
    conn: &Connection,
    subject_id: Option<&str>,
    status: Option<&str>,
) -> Result<Vec<GrowthItem>> {
    let mut st = conn.prepare(
        "SELECT g.id, g.subject_id, COALESCE(s.name, g.subject_id), g.kind, g.title, g.detail,
                g.status, g.origin, g.evidence, g.created_at, g.updated_at, g.resolved_at
           FROM growth_items g
           LEFT JOIN subjects s ON s.id = g.subject_id
          WHERE (?1 IS NULL OR g.subject_id = ?1) AND (?2 IS NULL OR g.status = ?2)
          ORDER BY g.kind, g.created_at DESC",
    )?;
    let rows = st.query_map(params![subject_id, status], row_to_item)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn valid_kind(k: &str) -> bool {
    KINDS.contains(&k)
}
fn valid_status(s: &str) -> bool {
    STATUSES.contains(&s)
}

/// Insert one item. `origin` is usually `'manual'` or `'derived'`; either way
/// the row exists only because this call ran — the model never writes on its own.
pub fn add(
    conn: &Connection,
    subject_id: &str,
    kind: &str,
    title: &str,
    detail: &str,
    evidence: Vec<Evidence>,
    origin: &str,
) -> Result<GrowthItem> {
    if !valid_kind(kind) {
        return Err(Error::Other(format!(
            "未知的成长条目类型：{kind}（应为 goal/misconception/review/strength）"
        )));
    }
    if title.trim().is_empty() {
        return Err(Error::Other("成长条目标题不能为空".into()));
    }
    let now = crate::db::now_ms();
    let id = crate::db::new_id();
    conn.execute(
        "INSERT INTO growth_items
            (id, subject_id, kind, title, detail, status, origin, evidence, created_at, updated_at)
         VALUES (?1,?2,?3,?4,?5,'active',?6,?7,?8,?8)",
        params![
            id,
            subject_id,
            kind,
            title.trim(),
            detail,
            origin,
            evidence_to_text(&evidence),
            now
        ],
    )?;
    list(conn, Some(subject_id), None)?
        .into_iter()
        .find(|i| i.id == id)
        .ok_or_else(|| Error::Other("刚写入的成长条目读不回来".into()))
}

/// Flip the lifecycle state. `resolved` stamps `resolved_at`; leaving it clears it.
pub fn set_status(conn: &Connection, id: &str, status: &str) -> Result<()> {
    if !valid_status(status) {
        return Err(Error::Other(format!(
            "非法的状态：{status}（应为 active/paused/resolved/dismissed）"
        )));
    }
    let now = crate::db::now_ms();
    let n = conn.execute(
        "UPDATE growth_items
            SET status=?2, updated_at=?3,
                resolved_at = CASE WHEN ?2='resolved' THEN ?3 ELSE NULL END
          WHERE id=?1",
        params![id, status, now],
    )?;
    if n == 0 {
        return Err(Error::Other(format!("成长条目不存在：{id}")));
    }
    Ok(())
}

/// Hard delete — used only from the UI after the user confirms.
pub fn remove(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM growth_items WHERE id=?1", params![id])?;
    Ok(())
}

/// Compact, prompt-ready block of *active* items for one subject. Empty string
/// when there is nothing live, so the tutor prompt can append conditionally.
pub fn active_prompt(conn: &Connection, subject_id: &str) -> Result<String> {
    let items = list(conn, Some(subject_id), Some("active"))?;
    if items.is_empty() {
        return Ok(String::new());
    }
    let order = ["goal", "review", "misconception", "strength"];
    let mut out = String::from("成长画像（跨对话的活跃条目，请自然融入本次陪练）：\n");
    for k in order {
        for i in items.iter().filter(|i| i.kind == k) {
            let lbl = match k {
                "goal" => "目标",
                "review" => "复习",
                "misconception" => "误区",
                "strength" => "优势",
                _ => "",
            };
            out.push_str(&format!("- [{}] {}\n", lbl, i.title));
        }
    }
    Ok(out.trim_end().to_string())
}

/// Markdown export of live items (active + paused, never resolved/dismissed).
pub fn export_md(conn: &Connection, subject_id: Option<&str>) -> Result<String> {
    let items = list(conn, subject_id, None)?;
    let live: Vec<_> = items
        .into_iter()
        .filter(|i| i.status == "active" || i.status == "paused")
        .collect();
    if live.is_empty() {
        return Ok("# 成长画像\n\n（暂无活跃条目）\n".to_string());
    }
    let mut out = String::from("# 成长画像\n\n");
    let sections = [
        ("goal", "目标"),
        ("review", "复习需求"),
        ("misconception", "误区"),
        ("strength", "优势"),
    ];
    for (k, label) in sections {
        let group: Vec<&GrowthItem> = live.iter().filter(|i| i.kind == k).collect();
        if group.is_empty() {
            continue;
        }
        out.push_str(&format!("## {label}\n\n"));
        for i in group {
            let tag = if i.status == "paused" { "（已暂停）" } else { "" };
            out.push_str(&format!("- **{}**{} — {}\n", i.title, tag, i.detail));
            if !i.evidence.is_empty() {
                let parts: Vec<String> = i.evidence.iter().map(|e| e.excerpt.clone()).collect();
                out.push_str(&format!("  - 依据：{}\n", parts.join("；")));
            }
        }
        out.push('\n');
    }
    Ok(out)
}

// ----- tauri commands -------------------------------------------------------

#[tauri::command]
pub fn growth_derive(
    state: tauri::State<'_, crate::AppState>,
    subject_id: Option<String>,
) -> Result<Vec<GrowthProposal>> {
    let c = state.db.lock().unwrap();
    derive(&c, subject_id.as_deref())
}

#[tauri::command]
pub fn growth_list(
    state: tauri::State<'_, crate::AppState>,
    subject_id: Option<String>,
    status: Option<String>,
) -> Result<Vec<GrowthItem>> {
    let c = state.db.lock().unwrap();
    list(&c, subject_id.as_deref(), status.as_deref())
}

#[tauri::command]
pub fn growth_add(
    state: tauri::State<'_, crate::AppState>,
    subject_id: String,
    kind: String,
    title: String,
    detail: String,
    evidence_json: Option<String>,
    origin: Option<String>,
) -> Result<GrowthItem> {
    let c = state.db.lock().unwrap();
    let evidence = parse_evidence(evidence_json.as_deref().unwrap_or(""))?;
    add(
        &c,
        &subject_id,
        &kind,
        &title,
        &detail,
        evidence,
        origin.as_deref().unwrap_or("manual"),
    )
}

#[tauri::command]
pub fn growth_set_status(
    state: tauri::State<'_, crate::AppState>,
    id: String,
    status: String,
) -> Result<()> {
    let c = state.db.lock().unwrap();
    set_status(&c, &id, &status)
}

#[tauri::command]
pub fn growth_remove(state: tauri::State<'_, crate::AppState>, id: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    remove(&c, &id)
}

#[tauri::command]
pub fn growth_export(
    state: tauri::State<'_, crate::AppState>,
    subject_id: Option<String>,
) -> Result<String> {
    let c = state.db.lock().unwrap();
    export_md(&c, subject_id.as_deref())
}

#[tauri::command]
pub fn growth_active_prompt(
    state: tauri::State<'_, crate::AppState>,
    subject_id: String,
) -> Result<String> {
    let c = state.db.lock().unwrap();
    active_prompt(&c, &subject_id)
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

    fn node(c: &Connection, sid: &str, key: &str, title: &str, mastery: f64, obs: i64) {
        c.execute(
            "INSERT INTO knowledge_nodes (id, subject_id, node_key, kind, title, position, mastery, observations, created_at, updated_at)
             VALUES (?1,?2,?3,'concept',?4,0,?5,?6,1,1)",
            params![format!("n_{key}"), sid, key, title, mastery, obs],
        )
        .unwrap();
    }

    fn misjudged(c: &Connection, sid: &str, q: &str, a: &str) {
        c.execute(
            "INSERT INTO tutor_sessions (id, subject_id, node_id, title, level, turns, status, created_at, updated_at)
             VALUES ('s1',?1,NULL,'t',3,0,'active',1,1)",
            params![sid],
        )
        .unwrap();
        c.execute(
            "INSERT INTO tutor_misjudged (id, session_id, subject_id, question, answer, scheduled_for, resolved, created_at)
             VALUES (?1,'s1',?2,?3,?4,1,0,1)",
            params![format!("m_{}", crate::db::new_id()), sid, q, a],
        )
        .unwrap();
    }

    #[test]
    fn derive_review_for_low_mastery() {
        let (st, sid) = seed();
        let c = st.db.lock().unwrap();
        node(&c, &sid, "k1", "虚拟语气", 0.2, 3);
        let props = derive(&c, None).unwrap();
        let rev = props.iter().find(|p| p.kind == "review" && p.title.contains("虚拟语气"));
        assert!(rev.is_some(), "应派生出复习条目：{props:?}");
        assert_eq!(rev.unwrap().evidence[0].source, "knowledge_node");
        // a subject-level goal should accompany the weak node
        assert!(props.iter().any(|p| p.kind == "goal"));
    }

    #[test]
    fn derive_strength_for_high_mastery() {
        let (st, sid) = seed();
        let c = st.db.lock().unwrap();
        node(&c, &sid, "k2", "时态一致", 0.9, 4);
        let props = derive(&c, None).unwrap();
        assert!(props
            .iter()
            .any(|p| p.kind == "strength" && p.title.contains("时态一致")));
    }

    #[test]
    fn derive_misconception_for_misjudged() {
        let (st, sid) = seed();
        let c = st.db.lock().unwrap();
        misjudged(&c, &sid, "解释双宾语结构", "我认为它总是可替换");
        let props = derive(&c, None).unwrap();
        let m = props.iter().find(|p| p.kind == "misconception");
        assert!(m.is_some(), "应为未解决的误判派生误区：{props:?}");
        assert_eq!(m.unwrap().evidence[0].source, "tutor_misjudged");
    }

    #[test]
    fn derive_ignores_under_observed_nodes() {
        let (st, sid) = seed();
        let c = st.db.lock().unwrap();
        node(&c, &sid, "k3", "孤立点", 0.1, 1); // observations < 2 → no signal
        let props = derive(&c, None).unwrap();
        assert!(props.is_empty(), "单次练习不应产生画像：{props:?}");
    }

    #[test]
    fn add_and_list_roundtrip() {
        let (st, sid) = seed();
        let c = st.db.lock().unwrap();
        let item = add(
            &c,
            &sid,
            "goal",
            "通过六级",
            "每周精听 3 篇",
            vec![],
            "manual",
        )
        .unwrap();
        assert_eq!(item.status, "active");
        let got = list(&c, Some(&sid), None).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].title, "通过六级");
    }

    #[test]
    fn evidence_persists_as_json() {
        let (st, sid) = seed();
        let c = st.db.lock().unwrap();
        let ev = vec![Evidence {
            source: "manual".into(),
            r#ref: "x1".into(),
            excerpt: "用户自述".into(),
        }];
        let item = add(&c, &sid, "goal", "自定义目标", "细节", ev, "manual").unwrap();
        assert_eq!(item.evidence[0].source, "manual");
        assert_eq!(item.evidence[0].excerpt, "用户自述");
    }

    #[test]
    fn set_status_transitions_and_stamps_resolved() {
        let (st, sid) = seed();
        let c = st.db.lock().unwrap();
        let item = add(&c, &sid, "goal", "G", "d", vec![], "manual").unwrap();
        set_status(&c, &item.id, "paused").unwrap();
        assert!(list(&c, Some(&sid), Some("paused"))
            .unwrap()
            .iter()
            .any(|i| i.id == item.id));
        set_status(&c, &item.id, "resolved").unwrap();
        let resolved = list(&c, Some(&sid), Some("resolved")).unwrap();
        let r = resolved.iter().find(|i| i.id == item.id).unwrap();
        assert!(r.resolved_at.is_some());
    }

    #[test]
    fn export_md_contains_titles_and_evidence() {
        let (st, sid) = seed();
        let c = st.db.lock().unwrap();
        // export reads committed rows, not derived proposals — add both kinds
        add(&c, &sid, "goal", "攻克倒装", "多练", vec![], "manual").unwrap();
        add(
            &c,
            &sid,
            "review",
            "复习：倒装句",
            "掌握度 30%",
            vec![Evidence {
                source: "knowledge_node".into(),
                r#ref: "n_k4".into(),
                excerpt: "倒装句".into(),
            }],
            "derived",
        )
        .unwrap();
        let md = export_md(&c, None).unwrap();
        assert!(md.contains("# 成长画像"));
        assert!(md.contains("攻克倒装"));
        assert!(md.contains("复习：倒装句"));
        assert!(md.contains("依据：倒装句"));
    }

    #[test]
    fn active_prompt_formats_only_active() {
        let (st, sid) = seed();
        let c = st.db.lock().unwrap();
        let g = add(&c, &sid, "review", "复习：X", "低掌握", vec![], "manual").unwrap();
        let prompt = active_prompt(&c, &sid).unwrap();
        assert!(prompt.contains("[复习] 复习：X"));
        set_status(&c, &g.id, "dismissed").unwrap();
        assert!(active_prompt(&c, &sid).unwrap().is_empty());
    }

    #[test]
    fn reject_unknown_kind() {
        let (st, sid) = seed();
        let c = st.db.lock().unwrap();
        assert!(add(&c, &sid, "bogus", "x", "y", vec![], "manual").is_err());
    }

    #[test]
    fn derive_scoped_to_one_subject() {
        let st = crate::AppState::in_memory().unwrap();
        let (sid_a, sid_b) = {
            let c = st.db.lock().unwrap();
            let a = repo::insert_subject(&c, "A", None, None, None).unwrap();
            let b = repo::insert_subject(&c, "B", None, None, None).unwrap();
            (a, b)
        };
        let c = st.db.lock().unwrap();
        node(&c, &sid_a, "ka", "A点", 0.2, 3);
        node(&c, &sid_b, "kb", "B点", 0.2, 3);
        let only_a = derive(&c, Some(&sid_a)).unwrap();
        assert!(only_a.iter().all(|p| p.subject_id == sid_a));
        assert_eq!(only_a.len(), 2); // one review + one goal
    }
}
