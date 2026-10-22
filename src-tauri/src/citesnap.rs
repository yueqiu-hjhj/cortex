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
}
