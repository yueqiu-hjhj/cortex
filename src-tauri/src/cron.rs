//! B2 (3.4): in-app scheduled automation — Khoj-style `Automations`, personal-use
//! edition. A job = a name + a prompt + a schedule (interval or daily-at-HH:MM).
//! A background OS thread ticks every 30 s; when a job is due it (1) advances
//! `next_run_at` FIRST (so a slow LLM call can't double-fire the same slot),
//! (2) runs the prompt on the chat-slot model, (3) stores the output in
//! `cron_runs` (and as a `txt` source when the job is subject-scoped), and
//! (4) fires a best-effort desktop notification.
//!
//! No cron-expression parser on purpose: interval + daily covers every real
//! personal automation ("每天拉一次 arXiv", "每晚总结今天的笔记"), and the
//! schedule math stays auditable std-only code. Local time comes from a
//! `tz_offset_minutes` value captured by the frontend at save time (China has
//! no DST; editing the job re-captures it).

use crate::db::{new_id, now_ms, AppState};
use crate::error::{Error, Result};
use crate::repo;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tauri::{AppHandle, Manager};

const TICK_SECS: u64 = 30;
const OUTPUT_CAP: usize = 60_000; // chars stored per run — prompts can't run away

#[derive(Debug, Clone, Serialize)]
pub struct CronJob {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub subject_id: Option<String>,
    /// "interval" (every N minutes) | "daily" (at HH:MM local)
    pub kind: String,
    pub interval_minutes: Option<i64>,
    pub daily_time: Option<String>,
    /// Minutes east of UTC, captured by the frontend when the job was saved.
    pub tz_offset_minutes: i64,
    pub enabled: bool,
    pub last_run_at: Option<i64>,
    pub next_run_at: i64,
    /// What to do when due: "prompt" (LLM text job) | "sync" (connector pull).
    /// Defaulted on read so pre-0055 rows behave exactly as before.
    pub action: String,
    /// For "sync": the connector_id to pull. Unused for "prompt".
    pub action_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CronRun {
    pub id: String,
    pub job_id: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub status: String, // running | ok | error
    pub source_id: Option<String>,
    pub output: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct JobIn {
    name: String,
    /// Required for "prompt" jobs; ignored (may be empty) for "sync".
    #[serde(default)]
    prompt: String,
    subject_id: Option<String>,
    kind: String,
    interval_minutes: Option<i64>,
    daily_time: Option<String>,
    #[serde(default = "default_tz")]
    tz_offset_minutes: i64,
    /// "prompt" | "sync" — absent means "prompt" (legacy frontend / API users).
    #[serde(default)]
    action: Option<String>,
    /// connector_id for "sync" jobs.
    #[serde(default)]
    action_ref: Option<String>,
}
fn default_tz() -> i64 {
    480 // UTC+8 — right for this user, and the frontend always sends the real one
}

// ---- schedule math ------------------------------------------------------

/// Parse "HH:MM" (local wall time). Falls back to 08:00 on bad input — a cron
/// job must never become unrunnable because of a typo.
fn parse_hhmm(s: Option<&str>) -> (i64, i64) {
    let Some(s) = s.map(str::trim).filter(|s| !s.is_empty()) else {
        return (8, 0);
    };
    let mut it = s.split(':');
    let h = it.next().and_then(|p| p.trim().parse::<i64>().ok()).unwrap_or(8);
    let m = it.next().and_then(|p| p.trim().parse::<i64>().ok()).unwrap_or(0);
    (h.clamp(0, 23), m.clamp(0, 59))
}

/// Next due timestamp (ms UTC) strictly after `from_ms`.
pub(crate) fn compute_next_run(
    kind: &str,
    interval_minutes: Option<i64>,
    daily_time: Option<&str>,
    tz_offset_minutes: i64,
    from_ms: i64,
) -> i64 {
    match kind {
        "daily" => {
            let (h, m) = parse_hhmm(daily_time);
            // Local wall clock → UTC ms: local = utc + offset, so the target's
            // UTC minutes-of-day is local minus the offset (may leave the 0..1440
            // range — ms arithmetic is linear, that's fine).
            let target_min_utc = h * 60 + m - tz_offset_minutes;
            let day = from_ms.div_euclid(86_400_000);
            let at = |day: i64| day * 86_400_000 + target_min_utc * 60_000;
            let next = at(day);
            if next > from_ms {
                next
            } else {
                at(day + 1)
            }
        }
        // interval (and anything unrecognized → default 60 min)
        _ => {
            let mins = interval_minutes.filter(|m| *m >= 1).unwrap_or(60);
            from_ms + mins * 60_000
        }
    }
}

fn row_to_job(r: &rusqlite::Row) -> rusqlite::Result<CronJob> {
    Ok(CronJob {
        id: r.get(0)?,
        name: r.get(1)?,
        prompt: r.get(2)?,
        subject_id: r.get(3)?,
        kind: r.get(4)?,
        interval_minutes: r.get(5)?,
        daily_time: r.get(6)?,
        tz_offset_minutes: r.get::<_, Option<i64>>(7)?.unwrap_or(480),
        enabled: r.get::<_, i64>(8)? != 0,
        last_run_at: r.get(9)?,
        next_run_at: r.get(10)?,
        action: r
            .get::<_, Option<String>>(11)?
            .filter(|a| !a.trim().is_empty())
            .unwrap_or_else(|| "prompt".into()),
        action_ref: r.get(12)?,
    })
}

const JOB_COLS: &str = "id, name, prompt, subject_id, kind, interval_minutes, daily_time, \
                        tz_offset_minutes, enabled, last_run_at, next_run_at, action, action_ref";

// ---- CRUD (used by the tauri commands below) ----------------------------

pub(crate) fn list_jobs(c: &rusqlite::Connection) -> Result<Vec<CronJob>> {
    let mut st = c.prepare(&format!("SELECT {JOB_COLS} FROM cron_jobs ORDER BY created_at"))?;
    let jobs = st
        .query_map([], row_to_job)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(jobs)
}

pub(crate) fn get_job(c: &rusqlite::Connection, id: &str) -> Result<CronJob> {
    let sql = format!("SELECT {JOB_COLS} FROM cron_jobs WHERE id=?1");
    Ok(c.query_row(&sql, params![id], row_to_job)?)
}

pub(crate) fn add_job(c: &rusqlite::Connection, input: &JobIn) -> Result<CronJob> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(Error::Other("定时任务需要名称".into()));
    }
    let (action, action_ref) = normalize_action(input)?;
    if action == "prompt" && input.prompt.trim().is_empty() {
        return Err(Error::Other("提示词任务需要填写提示词".into()));
    }
    let id = new_id();
    let next = compute_next_run(
        &input.kind,
        input.interval_minutes,
        input.daily_time.as_deref(),
        input.tz_offset_minutes,
        now_ms(),
    );
    c.execute(
        "INSERT INTO cron_jobs (id, name, prompt, subject_id, kind, interval_minutes, \
         daily_time, tz_offset_minutes, enabled, next_run_at, created_at, action, action_ref) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,1,?9,?10,?11,?12)",
        params![
            id,
            name,
            input.prompt.trim(),
            input.subject_id,
            input.kind,
            input.interval_minutes,
            input.daily_time,
            input.tz_offset_minutes,
            next,
            now_ms(),
            action,
            action_ref
        ],
    )?;
    get_job(c, &id)
}

