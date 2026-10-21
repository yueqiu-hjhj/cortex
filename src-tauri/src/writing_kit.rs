//! Writing toolbox + the upgrade ledger — 知源's 写作工具箱 with the part
//! that makes it grow: every upgrade you ACCEPT flows back in as a reusable
//! entry, so the toolbox becomes a record of how *you* write well, not a
//! generic phrase list.
//!
//! Two tables (0071), two jobs:
//!
//!   writing_kit     reference material. Built-ins are seeded once by
//!                   (category, title) and never overwrite an edited copy;
//!                   harvested entries come from accepted upgrades.
//!   upgrade_history the ledger: one row per upgrade/rewrite suggestion the
//!                   user acted on — what it was, why, and whether it was
//!                   accepted. Ignored suggestions are recorded too: knowing
//!                   what you consistently reject is what keeps the harvest
//!                   honest.
//!
//! The harvest rule is deliberately narrow: only ACCEPTED upgrades land in
//! the toolbox, under category 升格实录, as a real before → after pair with
//! its reason. A phrase list guessed from a rejected suggestion is worse
//! than no phrase list.

use crate::db::{new_id, now_ms, AppState};
use crate::error::{Error, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use tauri::State;

/// The auto-harvest category. Kept separate from the five built-in ones so a
/// user can tell "the app shipped this" from "this is my own accepted edit".
pub const HARVEST_CATEGORY: &str = "升格实录";

#[derive(Debug, Clone, Serialize)]
pub struct KitEntry {
    pub id: String,
    pub category: String,
    pub title: String,
    pub content: String,
    /// builtin | user | harvest
    pub source: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpgradeRecord {
    pub id: String,
    pub draft_id: String,
    /// upgrade | rewrite
    pub kind: String,
    pub before_text: String,
    pub after_text: String,
    pub reason: Option<String>,
    pub accepted: bool,
    pub created_at: i64,
}

// ---- built-in toolbox -----------------------------------------------------

/// (category, title, content). Real content, not placeholders — a toolbox
/// entry you cannot use is worse than no entry, because it costs screen
/// space and trust. English-exam register (CET/考研) per the 知源 comparison.
const BUILTIN_KIT: &[(&str, &str, &str)] = &[
    // 模板句型
    ("模板句型", "观点引入", "It is widely acknowledged that …, yet this consensus warrants closer scrutiny. / 公认……，但这一共识值得更严格的审视。"),
    ("模板句型", "转折推进", "While the aforementioned findings appear compelling, they rest on the assumption that …. / 前述发现看似有力，但其成立依赖于……这一假设。"),
    ("模板句型", "结论限定", "These results should be interpreted with caution, as they hold only under …. / 上述结果应谨慎解读，因其仅在……条件下成立。"),
    ("模板句型", "呼吁研究", "Future work should therefore examine … before any general claim is made. / 后续研究应先考察……，再作一般性论断。"),
    // 替换词库
    ("替换词库", "升级动词", concat!(
        "show → demonstrate / reveal / establish；think → argue / contend / maintain；use → employ / adopt / leverage；get → obtain / derive / yield；",
        "big → substantial / pronounced / marked；good → favorable / sound / robust"
    )),
    ("替换词库", "慎用词", "obviously / clearly / of course —— 论证里出现这些词，通常说明作者没给证据；very / really / a lot —— 学术文体里几乎总能找到更准的量词。"),
    ("替换词库", "中式英语", concat!(
        "although…but…（although 与 but 不同时用）；because…so…（同理）；according to my opinion（in my view）；",
        "more and more（an increasing number of）；study knowledge（acquire knowledge）"
    )),
    // 过渡词
    ("过渡词", "对照", "by contrast / conversely / on the other hand / whereas / while"),
    ("过渡词", "递进", "moreover / furthermore / what is more / in addition / building on this"),
    ("过渡词", "因果", "consequently / as a result / thereby / hence / owing to / on account of"),
    ("过渡词", "让步", "admittedly / granted / even so / nevertheless / notwithstanding"),
    // 中式英语对照
    ("中式英语对照", "常见病句", concat!(
        "The reason is because … → The reason is that …；Open the light → Turn on the light；",
        "My English level is poor → My English is limited；I very like it → I like it very much"
    )),
    ("中式英语对照", "冗余", concat!(
        "in my personal opinion → in my view；completely eliminate → eliminate；",
        "future prospects → prospects；repeat again → repeat"
    )),
    // 评分标准
    ("评分标准", "四六级作文档位", concat!(
        "13-15 分：切题，思想表达清楚，文字通顺，连贯性好，基本无语言错误；11-12 分：切题，表达清楚，文字连贯，少量语言错误；",
        "8-10 分：基本切题，有些地方表达不够清楚，文字勉强连贯，语言错误相当多；5-7 分：基本切题，表达思想不清楚，连贯性差，多数句子有错。"
    )),
    ("评分标准", "考研英语一大作文档位", concat!(
        "17-20 分（优秀）：很好地完成试题规定任务，包含所有内容要点，使用丰富的语法结构和词汇，语言自然流畅基本无误；",
        "13-16 分（良好）：包含所有内容要点，允许漏掉一两个次重点，语法结构及词汇较丰富，语法结构或词汇方面有少量错误；",
        "9-12 分（一般）：漏掉内容要点，语法结构单调，词汇较贫乏，有较多错误但基本能读懂；5-8 分（差）：与要求有关的内容很少，语法项目单调，严重缺乏语法和词汇知识。"
    )),
    ("评分标准", "提分自查", concat!(
        "1. 每段第一句是否是主题句？2. 段间是否有过渡词？3. 是否有三个以上升级动词/名词化表达？4. 长句是否超过 35 词？",
        "5. 结论段是否复现了题干关键词？6. 是否有 obviously/very 之类的慎用词？"
    )),
];

/// Seed the built-ins once. By (category, title), never overwriting: a user
/// who edited a built-in entry keeps their version, and the next launch
/// does not silently revert it.
pub fn seed_builtins(conn: &Connection) -> Result<usize> {
    let mut seeded = 0;
    for (category, title, content) in BUILTIN_KIT {
        let exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM writing_kit WHERE category=?1 AND title=?2 AND source='builtin'",
            params![category, title],
            |r| r.get(0),
        )?;
        if exists > 0 {
            continue;
        }
        let now = now_ms();
        conn.execute(
            "INSERT INTO writing_kit (id, category, title, content, source, created_at, updated_at)
             VALUES (?1,?2,?3,?4,'builtin',?5,?5)",
            params![new_id(), category, title, content, now],
        )?;
        seeded += 1;
    }
    Ok(seeded)
}

// ---- toolbox ----------------------------------------------------------------

pub fn kit_list(conn: &Connection, category: Option<&str>) -> Result<Vec<KitEntry>> {
    let (sql, cat): (&str, Option<String>) = match category {
        Some(c) => (
            "SELECT id, category, title, content, source, created_at, updated_at
               FROM writing_kit WHERE category=?1 ORDER BY source, created_at",
            Some(c.to_string()),
        ),
        None => (
            "SELECT id, category, title, content, source, created_at, updated_at
               FROM writing_kit ORDER BY category, source, created_at",
            None,
        ),
    };
    let mut st = conn.prepare(sql)?;
    let map = |r: &rusqlite::Row| -> rusqlite::Result<KitEntry> {
        Ok(KitEntry {
            id: r.get(0)?,
            category: r.get(1)?,
            title: r.get(2)?,
            content: r.get(3)?,
            source: r.get(4)?,
            created_at: r.get(5)?,
            updated_at: r.get(6)?,
        })
    };
    let rows = match &cat {
        Some(c) => st.query_map(params![c], map)?.collect::<rusqlite::Result<_>>()?,
        None => st.query_map([], map)?.collect::<rusqlite::Result<_>>()?,
    };
    Ok(rows)
}

/// User-added entry. Same shape as a built-in, different provenance.
pub fn kit_save(conn: &Connection, category: &str, title: &str, content: &str) -> Result<String> {
    if title.trim().is_empty() || content.trim().is_empty() {
        return Err(Error::Other("标题和内容都不能为空".into()));
    }
    let id = new_id();
    let now = now_ms();
    conn.execute(
        "INSERT INTO writing_kit (id, category, title, content, source, created_at, updated_at)
         VALUES (?1,?2,?3,?4,'user',?5,?5)",
        params![id, category.trim(), title.trim(), content.trim(), now],
    )?;
    Ok(id)
}

/// Delete one entry. Built-ins can be deleted too — the seed only runs once
/// and never resurrects a deleted entry (the seed checks existence, and a
/// deleted row is gone), which is what "delete" has to mean here.
pub fn kit_delete(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM writing_kit WHERE id=?1", [id])?;
    Ok(())
}

// ---- the upgrade ledger ------------------------------------------------------

pub fn history_list(conn: &Connection, draft_id: &str) -> Result<Vec<UpgradeRecord>> {
    let mut st = conn.prepare(
        "SELECT id, draft_id, kind, before_text, after_text, reason, accepted, created_at
           FROM upgrade_history WHERE draft_id=?1 ORDER BY created_at DESC LIMIT 100",
    )?;
    let rows = st
        .query_map(params![draft_id], |r| {
            Ok(UpgradeRecord {
                id: r.get(0)?,
                draft_id: r.get(1)?,
                kind: r.get(2)?,
                before_text: r.get(3)?,
                after_text: r.get(4)?,
                reason: r.get(5)?,
                accepted: r.get::<_, i64>(6)? != 0,
                created_at: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Record one acted-on suggestion. On accept, the pair is harvested into
/// the toolbox (returns the new entry id); on reject, only the ledger row
/// is written. `before`/`after` are clamped so a whole paragraph pasted
/// into the toolbox cannot happen.
pub fn record_outcome(
    conn: &Connection,
    draft_id: &str,
    kind: &str,
    before: &str,
    after: &str,
    reason: Option<&str>,
    accepted: bool,
) -> Result<Option<String>> {
    if before.trim().is_empty() || after.trim().is_empty() {
        return Err(Error::Other("改写前后都不能为空".into()));
    }
    let clamp = |s: &str, n: usize| s.trim().chars().take(n).collect::<String>();
    let before = clamp(before, 400);
    let after = clamp(after, 400);
    if before == after {
        return Ok(None); // a no-op rewrite is not worth a ledger row
    }
    let id = new_id();
    let now = now_ms();
    conn.execute(
        "INSERT INTO upgrade_history (id, draft_id, kind, before_text, after_text, reason, accepted, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            id,
            draft_id,
            if kind == "rewrite" { "rewrite" } else { "upgrade" },
            before,
            after,
            reason.map(|r| clamp(r, 300)),
            accepted as i64,
            now
        ],
    )?;
    if !accepted {
        return Ok(None);
    }
    // Harvest: the accepted pair itself, reason attached. Idempotent — the
    // same before→after harvested twice is one entry (dedup on
    // category+title+content), so re-running an upgrade cannot flood the
    // toolbox with duplicates.
    let content = match reason.map(|r| r.trim()).filter(|r| !r.is_empty()) {
        Some(r) => format!("{before}\n→ {after}\n理由：{r}"),
        None => format!("{before}\n→ {after}"),
    };
    let title = clamp(&after, 60);
    let dup: i64 = conn.query_row(
        "SELECT COUNT(*) FROM writing_kit WHERE category=?1 AND title=?2 AND content=?3",
        params![HARVEST_CATEGORY, title, content],
        |r| r.get(0),
    )?;
    if dup > 0 {
        return Ok(None);
    }
    let entry = new_id();
    conn.execute(
        "INSERT INTO writing_kit (id, category, title, content, source, created_at, updated_at)
         VALUES (?1,?2,?3,?4,'harvest',?5,?5)",
        params![entry, HARVEST_CATEGORY, title, content, now],
    )?;
    Ok(Some(entry))
}

// ---- commands -------------------------------------------------------------

#[tauri::command]
pub fn writing_kit_list(
    state: State<'_, AppState>,
    category: Option<String>,
) -> Result<Vec<KitEntry>> {
    let c = state.db.lock().unwrap();
    kit_list(&c, category.as_deref())
}

#[tauri::command]
pub fn writing_kit_save(
    state: State<'_, AppState>,
    category: String,
    title: String,
    content: String,
) -> Result<String> {
    let c = state.db.lock().unwrap();
    kit_save(&c, &category, &title, &content)
}

#[tauri::command]
pub fn writing_kit_delete(state: State<'_, AppState>, id: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    kit_delete(&c, &id)
}

#[tauri::command]
pub fn upgrade_history_list(
    state: State<'_, AppState>,
    draft_id: String,
) -> Result<Vec<UpgradeRecord>> {
    let c = state.db.lock().unwrap();
    history_list(&c, &draft_id)
}

#[tauri::command]
pub fn upgrade_outcome_record(
    state: State<'_, AppState>,
    draft_id: String,
    kind: String,
    before: String,
    after: String,
    reason: Option<String>,
    accepted: bool,
) -> Result<Option<String>> {
    let c = state.db.lock().unwrap();
    record_outcome(
        &c,
        &draft_id,
        &kind,
        &before,
        &after,
        reason.as_deref(),
        accepted,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_draft(st: &AppState) -> String {
        let c = st.db.lock().unwrap();
        let id = new_id();
        let now = now_ms();
        c.execute(
            "INSERT INTO drafts (id, title, excerpt_ids, payload, created_at)
             VALUES (?1,'测试成稿','[]','{}',?2)",
            params![id, now],
        )
        .unwrap();
        id
    }

    #[test]
    fn builtins_seed_once_and_never_overwrite() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        assert_eq!(seed_builtins(&c).unwrap(), BUILTIN_KIT.len());
        assert_eq!(seed_builtins(&c).unwrap(), 0, "second seed is a no-op");
        // A user edit of a built-in survives re-seeding.
        let id: String = c
            .query_row(
                "SELECT id FROM writing_kit WHERE source='builtin' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        c.execute(
            "UPDATE writing_kit SET content='我的版本' WHERE id=?1",
            [&id],
        )
        .unwrap();
        seed_builtins(&c).unwrap();
        let content: String = c
            .query_row("SELECT content FROM writing_kit WHERE id=?1", [&id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(content, "我的版本");
    }

    #[test]
    fn an_accepted_upgrade_is_harvested_once() {
        let st = AppState::in_memory().unwrap();
        let draft = seed_draft(&st); // locks internally — take ours after
        let c = st.db.lock().unwrap();
        let entry = record_outcome(
            &c,
            &draft,
            "upgrade",
            "This is very good.",
            "This is remarkably effective.",
            Some("去掉 very，换成有信息量的形容词"),
            true,
        )
        .unwrap();
        assert!(entry.is_some());
        // Same pair again: deduped, not duplicated.
        let again = record_outcome(
            &c,
            &draft,
            "upgrade",
            "This is very good.",
            "This is remarkably effective.",
            Some("去掉 very，换成有信息量的形容词"),
            true,
        )
        .unwrap();
        assert!(again.is_none(), "the same accepted pair is one entry");
        let harvested: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM writing_kit WHERE category=?1",
                params![HARVEST_CATEGORY],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(harvested, 1);
        let e = kit_list(&c, Some(HARVEST_CATEGORY)).unwrap();
        assert_eq!(e[0].source, "harvest");
        assert!(e[0].content.contains("very good"));
        assert!(e[0].content.contains("remarkably effective"));
        assert!(e[0].content.contains("理由："));
    }

    #[test]
    fn a_rejected_suggestion_lands_in_the_ledger_but_not_the_toolbox() {
        let st = AppState::in_memory().unwrap();
        let draft = seed_draft(&st); // locks internally — take ours after
        let c = st.db.lock().unwrap();
        let entry = record_outcome(
            &c,
            &draft,
            "rewrite",
            "We think this is ok.",
            "We contend this is acceptable.",
            None,
            false,
        )
        .unwrap();
        assert!(entry.is_none(), "a rejected suggestion harvests nothing");
        let h = history_list(&c, &draft).unwrap();
        assert_eq!(h.len(), 1);
        assert!(!h[0].accepted);
        assert_eq!(h[0].kind, "rewrite");
        assert_eq!(kit_list(&c, Some(HARVEST_CATEGORY)).unwrap().len(), 0);
    }

    #[test]
    fn a_no_op_rewrite_is_not_worth_a_ledger_row() {
        let st = AppState::in_memory().unwrap();
        let draft = seed_draft(&st); // locks internally — take ours after
        let c = st.db.lock().unwrap();
        let r = record_outcome(&c, &draft, "upgrade", "same", "same", None, true).unwrap();
        assert!(r.is_none());
        assert_eq!(history_list(&c, &draft).unwrap().len(), 0);
    }

    #[test]
    fn user_entries_roundtrip_and_delete() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        assert!(kit_save(&c, "替换词库", "  ", "x").is_err(), "empty title refused");
        let id = kit_save(&c, "替换词库", "常用升级", "important → pivotal").unwrap();
        let e = kit_list(&c, Some("替换词库")).unwrap();
        assert!(e.iter().any(|x| x.id == id && x.source == "user"));
        kit_delete(&c, &id).unwrap();
        assert!(!kit_list(&c, Some("替换词库"))
            .unwrap()
            .iter()
            .any(|x| x.id == id));
    }

    #[test]
    fn deleting_a_builtin_stays_deleted() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        seed_builtins(&c).unwrap();
        let id: String = c
            .query_row(
                "SELECT id FROM writing_kit WHERE source='builtin' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        kit_delete(&c, &id).unwrap();
        seed_builtins(&c).unwrap();
        let gone: i64 = c
            .query_row("SELECT COUNT(*) FROM writing_kit WHERE id=?1", [&id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(gone, 0, "delete means delete");
    }
}
