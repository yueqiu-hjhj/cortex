//! Answer health: one page that answers "how much can I trust what this
//! library tells me?".
//!
//! Every honest signal was already being recorded somewhere — terminal state
//! (0077), retrieval accounting (0044), stripped fabrications (0069), rollover
//! lineage (0082), frozen-quote drift (0078) — but never together. Read
//! separately they are trivia; read together they say whether the failure is
//! the model, the question, or the library.
//!
//! Pure read: no schema, no writes. The verdict is mechanical (fixed
//! thresholds over counts), because a health report that needs a model to
//! interpret itself is not a health report.

use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashSet;
use tauri::State;

use crate::db::AppState;
use crate::error::Result;
use crate::models::Citation;

/// Terminal states, in reporting order (0077). Rows written before the column
/// existed have NULL — reported as `unrecorded`, never guessed into a state.
pub const STATE_ORDER: [&str; 5] = ["completed", "truncated", "blocked", "aborted", "failed"];

#[derive(Debug, Clone, Serialize)]
pub struct StateCount {
    pub state: String,
    pub count: i64,
}

/// What the frozen citations of these answers look like against the sources'
/// CURRENT text — the same check the evidence panel runs, at library scale.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SnapshotHealth {
    /// Citations that carried a snapshot and could be checked.
    pub checked: i64,
    pub exact: i64,
    pub moved: i64,
    pub drifted: i64,
    pub gone: i64,
    /// Pre-0078 citations: nothing to check, never counted as drift.
    pub unchecked: i64,
    /// Citations repaired by citesnap's re-anchor (subset of the above).
    pub reanchored: i64,
}

impl SnapshotHealth {
    /// Share of checkable snapshots that no longer hold, as a percentage.
    pub fn drift_pct(&self) -> f64 {
        if self.checked == 0 {
            return 0.0;
        }
        (self.drifted + self.gone) as f64 * 100.0 / self.checked as f64
    }
}

