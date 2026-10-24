//! End-to-end flows across modules, on a real (in-memory) database.
//!
//! Unit tests cover each module's internals; these cover the WIRING between
//! them — the paths a user actually walks. They call the same service
//! functions the Tauri commands call, with a scripted model instead of a
//! provider, so they are deterministic and free.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::agents::{self, DEFAULT_USER};
use crate::db::AppState;
use crate::error::Result;
use crate::llm::Llm;
use crate::planner::{self, PlanRequest};
use crate::workitems::{self, ItemInput};

struct Scripted {
    replies: Mutex<VecDeque<String>>,
}

impl Scripted {
    fn new(replies: &[&str]) -> Self {
        Self {
            replies: Mutex::new(replies.iter().map(|s| s.to_string()).collect()),
        }
    }
}

impl Llm for Scripted {
    fn complete(&self, _system: &str, _user: &str) -> Result<String> {
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| crate::error::Error::Other("script exhausted".into()))
    }
    fn name(&self) -> String {
        "scripted".into()
    }
}

/// 导入 → 分块 → 检索 → 笔记：the knowledge path every other feature reads from.
#[test]
fn knowledge_chain_import_search_note() {
    let st = AppState::in_memory().unwrap();
    let c = st.db.lock().unwrap();

    let sid = crate::repo::insert_subject(&c, "线性代数", Some("MATH101"), None, None).unwrap();
    let src = crate::repo::insert_source(&c, &sid, None, "讲义.pdf", "pdf", None).unwrap();
    crate::repo::finalize_source(
        &c,
        &src,
        "ready",
        None,
        Some("矩阵的秩是行向量张成空间的维数。"),
        None,
    )
    .unwrap();
    crate::repo::insert_chunk(
        &c,
        &src,
        &sid,
        None,
        0,
        "矩阵的秩是行向量张成空间的维数。",
        Some("p.1"),
        0,
        &[],
        "",
    )
    .unwrap();

    let hits = crate::repo::keyword_search_chunks(&c, Some(&sid), None, "秩", 5).unwrap();
    assert!(!hits.is_empty(), "keyword search must find the imported chunk");

    let nid = crate::repo::insert_note(&c, Some(&sid), None, "秩的定义", "秩 = 行空间维数").unwrap();
    let notes = crate::repo::list_notes(&c, Some(&sid)).unwrap();
    assert!(notes.iter().any(|n| n.id == nid), "the note must be listed");
}

/// 目标 + 长期计划 → 周任务 → 排进日历：the planning chain built in 0085.
#[test]
fn planning_chain_from_long_term_plan_to_blocks() {
    let st = AppState::in_memory().unwrap();
    let c = st.db.lock().unwrap();

    let goal = workitems::upsert_goal(
        &c,
        &workitems::GoalInput {
            id: None,
            title: "期末复习".into(),
            kind: Some("exam".into()),
            metric: None,
            target: Some(85.0),
            current: None,
            deadline_ms: Some(workitems::parse_day("2026-11-15").unwrap() * 86_400_000),
            subject_id: None,
            status: Some("active".into()),
        },
    )
    .unwrap();

    let goal = crate::plan_build::set_plan(
        &c,
        &goal.id,
        "第 1 周通读教材；第 2 周做题。",
        4,
        Some("2026-10-26"),
    )
    .unwrap();
    assert_eq!(goal.plan_weeks, Some(4));

    let model = Scripted::new(&[
        r#"{"items":[{"week":1,"title":"通读第 1 章","minutes":60},{"week":2,"title":"做课后题","minutes":90}]}"#,
    ]);
    let drafts = crate::plan_build::build(&model, &goal, "2026-10-26").unwrap();
    assert_eq!(drafts.len(), 2);
    let applied = crate::plan_build::apply(&c, &goal, &drafts).unwrap();
    assert_eq!(applied.created, 2);

    let out = planner::plan_week(
        &c,
        &PlanRequest {
            start_day: "2026-10-26".into(),
            days: Some(7),
            daily_minutes: Some(120),
            day_start_min: Some(540),
            day_end_min: Some(1320),
            break_min: Some(10),
            replace: Some(true),
            only_week: Some(1),
        },
    )
    .unwrap();
    assert!(!out.blocks.is_empty(), "week 1 must get a block");
    let titles: Vec<String> = out
        .blocks
        .iter()
        .map(|b| {
            workitems::get_item(&c, &b.item_id)
                .map(|i| i.title)
                .unwrap_or_default()
        })
        .collect();
    assert!(
        !titles.iter().any(|t| t == "做课后题"),
        "only_week must not borrow week 2: {titles:?}"
    );
}