/// Validate + canonicalize the action pair. "sync" needs a connector ref AND a
/// target subject (a pull has nowhere to land otherwise); anything unrecognized
/// degrades to "prompt" rather than a job that can never run.
fn normalize_action(input: &JobIn) -> Result<(String, Option<String>)> {
    match input.action.as_deref().map(str::trim) {
        Some("sync") => {
            let r = input
                .action_ref
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| Error::Other("同步任务需要选择连接器".into()))?;
            if input.subject_id.as_deref().map(str::trim).unwrap_or("").is_empty() {
                return Err(Error::Other("同步任务需要关联科目（新内容要落到某个科目下）".into()));
            }
            Ok(("sync".into(), Some(r.to_string())))
        }
        _ => Ok(("prompt".into(), None)),
    }
}

pub(crate) fn update_job(c: &rusqlite::Connection, id: &str, input: &JobIn) -> Result<CronJob> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(Error::Other("定时任务需要名称".into()));
    }
    let (action, action_ref) = normalize_action(input)?;
    if action == "prompt" && input.prompt.trim().is_empty() {
        return Err(Error::Other("提示词任务需要填写提示词".into()));
    }
    // Editing recomputes the schedule from now (and re-captures the tz offset).
    let next = compute_next_run(
        &input.kind,
        input.interval_minutes,
        input.daily_time.as_deref(),
        input.tz_offset_minutes,
        now_ms(),
    );
    let n = c.execute(
        "UPDATE cron_jobs SET name=?2, prompt=?3, subject_id=?4, kind=?5, interval_minutes=?6, \
         daily_time=?7, tz_offset_minutes=?8, next_run_at=?9, action=?10, action_ref=?11 WHERE id=?1",
        params![
            id,
            name,
            input.prompt.trim(),
            input.subject_id,
            input.kind,
            input.interval_minutes,
            input.daily_time,
            input.tz_offset_minutes,
            next,
            action,
            action_ref
        ],
    )?;
    if n == 0 {
        return Err(Error::NotFound(format!("cron job {id}")));
    }
    get_job(c, id)
}

