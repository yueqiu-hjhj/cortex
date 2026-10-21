//! MOC — a map of content.
//!
//! A review answers "what is the state of this field". A MOC answers "where does
//! everything live, and what is missing". They are different artifacts, and only
//! the second one stays useful six months later: the review goes stale, the map
//! keeps pointing at the notes.
//!
//! So this module turns a subject's notes + knowledge points into one Markdown
//! map with `[[double links]]`, and — this is the part a plain export cannot do
//! — a 缺口 section built from coverage: the points you have material on but
//! never wrote about are exactly what the map should tell you next.
//!
//! Two composers, one output shape:
//!   compose_local  deterministic, no model, always works (also the fallback)
//!   compose_with   the model writes the prose; its links are then checked
//!
//! The check is the point: a model that invents `[[量子引力笔记]]` would produce
//! a map full of dead links, so anything it returns must reference real note
//! titles — otherwise the local map wins and the user gets a useful file instead
//! of an apology.

use crate::db::AppState;
use crate::error::{Error, Result};
use crate::llm::Llm;
use crate::models::{KnowledgeNode, Note};
use crate::repo;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use tauri::Manager;

/// Everything the map is built from, gathered once so both composers see the
/// same facts (and so the model can never see something the validator doesn't).
struct Inventory {
    subject_name: String,
    notes: Vec<Note>,
    nodes: Vec<KnowledgeNode>,
    /// note_id → node ids it mentions (from 0064's index).
    links: HashMap<String, Vec<String>>,
    /// node_id → how many notes mention it.
    mentions: HashMap<String, i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MocOutcome {
    pub note_id: String,
    pub title: String,
    /// Relative path under the vault's `moc/` folder, when a vault is configured.
    pub vault_path: Option<String>,
    pub notes: usize,
    pub gaps: usize,
    /// False when the local composer was used (no model, or an unusable reply).
    pub used_model: bool,
}

impl Inventory {
    fn load(conn: &Connection, subject_id: &str) -> Result<Self> {
        let subject = repo::get_subject(conn, subject_id)?;
        let notes = repo::list_notes(conn, Some(subject_id))?;
        let nodes = repo::list_knowledge_nodes(conn, subject_id)?;
        let mut links: HashMap<String, Vec<String>> = HashMap::new();
        {
            let mut st = conn.prepare(
                "SELECT nc.note_id, nc.node_id FROM note_concepts nc
                   JOIN notes n ON n.id = nc.note_id
                  WHERE n.subject_id = ?1",
            )?;
            let rows = st.query_map(params![subject_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (note_id, node_id) = row?;
                links.entry(note_id).or_default().push(node_id);
            }
        }
        let mut mentions: HashMap<String, i64> = HashMap::new();
        for ids in links.values() {
            for id in ids {
                *mentions.entry(id.clone()).or_insert(0) += 1;
            }
        }
        Ok(Self {
            subject_name: subject.name,
            notes,
            nodes,
            links,
            mentions,
        })
    }

    fn is_review(&self, note: &Note) -> bool {
        note.title.contains("综述") || note.title.to_lowercase().contains("review")
    }

    fn first_line(body: &str) -> String {
        body.lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('>'))
            .unwrap_or("")
            .chars()
            .take(140)
            .collect()
    }

    fn topics(&self) -> Vec<&KnowledgeNode> {
        self.nodes.iter().filter(|n| n.kind == "topic").collect()
    }

