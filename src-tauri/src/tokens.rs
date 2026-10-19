//! Token accounting — the honest kind (Reviva ships a token dashboard; Cortex
//! had no accounting at all).
//!
//! The `Llm` trait exposes no provider usage numbers, and retrofit-reading
//! fourteen call sites' streaming bodies is a rewrite. So this records an
//! ESTIMATE per completed call — clearly labeled in the UI as 估算 — which is
//! enough to answer the questions people actually ask: where did my budget go
//! (kind/model), when (per day), and how much (total). Honest > precise and
//! absent.
//!
//! Estimate: CJK chars count ~1 token each; latin/other ~4 chars per token
//! (the usual English heuristic). Both sides (prompt = system+user, completion)
//! are counted.

use crate::error::Result;
use rusqlite::{params, Connection};
use serde::Serialize;

/// Rough token estimate for a text. CJK ≈ 1 token/char, latin ≈ 4 chars/token.
pub fn estimate(s: &str) -> i64 {
    let mut cjk = 0i64;
    let mut other = 0i64;
    for c in s.chars() {
        if (c as u32) >= 0x2E80 {
            cjk += 1;
        } else if !c.is_whitespace() {
            other += 1;
        }
    }
    cjk + (other + 3) / 4
}

pub fn record(
    conn: &Connection,
    kind: &str,
    model: &str,
    prompt: &str,
    completion: &str,
    subject_id: Option<&str>,
) {
    let pt = estimate(prompt);
    let ct = estimate(completion);
    let _ = conn.execute(
        "INSERT INTO token_events (id, ts, kind, model, prompt_tokens, completion_tokens, subject_id)
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            crate::db::new_id(),
            crate::db::now_ms(),
            kind,
            model,
            pt,
            ct,
            subject_id
        ],
    );
}

/// One completed LLM call, accounted. `conn` None (rare: no DB handy) still
/// runs the call — it just isn't recorded.
pub fn complete_tracked(
    model: &dyn crate::llm::Llm,
    conn: Option<&Connection>,
    kind: &str,
    sys: &str,
    user: &str,
) -> Result<String> {
    let out = model.complete(sys, user)?;
    if let Some(c) = conn {
        record(c, kind, &model.name(), &format!("{sys}\n{user}"), &out, None);
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
pub struct DayBucket {
    pub day: String, // YYYY-MM-DD
    pub prompt: i64,
    pub completion: i64,
    pub calls: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct KindBucket {
    pub kind: String,
    pub total: i64,
    pub calls: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TokenSummary {
    pub days: i64,
    pub total_prompt: i64,
    pub total_completion: i64,
    pub calls: i64,
    pub by_day: Vec<DayBucket>,
    pub by_kind: Vec<KindBucket>,
    pub by_model: Vec<KindBucket>,
}

/// Usage summary for the last `days` days (default 30, cap 365).
pub fn summary(conn: &Connection, days: i64) -> Result<TokenSummary> {
    let days = days.clamp(1, 365);
    let since = crate::db::now_ms() - days * 86_400_000;
    let (tp, tc, calls): (i64, i64, i64) = conn.query_row(
        "SELECT COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0), COUNT(*)
           FROM token_events WHERE ts >= ?1",
        params![since],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    // Local-day buckets (ms epoch → date in UTC+8 kept simple: shift then divide).
    let mut by_day: Vec<DayBucket> = Vec::new();
    {
        let mut st = conn.prepare(
            "SELECT date((ts + 28800000) / 1000, 'unixepoch') AS d,
                    SUM(prompt_tokens), SUM(completion_tokens), COUNT(*)
               FROM token_events WHERE ts >= ?1 GROUP BY d ORDER BY d"
        )?;
        let rows = st.query_map(params![since], |r| {
            Ok(DayBucket {
                day: r.get(0)?,
                prompt: r.get(1)?,
                completion: r.get(2)?,
                calls: r.get(3)?,
            })
        })?;
        by_day = rows.collect::<rusqlite::Result<_>>()?;
    }
    let by_kind = bucket(conn, since, "kind")?;
    let by_model = bucket(conn, since, "model")?;
    Ok(TokenSummary {
        days,
        total_prompt: tp,
        total_completion: tc,
        calls,
        by_day,
        by_kind,
        by_model,
    })
}

fn bucket(conn: &Connection, since: i64, col: &str) -> Result<Vec<KindBucket>> {
    let sql = format!(
        "SELECT {col}, SUM(prompt_tokens)+SUM(completion_tokens), COUNT(*)
           FROM token_events WHERE ts >= ?1 GROUP BY {col} ORDER BY 2 DESC LIMIT 8"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params![since], |r| {
        Ok(KindBucket {
            kind: r.get(0)?,
            total: r.get(1)?,
            calls: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_separates_cjk_and_latin() {
        // 10 CJK chars ≈ 10 tokens; 20 latin chars ≈ 5 tokens.
        assert_eq!(estimate("矩阵特征值分解方法一"), 10);
        assert_eq!(estimate("abcd efgh ijkl mnop"), 4); // 16 latin + 3 spaces → ceil(16/4)
        // Mixed: 3 CJK + ceil(10 latin / 4) = 3.
        let e = estimate("特征值 eigenvalue");
        assert_eq!(e, 6);
    }

    #[test]
    fn record_and_summarize_by_day_and_kind() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        record(&c, "chat", "deepseek", "你好世界", "回复内容", Some("s1"));
        record(&c, "cheatsheet", "deepseek", "a".repeat(400).as_str(), "b", None);
        let s = summary(&c, 30).unwrap();
        assert_eq!(s.calls, 2);
        assert!(s.total_prompt >= 100);
        assert_eq!(s.by_day.len(), 1);
        assert_eq!(s.by_kind.len(), 2);
        assert!(s.by_kind[0].total >= s.by_kind[1].total);
        assert_eq!(s.by_model.len(), 1);
        // Range beyond data is still coherent.
        let s0 = summary(&c, 365).unwrap();
        assert_eq!(s0.calls, 2);
    }
}