pub(crate) fn set_job_enabled(c: &rusqlite::Connection, id: &str, enabled: bool) -> Result<CronJob> {
    // Re-enabling (or pausing) reschedules from now: a job paused across its
    // slot shouldn't fire the moment it's turned back on.
    let next = compute_next_from_row(c, id, now_ms())?;
    c.execute(
        "UPDATE cron_jobs SET enabled=?2, next_run_at=?3 WHERE id=?1",
        params![id, enabled as i64, next],
    )?;
    get_job(c, id)
}

fn compute_next_from_row(c: &rusqlite::Connection, id: &str, from_ms: i64) -> Result<i64> {
    let job = get_job(c, id)?;
    Ok(compute_next_run(
        &job.kind,
        job.interval_minutes,
        job.daily_time.as_deref(),
        job.tz_offset_minutes,
        from_ms,
    ))
}

pub(crate) fn delete_job(c: &rusqlite::Connection, id: &str) -> Result<()> {
    c.execute("DELETE FROM cron_runs WHERE job_id=?1", params![id])?;
    c.execute("DELETE FROM cron_jobs WHERE id=?1", params![id])?;
    Ok(())
}

pub(crate) fn list_runs(c: &rusqlite::Connection, job_id: &str, limit: i64) -> Result<Vec<CronRun>> {
    let mut st = c.prepare(
        "SELECT id, job_id, started_at, finished_at, status, source_id, output, error \
         FROM cron_runs WHERE job_id=?1 ORDER BY started_at DESC LIMIT ?2",
    )?;
    let runs = st
        .query_map(params![job_id, limit.clamp(1, 50)], |r| {
            Ok(CronRun {
                id: r.get(0)?,
                job_id: r.get(1)?,
                started_at: r.get(2)?,
                finished_at: r.get(3)?,
                status: r.get(4)?,
                source_id: r.get(5)?,
                output: r.get(6)?,
                error: r.get(7)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(runs)
}

// ---- execution ----------------------------------------------------------

/// Run one job now (manual "run now" button). Logs a run row, stores output,
/// notifies. Returns the finished run.
pub fn run_now(app: &AppHandle, job_id: &str) -> Result<CronRun> {
    let state = app.state::<AppState>();
    let job = {
        let c = state.db.lock().unwrap();
        get_job(&c, job_id)?
    };
    execute(app, &job)
}

/// Claim + run + record. The claim (advance next_run_at, stamp last_run_at,
/// insert a `running` run row) happens in one lock; the LLM call happens with
/// NO lock held; the outcome is written back in a second short lock.
fn execute(app: &AppHandle, job: &CronJob) -> Result<CronRun> {
    let state = app.state::<AppState>();
    let run_id = new_id();
    let started = now_ms();
    {
        let c = state.db.lock().unwrap();
        let next = compute_next_run(
            &job.kind,
            job.interval_minutes,
            job.daily_time.as_deref(),
            job.tz_offset_minutes,
            started,
        );
        c.execute(
            "UPDATE cron_jobs SET last_run_at=?2, next_run_at=?3 WHERE id=?1",
            params![job.id, started, next],
        )?;
        c.execute(
            "INSERT INTO cron_runs (id, job_id, started_at, status) VALUES (?1,?2,?3,'running')",
            params![run_id, job.id, started],
        )?;
    }

    let outcome = match job.action.as_str() {
        "sync" => sync_action(app, job),
        _ => generate(app, job),
    };
    let finished = now_ms();
    let run = match outcome {
        Ok((text, source_id)) => {
            let c = state.db.lock().unwrap();
            c.execute(
                "UPDATE cron_runs SET finished_at=?2, status='ok', source_id=?3, output=?4 WHERE id=?1",
                params![run_id, finished, source_id, truncate(&text)],
            )?;
            CronRun {
                id: run_id,
                job_id: job.id.clone(),
                started_at: started,
                finished_at: Some(finished),
                status: "ok".into(),
                source_id,
                output: Some(truncate(&text)),
                error: None,
            }
        }
        Err(e) => {
            {
                let c = state.db.lock().unwrap();
                c.execute(
                    "UPDATE cron_runs SET finished_at=?2, status='error', error=?3 WHERE id=?1",
                    params![run_id, finished, e.to_string()],
                )?;
            }
            CronRun {
                id: run_id,
                job_id: job.id.clone(),
                started_at: started,
                finished_at: Some(finished),
                status: "error".into(),
                source_id: None,
                output: None,
                error: Some(e.to_string()),
            }
        }
    };

    let title = if run.status == "ok" { "定时任务完成" } else { "定时任务失败" };
    crate::alerts::notify_routed(
        app,
        &format!("cron:{}", job.id),
        &format!("⏰ {title} — {}", job.name),
        run.error.clone().unwrap_or_else(|| "点开应用查看结果".into()).as_str(),
        serde_json::json!({ "kind": "cron", "jobId": job.id }),
    );
    Ok(run)
}

/// The actual work: prompt → chat-slot model → text (+ optional source row).
fn generate(app: &AppHandle, job: &CronJob) -> Result<(String, Option<String>)> {
    let state = app.state::<AppState>();
    // Gather what the LLM needs (model, optional subject name) under the lock,
    // then RELEASE it — completion is a network call that must not block UI.
    let (model, subject_name) = {
        let c = state.db.lock().unwrap();
        let keys = crate::commands::read_keys(&c)?;
        let model = crate::commands::load_model(&c, &keys, "chat")?;
        let subject_name = job
            .subject_id
            .as_deref()
            .and_then(|sid| repo::get_subject(&c, sid).ok())
            .map(|s| s.name);
        (model, subject_name)
    };
    let mut system = "你是用户的学习助手 Cortex，正在执行一个定时任务。直接输出任务结果，\
                      结构清晰（可用 Markdown），不要寒暄、不要复述任务定义。"
        .to_string();
    if let Some(name) = &subject_name {
        system.push_str(&format!(" 该任务关联科目：{name}。"));
    }
    let text = model.complete(&system, &job.prompt)?;

    // Subject-scoped jobs archive the result as a real source so it joins the
    // normal knowledge flow (chunked/embedded like any other txt source).
    let mut source_id = None;
    if let Some(sid) = &job.subject_id {
        let stamp = local_stamp(now_ms(), job.tz_offset_minutes);
        let name = format!("{} · {}", job.name, stamp);
        let c = state.db.lock().unwrap();
        let id = repo::insert_source(&c, sid, None, &name, "txt", Some(&format!("cron:{}", job.id)))?;
        repo::finalize_source(&c, &id, "ready", None, Some(&text), None)?;
        source_id = Some(id);
    }
    Ok((text, source_id))
}

/// The other action: pull a research connector into the job's subject. The
/// sync itself creates the real sources (with connector tags); the run record
/// only keeps the human-readable summary. Not archived as a source — a "拉了
/// 3 篇论文" text row would be noise in the knowledge flow.
fn sync_action(app: &AppHandle, job: &CronJob) -> Result<(String, Option<String>)> {
    let connector = job
        .action_ref
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| Error::Other("同步任务缺少连接器配置".into()))?;
    let sid = job
        .subject_id
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| Error::Other("同步任务缺少关联科目".into()))?
        .to_string();

    // connectors::sync is async (spawn_blocking network phase) — this thread is
    // either the cron loop or a spawn_blocking command, both safe to block on.
    let summary = tauri::async_runtime::block_on(crate::connectors::sync(
        app.clone(),
        connector,
        sid,
        None,
    ))?;

    let mut text = format!(
        "连接器 {}：抓取 {} 条，新入库 {} 条，跳过（重复/无法映射）{} 条。",
        connector, summary.fetched, summary.imported, summary.skipped
    );
    if !summary.errors.is_empty() {
        text.push_str("\n错误：\n");
        for e in &summary.errors {
            text.push_str(&format!("- {e}\n"));
        }
    }
    Ok((text, None))
}

/// Local wall-clock stamp "MM-DD HH:MM" via the stored tz offset — good enough
/// for a display name, and never wrong by more than a DST shift (none in CN).
fn local_stamp(ms: i64, tz_offset_minutes: i64) -> String {
    let local = ms + tz_offset_minutes * 60_000;
    let day = local.div_euclid(86_400_000);
    let min_of_day = local.rem_euclid(86_400_000) / 60_000;
    // Day 0 = 1970-01-01 (Thursday). Convert to (y, m, d) civil date.
    let (y, m, d) = civil_from_days(day);
    let (hh, mm) = (min_of_day / 60, min_of_day % 60);
    format!("{m:02}-{d:02} {hh:02}:{mm:02} ({y})")
}

/// Howard Hinnant's days→civil algorithm (std-only date math).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn truncate(s: &str) -> String {
    if s.chars().count() <= OUTPUT_CAP {
        s.to_string()
    } else {
        s.chars().take(OUTPUT_CAP).collect()
    }
}

// ---- background loop ----------------------------------------------------

/// Start the scheduler thread. Ticks every 30 s; each tick is panic-caught so
/// a poisoned lock or a bad row can never kill the loop (same pattern as the
/// sync background thread in lib.rs).
pub fn start(handle: AppHandle) {
    std::thread::spawn(move || {
        // Let startup settle before the first sweep.
        std::thread::sleep(Duration::from_secs(15));
        loop {
            let h = handle.clone();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Err(e) = tick(&h) {
                    eprintln!("[cron] tick failed: {e}");
                }
            }));
            std::thread::sleep(Duration::from_secs(TICK_SECS));
        }
    });
}

