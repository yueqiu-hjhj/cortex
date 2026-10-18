//! Deterministic planner: turns open work items into concrete `plan_blocks`.
//!
//! Phase 2 of the five-mode platform. Before this module a plan was a *report* —
//! `repo::sprint_plan` computed "you need 340 more minutes across 6 topics" and
//! then threw it away. A report cannot be executed, dragged, rolled over or
//! reconciled against what actually happened. This module writes rows instead.
//!
//! Design choices, all deliberate:
//!
//!   • **Rules first, no model call.** The schedule must be explainable ("this
//!     is here because it was due yesterday and urgent") and instant. An LLM
//!     planner would be neither. The model's job is content, not arithmetic.
//!   • **Idempotent.** Re-planning deletes the blocks the *planner* wrote in the
//!     horizon and rewrites them; blocks the user placed by hand are treated as
//!     fixed commitments and are never moved or deleted.
//!   • **Capacity is honest.** If the week cannot hold the work, the leftovers
//!     come back in `unscheduled` with the missing minutes, rather than being
//!     silently crammed or dropped.
//!   • **Minute-granularity occupancy.** A 1440-slot bitmap per day makes
//!     "find the next free window around my lectures" trivial and exact.
//!
//! Chunking: no single block exceeds 90 minutes, so a four-hour task becomes
//! several sittings instead of one impossible wall — which is also what makes
//! the day's plan survive contact with reality.

use crate::db::{new_id, now_ms};
use crate::error::{Error, Result};
use crate::workitems::{self, ItemInput, ItemQuery, PlanBlock};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

const MINUTES_PER_DAY: i64 = 1440;
/// A sitting. Beyond ~90 minutes the marginal retention is negative, and the
/// block stops being schedulable into a normal evening.
const MAX_CHUNK: i64 = 90;
/// Below this a sliver is not worth a block — better to roll it into tomorrow.
const MIN_CHUNK: i64 = 5;