/// One mechanical verdict line. `level` is what colours it in the UI.
#[derive(Debug, Clone, Serialize)]
pub struct HealthSignal {
    pub level: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnswerHealth {
    pub days: i64,
    pub answers: i64,
    /// Only states that occurred (plus `unrecorded` when pre-0077 rows exist).
    pub states: Vec<StateCount>,
    /// Answers whose retrieval found at least one passage.
    pub grounded: i64,
    /// Answers that explicitly had nothing to ground on (0044).
    pub abstained: i64,
    /// Answers that cited something inline.
    pub cited: i64,
    /// Fabricated citation tokens citecheck stripped, and how many answers
    /// carried any.
    pub fabricated_total: i64,
    pub fabricated_answers: i64,
    /// Distinct threads these answers live in that began as a rollover.
    pub rolled_threads: i64,
    pub snapshots: SnapshotHealth,
    pub signals: Vec<HealthSignal>,
}

fn pct(n: i64, d: i64) -> f64 {
    if d == 0 {
        0.0
    } else {
        n as f64 * 100.0 / d as f64
    }
}

/// Aggregate answer health. `subject_id` scopes to one subject; `days <= 0`
/// means the whole history (an honest answer for a library that is two days
/// old and has no 30-day window to report).
pub fn health(conn: &Connection, subject_id: Option<&str>, days: i64) -> Result<AnswerHealth> {
    let since = if days > 0 {
        crate::db::now_ms() - days * 86_400_000
    } else {
        0
    };
    let sql = "SELECT m.citations, m.retrieval, m.fabricated_dropped, m.answer_state, t.rolled_from
               FROM chat_messages m JOIN chat_threads t ON t.id = m.thread_id
               WHERE m.role='assistant' AND m.created_at >= ?1";
    let scoped = format!("{sql} AND t.subject_id = ?2");

    let mut counts = [0i64; 5];
    let mut unrecorded = 0i64;
    let mut answers = 0i64;
    let mut grounded = 0i64;
    let mut abstained_answers = 0i64;
    let mut cited_answers = 0i64;
    let mut fabricated_total = 0i64;
    let mut fabricated_answers = 0i64;
    let mut rolled: HashSet<String> = HashSet::new();
    let mut snap = SnapshotHealth::default();

    let mut read_row = |citations: Option<String>,
                        retrieval: Option<String>,
                        fabricated: Option<i64>,
                        state: Option<String>,
                        rolled_from: Option<String>| {
        answers += 1;
        let mi = state
            .as_deref()
            .and_then(|s| STATE_ORDER.iter().position(|x| *x == s));
        match mi {
            Some(i) => counts[i] += 1,
            None => unrecorded += 1,
        }
        if let Some(rf) = rolled_from.filter(|s| !s.is_empty()) {
            rolled.insert(rf);
        }
        let meta: Option<crate::models::RetrievalMeta> =
            retrieval.and_then(|s| serde_json::from_str(&s).ok());
        if let Some(m) = meta {
            if m.hits > 0 {
                grounded += 1;
            }
            if m.abstained {
                abstained_answers += 1;
            }
        }
        if let Some(f) = fabricated.filter(|n| *n > 0) {
            fabricated_total += f;
            fabricated_answers += 1;
        }
        let cites: Vec<Citation> = citations
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if cites.iter().any(|c| c.cited) {
            cited_answers += 1;
        }
        if cites.is_empty() {
            return;
        }
        for (c, check) in cites.iter().zip(crate::citesnap::verify_message(conn, &cites)) {
            if c.reanchored_at.is_some() {
                snap.reanchored += 1;
            }
            match check {
                None => snap.unchecked += 1,
                Some(crate::citesnap::QuoteCheck::Exact) => {
                    snap.checked += 1;
                    snap.exact += 1;
                }
                Some(crate::citesnap::QuoteCheck::Moved { .. }) => {
                    snap.checked += 1;
                    snap.moved += 1;
                }
                Some(crate::citesnap::QuoteCheck::Drifted) => {
                    snap.checked += 1;
                    snap.drifted += 1;
                }
                Some(crate::citesnap::QuoteCheck::SourceGone) => {
                    snap.checked += 1;
                    snap.gone += 1;
                }
            }
        }
    };

    if let Some(sid) = subject_id {
        let mut stmt = conn.prepare(&scoped)?;
        let mut rows = stmt.query(rusqlite::params![since, sid])?;
        while let Some(r) = rows.next()? {
            read_row(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?);
        }
    } else {
        let mut stmt = conn.prepare(sql)?;
        let mut rows = stmt.query(rusqlite::params![since])?;
        while let Some(r) = rows.next()? {
            read_row(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?);
        }
    }

    let mut states: Vec<StateCount> = STATE_ORDER
        .iter()
        .enumerate()
        .filter(|(i, _)| counts[*i] > 0)
        .map(|(i, s)| StateCount { state: (*s).to_string(), count: counts[i] })
        .collect();
    if unrecorded > 0 {
        states.push(StateCount { state: "unrecorded".into(), count: unrecorded });
    }

    let signals = verdicts(
        answers,
        &counts,
        abstained_answers,
        grounded,
        fabricated_total,
        fabricated_answers,
        &snap,
    );

    Ok(AnswerHealth {
        days,
        answers,
        states,
        grounded,
        abstained: abstained_answers,
        cited: cited_answers,
        fabricated_total,
        fabricated_answers,
        rolled_threads: rolled.len() as i64,
        snapshots: snap,
        signals,
    })
}

/// The mechanical read of the numbers. Ordered worst-first so the first line
/// is the one worth acting on; a library with nothing wrong still says so,
/// because silence reads as "not measured".
#[allow(clippy::too_many_arguments)]
fn verdicts(
    answers: i64,
    counts: &[i64; 5],
    abstained: i64,
    grounded: i64,
    fabricated_total: i64,
    fabricated_answers: i64,
    snap: &SnapshotHealth,
) -> Vec<HealthSignal> {
    let mut out = Vec::new();
    let sig = |level: &str, text: String| HealthSignal { level: level.into(), text };

    if answers == 0 {
        out.push(sig("info", "这段时间里没有回答记录——先问几个问题再回来看。".into()));
        return out;
    }

    // Failure at the model layer: the answer never arrived, or a filter cut it.
    let flopped = counts[2] + counts[4]; // blocked + failed
    if answers >= 5 && pct(flopped, answers) > 15.0 {
        out.push(sig(
            "warn",
            format!(
                "模型侧不稳定：{flopped}/{answers} 个回答以失败或拦截结束（{}%）——先看 provider 与额度",
                pct(flopped, answers).round() as i64
            ),
        ));
    }

    // Failure at the library layer: the question had nothing to stand on.
    if answers >= 5 && pct(abstained, answers) > 40.0 {
        out.push(sig(
            "warn",
            format!(
                "{abstained}/{answers} 个回答在库里找不到依据（{}%）——问题多半不在这批资料里，补来源比换模型有用",
                pct(abstained, answers).round() as i64
            ),
        ));
    }

    if fabricated_total > 0 {
        out.push(sig(
            "warn",
            format!(
                "剥离伪造引用 {fabricated_total} 条（涉及 {fabricated_answers} 个回答）——模型在编来源名，答案正文已清理但值得警惕"
            ),
        ));
    }

    if snap.checked >= 5 && snap.drift_pct() > 10.0 {
        out.push(sig(
            "warn",
            format!(
                "{} 条引用快照里 {} 条已与现文不符（{}%）——来源被改写或重新提取过，可在证据面板逐条重锚",
                snap.checked,
                snap.drifted + snap.gone,
                snap.drift_pct().round() as i64
            ),
        ));
    }

    if counts[1] > 0 {
        out.push(sig(
            "info",
            format!("{} 个回答因长度上限被截断——问题太宽或预算太紧，回答保留了前半段", counts[1]),
        ));
    }

    if out.is_empty() {
        let grounded_pct = pct(grounded, answers).round() as i64;
        out.push(sig(
            "ok",
            format!(
                "{answers} 个回答全部正常结束，{grounded_pct}% 有检索依据，引用快照与现文一致"
            ),
        ));
    }
    out
}

/// Answer health for the Insights page. `days = 0` means all history.
#[tauri::command]
pub fn answer_health(
    state: State<AppState>,
    subject_id: Option<String>,
    days: i64,
) -> Result<AnswerHealth> {
    let c = state.db.lock().unwrap();
    health(&c, subject_id.as_deref(), days)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::RetrievalMeta;

    struct Fixture {
        st: AppState,
        sid: String,
        src: String,
    }

    fn fixture() -> Fixture {
        let st = AppState::in_memory().unwrap();
        let mut f = Fixture { st, sid: String::new(), src: String::new() };
        {
            let c = f.st.db.lock().unwrap();
            f.sid = crate::repo::insert_subject(&c, "线代", None, None, None).unwrap();
            f.src = crate::repo::insert_source(&c, &f.sid, None, "讲义", "pdf", None).unwrap();
            crate::repo::finalize_source(&c, &f.src, "ok", None, Some("特征值就是缩放因子。"), None)
                .unwrap();
            c.execute(
                "INSERT INTO chunks (id, source_id, subject_id, ord, text, dim, embedding, created_at, char_start, char_end, vec_space)
                 VALUES ('ch1', ?1, ?2, 0, '特征值就是缩放因子。', 1, X'00', 1, 0, 10, '')",
                rusqlite::params![f.src, f.sid],
            )
            .unwrap();
        }
        f
    }

    fn thread(c: &Connection, sid: &str, id: &str, rolled_from: Option<&str>) {
        c.execute(
            "INSERT INTO chat_threads (id, subject_id, scope, created_at, updated_at, rolled_from)
             VALUES (?1, ?2, 'subject', 1, 1, ?3)",
            rusqlite::params![id, sid, rolled_from],
        )
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn msg(
        c: &Connection,
        thread: &str,
        created: i64,
        state: Option<&str>,
        meta: Option<RetrievalMeta>,
        cited_quote: Option<&str>,
        fabricated: Option<i64>,
    ) {
        let cites: Vec<Citation> = cited_quote
            .map(|q| {
                // The real source id, so citesnap can find the text to check.
                let source_id: String = c
                    .query_row("SELECT source_id FROM chunks WHERE id='ch1'", [], |r| r.get(0))
                    .unwrap_or_default();
                vec![Citation {
                    source_id,
                    chunk_id: "ch1".into(),
                    source_name: "讲义".into(),
                    loc: None,
                    char_start: Some(0),
                    char_end: Some(10),
                    snippet: q.into(),
                    quote: q.into(),
                    superseded_quote: None,
                    reanchored_at: None,
                    score: 0.9,
                    cited: true,
                }]
            })
            .unwrap_or_default();
        c.execute(
            "INSERT INTO chat_messages (id, thread_id, role, text, citations, created_at, retrieval, fabricated_dropped, answer_state)
             VALUES (?1, ?2, 'assistant', '答案', ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                crate::db::new_id(),
                thread,
                if cites.is_empty() { None } else { Some(serde_json::to_string(&cites).unwrap()) },
                created,
                meta.map(|m| serde_json::to_string(&m).unwrap()),
                fabricated,
                state
            ],
        )
        .unwrap();
    }

    #[test]
    fn empty_history_says_so_instead_of_reporting_zeroes_as_health() {
        let f = fixture();
        let c = f.st.db.lock().unwrap();
        let h = health(&c, Some(&f.sid), 30).unwrap();
        assert_eq!(h.answers, 0);
        assert_eq!(h.signals.len(), 1);
        assert_eq!(h.signals[0].level, "info");
    }

    #[test]
    fn a_healthy_run_reports_ok_and_counts_the_states_that_occurred() {
        let f = fixture();
        let c = f.st.db.lock().unwrap();
        thread(&c, &f.sid, "t1", None);
        let meta = || RetrievalMeta { hits: 3, ..Default::default() };
        let now = crate::db::now_ms();
        msg(&c, "t1", now + 10, Some("completed"), Some(meta()), Some("特征值就是缩放因子。"), Some(0));
        msg(&c, "t1", now + 11, Some("completed"), Some(meta()), Some("特征值就是缩放因子。"), None);

        let h = health(&c, Some(&f.sid), 30).unwrap();
        assert_eq!(h.answers, 2);
        assert_eq!(h.grounded, 2);
        assert_eq!(h.cited, 2);
        assert_eq!(h.states.len(), 1);
        assert_eq!(h.states[0].state, "completed");
        assert_eq!(h.states[0].count, 2);
        assert_eq!(h.snapshots.exact, 2);
        assert_eq!(h.snapshots.drift_pct(), 0.0);
        assert_eq!(h.signals[0].level, "ok");
    }

    #[test]
    fn failure_abstention_and_fabrication_each_raise_a_warning() {
        let f = fixture();
        let c = f.st.db.lock().unwrap();
        thread(&c, &f.sid, "t1", None);
        let grounded = RetrievalMeta { hits: 2, ..Default::default() };
        let empty = RetrievalMeta { hits: 0, abstained: true, ..Default::default() };
        let now = crate::db::now_ms();
        for i in 0..4 {
            msg(&c, "t1", now + 10 + i, Some("failed"), Some(grounded.clone()), None, None);
        }
        for i in 0..5 {
            msg(&c, "t1", now + 20 + i, Some("completed"), Some(empty.clone()), None, None);
        }
        msg(&c, "t1", now + 30, Some("completed"), Some(grounded.clone()), None, Some(3));
        msg(&c, "t1", now + 31, Some("truncated"), Some(grounded.clone()), None, None);

        let h = health(&c, Some(&f.sid), 30).unwrap();
        assert_eq!(h.answers, 11);
        assert_eq!(h.fabricated_total, 3);
        assert_eq!(h.fabricated_answers, 1);
        let texts: Vec<&str> = h.signals.iter().map(|s| s.text.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("模型侧不稳定")));
        assert!(texts.iter().any(|t| t.contains("找不到依据")));
        assert!(texts.iter().any(|t| t.contains("伪造引用 3 条")));
        assert!(texts.iter().any(|t| t.contains("被截断")));
        assert!(h.signals.iter().all(|s| s.level != "ok"));
    }

    #[test]
    fn drift_is_counted_against_the_current_text_and_reanchored_is_flagged() {
        let f = fixture();
        let c = f.st.db.lock().unwrap();
        thread(&c, &f.sid, "t1", None);
        // Snapshot matches the source.
        let now = crate::db::now_ms();
        msg(&c, "t1", now + 10, Some("completed"), None, Some("特征值就是缩放因子。"), None);
        // Snapshot no longer matches (source was rewritten after this answer).
        let mut stale = vec![Citation {
            source_id: f.src.clone(),
            chunk_id: "ch1".into(),
            source_name: "讲义".into(),
            loc: None,
            char_start: Some(0),
            char_end: Some(10),
            snippet: "这句已经被改掉了。".into(),
            quote: "这句已经被改掉了。".into(),
            superseded_quote: None,
            reanchored_at: None,
            score: 0.9,
            cited: true,
        }];
        stale[0].superseded_quote = Some("旧句子".into());
        stale[0].reanchored_at = Some(99);
        c.execute(
            "INSERT INTO chat_messages (id, thread_id, role, text, citations, created_at)
             VALUES ('m2', 't1', 'assistant', '答案', ?1, ?2)",
            rusqlite::params![serde_json::to_string(&stale).unwrap(), now + 20],
        )
        .unwrap();

        let h = health(&c, Some(&f.sid), 30).unwrap();
        assert_eq!(h.snapshots.checked, 2);
        assert_eq!(h.snapshots.exact, 1);
        assert_eq!(h.snapshots.drifted, 1);
        assert_eq!(h.snapshots.reanchored, 1);
        assert!((h.snapshots.drift_pct() - 50.0).abs() < 1e-9);
    }

    #[test]
    fn window_and_subject_scoping_exclude_what_they_should() {
        let f = fixture();
        let c = f.st.db.lock().unwrap();
        let other = crate::repo::insert_subject(&c, "英语", None, None, None).unwrap();
        thread(&c, &f.sid, "t1", None);
        thread(&c, &other, "t2", None);
        let now = crate::db::now_ms();
        msg(&c, "t1", now, Some("completed"), None, None, None);
        msg(&c, "t2", now, Some("completed"), None, None, None);
        // Outside the 30-day window.
        msg(&c, "t1", now - 40 * 86_400_000, Some("failed"), None, None, None);

        let scoped = health(&c, Some(&f.sid), 30).unwrap();
        assert_eq!(scoped.answers, 1);
        let all = health(&c, None, 30).unwrap();
        assert_eq!(all.answers, 2);
        let forever = health(&c, None, 0).unwrap();
        assert_eq!(forever.answers, 3);
        assert_eq!(forever.states.len(), 2); // completed + failed
    }

    #[test]
    fn rolled_threads_are_counted_once_each() {
        let f = fixture();
        let c = f.st.db.lock().unwrap();
        thread(&c, &f.sid, "t0", None);
        thread(&c, &f.sid, "t1", Some("t0"));
        let now = crate::db::now_ms();
        msg(&c, "t1", now + 10, Some("completed"), None, None, None);
        msg(&c, "t1", now + 11, Some("completed"), None, None, None);
        msg(&c, "t0", now + 12, Some("completed"), None, None, None);
        let h = health(&c, Some(&f.sid), 30).unwrap();
        assert_eq!(h.rolled_threads, 1);
    }
}