fn tick(app: &AppHandle) -> Result<()> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| Error::Other("app state not ready".into()))?;
    let due = {
        let c = state.db.lock().unwrap();
        list_due_jobs(&c, now_ms())?
    };
    for job in due {
        // execute() claims first, so a crash mid-run can't double-fire.
        let _ = execute(app, &job);
    }
    Ok(())
}

fn list_due_jobs(c: &rusqlite::Connection, now: i64) -> Result<Vec<CronJob>> {
    let sql = format!(
        "SELECT {JOB_COLS} FROM cron_jobs WHERE enabled=1 AND next_run_at <= ?1 ORDER BY next_run_at"
    );
    let mut st = c.prepare(&sql)?;
    let jobs = st
        .query_map(params![now], row_to_job)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(jobs)
}

// ---- tauri commands -----------------------------------------------------

#[tauri::command]
pub fn cron_list(state: tauri::State<'_, AppState>) -> Result<Vec<CronJob>> {
    let c = state.db.lock().unwrap();
    list_jobs(&c)
}

#[tauri::command]
pub fn cron_add(state: tauri::State<'_, AppState>, job: serde_json::Value) -> Result<CronJob> {
    let input: JobIn = serde_json::from_value(job).map_err(|e| Error::Other(e.to_string()))?;
    let c = state.db.lock().unwrap();
    add_job(&c, &input)
}