#[derive(Debug, Clone, Deserialize)]
pub struct PlanRequest {
    /// Local `YYYY-MM-DD` of "today" — supplied by the frontend so the backend
    /// never guesses a timezone.
    pub start_day: String,
    /// Horizon in days (1..=30). Default 7.
    pub days: Option<i64>,
    /// Focused minutes available per day. Default 120.
    pub daily_minutes: Option<i64>,
    /// Earliest minute of the day a block may start (540 = 09:00).
    pub day_start_min: Option<i64>,
    /// Latest minute a block may end (1320 = 22:00).
    pub day_end_min: Option<i64>,
    /// Gap left between two blocks on the same day.
    pub break_min: Option<i64>,
    /// Rewrite planner-owned blocks in the horizon. Manual blocks are always
    /// preserved. Default true.
    pub replace: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Unscheduled {
    pub item_id: String,
    pub title: String,
    /// How many minutes could not be placed.
    pub minutes: i64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanOutcome {
    pub start_day: String,
    pub days: i64,
    pub blocks: Vec<PlanBlock>,
    pub scheduled_items: i64,
    pub unscheduled: Vec<Unscheduled>,
    pub minutes_planned: i64,
    pub minutes_capacity: i64,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RolloverOutcome {
    /// Blocks from earlier days that were marked `moved`.
    pub moved: i64,
    /// The days they came from, oldest first (for the UI's explanation line).
    pub from_days: Vec<String>,
}

// --------------------------------------------------------------- planning

/// One schedulable unit, scored so the ordering is inspectable rather than
/// emergent. Higher score = earlier.
struct Candidate {
    id: String,
    title: String,
    remaining: i64,
    score: i64,
}

pub fn plan_week(conn: &Connection, req: &PlanRequest) -> Result<PlanOutcome> {
    let days = req.days.unwrap_or(7).clamp(1, 30);
    let daily = req.daily_minutes.unwrap_or(120).clamp(15, 960);
    let win_start = req.day_start_min.unwrap_or(540).clamp(0, MINUTES_PER_DAY - 30);
    let win_end = req
        .day_end_min
        .unwrap_or(1320)
        .clamp(win_start + 30, MINUTES_PER_DAY);
    let brk = req.break_min.unwrap_or(10).clamp(0, 60);
    let replace = req.replace.unwrap_or(true);

    let start_idx = workitems::parse_day(&req.start_day)
        .ok_or_else(|| Error::Other(format!("invalid start_day '{}' (expected YYYY-MM-DD)", req.start_day)))?;
    // Learn from history before deciding how long anything takes. Without this
    // the plan is just the user's optimism, straight from the settings file.
    let calib = calibration(conn, 60)?;
    let day_keys: Vec<String> = (0..days)
        .map(|i| workitems::format_day(start_idx + i))
        .collect();

    // ---- 1. read what is already on the calendar -------------------------
    let mut busy: Vec<Vec<bool>> = day_keys.iter().map(|_| vec![false; MINUTES_PER_DAY as usize]).collect();
    let mut manual_items: HashSet<String> = HashSet::new();

    for (d, key) in day_keys.iter().enumerate() {
        for b in workitems::list_blocks(conn, key)? {
            if b.origin == "manual" {
                // The user put this here on purpose: it is a fixed commitment,
                // so the item is not re-planned and the slot is not offered.
                manual_items.insert(b.item_id.clone());
            }
            let keep = b.origin == "manual" || !replace;
            if keep {
                let from = b.start_min.clamp(0, MINUTES_PER_DAY);
                let to = b.end_min.clamp(0, MINUTES_PER_DAY);
                for m in from..to {
                    busy[d][m as usize] = true;
                }
            }
        }
    }

    // ---- 2. clear last run's proposals -----------------------------------
    if replace {
        for key in &day_keys {
            conn.execute(
                "DELETE FROM plan_blocks WHERE day = ?1 AND origin = 'planner' AND status = 'planned'",
                params![key],
            )?;
        }
    }

    // ---- 3. candidates ---------------------------------------------------
    let items = workitems::list_items(
        conn,
        &ItemQuery {
            status: Some("active".into()),
            limit: Some(500),
            ..Default::default()
        },
    )?;

    let horizon_end = start_idx + days;
    let mut candidates: Vec<Candidate> = Vec::new();
    for it in items {
        // Project headers carry no time of their own; their children do. Planning
        // both would double-count the same work.
        if it.child_count > 0 {
            continue;
        }
        if manual_items.contains(&it.id) {
            continue;
        }
        // A recurring item is a habit, not a backlog entry: it shows up in Today
        // via its rrule and must not be pinned to a single day here.
        if it.rrule.as_deref().map(|r| !r.trim().is_empty()).unwrap_or(false) {
            continue;
        }

        let mut score: i64 = 0;
        if let Some(due) = it.due_ms {
            let due_day = due.div_euclid(86_400_000);
            if due_day < start_idx {
                // Overdue: dominates everything else, more so the older it is.
                score += 1000 + (start_idx - due_day).min(30) * 10;
            } else if due_day < horizon_end {
                // Due inside the horizon: the sooner, the stronger.
                score += 600 + (horizon_end - due_day) * 10;
            } else {
                score += 100;
            }
        }
        score += match it.priority.as_deref() {
            Some("urgent") => 300,
            Some("high") => 200,
            Some("med") => 100,
            _ => 0,
        };
        if it.status == "doing" {
            // Already started — finishing beats starting.
            score += 150;
        }
        if it.goal_id.is_some() {
            score += 50;
        }

        candidates.push(Candidate {
            id: it.id,
            title: it.title,
            remaining: effective_estimate_min(it.estimate_min, &it.kind, &calib),
            score,
        });
    }
    // Deterministic: score, then title, so re-running on the same data produces
    // the same schedule (a planner that shuffles on every run is unusable).
    candidates.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.title.cmp(&b.title)));

    // ---- 4. place --------------------------------------------------------
    let mut cursor = vec![win_start; days as usize];
    let mut used = vec![0i64; days as usize];
    let mut unscheduled: Vec<Unscheduled> = Vec::new();
    let mut scheduled_items: HashSet<String> = HashSet::new();
    let mut minutes_planned = 0i64;

    for cand in &candidates {
        let mut remaining = cand.remaining;
        while remaining > 0 {
            let mut placed = false;
            for d in 0..days as usize {
                let room = daily - used[d];
                if room < MIN_CHUNK {
                    continue;
                }
                let need = remaining.min(MAX_CHUNK).min(room);
                if need < MIN_CHUNK {
                    continue;
                }
                if let Some(start) = find_slot(&busy[d], cursor[d], need, win_end) {
                    let end = start + need;
                    conn.execute(
                        "INSERT INTO plan_blocks (id, item_id, day, start_min, end_min, status, \
                         origin, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,'planned','planner',?6,?6)",
                        params![new_id(), cand.id, day_keys[d], start, end, now_ms()],
                    )?;
                    for m in start..end {
                        busy[d][m as usize] = true;
                    }
                    cursor[d] = end + brk;
                    used[d] += need;
                    minutes_planned += need;
                    remaining -= need;
                    scheduled_items.insert(cand.id.clone());
                    placed = true;
                    break;
                }
            }
            if !placed {
                unscheduled.push(Unscheduled {
                    item_id: cand.id.clone(),
                    title: cand.title.clone(),
                    minutes: remaining,
                    reason: format!(
                        "容量不足：未来 {days} 天还差 {remaining} 分钟（每天 {daily} 分钟）"
                    ),
                });
                break;
            }
        }
    }

    // ---- 5. read back + explain -----------------------------------------
    let mut blocks = Vec::new();
    for key in &day_keys {
        blocks.extend(
            workitems::list_blocks(conn, key)?
                .into_iter()
                .filter(|b| b.origin == "planner"),
        );
    }

    let capacity = daily * days;
    let mut notes = Vec::new();
    if !unscheduled.is_empty() {
        let short: i64 = unscheduled.iter().map(|u| u.minutes).sum();
        notes.push(format!(
            "{} 项未排入，共差 {short} 分钟。要么提高每日容量，要么把目标往后挪。",
            unscheduled.len()
        ));
    }
    if minutes_planned == 0 && !candidates.is_empty() {
        notes.push("没有可用的时间窗口：检查每日容量与「最早/最晚」时间设置。".into());
    }
    if candidates.is_empty() {
        notes.push("没有待排的工作项（没有预估时长的项目会按 30 分钟计）。".into());
    }
    // Say why the schedule is longer than the raw estimates, otherwise the user
    // reads a 1.4x plan as a bug in the planner.
    if calib.samples >= MIN_KIND_SAMPLES && (calib.factor - 1.0).abs() >= 0.15 {
        let dir = if calib.factor > 1.0 { "长" } else { "短" };
        notes.push(format!(
            "过去 {d} 天完成 {n} 项，实际用时平均是预估的 {f:.2} 倍，本次排期按校准后的时长分配（比填的预估{dir}）。",
            d = calib.window_days,
            n = calib.samples,
            f = calib.factor,
            dir = dir,
        ));
    }

    Ok(PlanOutcome {
        start_day: req.start_day.clone(),
        days,
        blocks,
        scheduled_items: scheduled_items.len() as i64,
        unscheduled,
        minutes_planned,
        minutes_capacity: capacity,
        notes,
    })
}

/// First minute >= `from` where `need` consecutive minutes are free and the
/// block still ends by `limit`. Scans forward one minute at a time — 1440 slots
/// is nothing, and this is exact around arbitrary manual commitments.
fn find_slot(busy: &[bool], from: i64, need: i64, limit: i64) -> Option<i64> {
    let from = from.max(0);
    let mut start = from;
    while start + need <= limit {
        let free = (start..start + need).all(|m| !busy[m as usize]);
        if free {
            return Some(start);
        }
        start += 1;
    }
    None
}

// ------------------------------------------------------- goal -> work items

/// Turn a goal into the work items the planner can actually schedule.
///
/// This is the join between the Exam mode (BKT gaps per topic) and the Plan mode
/// (a week of blocks). Without it a goal is a number on a dashboard; with it, a
/// goal becomes a list of sittings.
#[derive(Debug, Clone, Deserialize)]
pub struct GoalPlanRequest {
    pub goal_id: String,
    pub start_day: String,
    pub daily_minutes: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GoalPlanOutcome {
    pub goal_id: String,
    pub created: i64,
    pub skipped: i64,
    pub minutes: i64,
    pub titles: Vec<String>,
    pub notes: Vec<String>,
}

pub fn materialize_goal(conn: &Connection, req: &GoalPlanRequest) -> Result<GoalPlanOutcome> {
    let goal = workitems::get_goal(conn, &req.goal_id)?;
    let daily = req.daily_minutes.unwrap_or(120).clamp(15, 960);
    let start_idx = workitems::parse_day(&req.start_day)
        .ok_or_else(|| Error::Other(format!("invalid start_day '{}'", req.start_day)))?;
    let deadline = goal
        .deadline_ms
        .unwrap_or_else(|| (start_idx + 30) * 86_400_000);

    let mut created = 0i64;
    let mut skipped = 0i64;
    let mut minutes = 0i64;
    let mut titles: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    if let Some(subject_id) = goal.subject_id.clone() {
        // Reuse the existing sprint estimator: it already blends BKT mastery,
        // coverage and recent accuracy into a per-topic minute budget. The only
        // thing it never did was persist the result.
        let target = goal.target.unwrap_or(85.0);
        let plan = crate::repo::sprint_plan(conn, &subject_id, target, deadline, daily)?;
        for t in plan.topics.iter().filter(|t| t.minutes >= 15) {
            let exists: i64 = conn.query_row(
                "SELECT COUNT(*) FROM work_items WHERE goal_id=?1 AND topic_id=?2 \
                 AND status IN ('open','doing','blocked')",
                params![goal.id, t.topic_id],
                |r| r.get(0),
            )?;
            if exists > 0 {
                skipped += 1;
                continue;
            }
            let title = format!("复习：{}", t.topic_name);
            workitems::create_item(
                conn,
                &ItemInput {
                    kind: Some("task".into()),
                    title: title.clone(),
                    body: Some(format!(
                        "掌握度 {:.0}% → 目标 {:.0}%，缺口 {:.0}%。由目标「{}」自动生成。",
                        t.mastery * 100.0,
                        plan.mastery_target * 100.0,
                        t.gap * 100.0,
                        goal.title
                    )),
                    status: Some("open".into()),
                    priority: Some(if t.rank <= 2 { "high".into() } else { "med".into() }),
                    due_ms: Some(deadline),
                    start_ms: None,
                    estimate_min: Some(t.minutes),
                    parent_id: None,
                    rrule: None,
                    subject_id: Some(subject_id.clone()),
                    topic_id: Some(t.topic_id.clone()),
                    goal_id: Some(goal.id.clone()),
                },
            )?;
            created += 1;
            minutes += t.minutes;
            titles.push(title);
        }
        if !plan.feasible {
            notes.push(format!(
                "按每天 {daily} 分钟算，这个目标在截止日前做不完（还差 {} 分钟）。",
                (plan.minutes_required - plan.minutes_available).max(0)
            ));
        }
    } else {
        // No subject to break down: emit one milestone the planner can carry.
        let exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM work_items WHERE goal_id=?1 AND status IN ('open','doing','blocked')",
            params![goal.id],
            |r| r.get(0),
        )?;
        if exists > 0 {
            skipped += 1;
        } else {
            let title = format!("推进目标：{}", goal.title);
            workitems::create_item(
                conn,
                &ItemInput {
                    kind: Some("milestone".into()),
                    title: title.clone(),
                    body: Some("由目标自动生成的里程碑（该目标未绑定科目，无法按主题拆解）。".into()),
                    status: Some("open".into()),
                    priority: Some("med".into()),
                    due_ms: Some(deadline),
                    start_ms: None,
                    estimate_min: Some(60),
                    parent_id: None,
                    rrule: None,
                    subject_id: None,
                    topic_id: None,
                    goal_id: Some(goal.id.clone()),
                },
            )?;
            created += 1;
            minutes += 60;
            titles.push(title);
        }
    }

