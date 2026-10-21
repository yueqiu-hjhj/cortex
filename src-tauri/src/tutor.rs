//! Tutor bench — 支架式陪练，外加一条学情校准。
//!
//! 知源's two ideas worth stealing:
//!
//! 1. **A scaffold ladder that moves both ways.** Five levels, from "just
//!    explain it" (1) to "no help, judge me" (5). Getting it right *while
//!    sure of yourself* climbs; getting it wrong drops a rung. A partial
//!    answer holds. The expertise-reversal effect is real: scaffolding that
//!    helped a beginner is interference for someone who already knows the
//!    material, so the ladder must be able to go down as well as up.
//!
//! 2. **Calibration.** "Wrong but certain" is a different failure from
//!    "wrong and unsure" — the first is a misconception that will survive
//!    the next review, so it lands in a next-day queue ("先修这个") instead
//!    of being averaged into a mastery number.
//!
//! The verdict line is machine-read; the ladder movement is rules, not the
//! model's opinion. Below 8 turns the UI refuses to claim anything about
//! your level — a small sample is not a diagnosis.

use crate::db::AppState;
use crate::error::{Error, Result};
use crate::llm::Llm;
use crate::repo;
use rusqlite::{params, Connection};
use serde::Serialize;
use tauri::Manager;

/// System prompt for one tutor turn. Registered in `prompts`.
pub const SYSTEM: &str = r#"你是陪练教练，在一个五档支架梯子上教一个学生。当前档位决定了你这一轮能给学生多少帮助：

- 第 1 档「直接讲」：把知识点讲清楚，可以给完整解释和例子
- 第 2 档「给线索」：不给答案，给关键线索和思考方向
- 第 3 档「提问引导」：只问引导性问题，用问题带学生自己想出来
- 第 4 档「只给目标」：只说"做到什么算会"，不给任何路径
- 第 5 档「纯自测」：只出题和判对错，不教任何内容

规则：
1. 第一行必须是 `VERDICT: correct`、`VERDICT: partial` 或 `VERDICT: wrong`，判定学生这一轮的回答——这一行是机器读的，必须严格按格式
2. 然后另起一行写你这一轮的回复：按当前档位决定讲多少；引用学生材料里的事实，不要编
3. 判定只看这轮回答：完全正确才 correct；方向对有关键错误或遗漏算 partial；跑题或错误算 wrong
4. 学生自带信心评分（1=完全没底，5=非常确定），它只用于统计，不影响你的判定
5. 回复 150–400 字，中文，直接写内容，不要复述规则"#;

/// The five rungs, in order. The number is what the ladder moves on; the
/// label is what the UI shows.
pub const LEVELS: &[(i64, &str)] = &[
    (1, "直接讲"),
    (2, "给线索"),
    (3, "提问引导"),
    (4, "只给目标"),
    (5, "纯自测"),
];

pub fn level_label(level: i64) -> &'static str {
    LEVELS
        .iter()
        .find(|(l, _)| *l == level.clamp(1, 5))
        .map(|(_, name)| *name)
        .unwrap_or("提问引导")
}

/// Ladder movement, pure so it can be tested without a model: right *and*
/// sure climbs; wrong drops; partial holds. Confidence below 4 never
/// climbs — a lucky guess is not evidence of mastery.
pub fn next_level(level: i64, verdict: &str, confidence: i64) -> i64 {
    match verdict {
        "correct" => {
            if confidence >= 4 {
                (level + 1).min(5)
            } else {
                level
            }
        }
        "wrong" => (level - 1).max(1),
        _ => level,
    }
}

/// Wrong *and* sure is the calibration signal. Wrong and unsure is just
/// learning; it does not earn a next-day slot.
pub fn is_misjudged(verdict: &str, confidence: i64) -> bool {
    verdict == "wrong" && confidence >= 4
}

