//! Citation snapshots (KnowNote borrow): every citation persisted with an
//! answer freezes the exact quote + loc + char span at answer time. Later,
//! [`verify_message`] re-checks each frozen quote against the source's
//! CURRENT text under KnowNote's content==slice invariant — so a re-index
//! that moves or rewrites chunks cannot silently invalidate the evidence an
//! answer once showed.
//!
//! Division of labour with `citecheck`: citecheck answers "did the model
//! INVENT this citation?" (checked once, at answer time, against what was
//! retrieved); this module answers "does the frozen quote still match the
//! source TODAY?" (checked on demand, against what is stored now).

use rusqlite::Connection;
use serde::Serialize;
use tauri::State;

use crate::db::AppState;
use crate::error::Result;
use crate::models::Citation;

/// Result of re-checking one frozen quote against the current source text.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum QuoteCheck {
    /// Verbatim at the recorded char span — the snapshot holds exactly.
    Exact,
    /// Verbatim, but at a different span: the source was re-indexed and the
    /// passage moved. The quote itself is intact, so the evidence stands.
    Moved { char_start: i64, char_end: i64 },
    /// No longer verbatim in the current source text — the content changed
    /// under the answer (edited source, re-extraction, or OCR drift).
    Drifted,
    /// The source row is gone — nothing left to check against.
    SourceGone,
}

/// Char (not byte — KnowNote semantics; JS/UI-sliceable) span of `len` bytes
/// starting at `byte_start` inside `text`.
fn char_span(text: &str, byte_start: usize, len: usize) -> (i64, i64) {
    let cs = text[..byte_start].chars().count() as i64;
    let ce = text[..byte_start + len].chars().count() as i64;
    (cs, ce)
}

/// Find `needle` verbatim in `haystack`, returning its char span.
fn find_span(haystack: &str, needle: &str) -> Option<(i64, i64)> {
    haystack
        .find(needle)
        .map(|b| char_span(haystack, b, needle.len()))
}

/// Whitespace-collapsed copy, for the tier-2 fallback: a quote whose only
/// difference from the source is run-together or extra whitespace is still
/// the same passage, and calling it drifted would be a lie.
fn squash_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Re-check one frozen quote. `recorded` is the char span stored with the
/// citation at answer time (when known).
pub fn verify_quote(source_text: &str, quote: &str, recorded: Option<(i64, i64)>) -> QuoteCheck {
    let needle = quote.trim();
    if needle.is_empty() {
        return QuoteCheck::Drifted;
    }
    // Tier 1: verbatim. Exact when it sits where the snapshot recorded it.
    if let Some(span) = find_span(source_text, needle) {
        return match recorded {
            Some(r) if r == span => QuoteCheck::Exact,
            _ => QuoteCheck::Moved { char_start: span.0, char_end: span.1 },
        };
    }
    // Tier 2: whitespace-only drift. The opening fragment anchors an
    // approximate span — the passage is recognizably still there.
    let sq_q = squash_ws(needle);
    let sq_s = squash_ws(source_text);
    if !sq_q.is_empty() && sq_s.contains(&sq_q) {
        let head: String = needle.chars().take(24).collect();
        let span = find_span(source_text, head.trim()).unwrap_or((0, 0));
        return QuoteCheck::Moved { char_start: span.0, char_end: span.1 };
    }
    QuoteCheck::Drifted
}

/// Re-check every citation of one message, in citation order. Sources are
/// fetched once per distinct id (an answer's citations often share one).
/// `None` for a citation that carries no snapshot (pre-0078 answers) —
/// showing those as drifted would be a lie; they simply can't be checked.
pub fn verify_message(conn: &Connection, citations: &[Citation]) -> Vec<Option<QuoteCheck>> {
    let mut cache: std::collections::HashMap<String, Option<String>> = std::collections::HashMap::new();
    citations
        .iter()
        .map(|c| {
            if c.quote.trim().is_empty() {
                return None;
            }
            let content = cache
                .entry(c.source_id.clone())
                .or_insert_with(|| {
                    conn.query_row(
                        "SELECT content FROM sources WHERE id=?1",
                        rusqlite::params![c.source_id],
                        |r| r.get::<_, Option<String>>(0),
                    )
                    .ok()
                    .flatten()
                });
            Some(match content {
                None => QuoteCheck::SourceGone,
                Some(text) => verify_quote(text, &c.quote, c.char_start.zip(c.char_end)),
            })
        })
        .collect()
}

