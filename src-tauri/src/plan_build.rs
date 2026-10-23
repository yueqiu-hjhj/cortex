//! Long-term plan → week-by-week tasks.
//!
//! `planner::materialize_goal` could only break a goal down deterministically
//! from BKT mastery, and only when the goal was bound to a subject with topics.
//! A pasted "12 周计划" had nowhere to go.
//!
//! This module closes that gap in three steps:
//!   1. `set_plan` stores the pasted text, the week count and the week-1 anchor
//!      on the goal (migration 0085).
//!   2. `build` asks the chat model for a week-tagged task list under hard
//!      numeric constraints, then `parse_drafts` validates every item before
//!      anything is written — a malformed reply can never create junk tasks.
//!   3. `apply` writes the items with `week_index`, deduped by
//!      (goal, week, title), and `auto_week` is the cron entry point that
//!      generates the current week and plans it.
//!
//! Deliberate: the model splits content, the planner still does arithmetic.
//! Extraction never schedules; `planner::plan_week` remains the only thing that
//! decides what happens at 9am on Tuesday.

use std::collections::HashMap;

use rusqlite::{params, Connection};
use serde::Serialize;

use crate::db::now_ms;
use crate::error::{Error, Result};
use crate::llm::{self, Llm};
use crate::workitems::{self, Goal, ItemInput};

const DAY_MS: i64 = 86_400_000;
const MAX_ITEMS: usize = 120;
const MAX_PER_WEEK: usize = 8;
const MIN_MINUTES: i64 = 30;
const MAX_MINUTES: i64 = 180;
const PLAN_TEXT_CAP: usize = 8000;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Draft {
    /// 1-based week.
    pub week: i64,
    pub title: String,
    pub minutes: i64,
    /// A recurring commitment (lecture, lab slot) — not counted against the
    /// per-week task cap and never reordered by the planner's priority score.
    pub fixed: bool,
    /// Which line of the plan this came from, for review.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyOutcome {
    pub created: i64,
    pub skipped: i64,
    pub minutes: i64,
    pub weeks: i64,
    pub titles: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BuildOutcome {
    pub drafts: Vec<Draft>,
    pub created: i64,
    pub skipped: i64,
    pub minutes: i64,
    pub weeks: i64,
}

/// Save the plan text + shape. `start_day` is optional: without it the anchor
/// is derived from the deadline (deadline - weeks * 7).
pub fn set_plan(
    conn: &Connection,
    goal_id: &str,
    plan_text: &str,
    weeks: i64,
    start_day: Option<&str>,
) -> Result<Goal> {
    let text = plan_text.trim();
    if text.is_empty() {
        return Err(Error::Other("长期计划内容为空".into()));
    }
    if text.chars().count() > PLAN_TEXT_CAP {
        return Err(Error::Other(format!(
            "长期计划太长（上限 {PLAN_TEXT_CAP} 字）"
        )));
    }
    let weeks = weeks.clamp(1, 52);
    let start_ms = match start_day {
        Some(day) => {
            let idx = workitems::parse_day(day)
                .ok_or_else(|| Error::Other(format!("invalid start day '{day}'")))?;
            Some(idx * DAY_MS)
        }
        None => None,
    };
    let n = conn.execute(
        "UPDATE goals SET plan_text=?2, plan_weeks=?3, start_ms=?4, updated_at=?5 WHERE id=?1",
        params![goal_id, text, weeks, start_ms, now_ms()],
    )?;
    if n == 0 {
        return Err(Error::NotFound(format!("goal {goal_id}")));
    }
    workitems::get_goal(conn, goal_id)
}

/// Week-1 anchor. Explicit start wins; otherwise the deadline defines it.
pub fn anchor_day(goal: &Goal) -> Option<i64> {
    if let Some(ms) = goal.start_ms {
        return Some(ms.div_euclid(DAY_MS));
    }
    let weeks = goal.plan_weeks?;
    goal.deadline_ms
        .map(|d| d.div_euclid(DAY_MS) - weeks * 7)
}

/// Inclusive day indices for week `week` (1-based).
pub fn week_bounds(goal: &Goal, week: i64) -> Result<(i64, i64)> {
    let anchor = anchor_day(goal)
        .ok_or_else(|| Error::Other("这个目标还没有起止时间（先填截止日期或开始日期）".into()))?;
    let w = week.clamp(1, goal.plan_weeks.unwrap_or(1).clamp(1, 52));
    let start = anchor + (w - 1) * 7;
    Ok((start, start + 6))
}

/// Which week of the plan `today` falls in. Clamped to 1..=weeks.
pub fn current_week(goal: &Goal, today_idx: i64) -> Option<i64> {
    let anchor = anchor_day(goal)?;
    let weeks = goal.plan_weeks?.clamp(1, 52);
    let raw = (today_idx - anchor).div_euclid(7) + 1;
    Some(raw.clamp(1, weeks))
}

/// Validate a model reply into drafts. Pure — the tests drive this directly.
pub fn parse_drafts(reply: &str, weeks: i64) -> Result<Vec<Draft>> {
    let weeks = weeks.clamp(1, 52);
    let value = llm::extract_json(reply)?;
    let raw = value
        .get("items")
        .and_then(|v| v.as_array())
        .or_else(|| value.as_array())
        .ok_or_else(|| Error::Other("模型没有返回 items 数组".into()))?;

    let mut out: Vec<Draft> = Vec::new();
    let mut per_week: HashMap<i64, usize> = HashMap::new();
    for item in raw {
        if out.len() >= MAX_ITEMS {
            break;
        }
        let title = item
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if title.is_empty() || title.chars().count() > 120 {
            continue;
        }
        let week = item
            .get("week")
            .and_then(|v| v.as_i64())
            .unwrap_or(1)
            .clamp(1, weeks);
        let fixed = item
            .get("fixed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let slot = per_week.entry(week).or_insert(0);
        if !fixed && *slot >= MAX_PER_WEEK {
            continue;
        }
        if !fixed {
            *slot += 1;
        }
        let minutes = item
            .get("minutes")
            .and_then(|v| v.as_i64())
            .unwrap_or(60)
            .clamp(MIN_MINUTES, MAX_MINUTES);
        let reason = item
            .get("reason")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let dup = out
            .iter()
            .any(|d| d.week == week && d.title.eq_ignore_ascii_case(&title));
        if dup {
            continue;
        }
        out.push(Draft {
            week,
            title,
            minutes,
            fixed,
            reason,
        });
    }
    if out.is_empty() {
        return Err(Error::Other("模型没有给出可用的任务（可能计划内容太笼统）".into()));
    }
    Ok(out)
}

fn weekday_cn(idx: i64) -> &'static str {
    // Day 0 (1970-01-01) was a Thursday.
    const NAMES: [&str; 7] = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];
    NAMES[((idx + 4).rem_euclid(7)) as usize]
}