/// Initial rung from the framework's own mastery estimate: cold points get
/// more scaffolding, solid ones get tested. No node → the middle rung.
pub fn initial_level(mastery: Option<f64>) -> i64 {
    match mastery {
        Some(m) if m < 0.4 => 2,
        Some(m) if m < 0.75 => 3,
        Some(_) => 4,
        None => 3,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TutorSession {
    pub id: String,
    pub subject_id: String,
    pub subject_name: String,
    pub node_id: Option<String>,
    pub node_title: Option<String>,
    pub title: String,
    pub level: i64,
    pub level_label: String,
    pub turns: i64,
    pub status: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TutorTurn {
    pub id: String,
    pub role: String,
    pub text: String,
    pub level: i64,
    pub confidence: Option<i64>,
    pub verdict: Option<String>,
    pub created_at: i64,
}

/// What one exchange produced. `misjudged` true means this turn also opened
/// a next-day calibration slot.
#[derive(Debug, Clone, Serialize)]
pub struct TutorExchange {
    pub session_id: String,
    pub verdict: String,
    pub reply: String,
    pub level: i64,
    pub level_label: String,
    pub turns: i64,
    pub misjudged: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TutorOpened {
    pub session: TutorSession,
    pub opening: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TutorMisjudged {
    pub id: String,
    pub session_id: String,
    pub subject_id: String,
    pub node_id: Option<String>,
    pub node_title: Option<String>,
    pub question: String,
    pub answer: String,
    pub scheduled_for: i64,
    pub due: bool,
    pub created_at: i64,
}

/// Read the machine line off a reply. Anything unrecognized degrades to
/// "partial": the ladder holds rather than guessing.
pub fn parse_verdict(raw: &str) -> (String, String) {
    for line in raw.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("VERDICT:") {
            let v = rest.trim().to_lowercase();
            let v = v.split_whitespace().next().unwrap_or("").trim_end_matches(['.', '。', ',', '，']);
            if matches!(v, "correct" | "partial" | "wrong") {
                let body = raw
                    .lines()
                    .skip_while(|l| !l.trim().to_uppercase().starts_with("VERDICT:"))
                    .skip(1)
                    .collect::<Vec<_>>()
                    .join("\n");
                return (v.to_string(), body.trim().to_string());
            }
        }
    }
    ("partial".to_string(), raw.trim().to_string())
}

fn session_row(conn: &Connection, id: &str) -> Result<TutorSession> {
    conn.query_row(
        "SELECT s.id, s.subject_id, sub.name, s.node_id, n.title, s.title, s.level, s.turns, s.status, s.updated_at
           FROM tutor_sessions s
           JOIN subjects sub ON sub.id = s.subject_id
           LEFT JOIN knowledge_nodes n ON n.id = s.node_id
          WHERE s.id=?1",
        params![id],
        |r| {
            let level: i64 = r.get(6)?;
            Ok(TutorSession {
                id: r.get(0)?,
                subject_id: r.get(1)?,
                subject_name: r.get(2)?,
                node_id: r.get(3)?,
                node_title: r.get(4)?,
                title: r.get(5)?,
                level,
                level_label: level_label(level).to_string(),
                turns: r.get(7)?,
                status: r.get(8)?,
                updated_at: r.get(9)?,
            })
        },
    )
    .map_err(|_| Error::Other(format!("陪练会话不存在：{id}")))
}

/// Everything the model needs for one turn, gathered under the DB lock so
/// the prompt can never see something the writer didn't.
struct TurnContext {
    session: TutorSession,
    node_summary: Option<String>,
    node_doc: Option<String>,
    history: Vec<TutorTurn>,
}

fn load_context(conn: &Connection, session_id: &str) -> Result<TurnContext> {
    let session = session_row(conn, session_id)?;
    let (node_summary, node_doc) = match &session.node_id {
        Some(nid) => {
            let summary = repo::get_knowledge_node(conn, nid).ok().and_then(|n| n.summary);
            let doc: Option<String> = conn
                .query_row(
                    "SELECT body FROM node_docs WHERE node_id=?1",
                    params![nid],
                    |r| r.get(0),
                )
                .ok();
            (summary, doc)
        }
        None => (None, None),
    };
    let mut st = conn.prepare(
        "SELECT id, role, text, level, confidence, verdict, created_at
           FROM tutor_turns WHERE session_id=?1 ORDER BY created_at DESC LIMIT 10",
    )?;
    let rows = st.query_map(params![session_id], |r| {
        Ok(TutorTurn {
            id: r.get(0)?,
            role: r.get(1)?,
            text: r.get(2)?,
            level: r.get(3)?,
            confidence: r.get(4)?,
            verdict: r.get(5)?,
            created_at: r.get(6)?,
        })
    })?;
    let mut history: Vec<TutorTurn> = rows.collect::<rusqlite::Result<_>>()?;
    history.reverse();
    Ok(TurnContext { session, node_summary, node_doc, history })
}

fn build_turn_prompt(ctx: &TurnContext, answer: &str, confidence: i64) -> String {
    let s = &ctx.session;
    let mut p = format!(
        "陪练对象：{}\n当前档位：第 {} 档「{}」\n学生信心：{} / 5\n\n",
        s.node_title.clone().unwrap_or_else(|| s.subject_name.clone()),
        s.level,
        level_label(s.level),
        confidence
    );
    if let Some(sum) = &ctx.node_summary {
        p.push_str(&format!("框架摘要：{}\n", sum));
    }
    if let Some(doc) = &ctx.node_doc {
        // Ground the coaching in the doc the user already generated — the
        // tutor should not freelance facts the framework doesn't have.
        let excerpt: String = doc.chars().take(1800).collect();
        p.push_str(&format!("知识点文档（据此教，不要超出它）：\n{}\n", excerpt));
    }
    if !ctx.history.is_empty() {
        p.push_str("\n最近的对话：\n");
        for t in &ctx.history {
            let who = if t.role == "user" { "学生" } else { "教练" };
            p.push_str(&format!("{}：{}\n", who, t.text.chars().take(400).collect::<String>()));
        }
    }
    p.push_str(&format!("\n学生这一轮的回答：{}\n\n按规则输出 VERDICT 行和你的回复。", answer));
    p
}

fn append_turn(
    conn: &Connection,
    session_id: &str,
    role: &str,
    text: &str,
    level: i64,
    confidence: Option<i64>,
    verdict: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO tutor_turns (id, session_id, role, text, level, confidence, verdict, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            crate::db::new_id(),
            session_id,
            role,
            text,
            level,
            confidence,
            verdict,
            crate::db::now_ms()
        ],
    )?;
    Ok(())
}

/// Create a session and ask for the opening probe. The first rung comes from
/// the framework's mastery estimate, not from the model.
pub async fn start(
    app: tauri::AppHandle,
    subject_id: String,
    node_id: Option<String>,
) -> Result<TutorOpened> {
    tauri::async_runtime::spawn_blocking(move || -> Result<TutorOpened> {
        let state = app.state::<AppState>();
        let model = {
            let c = state.db.lock().unwrap();
            let keys = crate::commands::read_keys(&c)?;
            crate::commands::load_model(&c, &keys, "chat")?
        };
        let c = state.db.lock().unwrap();
        let (title, mastery) = match &node_id {
            Some(nid) => {
                let n = repo::get_knowledge_node(&c, nid)?;
                (n.title.clone(), Some(n.mastery))
            }
            None => {
                let name: String = c
                    .query_row("SELECT name FROM subjects WHERE id=?1", params![subject_id], |r| r.get(0))
                    .unwrap_or_else(|_| "学科".into());
                (name, None)
            }
        };
        let level = initial_level(mastery);
        let id = crate::db::new_id();
        let now = crate::db::now_ms();
        c.execute(
            "INSERT INTO tutor_sessions (id, subject_id, node_id, title, level, turns, status, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,0,'active',?6,?6)",
            params![id, subject_id, node_id, title, level, now],
        )?;
        let session = session_row(&c, &id)?;
        let ctx = load_context(&c, &id)?;
        let prompt = build_turn_prompt(&ctx, "（开始，先给我第一道题）", 3);
        let raw = model.complete(&crate::prompts::text(crate::prompts::TUTOR_TURN), &prompt)?;
        let (_verdict, opening) = parse_verdict(&raw);
        let opening = if opening.trim().is_empty() { raw.trim().to_string() } else { opening };
        append_turn(&c, &id, "tutor", &opening, level, None, None)?;
        crate::tokens::record(&c, "chat", &model.name(), &prompt, &opening, Some(&subject_id));
        Ok(TutorOpened { session, opening })
    })
    .await
    .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

/// One exchange: judge the answer, move the ladder by rule, feed BKT, and
/// open a calibration slot when the student was wrong but sure.
pub fn reply(
    conn: &Connection,
    session_id: &str,
    answer: &str,
    confidence: i64,
    model: &dyn Llm,
) -> Result<TutorExchange> {
    if answer.trim().is_empty() {
        return Err(Error::Other("回答不能为空".into()));
    }
    if !(1..=5).contains(&confidence) {
        return Err(Error::Other("信心评分必须是 1–5".into()));
    }
    let ctx = load_context(conn, session_id)?;
    let answer_owned = answer.trim().to_string();
    let prompt = build_turn_prompt(&ctx, &answer_owned, confidence);
    let raw = model.complete(&crate::prompts::text(crate::prompts::TUTOR_TURN), &prompt)?;
    let (verdict, reply_text) = parse_verdict(&raw);
    let reply_text = if reply_text.trim().is_empty() { raw.trim().to_string() } else { reply_text };

    let level = next_level(ctx.session.level, &verdict, confidence);
    let now = crate::db::now_ms();

    append_turn(conn, session_id, "user", &answer_owned, ctx.session.level, Some(confidence), Some(&verdict))?;
    append_turn(conn, session_id, "tutor", &reply_text, level, None, Some(&verdict))?;
    conn.execute(
        "UPDATE tutor_sessions SET level=?1, turns=turns+1, updated_at=?2 WHERE id=?3",
        params![level, now, session_id],
    )?;

    // The framework's mastery estimate learns from every answered turn.
    // Low-confidence answers are ignored by apply_observation itself
    // (BKT_MIN_CONFIDENCE) — a guess must not move mastery.
    let mut misjudged = false;
    if let Some(nid) = &ctx.session.node_id {
        let correct = verdict == "correct";
        let note: String = answer_owned.chars().take(120).collect();
        let _ = repo::apply_observation(
            conn,
            nid,
            correct,
            confidence as f64 / 5.0,
            "tutor",
            Some(&note),
        );
    }
    if is_misjudged(&verdict, confidence) {
        misjudged = true;
        let question = ctx
            .history
            .iter()
            .rev()
            .find(|t| t.role == "tutor")
            .map(|t| t.text.chars().take(300).collect::<String>())
            .unwrap_or_else(|| ctx.session.title.clone());
        conn.execute(
            "INSERT INTO tutor_misjudged
               (id, session_id, subject_id, node_id, question, answer, scheduled_for, resolved, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,0,?8)",
            params![
                crate::db::new_id(),
                session_id,
                ctx.session.subject_id,
                ctx.session.node_id,
                question,
                answer_owned.chars().take(300).collect::<String>(),
                next_midnight(now),
                now
            ],
        )?;
    }
    crate::tokens::record(conn, "chat", &model.name(), &prompt, &reply_text, Some(&ctx.session.subject_id));
    Ok(TutorExchange {
        session_id: session_id.to_string(),
        verdict,
        reply: reply_text,
        level,
        level_label: level_label(level).to_string(),
        turns: ctx.session.turns + 1,
        misjudged,
    })
}

fn next_midnight(now_ms: i64) -> i64 {
    (now_ms / 86_400_000 + 1) * 86_400_000
}

pub fn sessions(conn: &Connection, subject_id: &str) -> Result<Vec<TutorSession>> {
    let mut st = conn.prepare(
        "SELECT s.id, s.subject_id, sub.name, s.node_id, n.title, s.title, s.level, s.turns, s.status, s.updated_at
           FROM tutor_sessions s
           JOIN subjects sub ON sub.id = s.subject_id
           LEFT JOIN knowledge_nodes n ON n.id = s.node_id
          WHERE s.subject_id=?1 AND s.status='active'
          ORDER BY s.updated_at DESC
          LIMIT 30",
    )?;
    let rows = st.query_map(params![subject_id], |r| {
        let level: i64 = r.get(6)?;
        Ok(TutorSession {
            id: r.get(0)?,
            subject_id: r.get(1)?,
            subject_name: r.get(2)?,
            node_id: r.get(3)?,
            node_title: r.get(4)?,
            title: r.get(5)?,
            level,
            level_label: level_label(level).to_string(),
            turns: r.get(7)?,
            status: r.get(8)?,
            updated_at: r.get(9)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn history(conn: &Connection, session_id: &str) -> Result<Vec<TutorTurn>> {
    let mut st = conn.prepare(
        "SELECT id, role, text, level, confidence, verdict, created_at
           FROM tutor_turns WHERE session_id=?1 ORDER BY created_at ASC",
    )?;
    let rows = st.query_map(params![session_id], |r| {
        Ok(TutorTurn {
            id: r.get(0)?,
            role: r.get(1)?,
            text: r.get(2)?,
            level: r.get(3)?,
            confidence: r.get(4)?,
            verdict: r.get(5)?,
            created_at: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn end_session(conn: &Connection, session_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE tutor_sessions SET status='done', updated_at=?1 WHERE id=?2",
        params![crate::db::now_ms(), session_id],
    )?;
    Ok(())
}

/// Unresolved calibration slots, soonest first. `due` is computed, not
/// stored: the slot exists the moment it opens, it just isn't "先修" until
/// the next day.
pub fn misjudged_list(conn: &Connection) -> Result<Vec<TutorMisjudged>> {
    let now = crate::db::now_ms();
    let mut st = conn.prepare(
        "SELECT m.id, m.session_id, m.subject_id, m.node_id, n.title, m.question, m.answer,
                m.scheduled_for, m.created_at
           FROM tutor_misjudged m
           LEFT JOIN knowledge_nodes n ON n.id = m.node_id
          WHERE m.resolved=0
          ORDER BY m.scheduled_for ASC
          LIMIT 50",
    )?;
    let rows = st.query_map(params![], |r| {
        let scheduled: i64 = r.get(7)?;
        Ok(TutorMisjudged {
            id: r.get(0)?,
            session_id: r.get(1)?,
            subject_id: r.get(2)?,
            node_id: r.get(3)?,
            node_title: r.get(4)?,
            question: r.get(5)?,
            answer: r.get(6)?,
            scheduled_for: scheduled,
            due: scheduled <= now,
            created_at: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn misjudged_resolve(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "UPDATE tutor_misjudged SET resolved=1 WHERE id=?1",
        params![id],
    )?;
    Ok(())
}

pub fn due_count(conn: &Connection) -> Result<i64> {
    let now = crate::db::now_ms();
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tutor_misjudged WHERE resolved=0 AND scheduled_for<=?1",
        params![now],
        |r| r.get(0),
    )?;
    Ok(n)
}

#[tauri::command]
pub async fn tutor_start(
    app: tauri::AppHandle,
    subject_id: String,
    node_id: Option<String>,
) -> Result<TutorOpened> {
    start(app, subject_id, node_id).await
}

#[tauri::command]
pub async fn tutor_reply(
    app: tauri::AppHandle,
    session_id: String,
    answer: String,
    confidence: i64,
) -> Result<TutorExchange> {
    tauri::async_runtime::spawn_blocking(move || -> Result<TutorExchange> {
        let state = app.state::<AppState>();
        let model = {
            let c = state.db.lock().unwrap();
            let keys = crate::commands::read_keys(&c)?;
            crate::commands::load_model(&c, &keys, "chat")?
        };
        let c = state.db.lock().unwrap();
        reply(&c, &session_id, &answer, confidence, model.as_ref())
    })
    .await
    .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

#[tauri::command]
pub fn tutor_sessions(state: tauri::State<'_, AppState>, subject_id: String) -> Result<Vec<TutorSession>> {
    let c = state.db.lock().unwrap();
    sessions(&c, &subject_id)
}

#[tauri::command]
pub fn tutor_history(state: tauri::State<'_, AppState>, session_id: String) -> Result<Vec<TutorTurn>> {
    let c = state.db.lock().unwrap();
    history(&c, &session_id)
}

#[tauri::command]
pub fn tutor_end(state: tauri::State<'_, AppState>, session_id: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    end_session(&c, &session_id)
}

#[tauri::command]
pub fn tutor_misjudged(state: tauri::State<'_, AppState>) -> Result<Vec<TutorMisjudged>> {
    let c = state.db.lock().unwrap();
    misjudged_list(&c)
}

#[tauri::command]
pub fn tutor_misjudged_resolve(state: tauri::State<'_, AppState>, id: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    misjudged_resolve(&c, &id)
}

#[tauri::command]
pub fn tutor_due_count(state: tauri::State<'_, AppState>) -> Result<i64> {
    let c = state.db.lock().unwrap();
    due_count(&c)
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

    fn seeded() -> (AppState, String, String) {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let sid = repo::insert_subject(&c, "网络", None, None, None).unwrap();
        let nid = {
            let id = crate::db::new_id();
            c.execute(
                "INSERT INTO knowledge_nodes (id, subject_id, node_key, kind, title, mastery, created_at, updated_at)
                 VALUES (?1,?2,'tcp','topic','TCP 握手',0.2,?3,?3)",
                params![id, sid, crate::db::now_ms()],
            )
            .unwrap();
            id
        };
        drop(c);
        (st, sid, nid)
    }

    fn start_session(st: &AppState, sid: &str, nid: &str) -> String {
        // start() is async and needs an AppHandle; the DB half is what
        // matters here, so drive the same SQL the command would.
        let c = st.db.lock().unwrap();
        let id = crate::db::new_id();
        c.execute(
            "INSERT INTO tutor_sessions (id, subject_id, node_id, title, level, turns, status, created_at, updated_at)
             VALUES (?1,?2,?3,'TCP 握手',2,0,'active',?4,?4)",
            params![id, sid, nid, crate::db::now_ms()],
        )
        .unwrap();
        id
    }

    #[test]
    fn verdict_is_parsed_off_the_machine_line() {
        let (v, body) = parse_verdict("VERDICT: correct\n答对了，三次握手里 SYN 的作用是…");
        assert_eq!(v, "correct");
        assert!(body.starts_with("答对了"));
    }

    #[test]
    fn an_unparseable_verdict_holds_the_ladder() {
        let (v, body) = parse_verdict("嗯，这个回答有点意思，我们来看看……");
        assert_eq!(v, "partial", "no verdict line means no ladder move, not a guess");
        assert_eq!(body, "嗯，这个回答有点意思，我们来看看……");
    }

    #[test]
    fn right_and_sure_climbs_right_and_unsure_holds() {
        assert_eq!(next_level(3, "correct", 5), 4);
        assert_eq!(next_level(3, "correct", 2), 3, "a lucky guess is not mastery");
        assert_eq!(next_level(3, "partial", 5), 3);
        assert_eq!(next_level(3, "wrong", 5), 2);
        assert_eq!(next_level(1, "wrong", 1), 1, "the floor holds");
        assert_eq!(next_level(5, "correct", 5), 5, "the ceiling holds");
    }

    #[test]
    fn wrong_and_sure_is_the_calibration_signal() {
        assert!(is_misjudged("wrong", 4));
        assert!(is_misjudged("wrong", 5));
        assert!(!is_misjudged("wrong", 2), "unsure-and-wrong is just learning");
        assert!(!is_misjudged("partial", 5));
        assert!(!is_misjudged("correct", 5));
    }

    #[test]
    fn cold_points_start_low_solid_points_start_high() {
        assert_eq!(initial_level(Some(0.1)), 2);
        assert_eq!(initial_level(Some(0.5)), 3);
        assert_eq!(initial_level(Some(0.9)), 4);
        assert_eq!(initial_level(None), 3);
    }

    #[test]
    fn a_wrong_but_sure_answer_drops_a_rung_and_opens_a_slot() {
        let (st, sid, nid) = seeded();
        let sess = start_session(&st, &sid, &nid);
        let llm = Canned("VERDICT: wrong\n不对，SYN 是同步序列号，不是确认。");
        let c = st.db.lock().unwrap();
        let ex = reply(&c, &sess, "SYN 就是确认连接用的", 5, &llm).unwrap();
        assert_eq!(ex.verdict, "wrong");
        assert_eq!(ex.level, 1, "wrong drops a rung from 2");
        assert!(ex.misjudged);
        let slots = misjudged_list(&c).unwrap();
        assert_eq!(slots.len(), 1);
        assert!(!slots[0].due, "the slot is for tomorrow, not now");
        assert!(slots[0].question.len() > 0);
        // The mastery estimate moved: BKT ignores nothing here (confidence 5).
        let node = repo::get_knowledge_node(&c, &nid).unwrap();
        assert!(node.observations >= 1, "the tutor feeds BKT like a quiz does");
        // BKT's learning term nudges p up after every observation; what a
        // wrong answer must never do is leave the point looking known.
        assert!(node.mastery < 0.3, "a wrong answer stays below the prior: {}", node.mastery);
    }

    #[test]
    fn an_unsure_wrong_answer_drops_a_rung_without_a_slot() {
        let (st, sid, nid) = seeded();
        let sess = start_session(&st, &sid, &nid);
        let llm = Canned("VERDICT: wrong\n再想想：谁先发序列号？");
        let c = st.db.lock().unwrap();
        let ex = reply(&c, &sess, "不知道，瞎猜的", 1, &llm).unwrap();
        assert_eq!(ex.level, 1);
        assert!(!ex.misjudged);
        assert!(misjudged_list(&c).unwrap().is_empty());
        // Confidence 1 → 0.2, below BKT_MIN_CONFIDENCE: a guess must not move mastery.
        let node = repo::get_knowledge_node(&c, &nid).unwrap();
        assert_eq!(node.observations, 0, "a guess does not count as an observation");
    }

    #[test]
    fn an_empty_answer_is_refused() {
        let (st, sid, nid) = seeded();
        let sess = start_session(&st, &sid, &nid);
        let llm = Canned("VERDICT: partial\n");
        let c = st.db.lock().unwrap();
        assert!(reply(&c, &sess, "  ", 3, &llm).is_err());
        assert!(reply(&c, &sess, "答", 9, &llm).is_err(), "confidence out of range");
    }

    #[test]
    fn history_keeps_both_sides_in_order() {
        let (st, sid, nid) = seeded();
        let sess = start_session(&st, &sid, &nid);
        let llm = Canned("VERDICT: partial\n方向对，但还差最后一步。");
        {
            let c = st.db.lock().unwrap();
            append_turn(&c, &sess, "tutor", "第一题：SYN 做什么？", 2, None, None).unwrap();
            reply(&c, &sess, "同步序列号", 4, &llm).unwrap();
        }
        let c = st.db.lock().unwrap();
        let h = history(&c, &sess).unwrap();
        assert_eq!(h.len(), 3);
        assert_eq!(h[0].role, "tutor");
        assert_eq!(h[1].role, "user");
        assert_eq!(h[1].confidence, Some(4));
        assert_eq!(h[1].verdict.as_deref(), Some("partial"));
        assert_eq!(h[2].role, "tutor");
    }

    #[test]
    fn resolving_a_slot_takes_it_off_the_list() {
        let (st, sid, nid) = seeded();
        let sess = start_session(&st, &sid, &nid);
        let llm = Canned("VERDICT: wrong\n不对。");
        let c = st.db.lock().unwrap();
        reply(&c, &sess, "很确定的错误答案", 5, &llm).unwrap();
        let slot = misjudged_list(&c).unwrap().remove(0);
        assert_eq!(due_count(&c).unwrap(), 0, "not due until tomorrow");
        // Force it due: the slot exists, the schedule is the only gate.
        c.execute(
            "UPDATE tutor_misjudged SET scheduled_for=?1 WHERE id=?2",
            params![crate::db::now_ms() - 1000, slot.id],
        )
        .unwrap();
        assert_eq!(due_count(&c).unwrap(), 1);
        misjudged_resolve(&c, &slot.id).unwrap();
        assert!(misjudged_list(&c).unwrap().is_empty());
        assert_eq!(due_count(&c).unwrap(), 0);
    }

    #[test]
    fn ending_a_session_hides_it_from_the_list() {
        let (st, sid, nid) = seeded();
        let sess = start_session(&st, &sid, &nid);
        {
            let c = st.db.lock().unwrap();
            assert_eq!(sessions(&c, &sid).unwrap().len(), 1);
            end_session(&c, &sess).unwrap();
            assert!(sessions(&c, &sid).unwrap().is_empty());
        }
    }

    #[test]
    fn the_prompt_grounds_the_tutor_in_the_generated_doc() {
        let (st, sid, nid) = seeded();
        let sess = start_session(&st, &sid, &nid);
        {
            let c = st.db.lock().unwrap();
            c.execute(
                "INSERT INTO node_docs (node_id, subject_id, body, why, model, created_at, updated_at)
                 VALUES (?1,?2,'SYN 用来同步序列号，是握手的第一步。','','canned',?3,?3)",
                params![nid, sid, crate::db::now_ms()],
            )
            .unwrap();
        }
        let c = st.db.lock().unwrap();
        let ctx = load_context(&c, &sess).unwrap();
        assert!(ctx.node_doc.is_some(), "the tutor should coach from the doc when one exists");
        let p = build_turn_prompt(&ctx, "我的回答", 4);
        assert!(p.contains("SYN 用来同步序列号"), "{p}");
        assert!(p.contains("第 2 档"), "{p}");
        assert!(p.contains("学生信心：4 / 5"), "{p}");
    }
}