/// Re-check one answer's frozen citation quotes against the sources' CURRENT
/// text (0078, KnowNote's content==slice invariant). One status per citation,
/// in citation order; `None` for pre-0078 citations that carry no snapshot.
/// An empty vec means no such message — a stale tab asking about a cleared
/// thread gets nothing, not an error.
#[tauri::command]
pub fn verify_citation_snapshots(
    state: State<AppState>,
    subject_id: String,
    created_at: i64,
) -> Result<Vec<Option<QuoteCheck>>> {
    let c = state.db.lock().unwrap();
    let cites = crate::repo::message_citations_at(&c, &subject_id, created_at)?.unwrap_or_default();
    Ok(verify_message(&c, &cites))
}

// ---- re-anchoring: turning "drifted" from a diagnosis into a repair ----

/// What re-anchoring a drifted snapshot can honestly do. Decided BEFORE
/// anything is written, so a plan can be shown/tested without mutating
/// evidence, and so the UI never reports a repair that didn't happen.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Reanchor {
    /// The quote is still verbatim in the source — only its position moved.
    /// Nothing about the evidence changes except where it sits, so the text
    /// is NOT rewritten (a re-chunk that also added sentences must not
    /// smuggle them into a quotation the model never made).
    Relocated {
        char_start: i64,
        char_end: i64,
        /// Which chunk now covers the passage, when one can be identified.
        chunk_id: Option<String>,
    },
    /// The cited chunk still exists; its current text is what this citation
    /// pointed at all along, so the snapshot is refreshed from it.
    FromChunk {
        chunk_id: String,
        quote: String,
        char_start: Option<i64>,
        char_end: Option<i64>,
    },
    /// No honest anchor exists. `reason` is one of `no_snapshot`,
    /// `source_gone`, `text_gone` — refusing to guess is the feature: a
    /// re-anchor to a look-alike passage would manufacture evidence.
    Unanchorable { reason: String },
}

