//! Unified cross-subject work items — the spine that makes the five modes one
//! platform instead of five apps.
//!
//! Everything before this module hung off a subject: sources, topics, notes,
//! materials, cards, events, citations. That models *study* well and *planning*
//! badly — a shopping item belongs to no subject, a paper may span three, and a
//! schedule is a timeline rather than a tree. `work_items` fixes that by making
//! the unit of work first-class and every attachment optional:
//!
//!   subject_id / topic_id / goal_id  → all nullable (this is what buys
//!                                      cross-subject reach)
//!   parent_id                        → projects and subtasks
//!   rrule                            → "every day", "Mon/Wed/Fri"
//!   item_links                       → sources / notes / citations / wiki / papers
//!
//! Capture is deliberately dumb: `status='inbox'` with no subject, no date and
//! no parent. Triage happens later, so nothing blocks the two-second capture
//! that a daily-use app lives or dies on.
//!
//! Recurrence is a small RFC 5545 subset (FREQ + INTERVAL + BYDAY + BYMONTHDAY
//! + UNTIL + COUNT) expanded on UTC day boundaries. Day arithmetic preserves the
//! anchor's time of day exactly; across a DST change an occurrence can therefore
//! land an hour off wall-clock. That is acceptable for study planning and is the
//! price of not pulling in a full calendar-timezone crate.