    Ok(GoalPlanOutcome {
        goal_id: goal.id,
        created,
        skipped,
        minutes,
        titles,
        notes,
    })
}

// ------------------------------------------------------------- board view

/// A block joined with the item it belongs to. The Plan view needs the title
/// and the item's state on every card; doing that join here keeps the frontend
/// from firing one `get_work_item` per block.
#[derive(Debug, Clone, Serialize)]
pub struct BlockRow {
    #[serde(flatten)]
    pub block: PlanBlock,
    pub title: String,
    pub item_status: String,
    pub kind: String,
    pub estimate_min: Option<i64>,
}

pub fn board(conn: &Connection, start_day: &str, days: i64) -> Result<Vec<BlockRow>> {
    let start = workitems::parse_day(start_day)
        .ok_or_else(|| Error::Other(format!("invalid start_day '{start_day}'")))?;
    let days = days.clamp(1, 31);
    let end = workitems::format_day(start + days - 1);
    let mut st = conn.prepare(
        "SELECT b.id, b.item_id, b.day, b.start_min, b.end_min, b.status, b.origin, \
                b.created_at, b.updated_at, w.title, w.status, w.kind, w.estimate_min \
         FROM plan_blocks b JOIN work_items w ON w.id = b.item_id \
         WHERE b.day >= ?1 AND b.day <= ?2 \
         ORDER BY b.day, b.start_min",
    )?;
    let rows = st.query_map(params![start_day, end], |r| {
        Ok(BlockRow {
            block: PlanBlock {
                id: r.get(0)?,
                item_id: r.get(1)?,
                day: r.get(2)?,
                start_min: r.get(3)?,
                end_min: r.get(4)?,
                status: r.get(5)?,
                origin: r.get(6)?,
                created_at: r.get(7)?,
                updated_at: r.get(8)?,
            },
            title: r.get(9)?,
            item_status: r.get(10)?,
            kind: r.get(11)?,
            estimate_min: r.get(12)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// ------------------------------------------------------ estimate learning

/// The planner's estimate for a task is the user's number, not reality. Left
/// alone, every plan is optimistic in the same direction, and the
/// "planned vs actual" chart becomes a receipt for wishful thinking.
///
/// This measures how far off estimates have actually been, per work-item kind,
/// and lets `plan_week` schedule the *honest* duration instead.
#[derive(Debug, Clone, Serialize)]
pub struct Calibration {
    /// actual / estimate over the window. 1.0 means estimates were right.
    pub factor: f32,
    /// How many finished items the number is based on. Few = weak signal.
    pub samples: usize,
    /// Per-kind factors, so a "reading" habit can differ from a "task" chore.
    pub per_kind: Vec<KindFactor>,
    /// Look-back window in days that produced this.
    pub window_days: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct KindFactor {
    pub kind: String,
    pub factor: f32,
    pub samples: usize,
}

/// Below this many finished items a kind's own ratio is noise, so it borrows
/// the global factor instead of following one wild datapoint.
const MIN_KIND_SAMPLES: usize = 3;
/// Hard bounds: a calibration that says "everything takes 6x longer" is a bug in
/// the data, not a plan.
const FACTOR_MIN: f32 = 0.5;
const FACTOR_MAX: f32 = 3.0;
/// A day in milliseconds.
const DAY_MS: i64 = 86_400_000;

/// Median of a ratio list. Median, not mean: one item abandoned at 5% progress
/// would drag an average straight to zero.
fn median(values: &mut [f32]) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = values.len() / 2;
    Some(if values.len() % 2 == 1 {
        values[mid]
    } else {
        (values[mid - 1] + values[mid]) / 2.0
    })
}

/// Measure estimate error over the last `days`.
///
/// Only items that were actually *finished* ("done") with both an estimate and
/// logged minutes count. An item still open tells you nothing.
pub fn calibration(conn: &Connection, days: i64) -> Result<Calibration> {
    let days = days.clamp(7, 365);
    let since = now_ms() - days * DAY_MS;
    let mut rows: Vec<(String, f64, f64)> = Vec::new();
    {
        let mut st = conn.prepare(
            "SELECT kind, estimate_min, actual_min FROM work_items \
             WHERE status = 'done' AND estimate_min > 0 AND actual_min > 0 \
               AND updated_at >= ?1 LIMIT 2000",
        )?;
        let out = st.query_map(params![since], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?))
        })?;
        for row in out.flatten() {
            rows.push(row);
        }
    }

    let mut all: Vec<f32> = Vec::new();
    let mut by_kind: HashMap<String, Vec<f32>> = HashMap::new();
    for (kind, est, act) in &rows {
        if *est <= 0.0 {
            continue;
        }
        let ratio = (act / est) as f32;
        if ratio.is_finite() && ratio > 0.0 {
            all.push(ratio);
            by_kind.entry(kind.clone()).or_default().push(ratio);
        }
    }

    // Global first, because a kind with too few samples borrows it.
    let mut all_ratios = all.clone();
    let global = median(&mut all_ratios).unwrap_or(1.0);

    let mut per_kind: Vec<KindFactor> = by_kind
        .iter()
        .filter(|(_, v)| v.len() >= MIN_KIND_SAMPLES)
        .map(|(kind, v)| {
            let mut v = v.clone();
            KindFactor {
                kind: kind.clone(),
                factor: median(&mut v).unwrap_or(global).clamp(FACTOR_MIN, FACTOR_MAX),
                samples: v.len(),
            }
        })
        .collect();
    per_kind.sort_by(|a, b| b.samples.cmp(&a.samples).then(a.kind.cmp(&b.kind)));

    Ok(Calibration {
        factor: global.clamp(FACTOR_MIN, FACTOR_MAX),
        samples: all.len(),
        per_kind,
        window_days: days,
    })
}

/// What the planner should budget for an item, given how honest the estimates
/// have been for that kind.
///
/// Falls back to the raw estimate when there is nothing to learn from yet, so a
/// brand-new install behaves exactly as before.
pub fn effective_estimate_min(estimate_min: Option<i64>, kind: &str, calib: &Calibration) -> i64 {
    let base = estimate_min.unwrap_or(30).clamp(5, 600);
    if calib.samples < MIN_KIND_SAMPLES {
        return base;
    }
    let factor = calib
        .per_kind
        .iter()
        .find(|k| k.kind == kind)
        .map(|k| k.factor)
        .unwrap_or(calib.factor);
    let scaled = (base as f32 * factor).round();
    // Snap to 5 minutes: a 63-minute slot is theatre.
    let snapped = ((scaled / 5.0).round() * 5.0) as i64;
    snapped.clamp(5, 960)
}

// --------------------------------------------------------- plan vs actual

/// One day of "what the plan said" against "what actually happened".
#[derive(Debug, Clone, Serialize)]
pub struct PlanDayStat {
    pub day: String,
    /// Minutes the plan allocated (sum of block lengths).
    pub planned_min: i64,
    /// Minutes from blocks that were ticked off.
    pub done_min: i64,
    pub planned_blocks: i64,
    pub done_blocks: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanVsActual {
    pub days: Vec<PlanDayStat>,
    pub planned_min: i64,
    pub done_min: i64,
    /// done / planned, 0..1. 0 when nothing was planned (never divide by zero
    /// into a fake "100% adherence").
    pub adherence: f64,
    /// Estimate vs actual for the items that were scheduled in the window — the
    /// signal the planner needs to calibrate itself later.
    pub estimate_min: i64,
    pub actual_min: i64,
}

/// Planned vs completed minutes per day, plus estimate-vs-actual for the items
/// that appeared in the window. Gaps are filled with zeros so the bar chart has
/// one column per day instead of a sparse series.
pub fn plan_vs_actual(conn: &Connection, start_day: &str, days: i64) -> Result<PlanVsActual> {
    let start = workitems::parse_day(start_day)
        .ok_or_else(|| Error::Other(format!("invalid start_day '{start_day}'")))?;
    let days = days.clamp(1, 90);
    let end_day = workitems::format_day(start + days - 1);

    let mut by_day: HashMap<String, (i64, i64, i64, i64)> = HashMap::new();
    {
        let mut st = conn.prepare(
            "SELECT day, COALESCE(SUM(end_min - start_min), 0), \
                    COALESCE(SUM(CASE WHEN status = 'done' THEN end_min - start_min ELSE 0 END), 0), \
                    COUNT(*), \
                    COALESCE(SUM(CASE WHEN status = 'done' THEN 1 ELSE 0 END), 0) \
             FROM plan_blocks WHERE day >= ?1 AND day <= ?2 GROUP BY day",
        )?;
        let rows = st.query_map(params![start_day, end_day], |r| {
            Ok((
                r.get::<_, String>(0)?,
                (
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                ),
            ))
        })?;
        for row in rows.flatten() {
            by_day.insert(row.0, row.1);
        }
    }

    let mut out = Vec::with_capacity(days as usize);
    let (mut planned_total, mut done_total) = (0i64, 0i64);
    for i in 0..days {
        let day = workitems::format_day(start + i);
        let (planned, done, pblocks, dblocks) = by_day.get(&day).copied().unwrap_or((0, 0, 0, 0));
        planned_total += planned;
        done_total += done;
        out.push(PlanDayStat {
            day,
            planned_min: planned,
            done_min: done,
            planned_blocks: pblocks,
            done_blocks: dblocks,
        });
    }

    let (estimate_min, actual_min) = conn.query_row(
        "SELECT COALESCE(SUM(w.estimate_min), 0), COALESCE(SUM(w.actual_min), 0) \
         FROM work_items w WHERE w.id IN ( \
             SELECT DISTINCT item_id FROM plan_blocks WHERE day >= ?1 AND day <= ?2)",
        params![start_day, end_day],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
    )?;

    let adherence = if planned_total > 0 {
        (done_total as f64 / planned_total as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };

    Ok(PlanVsActual {
        days: out,
        planned_min: planned_total,
        done_min: done_total,
        adherence,
        estimate_min,
        actual_min,
    })
}

// -------------------------------------------------------------- rollover

/// Mark yesterday's unstarted blocks as `moved`.
///
/// The items themselves stay open, so the next `plan_week` run picks them up
/// again — with their overdue weight now higher. That is the whole rollover
/// mechanism: no hidden copy of "today", just blocks that stopped being true.
pub fn rollover(conn: &Connection, today: &str) -> Result<RolloverOutcome> {
    workitems::parse_day(today)
        .ok_or_else(|| Error::Other(format!("invalid today '{today}' (expected YYYY-MM-DD)")))?;

    let mut from_days: Vec<String> = {
        let mut st = conn.prepare(
            "SELECT DISTINCT day FROM plan_blocks WHERE status = 'planned' AND day < ?1 \
             ORDER BY day",
        )?;
        let rows = st.query_map(params![today], |r| r.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };
    if from_days.is_empty() {
        return Ok(RolloverOutcome { moved: 0, from_days });
    }

    let moved = conn.execute(
        "UPDATE plan_blocks SET status = 'moved', updated_at = ?2 \
         WHERE status = 'planned' AND day < ?1",
        params![today, now_ms()],
    )? as i64;

    from_days.truncate(7);
    Ok(RolloverOutcome { moved, from_days })
}

// -------------------------------------------------------------- completion

/// Mark a block done and log its length against the item.
///
/// Time is written on completion rather than on "start" so the estimate-vs-actual
/// comparison stays honest: a block you never touched logs nothing.
pub fn complete_block(conn: &Connection, block_id: &str) -> Result<PlanBlock> {
    let block = workitems::get_block(conn, block_id)?;
    if block.status == "done" {
        return Ok(block);
    }
    conn.execute(
        "UPDATE plan_blocks SET status = 'done', updated_at = ?2 WHERE id = ?1",
        params![block_id, now_ms()],
    )?;
    let minutes = (block.end_min - block.start_min).max(0);
    workitems::add_actual_minutes(conn, &block.item_id, minutes)?;
    workitems::get_block(conn, block_id)
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::AppState;
    use crate::workitems::{create_item, ItemInput, WorkItem};

    fn state() -> AppState {
        AppState::in_memory().unwrap()
    }

    fn item(title: &str, estimate: i64, due_day_offset: Option<i64>, priority: &str) -> ItemInput {
        // Anchor dates on 2026-03-10 so the tests never depend on "now".
        let base = workitems::parse_day("2026-03-10").unwrap();
        ItemInput {
            kind: None,
            title: title.into(),
            body: None,
            status: None,
            priority: if priority.is_empty() { None } else { Some(priority.into()) },
            due_ms: due_day_offset.map(|d| (base + d) * 86_400_000 + 12 * 3600_000),
            start_ms: None,
            estimate_min: Some(estimate),
            parent_id: None,
            rrule: None,
            subject_id: None,
            topic_id: None,
            goal_id: None,
        }
    }

    fn request(days: i64, daily: i64) -> PlanRequest {
        PlanRequest {
            start_day: "2026-03-10".into(),
            days: Some(days),
            daily_minutes: Some(daily),
            day_start_min: Some(540),
            day_end_min: Some(1320),
            break_min: Some(10),
            replace: Some(true),
        }
    }

    fn titles(conn: &Connection, day: &str) -> Vec<String> {
        let mut out = Vec::new();
        for b in workitems::list_blocks(conn, day).unwrap() {
            let it: WorkItem = workitems::get_item(conn, &b.item_id).unwrap();
            out.push(it.title);
        }
        out
    }

    #[test]
    fn schedules_overdue_before_everything_else() {
        let st = state();
        let c = st.db.lock().unwrap();
        create_item(&c, &item("future reading", 60, Some(5), "low")).unwrap();
        create_item(&c, &item("overdue essay", 60, Some(-2), "low")).unwrap();

        let out = plan_week(&c, &request(3, 120)).unwrap();
        assert_eq!(out.scheduled_items, 2);
        let first_day = titles(&c, "2026-03-10");
        assert_eq!(first_day.first().map(String::as_str), Some("overdue essay"));
    }

    #[test]
    fn never_exceeds_daily_capacity() {
        let st = state();
        let c = st.db.lock().unwrap();
        for i in 0..6 {
            create_item(&c, &item(&format!("task {i}"), 60, Some(1), "med")).unwrap();
        }
        let out = plan_week(&c, &request(2, 120)).unwrap();
        // 2 days x 120 minutes = 240 minutes of capacity, 360 minutes of work.
        assert_eq!(out.minutes_capacity, 240);
        assert!(out.minutes_planned <= 240, "planned {} > capacity", out.minutes_planned);
        assert_eq!(out.unscheduled.len(), 2, "leftovers must be reported, not hidden");
        assert!(out.unscheduled.iter().all(|u| u.minutes == 60));
    }

    #[test]
    fn chunks_long_items_into_sittings() {
        let st = state();
        let c = st.db.lock().unwrap();
        create_item(&c, &item("thesis chapter", 240, Some(1), "high")).unwrap();
        let out = plan_week(&c, &request(4, 120)).unwrap();
        assert_eq!(out.minutes_planned, 240);
        assert!(out.blocks.len() >= 3, "240 min should be split into sittings");
        assert!(out.blocks.iter().all(|b| b.end_min - b.start_min <= MAX_CHUNK));
    }

    #[test]
    fn manual_blocks_are_respected_and_not_duplicated() {
        let st = state();
        let c = st.db.lock().unwrap();
        let fixed = create_item(&c, &item("lecture", 90, Some(1), "high")).unwrap();
        // The user pins it at 09:00 by hand.
        workitems::upsert_block(&c, None, &fixed.id, "2026-03-10", 540, 630, None).unwrap();

        let movable = create_item(&c, &item("problem set", 60, Some(1), "high")).unwrap();
        let out = plan_week(&c, &request(1, 180)).unwrap();

        // The hand-placed item is not re-planned...
        assert!(
            !out.blocks.iter().any(|b| b.item_id == fixed.id),
            "manual block must not be re-planned"
        );
        // ...and the new block does not overlap it.
        let mine = out.blocks.iter().find(|b| b.item_id == movable.id).unwrap();
        assert!(mine.start_min >= 630, "must start after the lecture, got {}", mine.start_min);
    }

    #[test]
    fn replanning_is_idempotent() {
        let st = state();
        let c = st.db.lock().unwrap();
        create_item(&c, &item("task a", 60, Some(1), "med")).unwrap();
        create_item(&c, &item("task b", 60, Some(2), "med")).unwrap();

        let first = plan_week(&c, &request(3, 120)).unwrap();
        let second = plan_week(&c, &request(3, 120)).unwrap();
        assert_eq!(first.blocks.len(), second.blocks.len());
        assert_eq!(first.minutes_planned, second.minutes_planned);

        let total: i64 = c
            .query_row("SELECT COUNT(*) FROM plan_blocks WHERE origin='planner'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, second.blocks.len() as i64, "no duplicate blocks accumulate");
    }

    #[test]
    fn rollover_marks_past_planned_blocks_only() {
        let st = state();
        let c = st.db.lock().unwrap();
        let a = create_item(&c, &item("yesterday", 30, None, "")).unwrap();
        let b = create_item(&c, &item("today", 30, None, "")).unwrap();
        workitems::upsert_block(&c, None, &a.id, "2026-03-09", 600, 630, None).unwrap();
        workitems::upsert_block(&c, None, &b.id, "2026-03-10", 600, 630, None).unwrap();

        let out = rollover(&c, "2026-03-10").unwrap();
        assert_eq!(out.moved, 1);
        assert_eq!(out.from_days, vec!["2026-03-09".to_string()]);

        let old = workitems::list_blocks(&c, "2026-03-09").unwrap();
        assert_eq!(old[0].status, "moved");
        let keep = workitems::list_blocks(&c, "2026-03-10").unwrap();
        assert_eq!(keep[0].status, "planned");
    }

    #[test]
    fn completing_a_block_logs_actual_minutes() {
        let st = state();
        let c = st.db.lock().unwrap();
        let it = create_item(&c, &item("problem set", 60, None, "")).unwrap();
        let block = workitems::upsert_block(&c, None, &it.id, "2026-03-10", 600, 660, None).unwrap();

        let done = complete_block(&c, &block.id).unwrap();
        assert_eq!(done.status, "done");
        let reloaded = workitems::get_item(&c, &it.id).unwrap();
        assert_eq!(reloaded.actual_min, 60);

        // Completing twice must not double-count.
        complete_block(&c, &block.id).unwrap();
        assert_eq!(workitems::get_item(&c, &it.id).unwrap().actual_min, 60);
    }

    #[test]
    fn recurring_items_are_not_pinned_to_a_day() {
        let st = state();
        let c = st.db.lock().unwrap();
        let mut habit = item("daily vocab", 20, None, "");
        habit.rrule = Some("FREQ=DAILY".into());
        create_item(&c, &habit).unwrap();
        let out = plan_week(&c, &request(3, 120)).unwrap();
        assert_eq!(out.blocks.len(), 0);
    }
}

#[cfg(test)]
mod calibration_tests {
    use super::*;
    use crate::db::AppState;
    use crate::workitems::{self, ItemInput};

    /// A finished item that recorded `actual` minutes against an `estimate`.
    fn done_with(conn: &Connection, kind: &str, estimate: i64, actual: i64) -> String {
        let it = workitems::create_item(
            conn,
            &ItemInput {
                kind: Some(kind.into()),
                title: format!("{kind} #{estimate}"),
                body: None,
                status: None,
                priority: None,
                due_ms: None,
                start_ms: None,
                estimate_min: Some(estimate),
                parent_id: None,
                rrule: None,
                subject_id: None,
                topic_id: None,
                goal_id: None,
            },
        )
        .unwrap();
        workitems::set_status(conn, &it.id, "done").unwrap();
        workitems::add_actual_minutes(conn, &it.id, actual).unwrap();
        it.id
    }

    #[test]
    fn nothing_to_learn_from_yet_means_no_behaviour_change() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let cb = calibration(&c, 60).unwrap();
        assert_eq!(cb.samples, 0);
        assert!((cb.factor - 1.0).abs() < 1e-6);
        // The raw estimate passes through untouched, so a fresh install plans
        // exactly as it did before this module existed.
        assert_eq!(effective_estimate_min(Some(45), "task", &cb), 45);
    }

    #[test]
    fn median_of_ratios_ignores_one_wild_outlier() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        for (est, act) in [(30, 45), (60, 90), (30, 45), (60, 600)] {
            done_with(&c, "task", est, act);
        }
        let cb = calibration(&c, 60).unwrap();
        assert_eq!(cb.samples, 4);
        // Ratios are 1.5, 1.5, 1.5, 10.0 -> the median is 1.5, not the 3.6 mean.
        assert!((cb.factor - 1.5).abs() < 1e-6, "got {}", cb.factor);
    }

    #[test]
    fn per_kind_factor_beats_the_global_one() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        for _ in 0..3 {
            done_with(&c, "reading", 60, 120); // twice over
        }
        for _ in 0..3 {
            done_with(&c, "task", 60, 30); // half
        }
        let cb = calibration(&c, 60).unwrap();
        let reading = cb.per_kind.iter().find(|k| k.kind == "reading").unwrap();
        let task = cb.per_kind.iter().find(|k| k.kind == "task").unwrap();
        assert!((reading.factor - 2.0).abs() < 1e-6);
        assert!((task.factor - 0.5).abs() < 1e-6);
        assert_eq!(effective_estimate_min(Some(60), "reading", &cb), 120);
        assert_eq!(effective_estimate_min(Some(60), "task", &cb), 30);
    }

    #[test]
    fn a_lonely_kind_borrows_the_global_factor() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        for _ in 0..3 {
            done_with(&c, "task", 100, 50); // global ratio 0.5
        }
        done_with(&c, "milestone", 100, 400); // 4x, but only ONE sample
        let cb = calibration(&c, 60).unwrap();
        assert_eq!(cb.per_kind.len(), 1, "the single-sample kind must be dropped");
        // 100 * 0.5, not 400.
        assert_eq!(effective_estimate_min(Some(100), "milestone", &cb), 50);
    }

    #[test]
    fn factors_are_clamped_so_bad_data_cannot_wreck_a_plan() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        for _ in 0..3 {
            done_with(&c, "task", 10, 1000); // 100x
        }
        let cb = calibration(&c, 60).unwrap();
        assert!((cb.factor - FACTOR_MAX).abs() < 1e-6, "got {}", cb.factor);
        // 60 minutes of work must not become 6000 minutes of plan.
        assert_eq!(effective_estimate_min(Some(60), "task", &cb), 180);
    }

    #[test]
    fn plans_use_the_calibrated_duration() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        for _ in 0..3 {
            done_with(&c, "task", 60, 90);
        }
        // A task the user thinks takes an hour actually takes 90 minutes.
        workitems::create_item(
            &c,
            &ItemInput {
                kind: Some("task".into()),
                title: "write essay".into(),
                body: None,
                status: None,
                priority: Some("high".into()),
                due_ms: None,
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

        let out = plan_week(
            &c,
            &PlanRequest {
                start_day: "2026-05-10".into(),
                days: Some(3),
                daily_minutes: Some(120),
                day_start_min: Some(540),
                day_end_min: Some(1320),
                break_min: Some(10),
                replace: Some(true),
            },
        )
        .unwrap();
        let scheduled: i64 = out.blocks.iter().map(|b| b.end_min - b.start_min).sum();
        assert_eq!(
            scheduled, 90,
            "planner must budget the honest 90 minutes, not the hopeful 60"
        );
        assert_eq!(out.scheduled_items, 1);
    }
}

#[cfg(test)]
mod adherence_tests {
    use super::*;
    use crate::db::AppState;
    use crate::workitems::{self, ItemInput};

    fn item(title: &str, estimate: i64) -> ItemInput {
        ItemInput {
            kind: None,
            title: title.into(),
            body: None,
            status: None,
            priority: None,
            due_ms: None,
            start_ms: None,
            estimate_min: Some(estimate),
            parent_id: None,
            rrule: None,
            subject_id: None,
            topic_id: None,
            goal_id: None,
        }
    }

    #[test]
    fn plan_vs_actual_counts_done_and_fills_empty_days() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();

        let a = workitems::create_item(&c, &item("done task", 60)).unwrap();
        let b = workitems::create_item(&c, &item("skipped task", 60)).unwrap();
        let ba = workitems::upsert_block(&c, None, &a.id, "2026-03-10", 540, 600, None).unwrap();
        workitems::upsert_block(&c, None, &b.id, "2026-03-10", 600, 660, None).unwrap();
        complete_block(&c, &ba.id).unwrap();

        let out = plan_vs_actual(&c, "2026-03-10", 3).unwrap();
        assert_eq!(out.days.len(), 3, "gaps must still produce a column");
        assert_eq!(out.days[0].planned_min, 120);
        assert_eq!(out.days[0].done_min, 60);
        assert_eq!(out.days[0].planned_blocks, 2);
        assert_eq!(out.days[0].done_blocks, 1);
        assert_eq!(out.days[1].planned_min, 0);
        assert_eq!(out.planned_min, 120);
        assert_eq!(out.done_min, 60);
        assert!((out.adherence - 0.5).abs() < 1e-9);
        // Completing the block also wrote 60 minutes of actual time.
        assert_eq!(out.actual_min, 60);
    }

    #[test]
    fn adherence_is_zero_when_nothing_was_planned() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let out = plan_vs_actual(&c, "2026-03-10", 7).unwrap();
        assert_eq!(out.adherence, 0.0, "no plan must not read as 100%");
        assert_eq!(out.planned_min, 0);
    }
}

#[cfg(test)]
mod goal_tests {
    use super::*;
    use crate::db::AppState;
    use crate::workitems::{self, GoalInput};