/// Ask the chat model to split the stored plan into week-tagged tasks.
pub fn build(model: &dyn Llm, goal: &Goal, today_label: &str) -> Result<Vec<Draft>> {
    let text = goal
        .plan_text
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::Other("这个目标还没有长期计划，先粘贴一份".into()))?;
    let weeks = goal.plan_weeks.unwrap_or(12).clamp(1, 52);
    let today_idx = workitems::parse_day(today_label)
        .ok_or_else(|| Error::Other(format!("invalid today '{today_label}'")))?;

    let system = format!(
        "你是大学生的学习计划拆解器。今天是 {today_label}（{}）。\n\
         用户给出一份 {weeks} 周的长期计划原文，请把它拆成按周排列的可执行任务。\n\n\
         硬约束（必须全部满足）：\n\
         1. 每条 30–{MAX_MINUTES} 分钟；超过 {MAX_MINUTES} 分钟必须拆成多条\n\
         2. 每周最多 {MAX_PER_WEEK} 条（fixed 事项不计入）\n\
         3. 只从计划原文提取，不要编造原文没有的内容\n\
         4. 标题要具体：动词 + 对象（如「推导第 3 章 5 个定理」），不要「复习」这种空话\n\
         5. week 从 1 开始，不超过 {weeks}\n\
         6. 周期性课程/例会 fixed=true\n\n\
         只输出 JSON，不要解释：\n\
         {{\"items\":[{{\"week\":1,\"title\":\"...\",\"minutes\":60,\"fixed\":false,\"reason\":\"对应原文的一句\"}}]}}",
        weekday_cn(today_idx)
    );
    let user = format!(
        "长期计划（共 {weeks} 周）：\n{}\n\n请按周拆解。",
        text.chars().take(PLAN_TEXT_CAP).collect::<String>()
    );
    let reply = model.complete(&system, &user)?;
    parse_drafts(&reply, weeks)
}

