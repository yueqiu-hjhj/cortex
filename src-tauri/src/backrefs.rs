//! Reverse provenance (KnowNote borrow): the bipartite graph, read backwards.
//! Answers already record which chunks they cited; this module walks every
//! chat message of a subject and indexes those edges from BOTH ends —
//! passage → citing answers ([`source_backrefs`]) and question → answer
//! provenance ([`question_index`]) — so "what did I ever ask about this
//! passage?" and "what did that question actually rest on?" are one scan.
//!
//! Only inline-cited citations count as edges: a chunk that was merely
//! retrieved and shown in the evidence panel is context, not evidence, and
//! back-listing it would inflate a passage's apparent importance.

use rusqlite::Connection;
use serde::Serialize;

use crate::error::Result;

/// One answer that cited a passage, with the question that provoked it.
#[derive(Debug, Clone, Serialize)]
pub struct AnswerRef {
    pub question: String,
    pub answer: String,
    pub created_at: i64,
    pub thread_id: String,
}

/// One passage of a source that at least one answer cited inline.
#[derive(Debug, Clone, Serialize)]
pub struct PassageBackref {
    pub chunk_id: String,
    pub ord: i64,
    pub loc: Option<String>,
    pub excerpt: String,
    pub cites: Vec<AnswerRef>,
}

/// One asked question with its paired answer's provenance.
#[derive(Debug, Clone, Serialize)]
pub struct QuestionEntry {
    pub question: String,
    pub created_at: i64,
    pub thread_id: String,
    /// The paired answer's terminal state (0077), when there was an answer.
    pub answer_state: Option<String>,
    /// Retrieval hits of the paired answer (None when it recorded none).
    pub hits: Option<i64>,
    /// Fabricated citations citecheck stripped from the paired answer.
    pub fabricated_dropped: Option<usize>,
    /// How many citations the answer's evidence panel showed.
    pub citation_count: usize,
}

/// A user question paired with the assistant reply that followed it in the
/// same thread. A question with no reply still pairs (nulls) — an
/// interrupted question is part of the index, not a hole in it.
struct QaPair {
    thread_id: String,
    created_at: i64,
    question: String,
    answer: Option<String>,
    answer_state: Option<String>,
    hits: Option<i64>,
    fabricated_dropped: Option<usize>,
    citation_count: usize,
    /// chunk ids the answer cited inline (the graph edges).
    cited_chunks: Vec<String>,
}

/// Display truncation for questions/answers in the index (char-safe).
fn clip(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        format!("{t}…")
    } else {
        t
    }
}

/// One pass over the subject's chat history, pairing questions with the
/// reply that followed them. Ordered by (thread, time) so the "nearest
/// preceding user row" is thread-local, never cross-thread contamination.
fn walk_qa(conn: &Connection, subject_id: &str) -> Result<Vec<QaPair>> {
    let mut stmt = conn.prepare(
        "SELECT m.thread_id, m.role, m.text, m.citations, m.retrieval, m.fabricated_dropped, m.answer_state, m.created_at
         FROM chat_messages m
         JOIN chat_threads t ON t.id = m.thread_id
         WHERE t.subject_id=?1
         ORDER BY m.thread_id, m.created_at ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![subject_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<i64>>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, i64>(7)?,
        ))
    })?;

    let mut out: Vec<QaPair> = Vec::new();
    let mut last_user: Option<(String, i64)> = None;
    let mut cur_thread: Option<String> = None;
    for row in rows {
        let (thread_id, role, text, citations, retrieval, fabricated, state, created_at) = row?;
        if cur_thread.as_deref() != Some(thread_id.as_str()) {
            // Thread boundary: a question left unanswered in the PREVIOUS
            // thread stays in the index (with nulls), not silently dropped.
            if let (Some(prev), Some((q, q_at))) = (cur_thread.take(), last_user.take()) {
                out.push(QaPair {
                    thread_id: prev,
                    created_at: q_at,
                    question: q,
                    answer: None,
                    answer_state: None,
                    hits: None,
                    fabricated_dropped: None,
                    citation_count: 0,
                    cited_chunks: Vec::new(),
                });
            }
            cur_thread = Some(thread_id.clone());
            last_user = None;
        }
        match role.as_str() {
            "user" => last_user = Some((text, created_at)),
            "assistant" => {
                let (cited_chunks, citation_count) = parse_citation_edges(citations.as_deref());
                let hits: Option<i64> = retrieval
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                    .and_then(|v| v.get("hits").and_then(|h| h.as_i64()));
                out.push(QaPair {
                    thread_id: thread_id.clone(),
                    created_at,
                    question: last_user.as_ref().map(|(q, _)| q.clone()).unwrap_or_default(),
                    answer: Some(text),
                    answer_state: state,
                    hits,
                    fabricated_dropped: fabricated.map(|n| n.max(0) as usize),
                    citation_count,
                    cited_chunks,
                });
                last_user = None; // one question per answer
            }
            _ => {}
        }
    }
    // The final thread's trailing question, if any.
    if let (Some(thread_id), Some((q, q_at))) = (cur_thread, last_user) {
        out.push(QaPair {
            thread_id,
            created_at: q_at,
            question: q,
            answer: None,
            answer_state: None,
            hits: None,
            fabricated_dropped: None,
            citation_count: 0,
            cited_chunks: Vec::new(),
        });
    }
    Ok(out)
}