    fn gaps(&self) -> Vec<&KnowledgeNode> {
        self.nodes
            .iter()
            .filter(|n| (n.kind == "topic" || n.kind == "concept") && n.coverage < 0.4)
            .collect()
    }
}

/// The map a machine can always write: notes grouped under the topics they
/// mention, review first, gaps last.
fn compose_local(inv: &Inventory) -> String {
    let mut out = format!(
        "# {} · 主题地图\n\n> {} 篇笔记，{} 个知识点。{}\n\n",
        inv.subject_name,
        inv.notes.len(),
        inv.nodes.len(),
        "按笔记提到的主题归位；缺口列在最后。"
    );

    let review = inv.notes.iter().find(|n| inv.is_review(n));
    let review_id = review.map(|n| n.id.clone());
    out.push_str("## 主线\n\n");
    match review {
        Some(n) => out.push_str(&format!("- [[{}]] — 本学科的综述/总览\n", n.title)),
        None => {
            // No review yet: say so instead of pretending one exists.
            out.push_str("- 还没有综述 —— 先在「科研 → 深度研究」出一份初稿，这张图才有主干\n");
        }
    }
    out.push('\n');

    out.push_str("## 分支\n\n");
    if inv.topics().is_empty() {
        out.push_str("- 还没有主题节点 —— 到学科页加主题，或导入材料让框架自动生成\n\n");
    }
    let mut placed: HashSet<String> = HashSet::new();
    if let Some(id) = &review_id {
        placed.insert(id.clone());
    }
    for topic in inv.topics() {
        let mut here: Vec<&Note> = inv
            .notes
            .iter()
            .filter(|n| {
                !placed.contains(&n.id)
                    && inv
                        .links
                        .get(&n.id)
                        .is_some_and(|ids| ids.contains(&topic.id))
            })
            .collect();
        here.sort_by_key(|n| std::cmp::Reverse(n.updated_at));
        let pct = (topic.coverage * 100.0).round() as i64;
        out.push_str(&format!("### {} · 覆盖 {}%\n\n", topic.title, pct));
        if here.is_empty() {
            out.push_str("- （还没有笔记）\n");
        } else {
            for n in &here {
                let hint = Inventory::first_line(&n.body);
                if hint.is_empty() {
                    out.push_str(&format!("- [[{}]]\n", n.title));
                } else {
                    out.push_str(&format!("- [[{}]] — {}\n", n.title, hint));
                }
                placed.insert(n.id.clone());
            }
        }
        out.push('\n');
    }

    let rest: Vec<&Note> = inv.notes.iter().filter(|n| !placed.contains(&n.id)).collect();
    if !rest.is_empty() {
        out.push_str("## 还没归到主题的笔记\n\n");
        for n in rest {
            let hint = Inventory::first_line(&n.body);
            if hint.is_empty() {
                out.push_str(&format!("- [[{}]]\n", n.title));
            } else {
                out.push_str(&format!("- [[{}]] — {}\n", n.title, hint));
            }
        }
        out.push('\n');
    }

    let gaps = inv.gaps();
    out.push_str("## 缺口\n\n");
    if inv.nodes.iter().all(|n| n.kind == "subject") {
        out.push_str("- 这个学科还没有知识点（主题/概念），缺口暂时算不出来\n");
    } else if gaps.is_empty() {
        out.push_str("- 每个知识点都有笔记覆盖。往下可以开始横向比较、写综述。\n");
    } else {
        for node in gaps.iter().take(12) {
            let pct = (node.coverage * 100.0).round() as i64;
            let why = match inv.mentions.get(&node.id).copied().unwrap_or(0) {
                0 => "还没有笔记".to_string(),
                n => format!("只有 {n} 篇笔记"),
            };
            out.push_str(&format!("- {} · 覆盖 {}% · {}\n", node.title, pct, why));
        }
        if gaps.len() > 12 {
            out.push_str(&format!("- …另有 {} 个覆盖偏低的知识点\n", gaps.len() - 12));
        }
    }
    out
}

fn inventory_for_prompt(inv: &Inventory) -> String {
    let mut out = format!("主题：{}\n\n笔记（{} 篇）：\n", inv.subject_name, inv.notes.len());
    for n in inv.notes.iter().take(60) {
        let preview = Inventory::first_line(&n.body);
        out.push_str(&format!("- 《{}》: {}\n", n.title, preview));
    }
    out.push_str("\n知识点：\n");
    for node in inv.nodes.iter().filter(|n| n.kind != "subject").take(80) {
        let notes = inv.mentions.get(&node.id).copied().unwrap_or(0);
        out.push_str(&format!(
            "- [{}] {} · 覆盖 {}% · {} 篇笔记\n",
            node.kind,
            node.title,
            (node.coverage * 100.0).round() as i64,
            notes
        ));
    }
    out
}

/// Ask the model for the map, then refuse to accept links to notes that do not
/// exist. A map with dead links is worse than a plain one.
fn compose_with(llm: &dyn Llm, inv: &Inventory) -> Result<String> {
    let prompt = format!(
        "{}\n\n把上面的材料整理成一张「主题地图」（MOC）。\n\n\
         要求：\n\
         1. 只允许链接上面列出的笔记，写成 [[完整标题]]，标题一个字都不能改；不要编造笔记、论文、数字\n\
         2. 结构：第一行 `# <学科> · 主题地图`；一句 `> 导语`；`## 主线`（3–6 条，按理解顺序，每条说明它解决什么问题）；`## 分支`（2–4 个 `###` 小节，把笔记按子题归位）；`## 缺口`（覆盖低或没有笔记的知识点，写清楚该补什么）\n\
         3. 主线是「先读什么、为什么」，不是书单；允许在条目后写一句你的判断\n\
         4. 600–900 字，中文，直接输出 Markdown，不要代码围栏、不要解释",
        inventory_for_prompt(inv)
    );
    let raw = llm.complete(
        "你是知识地图编辑。你只整理用户已有的笔记，绝不虚构不存在的条目。",
        &prompt,
    )?;
    let body = strip_fence(&raw);
    validate_links(&body, &inv.notes)?;
    Ok(body)
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
    let s = s.strip_suffix("```").unwrap_or(s);
    s.trim().to_string()
}

/// At least one real `[[title]]`, and no link pointing at a note that isn't in
/// this subject. The first rule rejects "here is a generic essay about MOCs";
/// the second rejects invented titles even when the rest looks fine.
fn validate_links(body: &str, notes: &[Note]) -> Result<()> {
    let titles: HashSet<&str> = notes.iter().map(|n| n.title.as_str()).collect();
    let mut real = 0;
    for target in wikilinks(body) {
        let target = target.trim();
        if titles.contains(target) {
            real += 1;
        } else {
            return Err(Error::Other(format!("地图引用了不存在的笔记：{target}")));
        }
    }
    if real == 0 {
        return Err(Error::Other("地图里没有一个真实的笔记链接".into()));
    }
    Ok(())
}

fn wikilinks(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else { break };
        out.push(after[..end].to_string());
        rest = &after[end + 2..];
    }
    out
}