use crate::db::{new_id, now_ms};
use crate::error::{Error, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

const DAY_MS: i64 = 86_400_000;

const KINDS: &[&str] = &[
    "task", "reading", "writing", "question", "excerpt", "milestone", "material",
];
const STATUSES: &[&str] = &["inbox", "open", "doing", "blocked", "done", "dropped"];
const PRIORITIES: &[&str] = &["low", "med", "high", "urgent"];

// ---------------------------------------------------------------- models

#[derive(Debug, Clone, Serialize)]
pub struct WorkItem {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub body: Option<String>,
    pub status: String,
    pub priority: Option<String>,
    pub due_ms: Option<i64>,
    pub start_ms: Option<i64>,
    pub estimate_min: Option<i64>,
    pub actual_min: i64,
    pub parent_id: Option<String>,
    pub rrule: Option<String>,
    pub subject_id: Option<String>,
    pub topic_id: Option<String>,
    pub goal_id: Option<String>,
    pub sort_order: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub done_at: Option<i64>,
    /// Derived: how many direct children exist, and how many are still open.
    /// Lets the UI render "3/7 done" on a project row without a second query.
    pub child_count: i64,
    pub open_child_count: i64,
    /// Derived: the next occurrence for a recurring item, when one falls after
    /// `now`. None for one-shot items.
    pub next_due_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ItemLink {
    pub item_id: String,
    pub entity_table: String,
    pub entity_id: String,
    pub role: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Goal {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub metric: Option<String>,
    pub target: Option<f64>,
    pub current: f64,
    pub deadline_ms: Option<i64>,
    pub subject_id: Option<String>,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// Derived: how many work items are attached and how many are done.
    pub item_count: i64,
    pub done_item_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanBlock {
    pub id: String,
    pub item_id: String,
    pub day: String,
    pub start_min: i64,
    pub end_min: i64,
    pub status: String,
    pub origin: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// The Today cockpit payload. One round-trip for the whole home screen so the
/// first paint never waits on four sequential queries.
#[derive(Debug, Clone, Serialize, Default)]
pub struct TodayBoard {
    pub overdue: Vec<WorkItem>,
    pub today: Vec<WorkItem>,
    pub inbox: Vec<WorkItem>,
    pub blocks: Vec<PlanBlock>,
    pub counts: TodayCounts,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct TodayCounts {
    pub overdue: i64,
    pub due_today: i64,
    pub inbox: i64,
    pub done_today: i64,
    pub open: i64,
    /// Sum of estimates on today's items — the plan's intended load.
    pub planned_minutes: i64,
    /// Sum of actual minutes logged today (pomodoro writes `actual_min`).
    pub done_minutes: i64,
}

// ----------------------------------------------------------------- input

#[derive(Debug, Clone, Deserialize)]
pub struct ItemInput {
    pub kind: Option<String>,
    pub title: String,
    pub body: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub due_ms: Option<i64>,
    pub start_ms: Option<i64>,
    pub estimate_min: Option<i64>,
    pub parent_id: Option<String>,
    pub rrule: Option<String>,
    pub subject_id: Option<String>,
    pub topic_id: Option<String>,
    pub goal_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ItemQuery {
    /// "inbox" | "active" (open+doing+blocked) | a concrete status | "all"
    pub status: Option<String>,
    pub kind: Option<String>,
    pub subject_id: Option<String>,
    pub goal_id: Option<String>,
    pub parent_id: Option<String>,
    /// Only items due strictly before this instant (used by the overdue lane).
    pub due_before_ms: Option<i64>,
    pub search: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GoalInput {
    pub id: Option<String>,
    pub title: String,
    pub kind: Option<String>,
    pub metric: Option<String>,
    pub target: Option<f64>,
    pub current: Option<f64>,
    pub deadline_ms: Option<i64>,
    pub subject_id: Option<String>,
    pub status: Option<String>,
}

// ------------------------------------------------------------ validation

fn check(value: &str, allowed: &[&str], field: &str) -> Result<()> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(Error::Other(format!(
            "invalid {field} '{value}' (expected one of: {})",
            allowed.join(", ")
        )))
    }
}

fn validate(input: &ItemInput) -> Result<()> {
    if input.title.trim().is_empty() {
        return Err(Error::Other("work item title cannot be empty".into()));
    }
    if let Some(k) = &input.kind {
        check(k, KINDS, "kind")?;
    }
    if let Some(s) = &input.status {
        check(s, STATUSES, "status")?;
    }
    if let Some(p) = &input.priority {
        check(p, PRIORITIES, "priority")?;
    }
    if let Some(r) = &input.rrule {
        // Parse eagerly so a typo is rejected at save time, not silently at
        // render time when the item just never shows up.
        if !r.trim().is_empty() {
            parse_rrule(r)?;
        }
    }
    if let (Some(est), Some(start)) = (input.estimate_min, input.start_ms) {
        let _ = (est, start); // no constraint today; kept for future validation
    }
    Ok(())
}

// ---------------------------------------------------------------- rrule

#[derive(Debug, Clone, Copy, PartialEq)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
}

#[derive(Debug, Clone)]
struct Rrule {
    freq: Freq,
    interval: i64,
    /// Monday = 0 … Sunday = 6. Empty = anchor's weekday (WEEKLY).
    byday: Vec<u32>,
    /// 1..=31. Empty = anchor's day-of-month (MONTHLY).
    bymonthday: Vec<u32>,
    until_ms: Option<i64>,
    count: Option<i64>,
}

/// Parse `FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;UNTIL=20261231`.
/// Unknown parts are ignored rather than fatal — a rule written by another tool
/// should degrade, not break the item.
fn parse_rrule(raw: &str) -> Result<Rrule> {
    let mut freq = None;
    let mut interval = 1i64;
    let mut byday = Vec::new();
    let mut bymonthday = Vec::new();
    let mut until_ms = None;
    let mut count = None;

    for part in raw.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        let (key, value) = match part.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        let key = key.trim().to_ascii_uppercase();
        let value = value.trim();
        match key.as_str() {
            "FREQ" => {
                freq = Some(match value.to_ascii_uppercase().as_str() {
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    other => {
                        return Err(Error::Other(format!(
                            "unsupported RRULE FREQ '{other}' (daily, weekly, monthly)"
                        )))
                    }
                })
            }
            "INTERVAL" => {
                interval = value
                    .parse::<i64>()
                    .map_err(|_| Error::Other(format!("invalid RRULE INTERVAL '{value}'")))?
                    .max(1)
            }
            "BYDAY" => {
                for d in value.split(',') {
                    let d = d.trim().to_ascii_uppercase();
                    // Strip an ordinal prefix (1MO / -1FR) — we expand weekly
                    // only, so the ordinal is meaningless here.
                    let d = d.trim_start_matches(['+', '-', '0', '1', '2', '3', '4', '5', '9']);
                    let idx = match d {
                        "MO" => 0,
                        "TU" => 1,
                        "WE" => 2,
                        "TH" => 3,
                        "FR" => 4,
                        "SA" => 5,
                        "SU" => 6,
                        _ => continue,
                    };
                    byday.push(idx);
                }
            }
            "BYMONTHDAY" => {
                for d in value.split(',') {
                    if let Ok(n) = d.trim().parse::<u32>() {
                        if (1..=31).contains(&n) {
                            bymonthday.push(n);
                        }
                    }
                }
            }
            "UNTIL" => {
                until_ms = parse_until(value);
            }
            "COUNT" => {
                count = value.parse::<i64>().ok().filter(|n| *n > 0);
            }
            _ => {}
        }
    }

    let freq = freq.ok_or_else(|| Error::Other("RRULE needs a FREQ part".into()))?;
    Ok(Rrule {
        freq,
        interval,
        byday,
        bymonthday,
        until_ms,
        count,
    })
}

/// `20261231` or `20261231T235959Z` → epoch ms (UTC midnight / the given time).
fn parse_until(value: &str) -> Option<i64> {
    let digits: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 8 {
        return None;
    }
    let year: i64 = digits[0..4].parse().ok()?;
    let month: i64 = digits[4..6].parse().ok()?;
    let day: i64 = digits[6..8].parse().ok()?;
    let (mut hour, mut minute, mut second) = (23i64, 59i64, 59i64);
    if digits.len() >= 14 {
        hour = digits[8..10].parse().ok()?;
        minute = digits[10..12].parse().ok()?;
        second = digits[12..14].parse().ok()?;
    }
    Some(days_from_civil(year, month, day) * DAY_MS + (hour * 3600 + minute * 60 + second) * 1000)
}

/// Days since the Unix epoch for a proleptic-Gregorian date (Hinnant's
/// days_from_civil). Kept local so the module has no date-crate dependency.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// (year, month, day) for a day index (Hinnant's civil_from_days).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn day_index(ms: i64) -> i64 {
    ms.div_euclid(DAY_MS)
}

/// Parse a `YYYY-MM-DD` local-date key into a day index. The planner and the
/// Today board both speak this format, and it is what `plan_blocks.day` stores —
/// a string, not an instant, so a device in another timezone still reads its own
/// "today".
pub fn parse_day(day: &str) -> Option<i64> {
    let mut it = day.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d))
}

/// Inverse of [`parse_day`].
pub fn format_day(idx: i64) -> String {
    let (y, m, d) = civil_from_days(idx);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Monday = 0 … Sunday = 6 (1970-01-01 was a Thursday).
fn weekday(day: i64) -> u32 {
    ((day + 3).rem_euclid(7)) as u32
}

/// Expand a rule into concrete instants inside `[from_ms, to_ms]`.
///
/// The anchor's time-of-day is preserved by construction: every occurrence is
/// `anchor + k * DAY_MS`, so we never reconstruct a wall-clock time.
pub fn rrule_occurrences(raw: &str, anchor_ms: i64, from_ms: i64, to_ms: i64) -> Vec<i64> {
    let Ok(rule) = parse_rrule(raw) else {
        return Vec::new();
    };
    if to_ms < anchor_ms {
        return Vec::new();
    }
    let anchor_day = day_index(anchor_ms);
    let from_day = day_index(from_ms);
    let to_day = day_index(to_ms).min(anchor_day + 3660); // 10-year guard
    let (_, _, anchor_dom) = civil_from_days(anchor_day);
    let anchor_wd = weekday(anchor_day);
    let anchor_month_idx = {
        let (y, m, _) = civil_from_days(anchor_day);
        y * 12 + m
    };

    let mut out = Vec::new();
    let mut seen = 0i64;
    for day in anchor_day..=to_day {
        let hit = match rule.freq {
            Freq::Daily => (day - anchor_day) % rule.interval == 0,
            Freq::Weekly => {
                let weeks = (day - anchor_day).div_euclid(7);
                let days = if rule.byday.is_empty() {
                    weekday(day) == anchor_wd
                } else {
                    rule.byday.contains(&weekday(day))
                };
                weeks % rule.interval == 0 && days
            }
            Freq::Monthly => {
                let (y, m, d) = civil_from_days(day);
                let months = (y * 12 + m) - anchor_month_idx;
                let dom_ok = if rule.bymonthday.is_empty() {
                    d == anchor_dom
                } else {
                    rule.bymonthday.contains(&(d as u32))
                };
                months % rule.interval == 0 && dom_ok
            }
        };
        if !hit {
            continue;
        }
        seen += 1;
        if let Some(n) = rule.count {
            if seen > n {
                break;
            }
        }
        let at = anchor_ms + (day - anchor_day) * DAY_MS;
        if let Some(until) = rule.until_ms {
            if at > until {
                break;
            }
        }
        if day >= from_day && at >= from_ms && at <= to_ms {
            out.push(at);
        }
    }
    out
}

/// Next occurrence strictly after `after_ms`, searching one year ahead.
fn next_occurrence(raw: &str, anchor_ms: i64, after_ms: i64) -> Option<i64> {
    rrule_occurrences(raw, anchor_ms, after_ms + 1, after_ms + 366 * DAY_MS)
        .into_iter()
        .next()
}

// ------------------------------------------------------------- row mapping

const ITEM_COLUMNS: &str = "w.id, w.kind, w.title, w.body, w.status, w.priority, \
     w.due_ms, w.start_ms, w.estimate_min, w.actual_min, w.parent_id, w.rrule, \
     w.subject_id, w.topic_id, w.goal_id, w.sort_order, w.created_at, w.updated_at, \
     w.done_at, \
     (SELECT COUNT(*) FROM work_items c WHERE c.parent_id = w.id), \
     (SELECT COUNT(*) FROM work_items c WHERE c.parent_id = w.id \
        AND c.status IN ('open','doing','blocked'))";

fn row_to_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkItem> {
    Ok(WorkItem {
        id: row.get(0)?,
        kind: row.get(1)?,
        title: row.get(2)?,
        body: row.get(3)?,
        status: row.get(4)?,
        priority: row.get(5)?,
        due_ms: row.get(6)?,
        start_ms: row.get(7)?,
        estimate_min: row.get(8)?,
        actual_min: row.get(9)?,
        parent_id: row.get(10)?,
        rrule: row.get(11)?,
        subject_id: row.get(12)?,
        topic_id: row.get(13)?,
        goal_id: row.get(14)?,
        sort_order: row.get(15)?,
        created_at: row.get(16)?,
        updated_at: row.get(17)?,
        done_at: row.get(18)?,
        child_count: row.get(19)?,
        open_child_count: row.get(20)?,
        next_due_ms: None,
    })
}

fn finish(mut items: Vec<WorkItem>, now: i64) -> Vec<WorkItem> {
    for it in &mut items {
        if let Some(rule) = &it.rrule {
            if !rule.trim().is_empty() {
                let anchor = it.start_ms.or(it.due_ms).unwrap_or(it.created_at);
                it.next_due_ms = next_occurrence(rule, anchor, now);
            }
        }
    }
    items
}

// ------------------------------------------------------------------- CRUD

pub fn get_item(conn: &Connection, id: &str) -> Result<WorkItem> {
    let sql = format!("SELECT {ITEM_COLUMNS} FROM work_items w WHERE w.id = ?1");
    let mut item = conn
        .query_row(&sql, params![id], row_to_item)
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("work item {id}")),
            other => Error::Db(other),
        })?;
    let now = now_ms();
    if let Some(rule) = &item.rrule {
        if !rule.trim().is_empty() {
            let anchor = item.start_ms.or(item.due_ms).unwrap_or(item.created_at);
            item.next_due_ms = next_occurrence(rule, anchor, now);
        }
    }
    Ok(item)
}