/// Agent → tool loop → delegation → child run linked to its parent.
#[test]
fn agent_chain_runs_tools_and_delegates() {
    let st = AppState::in_memory().unwrap();
    let c = st.db.lock().unwrap();
    agents::seed_file_tools(&c).unwrap();

    let child = agents::upsert_agent(
        &c, DEFAULT_USER, None, "资料员", None, "你负责查资料。", &[], &[], None, 0.2, 3, true,
        "read",
    )
    .unwrap();
    let parent = agents::upsert_agent(
        &c,
        DEFAULT_USER,
        None,
        "主笔",
        None,
        "你负责汇总。",
        &[],
        &["delegate".to_string()],
        None,
        0.2,
        4,
        true,
        "read",
    )
    .unwrap();

    let run = agents::start_run(&c, DEFAULT_USER, &parent.id, "总结一下秩").unwrap();
    let first = format!(
        r#"TOOL delegate {{"agent_id":"{}","task":"查秩的定义"}}"#,
        child.id
    );
    let model = Scripted::new(&[first.as_str(), "ANSWER: 秩是行空间的维数", "ANSWER: 已完成"]);
    let done =
        crate::agent_exec::execute_run(&c, DEFAULT_USER, &parent, &run.id, &model, None).unwrap();
    assert_eq!(done.status, "success", "{:?}", done.error);
    assert!(
        done.result.as_deref().unwrap_or("").contains("已完成"),
        "the parent must answer after the child returns"
    );
    assert!(done.steps >= 2, "the loop must run the tool then answer");

    let linked: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM agent_runs WHERE parent_run_id=?1",
            [&run.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(linked, 1, "the delegated child run must point at the parent");
}

/// Skill export → import round trip, including the conflict guard.
#[test]
fn skill_chain_exports_and_reimports() {
    let st = AppState::in_memory().unwrap();
    let c = st.db.lock().unwrap();
    let sk = agents::upsert_skill(&c, DEFAULT_USER, None, "推导模板", "逐步推导", "第一步…", true)
        .unwrap();

    let dir = std::env::temp_dir().join(format!("cortex-flow-{}", crate::db::new_id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.join("skill.json");
    agents::export_skill(&c, DEFAULT_USER, &sk.id, &dest).unwrap();

    let err = agents::import_skill(&c, DEFAULT_USER, &dest, false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("已存在"), "{err}");
    let imported = agents::import_skill(&c, DEFAULT_USER, &dest, true).unwrap();
    assert_eq!(imported.source, "imported");
    assert_eq!(imported.body, "第一步…");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 复习 → 完成 → 校准：the loop that feeds the planner's duration estimate.
#[test]
fn review_chain_logs_completion_and_calibrates() {
    let st = AppState::in_memory().unwrap();
    let c = st.db.lock().unwrap();

    let item = workitems::create_item(
        &c,
        &ItemInput {
            kind: Some("task".into()),
            title: "做 10 道线代题".into(),
            body: None,
            status: Some("open".into()),
            priority: Some("med".into()),
            due_ms: Some(workitems::parse_day("2026-10-26").unwrap() * 86_400_000),
            start_ms: None,
            estimate_min: Some(60),
            parent_id: None,
            rrule: None,
            subject_id: None,
            topic_id: None,
            goal_id: None,
        },
    )
    .unwrap();

    let out = planner::plan_week(
        &c,
        &PlanRequest {
            start_day: "2026-10-26".into(),
            days: Some(1),
            daily_minutes: Some(120),
            day_start_min: Some(540),
            day_end_min: Some(1320),
            break_min: Some(10),
            replace: Some(true),
            only_week: None,
        },
    )
    .unwrap();
    let block = out
        .blocks
        .iter()
        .find(|b| b.item_id == item.id)
        .expect("the item must be planned");

    planner::complete_block(&c, &block.id).unwrap();
    let block_after = workitems::get_block(&c, &block.id).unwrap();
    assert_eq!(block_after.status, "done");
    let after = workitems::get_item(&c, &item.id).unwrap();
    assert!(after.actual_min > 0, "completion must log actual minutes");

    // Completing a block logs time; closing the item is the user's call.
    workitems::set_status(&c, &item.id, "done").unwrap();
    let calib = planner::calibration(&c, 60).unwrap();
    assert_eq!(calib.samples, 1, "the completion must feed the calibration");
}