/// (cited chunk ids, total citations shown) from one persisted citations JSON.
fn parse_citation_edges(citations: Option<&str>) -> (Vec<String>, usize) {
    let Some(json) = citations else {
        return (Vec::new(), 0);
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return (Vec::new(), 0);
    };
    let Some(arr) = v.as_array() else {
        return (Vec::new(), 0);
    };
    let edges = arr
        .iter()
        .filter(|c| c.get("cited").and_then(|x| x.as_bool()).unwrap_or(false))
        .filter_map(|c| c.get("chunk_id").and_then(|x| x.as_str()).map(str::to_string))
        .collect();
    (edges, arr.len())
}

/// Passages of `source_id` that at least one answer cited inline, with the
/// citing answers (and their questions) attached. Sorted by citation count
/// desc — the most-leaned-on passage first.
pub fn source_backrefs(
    conn: &Connection,
    subject_id: &str,
    source_id: &str,
) -> Result<Vec<PassageBackref>> {
    let mut chunks: Vec<(String, i64, Option<String>, String)> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, ord, loc, text FROM chunks WHERE source_id=?1 ORDER BY ord",
        )?;
        let rows = stmt.query_map(rusqlite::params![source_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for r in rows {
            chunks.push(r?);
        }
    }
    if chunks.is_empty() {
        return Ok(Vec::new());
    }

    // chunk_id → citing answers, in one scan.
    let mut edges: std::collections::HashMap<String, Vec<AnswerRef>> = std::collections::HashMap::new();
    for pair in walk_qa(conn, subject_id)? {
        let Some(answer) = pair.answer else { continue };
        if pair.cited_chunks.is_empty() {
            continue;
        }
        let r = AnswerRef {
            question: clip(&pair.question, 120),
            answer: clip(&answer, 160),
            created_at: pair.created_at,
            thread_id: pair.thread_id.clone(),
        };
        for cid in &pair.cited_chunks {
            edges.entry(cid.clone()).or_default().push(r.clone());
        }
    }

    let mut out: Vec<PassageBackref> = chunks
        .into_iter()
        .filter_map(|(id, ord, loc, text)| {
            let cites = edges.remove(&id)?;
            Some(PassageBackref {
                chunk_id: id,
                ord,
                loc,
                excerpt: clip(&text, 200),
                cites,
            })
        })
        .collect();
    out.sort_by(|a, b| b.cites.len().cmp(&a.cites.len()).then(a.ord.cmp(&b.ord)));
    Ok(out)
}