#[tauri::command]
pub fn cron_update(
    state: tauri::State<'_, AppState>,
    id: String,
    job: serde_json::Value,
) -> Result<CronJob> {
    let input: JobIn = serde_json::from_value(job).map_err(|e| Error::Other(e.to_string()))?;
    let c = state.db.lock().unwrap();
    update_job(&c, &id, &input)
}

#[tauri::command]
pub fn cron_set_enabled(
    state: tauri::State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<CronJob> {
    let c = state.db.lock().unwrap();
    set_job_enabled(&c, &id, enabled)
}

#[tauri::command]
pub fn cron_delete(state: tauri::State<'_, AppState>, id: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    delete_job(&c, &id)
}

#[tauri::command]
pub fn cron_runs(state: tauri::State<'_, AppState>, job_id: String) -> Result<Vec<CronRun>> {
    let c = state.db.lock().unwrap();
    list_runs(&c, &job_id, 20)
}

#[tauri::command]
pub async fn cron_run_now(app: AppHandle, id: String) -> Result<CronRun> {
    tauri::async_runtime::spawn_blocking(move || run_now(&app, &id))
        .await
        .map_err(|e| Error::Other(format!("cron run task failed: {e}")))?
}

// ---- tests --------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_schedule_advances_by_minutes() {
        let base = 1_700_000_000_000;
        assert_eq!(
            compute_next_run("interval", Some(30), None, 480, base),
            base + 30 * 60_000
        );
        // < 1 minute clamps to the 60-min default
        assert_eq!(
            compute_next_run("interval", Some(0), None, 480, base),
            base + 60 * 60_000
        );
    }

    #[test]
    fn daily_schedule_targets_next_local_wall_time() {
        // from = day 20752 at 04:40 UTC = 12:40 local (UTC+8)
        let from = 20_752 * 86_400_000 + 4 * 3_600_000 + 40 * 60_000;
        // local 07:30 → 23:30 UTC tonight = next-day 07:30 local
        let next = compute_next_run("daily", None, Some("07:30"), 480, from);
        let expected = 20_752 * 86_400_000 + 23 * 3_600_000 + 30 * 60_000;
        assert_eq!(next, expected);
        // local 22:30 → 14:30 UTC today, still ahead of `from` → fires today
        let next2 = compute_next_run("daily", None, Some("22:30"), 480, from);
        let expected2 = 20_752 * 86_400_000 + 14 * 3_600_000 + 30 * 60_000;
        assert_eq!(next2, expected2);
        // local 03:00 → 19:00 UTC of the PREVIOUS day → today's slot passed,
        // so it rolls to tomorrow local 03:00 = 19:00 UTC today
        let next3 = compute_next_run("daily", None, Some("03:00"), 480, from);
        let expected3 = 20_752 * 86_400_000 + 19 * 3_600_000;
        assert_eq!(next3, expected3);
    }

    #[test]
    fn bad_daily_time_falls_back_to_0800() {
        let from = 20_752 * 86_400_000 + 4 * 3_600_000;
        // non-numeric → 08:00 local (= 00:00 UTC); from 04:40 UTC that's
        // tomorrow 00:00 UTC (today's 00:00 slot already passed)
        let next = compute_next_run("daily", None, Some("abc"), 480, from);
        let next2 = compute_next_run("daily", None, Some("08:00"), 480, from);
        assert_eq!(next, next2);
        // out-of-range numbers clamp instead of discarding: "25:99" → 23:59
        // local = 15:59 UTC, still ahead of 04:40 UTC today
        let next3 = compute_next_run("daily", None, Some("25:99"), 480, from);
        assert_eq!(next3, 20_752 * 86_400_000 + 959 * 60_000);
    }

    #[test]
    fn civil_date_math_is_correct() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(20_746), (2026, 10, 20));
    }

    #[test]
    fn crud_roundtrip_and_due_query() {
        let st = crate::db::AppState::in_memory().unwrap();
        let conn = st.db.lock().unwrap();
        let job = add_job(
            &conn,
            &JobIn {
                name: "晨报".into(),
                prompt: "总结今天的安排".into(),
                subject_id: None,
                kind: "daily".into(),
                interval_minutes: None,
                daily_time: Some("07:30".into()),
                tz_offset_minutes: 480,
                action: None,
                action_ref: None,
            },
        )
        .unwrap();
        assert!(job.enabled);
        assert_eq!(job.daily_time.as_deref(), Some("07:30"));
        // Not due yet (next_run_at is in the future)
        assert!(list_due_jobs(&conn, job.next_run_at - 1).unwrap().is_empty());
        assert_eq!(list_due_jobs(&conn, job.next_run_at).unwrap().len(), 1);

        // update changes fields + reschedules
        let updated = update_job(
            &conn,
            &job.id,
            &JobIn {
                name: "晨报 v2".into(),
                prompt: "新的提示词".into(),
                subject_id: Some("s1".into()),
                kind: "interval".into(),
                interval_minutes: Some(45),
                daily_time: None,
                tz_offset_minutes: 480,
                action: None,
                action_ref: None,
            },
        )
        .unwrap();
        assert_eq!(updated.prompt, "新的提示词");
        assert_eq!(updated.kind, "interval");
        assert_eq!(updated.interval_minutes, Some(45));

        // pause keeps it out of the due list
        let paused = set_job_enabled(&conn, &job.id, false).unwrap();
        assert!(!paused.enabled);
        assert!(list_due_jobs(&conn, now_ms() + 86_400_000).unwrap().is_empty());

        // runs log is empty until first execution; delete removes job + runs
        assert!(list_runs(&conn, &job.id, 20).unwrap().is_empty());
        delete_job(&conn, &job.id).unwrap();
        assert!(get_job(&conn, &job.id).is_err());
    }

    fn sync_input(name: &str, subject: Option<&str>, r: Option<&str>) -> JobIn {
        JobIn {
            name: name.into(),
            prompt: String::new(), // sync jobs carry no prompt
            subject_id: subject.map(str::to_string),
            kind: "daily".into(),
            interval_minutes: None,
            daily_time: Some("07:30".into()),
            tz_offset_minutes: 480,
            action: Some("sync".into()),
            action_ref: r.map(str::to_string),
        }
    }

    #[test]
    fn sync_jobs_validate_and_persist() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();

        // missing connector ref / missing subject are rejected with a message
        let err = add_job(&c, &sync_input("拉论文", Some("s1"), None)).unwrap_err();
        assert!(err.to_string().contains("连接器"));
        let err = add_job(&c, &sync_input("拉论文", None, Some("arxiv-cs"))).unwrap_err();
        assert!(err.to_string().contains("科目"));

        // valid sync job round-trips (and needs no prompt)
        let job = add_job(&c, &sync_input("每日 arXiv", Some("s1"), Some("arxiv-cs"))).unwrap();
        assert_eq!(job.action, "sync");
        assert_eq!(job.action_ref.as_deref(), Some("arxiv-cs"));
        let reread = get_job(&c, &job.id).unwrap();
        assert_eq!(reread.action, "sync");

        // update can flip it back to a prompt job (action cleared, prompt required again)
        let mut inp = sync_input("改名", Some("s1"), Some("arxiv-cs"));
        inp.action = None;
        inp.prompt = "变成晨报".into();
        let flipped = update_job(&c, &job.id, &inp).unwrap();
        assert_eq!(flipped.action, "prompt");
        assert_eq!(flipped.action_ref, None);
        assert_eq!(flipped.prompt, "变成晨报");
    }

    #[test]
    fn unknown_action_degrades_to_prompt_and_must_carry_prompt() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let mut inp = sync_input("x", Some("s1"), Some("arxiv-cs"));
        inp.action = Some("teleport".into());
        // garbage action → treated as prompt → empty prompt rejected
        assert!(add_job(&c, &inp).is_err());
        inp.prompt = "正常提示词".into();
        let job = add_job(&c, &inp).unwrap();
        assert_eq!(job.action, "prompt");
        assert_eq!(job.action_ref, None);
    }
}