pub fn list_items(conn: &Connection, q: &ItemQuery) -> Result<Vec<WorkItem>> {
    let mut sql = format!("SELECT {ITEM_COLUMNS} FROM work_items w WHERE 1=1");
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    match q.status.as_deref() {
        Some("all") | None => {}
        Some("active") => sql.push_str(" AND w.status IN ('open','doing','blocked')"),
        Some(s) => {
            check(s, STATUSES, "status")?;
            sql.push_str(" AND w.status = ?");
            args.push(Box::new(s.to_string()));
        }
    }
    if let Some(k) = &q.kind {
        check(k, KINDS, "kind")?;
        sql.push_str(" AND w.kind = ?");
        args.push(Box::new(k.clone()));
    }
    if let Some(s) = &q.subject_id {
        sql.push_str(" AND w.subject_id = ?");
        args.push(Box::new(s.clone()));
    }
    if let Some(g) = &q.goal_id {
        sql.push_str(" AND w.goal_id = ?");
        args.push(Box::new(g.clone()));
    }
    if let Some(p) = &q.parent_id {
        sql.push_str(" AND w.parent_id = ?");
        args.push(Box::new(p.clone()));
    }
    if let Some(d) = q.due_before_ms {
        sql.push_str(" AND w.due_ms IS NOT NULL AND w.due_ms < ?");
        args.push(Box::new(d));
    }
    if let Some(term) = &q.search {
        let like = format!("%{}%", term.trim());
        sql.push_str(" AND (w.title LIKE ? OR w.body LIKE ?)");
        args.push(Box::new(like.clone()));
        args.push(Box::new(like));
    }
    // Open work first, then by due date (undated last), then manual order.
    sql.push_str(
        " ORDER BY CASE w.status WHEN 'doing' THEN 0 WHEN 'open' THEN 1 \
         WHEN 'inbox' THEN 2 WHEN 'blocked' THEN 3 WHEN 'done' THEN 4 ELSE 5 END, \
         w.due_ms IS NULL, w.due_ms, w.sort_order, w.created_at DESC",
    );
    let limit = q.limit.unwrap_or(500).clamp(1, 2000);
    sql.push_str(&format!(" LIMIT {limit}"));

    let mut st = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let rows = st.query_map(refs.as_slice(), row_to_item)?;
    let items: Vec<WorkItem> = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(finish(items, now_ms()))
}