    fn goal_input(title: &str, subject: Option<String>) -> GoalInput {
        GoalInput {
            id: None,
            title: title.into(),
            kind: Some("exam".into()),
            metric: Some("score".into()),
            target: Some(85.0),
            current: Some(50.0),
            deadline_ms: Some(workitems::parse_day("2026-04-10").unwrap() * 86_400_000),
            subject_id: subject,
            status: None,
        }
    }

    #[test]
    fn goal_without_subject_becomes_one_milestone_and_is_idempotent() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let goal = workitems::upsert_goal(&c, &goal_input("读完 20 篇论文", None)).unwrap();

        let req = GoalPlanRequest {
            goal_id: goal.id.clone(),
            start_day: "2026-03-10".into(),
            daily_minutes: Some(120),
        };
        let first = materialize_goal(&c, &req).unwrap();
        assert_eq!(first.created, 1);
        assert_eq!(first.titles[0], "推进目标：读完 20 篇论文");

        // Running it again must not pile up duplicate tasks.
        let second = materialize_goal(&c, &req).unwrap();
        assert_eq!(second.created, 0);
        assert_eq!(second.skipped, 1);

        let items = workitems::list_items(
            &c,
            &ItemQuery {
                goal_id: Some(goal.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "milestone");
    }

    #[test]
    fn goal_items_are_then_schedulable() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let goal = workitems::upsert_goal(&c, &goal_input("过线", None)).unwrap();
        materialize_goal(
            &c,
            &GoalPlanRequest {
                goal_id: goal.id.clone(),
                start_day: "2026-03-10".into(),
                daily_minutes: Some(120),
            },
        )
        .unwrap();

        let out = plan_week(
            &c,
            &PlanRequest {
                start_day: "2026-03-10".into(),
                days: Some(7),
                daily_minutes: Some(120),
                day_start_min: Some(540),
                day_end_min: Some(1320),
                break_min: Some(10),
                replace: Some(true),
            },
        )
        .unwrap();
        assert_eq!(out.blocks.len(), 1, "the milestone should reach the calendar");
    }
}

// -------------------------------------------------- D4: calendar write-back

/// One block joined with its item title, shaped for the calendar write-back.
#[derive(Debug, Clone)]
pub struct BlockSyncRow {
    pub id: String,
    pub day: String,
    pub start_min: i64,
    pub end_min: i64,
    pub status: String,
    pub title: String,
    pub updated_at: i64,
}

/// Planned and terminal blocks in a day range — the calendar sync reconciles
/// all of them: planned → create/update, done/skipped/moved → remove the slot.
pub fn blocks_for_sync(
    conn: &Connection,
    from_day: &str,
    to_day: &str,
) -> Result<Vec<BlockSyncRow>> {
    let mut st = conn.prepare(
        "SELECT b.id, b.day, b.start_min, b.end_min, b.status, b.updated_at, w.title \
         FROM plan_blocks b JOIN work_items w ON w.id = b.item_id \
         WHERE b.day >= ?1 AND b.day <= ?2 \
         ORDER BY b.day, b.start_min",
    )?;
    let rows = st
        .query_map([from_day, to_day], |r| {
            Ok(BlockSyncRow {
                id: r.get(0)?,
                day: r.get(1)?,
                start_min: r.get(2)?,
                end_min: r.get(3)?,
                status: r.get(4)?,
                updated_at: r.get(5)?,
                title: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// The Google event id a block was pushed as, if any.
pub fn mapping_event_id(conn: &Connection, block_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT google_event_id FROM plan_block_events WHERE block_id = ?1",
            [block_id],
            |r| r.get::<_, String>(0),
        )
        .ok())
}

/// What the last push wrote to Google, for cheap change detection.
#[derive(Debug, Clone)]
pub struct MappingSnapshot {
    pub event_id: String,
    pub synced_title: Option<String>,
    pub synced_start: Option<i64>,
    pub synced_end: Option<i64>,
}

pub fn mapping_snapshot(conn: &Connection, block_id: &str) -> Result<Option<MappingSnapshot>> {
    conn.query_row(
        "SELECT google_event_id, synced_title, synced_start, synced_end \
         FROM plan_block_events WHERE block_id = ?1",
        [block_id],
        |r| {
            Ok(MappingSnapshot {
                event_id: r.get(0)?,
                synced_title: r.get(1)?,
                synced_start: r.get(2)?,
                synced_end: r.get(3)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

pub fn upsert_mapping(
    conn: &Connection,
    block_id: &str,
    event_id: &str,
    synced_title: &str,
    synced_start: i64,
    synced_end: i64,
) -> Result<()> {
    let now = crate::db::now_ms();
    conn.execute(
        "INSERT INTO plan_block_events (id, block_id, google_event_id, synced_title, \
            synced_start, synced_end, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7) \
         ON CONFLICT (block_id) DO UPDATE SET google_event_id = ?3, synced_title = ?4, \
            synced_start = ?5, synced_end = ?6, updated_at = ?7",
        params![
            crate::db::new_id(),
            block_id,
            event_id,
            synced_title,
            synced_start,
            synced_end,
            now
        ],
    )?;
    Ok(())
}

pub fn delete_mapping(conn: &Connection, block_id: &str) -> Result<()> {
    conn.execute("DELETE FROM plan_block_events WHERE block_id = ?1", [block_id])?;
    Ok(())
}

#[cfg(test)]
mod sync_tests {
    use super::*;

    #[test]
    fn mapping_roundtrips_and_deletes() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        assert!(mapping_event_id(&c, "b1").unwrap().is_none());
        upsert_mapping(&c, "b1", "evt-1", "读论文", 1_000, 2_000).unwrap();
        assert_eq!(mapping_event_id(&c, "b1").unwrap().as_deref(), Some("evt-1"));
        // Upsert same block → replaces, never duplicates (UNIQUE block_id).
        upsert_mapping(&c, "b1", "evt-2", "读论文", 3_000, 4_000).unwrap();
        let snap = mapping_snapshot(&c, "b1").unwrap().unwrap();
        assert_eq!(snap.event_id, "evt-2");
        assert_eq!(snap.synced_start, Some(3_000));
        delete_mapping(&c, "b1").unwrap();
        assert!(mapping_snapshot(&c, "b1").unwrap().is_none());
    }

    #[test]
    fn blocks_for_sync_joins_titles_and_filters_the_range() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let item = workitems::create_item(
            &c,
            &workitems::ItemInput {
                kind: Some("task".into()),
                title: "复习特征值".into(),
                body: None,
                status: None,
                priority: Some("high".into()),
                due_ms: None,
                start_ms: None,
                estimate_min: Some(45),
                parent_id: None,
                rrule: None,
                subject_id: None,
                topic_id: None,
                goal_id: None,
            },
        )
        .unwrap();
        conn_execute_block(&c, &item.id, "2026-10-19", 540, 585);
        conn_execute_block(&c, &item.id, "2026-10-20", 540, 585);
        let rows = blocks_for_sync(&c, "2026-10-19", "2026-10-19").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "复习特征值");
        assert_eq!(rows[0].start_min, 540);
    }

    fn conn_execute_block(c: &Connection, item_id: &str, day: &str, s: i64, e: i64) {
        c.execute(
            "INSERT INTO plan_blocks (id, item_id, day, start_min, end_min, status, origin, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, 'planned', 'manual', 1, 1)",
            rusqlite::params![crate::db::new_id(), item_id, day, s, e],
        )
        .unwrap();
    }
}