/// Every question asked in the subject, newest first, with the paired
/// answer's provenance (state / retrieval hits / stripped fabrications).
pub fn question_index(conn: &Connection, subject_id: &str, limit: usize) -> Result<Vec<QuestionEntry>> {
    let mut out: Vec<QuestionEntry> = walk_qa(conn, subject_id)?
        .into_iter()
        .map(|p| QuestionEntry {
            question: clip(&p.question, 120),
            created_at: p.created_at,
            thread_id: p.thread_id,
            answer_state: p.answer_state,
            hits: p.hits,
            fabricated_dropped: p.fabricated_dropped,
            citation_count: p.citation_count,
        })
        .collect();
    // Newest first; unanswered questions carry their ask time, so they sit
    // in chronological place rather than sinking.
    out.sort_by_key(|q| std::cmp::Reverse(q.created_at));
    out.truncate(limit.clamp(1, 200));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> crate::db::AppState {
        crate::db::AppState::in_memory().unwrap()
    }

    /// Persist one assistant answer citing the given chunks.
    fn answer(
        c: &Connection,
        sid: &str,
        question: &str,
        text: &str,
        cited_chunks: &[&str],
        state: Option<&str>,
        hits: i64,
        fabricated: Option<usize>,
    ) {
        crate::repo::add_chat_message(c, sid, "user", question).unwrap();
        let cites: Vec<crate::models::Citation> = cited_chunks
            .iter()
            .map(|id| crate::models::Citation {
                source_id: "src1".into(),
                chunk_id: (*id).into(),
                source_name: "讲义".into(),
                loc: None,
                char_start: None,
                char_end: None,
                snippet: "…".into(),
                quote: "…".into(),
                superseded_quote: None,
                reanchored_at: None,
                score: 0.9,
                cited: true,
            })
            .collect();
        crate::repo::add_chat_message_full(
            c,
            sid,
            "assistant",
            text,
            &cites,
            Some(&crate::models::RetrievalMeta { hits, scope: "subject".into(), abstained: false, history_turns: 0, intent: "balanced".into() }),
            fabricated,
            state,
        )
        .unwrap();
    }

    #[test]
    fn backrefs_group_citing_answers_per_passage() {
        let st = state();
        let c = st.db.lock().unwrap();
        let sid = crate::repo::insert_subject(&c, "线代", None, None, None).unwrap();
        let src = crate::repo::insert_source(&c, &sid, None, "讲义", "pdf", None).unwrap();
        // Two chunks; only ch1 gets cited inline.
        for (id, ord) in [("ch1", 0), ("ch2", 1)] {
            c.execute(
                "INSERT INTO chunks (id, source_id, subject_id, ord, text, dim, embedding, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 4, X'00', 1)",
                rusqlite::params![id, src, sid, ord, format!("第{ord}段内容")],
            )
            .unwrap();
        }
        answer(&c, &sid, "什么是特征值？", "特征值是…⟦讲义⟧", &["ch1"], Some("completed"), 4, None);
        answer(&c, &sid, "怎么算？", "先求…⟦讲义⟧", &["ch1"], Some("truncated"), 4, Some(1));
        // A retrieved-but-uncited chunk must NOT become a backref edge.
        answer(&c, &sid, "无关问题", "泛泛而谈", &[], Some("completed"), 4, None);

        let refs = source_backrefs(&c, &sid, &src).unwrap();
        assert_eq!(refs.len(), 1, "only the cited passage gets backrefs");
        assert_eq!(refs[0].chunk_id, "ch1");
        assert_eq!(refs[0].cites.len(), 2);
        assert_eq!(refs[0].cites[0].question, "什么是特征值？");
        assert_eq!(refs[0].cites[1].question, "怎么算？");
    }

    #[test]
    fn question_index_carries_answer_provenance_newest_first() {
        let st = state();
        let c = st.db.lock().unwrap();
        let sid = crate::repo::insert_subject(&c, "线代", None, None, None).unwrap();
        answer(&c, &sid, "第一个问题", "回答一", &[], Some("completed"), 3, None);
        answer(&c, &sid, "第二个问题", "回答二", &[], Some("blocked"), 0, Some(2));

        let idx = question_index(&c, &sid, 50).unwrap();
        assert_eq!(idx.len(), 2);
        assert_eq!(idx[0].question, "第二个问题", "newest first");
        assert_eq!(idx[0].answer_state.as_deref(), Some("blocked"));
        assert_eq!(idx[0].hits, Some(0));
        assert_eq!(idx[0].fabricated_dropped, Some(2));
        assert_eq!(idx[1].answer_state.as_deref(), Some("completed"));
    }

    #[test]
    fn clip_truncates_on_char_boundaries() {
        assert_eq!(clip("abc", 5), "abc");
        assert_eq!(clip("abcdef", 3), "abc…");
        assert_eq!(clip("矩阵分析", 2), "矩阵…");
    }

    /// An interrupted question (user row with no reply) stays in the index
    /// with null provenance — a hole in the history is not a hole in the
    /// index.
    #[test]
    fn unanswered_questions_stay_in_the_index() {
        let st = state();
        let c = st.db.lock().unwrap();
        let sid = crate::repo::insert_subject(&c, "线代", None, None, None).unwrap();
        crate::repo::add_chat_message(&c, &sid, "user", "答到一半的问题").unwrap();
        let idx = question_index(&c, &sid, 50).unwrap();
        assert_eq!(idx.len(), 1);
        assert_eq!(idx[0].question, "答到一半的问题");
        assert!(idx[0].answer_state.is_none());
        assert!(idx[0].hits.is_none());
    }
}