pub fn create_item(conn: &Connection, input: &ItemInput) -> Result<WorkItem> {
    validate(input)?;
    let id = new_id();
    let now = now_ms();
    let status = input.status.clone().unwrap_or_else(|| "open".into());
    // Captured-but-not-triaged items get an explicit done stamp only when they
    // are created already finished (rare, but keeps the invariant true).
    let done_at = if status == "done" { Some(now) } else { None };
    conn.execute(
        "INSERT INTO work_items \
         (id, kind, title, body, status, priority, due_ms, start_ms, estimate_min, \
          actual_min, parent_id, rrule, subject_id, topic_id, goal_id, sort_order, \
          created_at, updated_at, done_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,0,?10,?11,?12,?13,?14,0,?15,?15,?16)",
        params![
            id,
            input.kind.clone().unwrap_or_else(|| "task".into()),
            input.title.trim(),
            input.body,
            status,
            input.priority,
            input.due_ms,
            input.start_ms,
            input.estimate_min,
            input.parent_id,
            input.rrule,
            input.subject_id,
            input.topic_id,
            input.goal_id,
            now,
            done_at,
        ],
    )?;
    get_item(conn, &id)
}

pub fn update_item(conn: &Connection, id: &str, input: &ItemInput) -> Result<WorkItem> {
    validate(input)?;
    let existing = get_item(conn, id)?;
    let now = now_ms();
    let status = input.status.clone().unwrap_or(existing.status);
    // Keep done_at consistent with status: stamp on transition in, clear on out.
    let done_at = match (status.as_str(), existing.done_at) {
        ("done", Some(prev)) => Some(prev),
        ("done", None) => Some(now),
        _ => None,
    };
    let changed = conn.execute(
        "UPDATE work_items SET kind=?2, title=?3, body=?4, status=?5, priority=?6, \
         due_ms=?7, start_ms=?8, estimate_min=?9, parent_id=?10, rrule=?11, \
         subject_id=?12, topic_id=?13, goal_id=?14, updated_at=?15, done_at=?16 \
         WHERE id=?1",
        params![
            id,
            input.kind.clone().unwrap_or_else(|| "task".into()),
            input.title.trim(),
            input.body,
            status,
            input.priority,
            input.due_ms,
            input.start_ms,
            input.estimate_min,
            input.parent_id,
            input.rrule,
            input.subject_id,
            input.topic_id,
            input.goal_id,
            now,
            done_at,
        ],
    )?;
    if changed == 0 {
        return Err(Error::NotFound(format!("work item {id}")));
    }
    get_item(conn, id)
}

pub fn set_status(conn: &Connection, id: &str, status: &str) -> Result<WorkItem> {
    check(status, STATUSES, "status")?;
    let now = now_ms();
    let done_at = if status == "done" { Some(now) } else { None };
    let changed = conn.execute(
        "UPDATE work_items SET status=?2, done_at=?3, updated_at=?4 WHERE id=?1",
        params![id, status, done_at, now],
    )?;
    if changed == 0 {
        return Err(Error::NotFound(format!("work item {id}")));
    }
    get_item(conn, id)
}