/// Write drafts as week-tagged work items. Idempotent by (goal, week, title).
pub fn apply(conn: &Connection, goal: &Goal, drafts: &[Draft]) -> Result<ApplyOutcome> {
    let weeks = goal.plan_weeks.unwrap_or(12).clamp(1, 52);
    let mut created = 0i64;
    let mut skipped = 0i64;
    let mut minutes = 0i64;
    let mut titles = Vec::new();
    let mut per_week: HashMap<i64, i64> = HashMap::new();

    for d in drafts {
        let week = d.week.clamp(1, weeks);
        let exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM work_items WHERE goal_id=?1 AND week_index=?2 \
             AND status IN ('open','doing','blocked') AND lower(title)=lower(?3)",
            params![goal.id, week, d.title],
            |r| r.get(0),
        )?;
        if exists > 0 {
            skipped += 1;
            continue;
        }
        let (_, end_idx) = week_bounds(goal, week)?;
        let due = (end_idx + 1) * DAY_MS - 1;
        let rank = per_week.entry(week).or_insert(0);
        *rank += 1;
        let priority = if *rank <= 2 { "high" } else { "med" };
        let body = format!(
            "由长期计划第 {week} 周自动生成。{}",
            d.reason
                .as_ref()
                .map(|r| format!("依据：{r}"))
                .unwrap_or_default()
        );
        let item = workitems::create_item(
            conn,
            &ItemInput {
                kind: Some(if d.fixed { "milestone".into() } else { "task".into() }),
                title: d.title.clone(),
                body: Some(body),
                status: Some("open".into()),
                priority: Some(priority.into()),
                due_ms: Some(due),
                start_ms: None,
                estimate_min: Some(d.minutes),
                parent_id: None,
                rrule: None,
                subject_id: goal.subject_id.clone(),
                topic_id: None,
                goal_id: Some(goal.id.clone()),
            },
        )?;
        workitems::set_week_index(conn, &item.id, week)?;
        created += 1;
        minutes += d.minutes;
        titles.push(d.title.clone());
    }
    Ok(ApplyOutcome {
        created,
        skipped,
        minutes,
        weeks,
        titles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal(weeks: i64, start_day: &str, deadline_day: &str) -> Goal {
        let start = workitems::parse_day(start_day).unwrap();
        Goal {
            id: "g1".into(),
            title: "期末复习".into(),
            kind: "exam".into(),
            metric: None,
            target: Some(85.0),
            current: 0.0,
            deadline_ms: Some(workitems::parse_day(deadline_day).unwrap() * DAY_MS),
            subject_id: None,
            status: "active".into(),
            created_at: 0,
            updated_at: 0,
            plan_text: Some("第 1 周：通读教材".into()),
            plan_weeks: Some(weeks),
            start_ms: Some(start * DAY_MS),
            item_count: 0,
            done_item_count: 0,
        }
    }

    #[test]
    fn drafts_are_clamped_and_capped() {
        let reply = r#"{"items":[
            {"week":1,"title":"推导第三章定理","minutes":400},
            {"week":1,"title":"做课后题","minutes":10},
            {"week":99,"title":"超出周数","minutes":60},
            {"week":2,"title":"","minutes":60}
        ]}"#;
        let d = parse_drafts(reply, 4).unwrap();
        assert_eq!(d.len(), 3);
        assert_eq!(d[0].minutes, 180, "minutes clamp to 180");
        assert_eq!(d[1].minutes, 30, "minutes clamp to 30");
        assert_eq!(d[2].week, 4, "week clamps to plan length");
    }

    #[test]
    fn weekly_cap_and_dedupe_hold() {
        let mut items = String::from("{\"items\":[");
        for i in 0..12 {
            if i > 0 {
                items.push(',');
            }
            items.push_str(&format!(
                "{{\"week\":1,\"title\":\"任务{i}\",\"minutes\":60}}"
            ));
        }
        items.push_str("]}");
        let d = parse_drafts(&items, 4).unwrap();
        assert_eq!(d.len(), MAX_PER_WEEK, "no more than 8 per week");
        let dup = parse_drafts(
            r#"{"items":[{"week":1,"title":"A","minutes":60},{"week":1,"title":"a","minutes":60}]}"#,
            4,
        )
        .unwrap();
        assert_eq!(dup.len(), 1, "case-insensitive dedupe");
    }

    #[test]
    fn fixed_items_do_not_count_against_the_cap() {
        let mut items = String::from("{\"items\":[");
        for i in 0..8 {
            if i > 0 {
                items.push(',');
            }
            items.push_str(&format!(
                "{{\"week\":1,\"title\":\"任务{i}\",\"minutes\":60}}"
            ));
        }
        items.push_str(r#",{"week":1,"title":"周三实验课","minutes":90,"fixed":true}]}"#);
        let d = parse_drafts(&items, 4).unwrap();
        assert_eq!(d.len(), 9);
        assert!(d.iter().any(|x| x.fixed && x.title.contains("实验课")));
    }

    #[test]
    fn empty_reply_is_an_error_not_an_empty_plan() {
        assert!(parse_drafts(r#"{"items":[]}"#, 4).is_err());
        assert!(parse_drafts("not json at all", 4).is_err());
    }

    #[test]
    fn apply_writes_week_tags_and_is_idempotent() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let g = workitems::upsert_goal(
            &c,
            &workitems::GoalInput {
                id: None,
                title: "期末复习".into(),
                kind: Some("exam".into()),
                metric: None,
                target: Some(85.0),
                current: None,
                deadline_ms: Some(workitems::parse_day("2026-11-01").unwrap() * DAY_MS),
                subject_id: None,
                status: Some("active".into()),
            },
        )
        .unwrap();
        let g = set_plan(&c, &g.id, "第 1 周读教材，第 2 周做题", 4, Some("2026-10-05")).unwrap();
        assert_eq!(g.plan_weeks, Some(4));

        let drafts = vec![
            Draft {
                week: 1,
                title: "读第一章".into(),
                minutes: 60,
                fixed: false,
                reason: None,
            },
            Draft {
                week: 2,
                title: "做课后题".into(),
                minutes: 90,
                fixed: false,
                reason: Some("第 2 周做题".into()),
            },
        ];
        let out = apply(&c, &g, &drafts).unwrap();
        assert_eq!(out.created, 2);
        assert_eq!(out.minutes, 150);

        let items = workitems::list_items(
            &c,
            &workitems::ItemQuery {
                status: Some("all".into()),
                goal_id: Some(g.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(items.len(), 2);
        let first = items.iter().find(|i| i.title == "读第一章").unwrap();
        assert_eq!(first.week_index, Some(1));
        let (_, week1_end) = week_bounds(&g, 1).unwrap();
        assert_eq!(first.due_ms, Some((week1_end + 1) * DAY_MS - 1));

        // Re-running the same extraction must not duplicate work.
        let again = apply(&c, &g, &drafts).unwrap();
        assert_eq!(again.created, 0);
        assert_eq!(again.skipped, 2);
    }

    #[test]
    fn week_bounds_follow_the_anchor() {
        let g = goal(4, "2026-10-05", "2026-11-01");
        let s = workitems::parse_day("2026-10-05").unwrap();
        assert_eq!(week_bounds(&g, 1).unwrap(), (s, s + 6));
        assert_eq!(week_bounds(&g, 2).unwrap(), (s + 7, s + 13));
        assert_eq!(current_week(&g, s).unwrap(), 1);
        assert_eq!(current_week(&g, s + 7).unwrap(), 2);
        assert_eq!(current_week(&g, s + 9999).unwrap(), 4, "clamped to the last week");
    }

    #[test]
    fn a_goal_without_anchor_cannot_be_planned() {
        let mut g = goal(4, "2026-10-05", "2026-11-01");
        g.start_ms = None;
        g.deadline_ms = None;
        assert!(week_bounds(&g, 1).is_err());
        assert!(current_week(&g, 20475).is_none());
    }
}