/// Compose step, shared by the command and the tests. The model is optional on
/// purpose: no key, offline mode, and a rejected reply all fall back to the same
/// deterministic map instead of failing the click.
fn prepare(inv: &Inventory, model: Option<&dyn Llm>) -> Prepared {
    let (body, used_model) = match model {
        Some(m) => match compose_with(m, inv) {
            Ok(text) => (text, true),
            Err(e) => {
                eprintln!("[moc] model map rejected ({e}) — using the local one");
                (compose_local(inv), false)
            }
        },
        None => (compose_local(inv), false),
    };
    Prepared {
        body,
        used_model,
        notes: inv.notes.len(),
        gaps: inv.gaps().len(),
    }
}

struct Prepared {
    body: String,
    used_model: bool,
    notes: usize,
    gaps: usize,
}

fn save_prepared(
    conn: &Connection,
    root: Option<&std::path::Path>,
    subject_id: &str,
    title: Option<&str>,
    prep: Prepared,
) -> Result<MocOutcome> {
    let title = title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| t.chars().take(80).collect::<String>())
        .unwrap_or_else(|| {
            let name: String = conn
                .query_row(
                    "SELECT name FROM subjects WHERE id=?1",
                    params![subject_id],
                    |r| r.get(0),
                )
                .unwrap_or_else(|_| "学科".into());
            format!("MOC · {name}")
        });
    let note_id = repo::insert_note(conn, Some(subject_id), None, &title, &prep.body)?;
    crate::note_links::rebuild(conn, &note_id, &prep.body)?;
    // A map is also a note: it should show up in coverage like anything else.
    if let Err(e) = crate::note_knowledge::rebuild(conn, &note_id) {
        eprintln!("[moc] coverage index skipped: {e}");
    }
    let vault_path = root.and_then(|r| match crate::vault::write_moc(r, &title, &prep.body) {
        Ok(rel) => Some(rel),
        Err(e) => {
            eprintln!("[moc] vault copy failed: {e}");
            None
        }
    });
    Ok(MocOutcome {
        note_id,
        title,
        vault_path,
        notes: prep.notes,
        gaps: prep.gaps,
        used_model: prep.used_model,
    })
}