/// Log time against an item (pomodoro writes here). Additive by design so a
/// session that is cancelled halfway still records what actually happened.
pub fn add_actual_minutes(conn: &Connection, id: &str, minutes: i64) -> Result<WorkItem> {
    let now = now_ms();
    let changed = conn.execute(
        "UPDATE work_items SET actual_min = actual_min + ?2, updated_at=?3 WHERE id=?1",
        params![id, minutes.max(0), now],
    )?;
    if changed == 0 {
        return Err(Error::NotFound(format!("work item {id}")));
    }
    get_item(conn, id)
}

/// Hard delete; children cascade and a tombstone records the intent so the next
/// sync merge cannot resurrect the row.
pub fn delete_item(conn: &Connection, id: &str) -> Result<()> {
    let changed = conn.execute("DELETE FROM work_items WHERE id = ?1", params![id])?;
    if changed == 0 {
        return Err(Error::NotFound(format!("work item {id}")));
    }
    Ok(())
}

// ------------------------------------------------------------------ links

pub fn link(conn: &Connection, item_id: &str, table: &str, entity_id: &str, role: &str) -> Result<()> {
    // Fail loudly on a dangling item id instead of writing an orphan edge.
    get_item(conn, item_id)?;
    conn.execute(
        "INSERT OR REPLACE INTO item_links (item_id, entity_table, entity_id, role, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![item_id, table, entity_id, role, now_ms()],
    )?;
    Ok(())
}

pub fn unlink(conn: &Connection, item_id: &str, table: &str, entity_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM item_links WHERE item_id=?1 AND entity_table=?2 AND entity_id=?3",
        params![item_id, table, entity_id],
    )?;
    Ok(())
}