/// Decide how to re-anchor one citation against the sources' current state.
/// Precedence matters: a verbatim match in the source wins over the chunk's
/// current text, because the verbatim match proves the quotation itself is
/// intact and only its coordinates need fixing.
pub fn plan_reanchor(conn: &Connection, cite: &Citation) -> Reanchor {
    let quote = cite.quote.trim();
    if quote.is_empty() {
        return Reanchor::Unanchorable { reason: "no_snapshot".into() };
    }
    let content: Option<Option<String>> = conn
        .query_row(
            "SELECT content FROM sources WHERE id=?1",
            rusqlite::params![cite.source_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok();

    match &content {
        None => return Reanchor::Unanchorable { reason: "source_gone".into() },
        Some(text) => {
            if let Some(text) = text.as_deref().filter(|t| !t.trim().is_empty()) {
                if let Some((cs, ce)) = find_span(text, quote) {
                    // Still verbatim: relocate, and name the chunk covering it.
                    let chunk_id: Option<String> = conn
                        .query_row(
                            "SELECT id FROM chunks
                             WHERE source_id=?1 AND char_start IS NOT NULL AND char_end IS NOT NULL
                               AND char_start <= ?2 AND char_end >= ?3
                             ORDER BY ord LIMIT 1",
                            rusqlite::params![cite.source_id, cs, ce],
                            |r| r.get(0),
                        )
                        .ok();
                    return Reanchor::Relocated { char_start: cs, char_end: ce, chunk_id };
                }
            }
        }
    }

    // Not verbatim anywhere in the source: the chunk is the last honest
    // handle on what this citation meant.
    let chunk: Option<(String, Option<i64>, Option<i64>)> = conn
        .query_row(
            "SELECT text, char_start, char_end FROM chunks WHERE id=?1",
            rusqlite::params![cite.chunk_id],
            |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    match chunk {
        Some((text, cs, ce)) if !text.trim().is_empty() => Reanchor::FromChunk {
            chunk_id: cite.chunk_id.clone(),
            quote: text,
            char_start: cs,
            char_end: ce,
        },
        _ => Reanchor::Unanchorable {
            reason: if content
                .as_ref()
                .and_then(|t| t.as_deref())
                .map_or(true, |t| t.trim().is_empty())
            {
                "source_gone".into()
            } else {
                "text_gone".into()
            },
        },
    }
}

/// Apply a plan to the citation, preserving the original quote. The FIRST
/// re-anchor is what `superseded_quote` remembers — a second repair must not
/// overwrite the original evidence with an intermediate state.
pub fn apply_reanchor(cite: &mut Citation, plan: &Reanchor, now: i64) -> bool {
    match plan {
        Reanchor::Unanchorable { .. } => false,
        Reanchor::Relocated { char_start, char_end, chunk_id } => {
            if cite.superseded_quote.is_none() {
                cite.superseded_quote = Some(cite.quote.clone());
            }
            cite.char_start = Some(*char_start);
            cite.char_end = Some(*char_end);
            if let Some(id) = chunk_id {
                cite.chunk_id = id.clone();
            }
            cite.reanchored_at = Some(now);
            true
        }
        Reanchor::FromChunk { chunk_id, quote, char_start, char_end } => {
            if cite.superseded_quote.is_none() {
                cite.superseded_quote = Some(cite.quote.clone());
            }
            cite.chunk_id = chunk_id.clone();
            cite.quote = quote.clone();
            cite.char_start = *char_start;
            cite.char_end = *char_end;
            cite.reanchored_at = Some(now);
            true
        }
    }
}

/// Result of asking to repair one citation: the plan always comes back (so an
/// unanchorable citation can explain itself), the citation only when the
/// repair was actually written.
#[derive(Debug, Clone, Serialize)]
pub struct RefreezeOutcome {
    pub reanchor: Reanchor,
    /// The refreshed citation; `None` when nothing was written.
    pub citation: Option<Citation>,
}

/// Re-anchor ONE citation of one answer to the sources' current text.
/// Addressable by `(created_at, chunk_id)` — the same pair the evidence panel
/// already renders, so the button always acts on the row the user is looking
/// at and on nothing else.
#[tauri::command]
pub fn refreeze_citation(
    state: State<AppState>,
    subject_id: String,
    created_at: i64,
    chunk_id: String,
) -> Result<RefreezeOutcome> {
    let c = state.db.lock().unwrap();
    let mut cites = crate::repo::message_citations_at(&c, &subject_id, created_at)?
        .ok_or_else(|| crate::error::Error::NotFound("answer".into()))?;
    let idx = cites
        .iter()
        .position(|x| x.chunk_id == chunk_id)
        .ok_or_else(|| crate::error::Error::NotFound("citation".into()))?;

    let plan = plan_reanchor(&c, &cites[idx]);
    if !apply_reanchor(&mut cites[idx], &plan, crate::db::now_ms()) {
        return Ok(RefreezeOutcome { reanchor: plan, citation: None });
    }
    crate::repo::update_message_citations(&c, &subject_id, created_at, &cites)?;
    let citation = cites[idx].clone();
    Ok(RefreezeOutcome { reanchor: plan, citation: Some(citation) })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "矩阵乘以特征向量等于缩放，这就是特征值的定义。后面继续展开对角化内容。";

    #[test]
    fn verbatim_at_the_recorded_span_is_exact() {
        let quote = "矩阵乘以特征向量等于缩放，这就是特征值的定义。";
        // char span of the quote inside SRC: 0..19
        assert_eq!(verify_quote(SRC, quote, Some((0, 23))), QuoteCheck::Exact);
    }

    #[test]
    fn verbatim_at_another_span_is_moved() {
        let quote = "后面继续展开对角化内容。";
        assert_eq!(verify_quote(SRC, quote, Some((0, 23))), QuoteCheck::Moved { char_start: 23, char_end: 35 });
    }

    #[test]
    fn unknown_recorded_span_still_reports_the_found_span() {
        let quote = "后面继续展开对角化内容。";
        match verify_quote(SRC, quote, None) {
            QuoteCheck::Moved { char_start, char_end } => {
                assert_eq!((char_start, char_end), (23, 35));
            }
            other => panic!("expected Moved, got {other:?}"),
        }
    }

    #[test]
    fn changed_content_is_drifted() {
        assert!(matches!(
            verify_quote(SRC, "这句话源文里从来没有。", Some((0, 12))),
            QuoteCheck::Drifted
        ));
    }

    #[test]
    fn whitespace_only_drift_is_moved_not_drifted() {
        let sqashed_source = "特征值  是\n线性代数的核心概念。";
        assert!(matches!(
            verify_quote(sqashed_source, "特征值 是 线性代数的核心概念。", None),
            QuoteCheck::Moved { .. }
        ));
    }

    #[test]
    fn empty_quote_is_drifted() {
        assert!(matches!(verify_quote(SRC, "   ", Some((0, 3))), QuoteCheck::Drifted));
    }

    #[test]
    fn squash_ws_collapses_runs_and_newlines() {
        assert_eq!(squash_ws(" a \n b  c "), "a b c");
    }

    #[test]
    fn verify_message_handles_missing_sources_and_reindexing() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let sid = crate::repo::insert_subject(&c, "线代", None, None, None).unwrap();
        let src = crate::repo::insert_source(&c, &sid, None, "讲义", "pdf", None).unwrap();
        crate::repo::finalize_source(&c, &src, "ok", None, Some(SRC), None).unwrap();

        let cite = |quote: &str, cs: i64, ce: i64| Citation {
            source_id: src.clone(),
            chunk_id: "ch1".into(),
            source_name: "讲义".into(),
            loc: Some("p1".into()),
            char_start: Some(cs),
            char_end: Some(ce),
            snippet: quote.chars().take(160).collect(),
            quote: quote.into(),
            superseded_quote: None,
            reanchored_at: None,
            score: 0.9,
            cited: true,
        };
        let gone = Citation { source_id: "no-such-source".into(), ..cite("x", 0, 1) };

        let checks = verify_message(&c, &[
            cite("矩阵乘以特征向量等于缩放，这就是特征值的定义。", 0, 23),
            cite("这句话不在源文里。", 0, 9),
            gone,
        ]);
        assert!(matches!(checks[0], Some(QuoteCheck::Exact)));
        assert!(matches!(checks[1], Some(QuoteCheck::Drifted)));
        assert!(matches!(checks[2], Some(QuoteCheck::SourceGone)));

        // Re-index: same content, passage moved → Moved, not Drifted.
        let moved = "前言。矩阵乘以特征向量等于缩放，这就是特征值的定义。";
        crate::repo::finalize_source(&c, &src, "ok", None, Some(moved), None).unwrap();
        let checks = verify_message(&c, &[cite("矩阵乘以特征向量等于缩放，这就是特征值的定义。", 0, 23)]);
        assert!(matches!(checks[0], Some(QuoteCheck::Moved { char_start: 3, .. })));

        // A pre-0078 citation (no frozen quote) is uncheckable, not drifted.
        let legacy = Citation { quote: String::new(), ..cite("x", 0, 1) };
        assert!(verify_message(&c, std::slice::from_ref(&legacy))[0].is_none());
    }

    // ---- re-anchoring ----

    /// One subject + source + fixed-id chunk, so a citation can address both.
    fn fixture() -> (crate::db::AppState, String, String, String) {
        let st = crate::db::AppState::in_memory().unwrap();
        let (sid, src);
        {
            let c = st.db.lock().unwrap();
            sid = crate::repo::insert_subject(&c, "线代", None, None, None).unwrap();
            src = crate::repo::insert_source(&c, &sid, None, "讲义", "pdf", None).unwrap();
            crate::repo::finalize_source(&c, &src, "ok", None, Some(SRC), None).unwrap();
            c.execute(
                "INSERT INTO chunks (id, source_id, subject_id, ord, text, dim, embedding, created_at, char_start, char_end, vec_space)
                 VALUES ('ch1', ?1, ?2, 0, ?3, 1, X'00', 1, 0, 35, '')",
                rusqlite::params![src, sid, SRC],
            )
            .unwrap();
        }
        (st, sid, src, "ch1".into())
    }

    fn cite_of(source_id: &str, chunk_id: &str, quote: &str, span: (i64, i64)) -> Citation {
        Citation {
            source_id: source_id.into(),
            chunk_id: chunk_id.into(),
            source_name: "讲义".into(),
            loc: Some("p1".into()),
            char_start: Some(span.0),
            char_end: Some(span.1),
            snippet: quote.chars().take(160).collect(),
            quote: quote.into(),
            superseded_quote: None,
            reanchored_at: None,
            score: 0.9,
            cited: true,
        }
    }

    #[test]
    fn verbatim_quote_relocates_instead_of_being_rewritten() {
        let (st, _sid, src, chid) = fixture();
        let c = st.db.lock().unwrap();
        let quote = "后面继续展开对角化内容。";
        let mut cite = cite_of(&src, &chid, quote, (0, 12));

        // The source grew a prefix: same words, new coordinates.
        let grown = format!("前言。{SRC}");
        crate::repo::finalize_source(&c, &src, "ok", None, Some(&grown), None).unwrap();

        let plan = plan_reanchor(&c, &cite);
        match plan {
            Reanchor::Relocated { char_start, char_end, .. } => assert_eq!((char_start, char_end), (26, 38)),
            other => panic!("expected Relocated, got {other:?}"),
        }
        // Relocation rewrites coordinates only — the quotation must stand.
        assert!(apply_reanchor(&mut cite, &plan, 1_000));
        assert_eq!(cite.quote, quote);
        assert_eq!(cite.superseded_quote.as_deref(), Some(quote));
        assert_eq!(cite.char_start, Some(26));
        assert_eq!(cite.reanchored_at, Some(1_000));
    }

    #[test]
    fn rewritten_chunk_refreshes_the_snapshot_from_its_current_text() {
        let (st, _sid, src, chid) = fixture();
        let c = st.db.lock().unwrap();
        let mut cite = cite_of(&src, &chid, "已经不在源文里的旧句子。", (0, 12));

        let rewritten = "重写后的分块：矩阵乘以特征向量等于缩放。";
        crate::repo::finalize_source(&c, &src, "ok", None, Some(rewritten), None).unwrap();
        c.execute("UPDATE chunks SET text=?1, char_start=0, char_end=20 WHERE id='ch1'", rusqlite::params![rewritten])
            .unwrap();

        let plan = plan_reanchor(&c, &cite);
        match &plan {
            Reanchor::FromChunk { quote, .. } => assert_eq!(quote, rewritten),
            other => panic!("expected FromChunk, got {other:?}"),
        }
        assert!(apply_reanchor(&mut cite, &plan, 2_000));
        assert_eq!(cite.quote, rewritten);
        // The sentence the model actually quoted survives as the audit trail.
        assert_eq!(cite.superseded_quote.as_deref(), Some("已经不在源文里的旧句子。"));
    }

    #[test]
    fn a_second_repair_never_overwrites_the_original_quote() {
        let (_st, _sid, src, chid) = fixture();
        let mut cite = cite_of(&src, &chid, "原始引用。", (0, 5));
        let first = Reanchor::FromChunk {
            chunk_id: chid.clone(),
            quote: "第一次重锚后的文本。".into(),
            char_start: Some(0),
            char_end: Some(10),
        };
        let second = Reanchor::FromChunk {
            chunk_id: chid.clone(),
            quote: "第二次重锚后的文本。".into(),
            char_start: Some(0),
            char_end: Some(10),
        };
        assert!(apply_reanchor(&mut cite, &first, 1_000));
        assert!(apply_reanchor(&mut cite, &second, 2_000));
        assert_eq!(cite.quote, "第二次重锚后的文本。");
        assert_eq!(cite.superseded_quote.as_deref(), Some("原始引用。"));
        assert_eq!(cite.reanchored_at, Some(2_000));
    }

    #[test]
    fn missing_source_and_lost_text_refuse_to_anchor() {
        let (st, _sid, src, chid) = fixture();
        let c = st.db.lock().unwrap();
        let mut cite = cite_of(&src, &chid, "源文里没有这句。", (0, 6));
        // Chunk gone AND text gone: no honest anchor exists.
        c.execute("DELETE FROM chunks WHERE id='ch1'", []).unwrap();
        assert_eq!(
            plan_reanchor(&c, &cite),
            Reanchor::Unanchorable { reason: "text_gone".into() }
        );
        assert!(!apply_reanchor(&mut cite, &Reanchor::Unanchorable { reason: "text_gone".into() }, 1));
        assert_eq!(cite.reanchored_at, None);

        // Deleted source: the citation can't be repaired at all.
        crate::repo::delete_source(&c, &src).unwrap();
        assert_eq!(
            plan_reanchor(&c, &cite),
            Reanchor::Unanchorable { reason: "source_gone".into() }
        );

        // A pre-0078 citation has nothing to re-freeze.
        let legacy = cite_of(&src, "x", "", (0, 1));
        assert_eq!(
            plan_reanchor(&c, &legacy),
            Reanchor::Unanchorable { reason: "no_snapshot".into() }
        );
    }

    #[test]
    fn refreeze_roundtrip_writes_back_only_the_citations_column() {
        let (st, sid, src, chid) = fixture();
        let c = st.db.lock().unwrap();

        let old = "已经不在源文里的旧句子。";
        crate::repo::add_chat_message_full(
            &c,
            &sid,
            "assistant",
            "带引用的回答 [1]",
            &[cite_of(&src, &chid, old, (0, 12))],
            Some(&crate::models::RetrievalMeta { hits: 1, ..Default::default() }),
            Some(2),
            Some("completed"),
        )
        .unwrap();

        let msg = crate::repo::list_chat_messages(&c, &sid).unwrap();
        let at = msg[0].created_at;

        // Re-anchor at the DB level (the command wrapper only adds the lock).
        let mut cites = crate::repo::message_citations_at(&c, &sid, at).unwrap().unwrap();
        let rewritten = "重写后的分块：矩阵乘以特征向量等于缩放。";
        crate::repo::finalize_source(&c, &src, "ok", None, Some(rewritten), None).unwrap();
        c.execute("UPDATE chunks SET text=?1, char_start=0, char_end=20 WHERE id='ch1'", rusqlite::params![rewritten])
            .unwrap();
        let plan = plan_reanchor(&c, &cites[0]);
        assert!(matches!(plan, Reanchor::FromChunk { .. }));
        assert!(apply_reanchor(&mut cites[0], &plan, 3_000));
        crate::repo::update_message_citations(&c, &sid, at, &cites).unwrap();

        let after = crate::repo::list_chat_messages(&c, &sid).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].text, "带引用的回答 [1]");
        assert_eq!(after[0].answer_state.as_deref(), Some("completed"));
        assert_eq!(after[0].fabricated_dropped, Some(2));
        assert_eq!(after[0].citations[0].quote, rewritten);
        assert_eq!(after[0].citations[0].superseded_quote.as_deref(), Some(old));
        assert!(matches!(
            verify_message(&c, &after[0].citations)[0],
            Some(QuoteCheck::Exact)
        ));
    }
}