#[tauri::command]
pub async fn moc_generate(
    app: tauri::AppHandle,
    subject_id: String,
    title: Option<String>,
) -> Result<MocOutcome> {
    tauri::async_runtime::spawn_blocking(move || -> Result<MocOutcome> {
        let state = app.state::<AppState>();
        // Gather and persist under the DB lock; compose outside it, because the
        // model call can take half a minute and the UI mutex must not wait.
        let (model, inv) = {
            let c = state.db.lock().unwrap();
            let keys = crate::commands::read_keys(&c)?;
            let model = if keys.offline {
                None
            } else {
                crate::commands::load_model(&c, &keys, "chat").ok()
            };
            (model, Inventory::load(&c, &subject_id)?)
        };
        if inv.notes.is_empty() && inv.nodes.is_empty() {
            return Err(Error::Other(
                "这个学科还没有笔记和知识点 —— 先写一篇，或者导入材料".into(),
            ));
        }
        let prep = prepare(&inv, model.as_deref());
        let c = state.db.lock().unwrap();
        let root = crate::vault::root_for(&c, state.data_dir.as_deref());
        save_prepared(&c, root.as_deref(), &subject_id, title.as_deref(), prep)
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

    /// One subject, two topics, a review note + a normal note filed under the
    /// first topic. Mirrors the shape of a real 科研 subject.
    fn seeded() -> (AppState, String) {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let sid = repo::insert_subject(&c, "网络", None, None, None).unwrap();
        repo::insert_topic(&c, &sid, "拥塞控制", None, &[]).unwrap();
        repo::insert_topic(&c, &sid, "路由", None, &[]).unwrap();
        repo::sync_knowledge_graph(&c, &sid).unwrap();
        repo::insert_note(
            &c,
            Some(&sid),
            None,
            "BBR 综述",
            "拥塞控制的两条路线：丢包驱动与模型驱动。",
        )
        .unwrap();
        let nid = repo::insert_note(
            &c,
            Some(&sid),
            None,
            "BBR 实测",
            "在拥塞控制里，BBR 的瓶颈带宽估计是关键。",
        )
        .unwrap();
        crate::note_knowledge::rebuild(&c, &nid).unwrap();
        drop(c);
        (st, sid)
    }

    fn inventory(st: &AppState, sid: &str) -> Inventory {
        let c = st.db.lock().unwrap();
        Inventory::load(&c, sid).unwrap()
    }

    #[test]
    fn local_map_links_every_note_once() {
        let (st, sid) = seeded();
        let inv = inventory(&st, &sid);
        let map = compose_local(&inv);
        assert_eq!(map.matches("[[BBR 综述]]").count(), 1, "review is the spine, not a branch too");
        assert_eq!(map.matches("[[BBR 实测]]").count(), 1);
        assert!(map.starts_with("# 网络 · 主题地图"));
    }

    #[test]
    fn the_review_is_the_spine_when_one_exists() {
        let (st, sid) = seeded();
        let map = compose_local(&inventory(&st, &sid));
        let spine = map.find("## 主线").unwrap();
        let branches = map.find("## 分支").unwrap();
        assert!(map[spine..branches].contains("[[BBR 综述]]"));
    }

    #[test]
    fn gaps_name_the_points_nobody_wrote_about() {
        let (st, sid) = seeded();
        let map = compose_local(&inventory(&st, &sid));
        let gaps = &map[map.find("## 缺口").unwrap()..];
        assert!(gaps.contains("路由"), "the topic with no notes must be called out");
        assert!(gaps.contains("还没有笔记"));
    }

    #[test]
    fn a_model_map_that_invents_a_note_is_rejected() {
        let (st, sid) = seeded();
        let inv = inventory(&st, &sid);
        let llm = Canned("# 网络 · 主题地图\n\n- [[量子引力笔记]]\n");
        let err = compose_with(&llm, &inv).unwrap_err();
        assert!(err.to_string().contains("不存在"));
        // …and the caller falls back to the local map instead of failing.
        let prep = prepare(&inv, Some(&llm));
        assert!(!prep.used_model);
        assert!(prep.body.contains("[[BBR 实测]]"));
    }

    #[test]
    fn a_model_map_that_links_real_notes_is_used() {
        let (st, sid) = seeded();
        let inv = inventory(&st, &sid);
        let llm = Canned("```markdown\n# 网络 · 主题地图\n\n## 主线\n\n- [[BBR 综述]] — 先看全局\n```");
        let prep = prepare(&inv, Some(&llm));
        assert!(prep.used_model);
        assert!(prep.body.starts_with("# 网络 · 主题地图"), "fence stripped: {}", prep.body);
    }

    #[test]
    fn saving_writes_a_note_that_points_back_at_the_notes() {
        let (st, sid) = seeded();
        let c = st.db.lock().unwrap();
        let inv = Inventory::load(&c, &sid).unwrap();
        let prep = prepare(&inv, None);
        let out = save_prepared(&c, None, &sid, None, prep).unwrap();
        assert_eq!(out.title, "MOC · 网络");
        assert_eq!(out.notes, 2);
        assert!(out.gaps >= 1);
        let note = repo::get_note(&c, &out.note_id).unwrap();
        assert!(note.body.contains("[[BBR 实测]]"));
        assert_eq!(note.subject_id.as_deref(), Some(sid.as_str()));
        // The map is a note, so its links must be live for backlinks.
        let back = crate::note_links::backlinks(&c, "BBR 综述").unwrap();
        assert!(back.iter().any(|b| b.title == "MOC · 网络"));
    }

    /// Manual end-to-end run against a COPY of the real library. This is the
    /// check that the 0063/0064 migrations apply to a 60-migration database and
    /// that all three 回流 legs work on real notes, not just fixtures:
    ///
    /// ```text
    /// CORTEX_REAL_DB="$APPDATA/app.study.cortex/cortex.db" \
    ///   cargo test --lib moc::tests::real_library_map -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "manual: set CORTEX_REAL_DB to the real cortex.db"]
    fn real_library_map() {
        let Ok(src) = std::env::var("CORTEX_REAL_DB") else {
            eprintln!("CORTEX_REAL_DB not set — skipping");
            return;
        };
        let dir = std::env::temp_dir().join(format!("cortex-moc-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let copy = dir.join("cortex.db");
        std::fs::copy(&src, &copy).unwrap();
        for ext in ["-wal", "-shm"] {
            let from = std::path::PathBuf::from(format!("{src}{ext}"));
            if from.exists() {
                std::fs::copy(&from, dir.join(format!("cortex.db{ext}"))).unwrap();
            }
        }
        let st = AppState::new(&copy).unwrap();
        let c = st.db.lock().unwrap();
        let version: i64 = c.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        eprintln!("schema version after migrate: {version}");

        // Pick the subject with the most notes — the hardest map to build.
        let subjects = repo::list_subjects(&c).unwrap();
        let picked = subjects
            .iter()
            .max_by_key(|s| repo::list_notes(&c, Some(&s.id)).map(|n| n.len()).unwrap_or(0))
            .expect("library has no subjects");
        let notes = repo::list_notes(&c, Some(&picked.id)).unwrap();
        eprintln!("subject: {} ({} notes)", picked.name, notes.len());

        // 回流 2: notes → coverage on real prose. Sync first — this is exactly
        // what the Subject Overview does on mount, and without it a library that
        // never opened that page has no nodes to attach notes to.
        let nodes = repo::sync_knowledge_graph(&c, &picked.id).unwrap();
        eprintln!("nodes after sync: {nodes}");
        let links = crate::note_knowledge::rebuild_all(&c, Some(&picked.id)).unwrap();
        let nodes = repo::list_knowledge_nodes(&c, &picked.id).unwrap();
        let touched: Vec<_> = nodes.iter().filter(|n| n.note_count > 0).take(6).collect();
        eprintln!("coverage links written: {links}");
        for n in &touched {
            eprintln!(
                "  {} [{}] coverage {:.0}% · {} 笔记",
                n.title,
                n.kind,
                n.coverage * 100.0,
                n.note_count
            );
        }

        // 回流 1: a real note's passage → a card in the real subject.
        let source_note = notes.iter().max_by_key(|n| n.body.chars().count()).unwrap();
        let quote: String = source_note.body.chars().take(160).collect();
        if quote.trim().chars().count() >= 20 {
            let clip = crate::clips::add(&c, &quote, crate::clips::KIND_NOTE, None, Some("e2e"), None).unwrap();
            let llm = Canned("{\"cards\":[{\"front\":\"这段在讲什么机制？\",\"back\":\"e2e 卡\"}]}");
            let drafts = crate::clips::draft_cards(&llm, &clip).unwrap();
            let saved = crate::clips::save_cards(&c, &clip.id, Some(&picked.id), None, &drafts).unwrap();
            eprintln!("clip → {} card(s), seeded {}", saved.cards, saved.seeded);
            assert!(saved.seeded >= 1);
            crate::clips::discard(&c, &clip.id).unwrap();
        }

        // 回流 3: the map itself, offline composer (no model in this run).
        let inv = Inventory::load(&c, &picked.id).unwrap();
        let prep = prepare(&inv, None);
        eprintln!("map: {} notes · {} gaps", prep.notes, prep.gaps);
        eprintln!("----- map preview -----\n{}\n----- end -----", prep.body.chars().take(900).collect::<String>());
        let out = save_prepared(&c, None, &picked.id, None, prep).unwrap();
        eprintln!("map gaps reported: {}", out.gaps);
        let note = repo::get_note(&c, &out.note_id).unwrap();
        assert!(note.body.contains("[["));
        assert!(note.body.contains("## 缺口"));
        eprintln!("saved map note: {} ({})", out.title, note.body.chars().count());

        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn regenerating_overwrites_the_same_vault_file() {
        let dir = std::env::temp_dir().join(format!("cortex-moc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (st, sid) = seeded();
        let c = st.db.lock().unwrap();
        let inv = Inventory::load(&c, &sid).unwrap();
        let a = save_prepared(&c, Some(&dir), &sid, None, prepare(&inv, None)).unwrap();
        let b = save_prepared(&c, Some(&dir), &sid, None, prepare(&inv, None)).unwrap();
        assert_eq!(a.vault_path, b.vault_path);
        assert_eq!(a.vault_path.as_deref(), Some("moc/MOC · 网络.md"));
        assert!(dir.join("moc/MOC · 网络.md").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