pub fn links(conn: &Connection, item_id: &str) -> Result<Vec<ItemLink>> {
    let mut st = conn.prepare(
        "SELECT item_id, entity_table, entity_id, role, created_at \
         FROM item_links WHERE item_id=?1 ORDER BY created_at",
    )?;
    let rows = st.query_map(params![item_id], |r| {
        Ok(ItemLink {
            item_id: r.get(0)?,
            entity_table: r.get(1)?,
            entity_id: r.get(2)?,
            role: r.get(3)?,
            created_at: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Reverse lookup: which work items reference this entity? Used by the source /
/// paper views to show "3 tasks mention this".
pub fn items_for_entity(conn: &Connection, table: &str, entity_id: &str) -> Result<Vec<WorkItem>> {
    let sql = format!(
        "SELECT {ITEM_COLUMNS} FROM work_items w \
         JOIN item_links l ON l.item_id = w.id \
         WHERE l.entity_table = ?1 AND l.entity_id = ?2 \
         ORDER BY w.due_ms IS NULL, w.due_ms"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params![table, entity_id], row_to_item)?;
    let items: Vec<WorkItem> = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(finish(items, now_ms()))
}

// ------------------------------------------------------------------ goals

fn row_to_goal(row: &rusqlite::Row<'_>) -> rusqlite::Result<Goal> {
    Ok(Goal {
        id: row.get(0)?,
        title: row.get(1)?,
        kind: row.get(2)?,
        metric: row.get(3)?,
        target: row.get(4)?,
        current: row.get(5)?,
        deadline_ms: row.get(6)?,
        subject_id: row.get(7)?,
        status: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
        item_count: row.get(11)?,
        done_item_count: row.get(12)?,
    })
}

const GOAL_COLUMNS: &str = "g.id, g.title, g.kind, g.metric, g.target, g.current, \
     g.deadline_ms, g.subject_id, g.status, g.created_at, g.updated_at, \
     (SELECT COUNT(*) FROM work_items w WHERE w.goal_id = g.id), \
     (SELECT COUNT(*) FROM work_items w WHERE w.goal_id = g.id AND w.status = 'done')";

pub fn list_goals(conn: &Connection) -> Result<Vec<Goal>> {
    let sql = format!(
        "SELECT {GOAL_COLUMNS} FROM goals g \
         ORDER BY CASE g.status WHEN 'active' THEN 0 ELSE 1 END, \
         g.deadline_ms IS NULL, g.deadline_ms"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map([], row_to_goal)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn get_goal(conn: &Connection, id: &str) -> Result<Goal> {
    let sql = format!("SELECT {GOAL_COLUMNS} FROM goals g WHERE g.id = ?1");
    conn.query_row(&sql, params![id], row_to_goal).map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("goal {id}")),
        other => Error::Db(other),
    })
}

pub fn upsert_goal(conn: &Connection, input: &GoalInput) -> Result<Goal> {
    if input.title.trim().is_empty() {
        return Err(Error::Other("goal title cannot be empty".into()));
    }
    let now = now_ms();
    let id = match &input.id {
        Some(existing) => {
            let changed = conn.execute(
                "UPDATE goals SET title=?2, kind=?3, metric=?4, target=?5, current=?6, \
                 deadline_ms=?7, subject_id=?8, status=?9, updated_at=?10 WHERE id=?1",
                params![
                    existing,
                    input.title.trim(),
                    input.kind.clone().unwrap_or_else(|| "exam".into()),
                    input.metric,
                    input.target,
                    input.current.unwrap_or(0.0),
                    input.deadline_ms,
                    input.subject_id,
                    input.status.clone().unwrap_or_else(|| "active".into()),
                    now,
                ],
            )?;
            if changed == 0 {
                return Err(Error::NotFound(format!("goal {existing}")));
            }
            existing.clone()
        }
        None => {
            let new = new_id();
            conn.execute(
                "INSERT INTO goals (id, title, kind, metric, target, current, deadline_ms, \
                 subject_id, status, created_at, updated_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)",
                params![
                    new,
                    input.title.trim(),
                    input.kind.clone().unwrap_or_else(|| "exam".into()),
                    input.metric,
                    input.target,
                    input.current.unwrap_or(0.0),
                    input.deadline_ms,
                    input.subject_id,
                    input.status.clone().unwrap_or_else(|| "active".into()),
                    now,
                ],
            )?;
            new
        }
    };
    let sql = format!("SELECT {GOAL_COLUMNS} FROM goals g WHERE g.id = ?1");
    Ok(conn.query_row(&sql, params![id], row_to_goal)?)
}

pub fn delete_goal(conn: &Connection, id: &str) -> Result<()> {
    // work_items.goal_id is ON DELETE SET NULL, so attached items survive and
    // simply lose their anchor — deleting a goal must never delete work.
    conn.execute("DELETE FROM goals WHERE id = ?1", params![id])?;
    Ok(())
}

// ------------------------------------------------------------ plan blocks

fn row_to_block(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlanBlock> {
    Ok(PlanBlock {
        id: row.get(0)?,
        item_id: row.get(1)?,
        day: row.get(2)?,
        start_min: row.get(3)?,
        end_min: row.get(4)?,
        status: row.get(5)?,
        origin: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

const BLOCK_COLUMNS: &str = "id, item_id, day, start_min, end_min, status, origin, \
     created_at, updated_at";

pub fn list_blocks(conn: &Connection, day: &str) -> Result<Vec<PlanBlock>> {
    let sql = format!("SELECT {BLOCK_COLUMNS} FROM plan_blocks WHERE day = ?1 ORDER BY start_min");
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params![day], row_to_block)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Insert a block, or move/resize an existing one (`id` set). Overlap is not
/// rejected — the planner deliberately allows a soft clash the UI can show,
/// because refusing to save is worse than a warning.
pub fn upsert_block(
    conn: &Connection,
    id: Option<&str>,
    item_id: &str,
    day: &str,
    start_min: i64,
    end_min: i64,
    status: Option<&str>,
) -> Result<PlanBlock> {
    if end_min <= start_min {
        return Err(Error::Other("block end must be after its start".into()));
    }
    let now = now_ms();
    let block_id = match id {
        Some(existing) => {
            let changed = conn.execute(
                "UPDATE plan_blocks SET item_id=?2, day=?3, start_min=?4, end_min=?5, \
                 status=?6, updated_at=?7 WHERE id=?1",
                params![
                    existing,
                    item_id,
                    day,
                    start_min,
                    end_min,
                    status.unwrap_or("planned"),
                    now
                ],
            )?;
            if changed == 0 {
                return Err(Error::NotFound(format!("plan block {existing}")));
            }
            existing.to_string()
        }
        None => {
            get_item(conn, item_id)?;
            let new = new_id();
            conn.execute(
                "INSERT INTO plan_blocks (id, item_id, day, start_min, end_min, status, \
                 origin, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,'manual',?7,?7)",
                params![
                    new,
                    item_id,
                    day,
                    start_min,
                    end_min,
                    status.unwrap_or("planned"),
                    now
                ],
            )?;
            new
        }
    };
    let sql = format!("SELECT {BLOCK_COLUMNS} FROM plan_blocks WHERE id = ?1");
    Ok(conn.query_row(&sql, params![block_id], row_to_block)?)
}

pub fn delete_block(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM plan_blocks WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn get_block(conn: &Connection, id: &str) -> Result<PlanBlock> {
    let sql = format!("SELECT {BLOCK_COLUMNS} FROM plan_blocks WHERE id = ?1");
    conn.query_row(&sql, params![id], row_to_block).map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("plan block {id}")),
        other => Error::Db(other),
    })
}

// ------------------------------------------------------------ today board

/// Everything the Today cockpit needs, in one pass.
///
/// `day_start_ms` / `day_end_ms` are computed by the frontend from the *local*
/// date, so "today" is the user's today regardless of the machine's timezone
/// database — the backend never guesses a timezone.
pub fn today_board(conn: &Connection, day_start_ms: i64, day_end_ms: i64, day: &str) -> Result<TodayBoard> {
    let mut overdue = list_items(
        conn,
        &ItemQuery {
            status: Some("active".into()),
            due_before_ms: Some(day_start_ms),
            limit: Some(200),
            ..Default::default()
        },
    )?;
    // A recurring item whose *next* occurrence is still ahead is not overdue
    // just because its anchor date has passed.
    overdue.retain(|i| {
        if i.rrule.as_deref().map(|r| !r.trim().is_empty()).unwrap_or(false) {
            i.next_due_ms.map(|n| n < day_start_ms).unwrap_or(false)
        } else {
            true
        }
    });

    // Due today, or recurring with an occurrence inside today's window.
    let candidates = list_items(
        conn,
        &ItemQuery {
            status: Some("active".into()),
            limit: Some(2000),
            ..Default::default()
        },
    )?;
    let mut today: Vec<WorkItem> = Vec::new();
    for it in candidates {
        let due_in_range = it.due_ms.map(|d| d >= day_start_ms && d < day_end_ms).unwrap_or(false);
        let recurs_today = it
            .rrule
            .as_deref()
            .filter(|r| !r.trim().is_empty())
            .map(|rule| {
                let anchor = it.start_ms.or(it.due_ms).unwrap_or(it.created_at);
                !rrule_occurrences(rule, anchor, day_start_ms, day_end_ms - 1).is_empty()
            })
            .unwrap_or(false);
        if due_in_range || recurs_today {
            today.push(it);
        }
    }
    today.sort_by_key(|i| (i.priority_rank(), i.due_ms.unwrap_or(i64::MAX)));

    let inbox = list_items(
        conn,
        &ItemQuery {
            status: Some("inbox".into()),
            limit: Some(50),
            ..Default::default()
        },
    )?;

    let blocks = list_blocks(conn, day)?;

    let count = |sql: &str, args: &[&dyn rusqlite::ToSql]| -> i64 {
        conn.query_row(sql, args, |r| r.get(0)).unwrap_or(0)
    };
    let open = count(
        "SELECT COUNT(*) FROM work_items WHERE status IN ('open','doing','blocked')",
        &[],
    );
    let done_today = count(
        "SELECT COUNT(*) FROM work_items WHERE status='done' AND done_at >= ?1 AND done_at < ?2",
        &[&day_start_ms, &day_end_ms],
    );
    let done_minutes = count(
        "SELECT COALESCE(SUM(actual_min),0) FROM work_items \
         WHERE done_at >= ?1 AND done_at < ?2",
        &[&day_start_ms, &day_end_ms],
    );
    let planned_minutes: i64 = today.iter().filter_map(|i| i.estimate_min).sum();

    let counts = TodayCounts {
        overdue: overdue.len() as i64,
        due_today: today.len() as i64,
        inbox: inbox.len() as i64,
        done_today,
        open,
        planned_minutes,
        done_minutes,
    };

    Ok(TodayBoard {
        overdue,
        today,
        inbox,
        blocks,
        counts,
    })
}

impl WorkItem {
    /// Sort key for the Today lane: urgent first, then high, then the rest.
    fn priority_rank(&self) -> u8 {
        match self.priority.as_deref() {
            Some("urgent") => 0,
            Some("high") => 1,
            Some("med") => 2,
            Some("low") => 4,
            _ => 3,
        }
    }
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::AppState;

    fn conn() -> AppState {
        AppState::in_memory().unwrap()
    }

    fn base(title: &str) -> ItemInput {
        ItemInput {
            kind: None,
            title: title.into(),
            body: None,
            status: None,
            priority: None,
            due_ms: None,
            start_ms: None,
            estimate_min: None,
            parent_id: None,
            rrule: None,
            subject_id: None,
            topic_id: None,
            goal_id: None,
        }
    }

    #[test]
    fn create_get_update_delete_roundtrip() {
        let st = conn();
        let c = st.db.lock().unwrap();

        let item = create_item(&c, &base("Buy milk")).unwrap();
        assert_eq!(item.status, "open");
        assert_eq!(item.kind, "task");

        let mut edit = base("Buy oat milk");
        edit.priority = Some("high".into());
        let updated = update_item(&c, &item.id, &edit).unwrap();
        assert_eq!(updated.title, "Buy oat milk");
        assert_eq!(updated.priority.as_deref(), Some("high"));

        let done = set_status(&c, &item.id, "done").unwrap();
        assert!(done.done_at.is_some());
        // Re-opening clears the stamp so "done today" counts stay honest.
        let reopened = set_status(&c, &item.id, "open").unwrap();
        assert!(reopened.done_at.is_none());

        delete_item(&c, &item.id).unwrap();
        assert!(get_item(&c, &item.id).is_err());
        // The tombstone trigger fired, so sync won't resurrect it.
        let tomb: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM tombstones WHERE entity_table='work_items' AND entity_id=?1",
                params![item.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tomb, 1);
    }

    #[test]
    fn rejects_bad_enum_values() {
        let st = conn();
        let c = st.db.lock().unwrap();
        let mut bad = base("x");
        bad.status = Some("nonsense".into());
        assert!(create_item(&c, &bad).is_err());

        let mut bad_rule = base("y");
        bad_rule.rrule = Some("FREQ=HOURLY".into());
        assert!(create_item(&c, &bad_rule).is_err());
    }

    #[test]
    fn hierarchy_counts_children() {
        let st = conn();
        let c = st.db.lock().unwrap();
        let parent = create_item(&c, &base("Thesis")).unwrap();

        for (i, status) in ["done", "open", "open"].iter().enumerate() {
            let mut child = base(&format!("chapter {i}"));
            child.parent_id = Some(parent.id.clone());
            child.status = Some((*status).to_string());
            create_item(&c, &child).unwrap();
        }

        let reloaded = get_item(&c, &parent.id).unwrap();
        assert_eq!(reloaded.child_count, 3);
        assert_eq!(reloaded.open_child_count, 2);

        // Deleting the parent cascades to children.
        delete_item(&c, &parent.id).unwrap();
        let left: i64 = c
            .query_row("SELECT COUNT(*) FROM work_items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn daily_rule_expands_with_interval() {
        // Anchor: 2026-01-01 09:00 UTC.
        let anchor = days_from_civil(2026, 1, 1) * DAY_MS + 9 * 3600_000;
        let from = days_from_civil(2026, 1, 1) * DAY_MS;
        let to = days_from_civil(2026, 1, 7) * DAY_MS;

        let hits = rrule_occurrences("FREQ=DAILY", anchor, from, to);
        assert_eq!(hits.len(), 6); // Jan 1..6 inclusive of the 7th's midnight cut
        assert!(hits.iter().all(|h| (h - anchor) % DAY_MS == 0));

        let every_other = rrule_occurrences("FREQ=DAILY;INTERVAL=2", anchor, from, to);
        assert_eq!(every_other.len(), 3); // 1st, 3rd, 5th
    }

    #[test]
    fn weekly_rule_respects_byday() {
        // 2026-01-05 is a Monday.
        let anchor = days_from_civil(2026, 1, 5) * DAY_MS + 8 * 3600_000;
        let from = days_from_civil(2026, 1, 5) * DAY_MS;
        let to = days_from_civil(2026, 1, 19) * DAY_MS;

        let mon_wed_fri = rrule_occurrences("FREQ=WEEKLY;BYDAY=MO,WE,FR", anchor, from, to);
        // Two full weeks of Mon/Wed/Fri = 6 occurrences.
        assert_eq!(mon_wed_fri.len(), 6);

        let count_limited = rrule_occurrences("FREQ=WEEKLY;BYDAY=MO;COUNT=2", anchor, from, to);
        assert_eq!(count_limited.len(), 2);
    }

    #[test]
    fn monthly_rule_lands_on_anchor_day() {
        let anchor = days_from_civil(2026, 1, 15) * DAY_MS + 10 * 3600_000;
        let from = days_from_civil(2026, 1, 1) * DAY_MS;
        let to = days_from_civil(2026, 4, 1) * DAY_MS;
        let hits = rrule_occurrences("FREQ=MONTHLY", anchor, from, to);
        assert_eq!(hits.len(), 3); // Jan 15, Feb 15, Mar 15
    }

    #[test]
    fn today_board_separates_overdue_and_due_today() {
        let st = conn();
        let c = st.db.lock().unwrap();
        let day_start = days_from_civil(2026, 3, 10) * DAY_MS;
        let day_end = day_start + DAY_MS;

        let mut late = base("late");
        late.due_ms = Some(day_start - DAY_MS);
        create_item(&c, &late).unwrap();

        let mut due = base("due");
        due.due_ms = Some(day_start + 3600_000);
        create_item(&c, &due).unwrap();

        let mut captured = base("captured");
        captured.status = Some("inbox".into());
        create_item(&c, &captured).unwrap();

        let board = today_board(&c, day_start, day_end, "2026-03-10").unwrap();
        assert_eq!(board.overdue.len(), 1);
        assert_eq!(board.today.len(), 1);
        assert_eq!(board.inbox.len(), 1);
        assert_eq!(board.counts.open, 2);
    }

    #[test]
    fn recurring_item_is_not_overdue_while_next_occurrence_is_ahead() {
        let st = conn();
        let c = st.db.lock().unwrap();
        let anchor = days_from_civil(2026, 1, 1) * DAY_MS;
        let day_start = days_from_civil(2026, 3, 10) * DAY_MS;
        let day_end = day_start + DAY_MS;

        let mut habit = base("vocab drill");
        habit.due_ms = Some(anchor);
        habit.rrule = Some("FREQ=DAILY".into());
        create_item(&c, &habit).unwrap();

        let board = today_board(&c, day_start, day_end, "2026-03-10").unwrap();
        assert!(board.overdue.is_empty(), "recurring item must not be overdue");
        assert_eq!(board.today.len(), 1, "it should appear in today's lane");
    }

    #[test]
    fn links_are_polymorphic_and_reversible() {
        let st = conn();
        let c = st.db.lock().unwrap();
        let item = create_item(&c, &base("read paper")).unwrap();

        link(&c, &item.id, "sources", "src-1", "evidence").unwrap();
        link(&c, &item.id, "papers", "doi-123", "context").unwrap();
        assert_eq!(links(&c, &item.id).unwrap().len(), 2);

        let reverse = items_for_entity(&c, "sources", "src-1").unwrap();
        assert_eq!(reverse.len(), 1);
        assert_eq!(reverse[0].id, item.id);

        unlink(&c, &item.id, "sources", "src-1").unwrap();
        assert_eq!(links(&c, &item.id).unwrap().len(), 1);
        // Linking to a missing item is an error, not a silent orphan.
        assert!(link(&c, "nope", "sources", "src-2", "context").is_err());
    }

    #[test]
    fn goal_deletion_keeps_work_items() {
        let st = conn();
        let c = st.db.lock().unwrap();
        let goal = upsert_goal(
            &c,
            &GoalInput {
                id: None,
                title: "Pass discrete maths".into(),
                kind: Some("exam".into()),
                metric: Some("score".into()),
                target: Some(85.0),
                current: Some(60.0),
                deadline_ms: None,
                subject_id: None,
                status: None,
            },
        )
        .unwrap();

        let mut item = base("chapter 1");
        item.goal_id = Some(goal.id.clone());
        let created = create_item(&c, &item).unwrap();

        delete_goal(&c, &goal.id).unwrap();
        let survivor = get_item(&c, &created.id).unwrap();
        assert!(survivor.goal_id.is_none());
    }

    #[test]
    fn plan_block_rejects_inverted_range() {
        let st = conn();
        let c = st.db.lock().unwrap();
        let item = create_item(&c, &base("block test")).unwrap();
        assert!(upsert_block(&c, None, &item.id, "2026-03-10", 600, 600, None).is_err());
        let ok = upsert_block(&c, None, &item.id, "2026-03-10", 600, 660, None).unwrap();
        assert_eq!(ok.end_min - ok.start_min, 60);
        assert_eq!(list_blocks(&c, "2026-03-10").unwrap().len(), 1);
    }
}
