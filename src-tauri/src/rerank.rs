//! Post-retrieval rerank.
//!
//! Cosine similarity (and a hashed-bag-of-words stand-in for it) answers "is
//! this chunk *about* the same topic as the query?" It does not answer "does
//! this chunk *contain* the thing being asked about". Those two come apart in a
//! very common way: a long, generic chunk that name-drops a topic once will
//! score highly against a short, dense chunk that actually explains it. Feeding
//! the long one to the model costs context and dilutes the answer.
//!
//! This reranker blends four cheap, deterministic signals — no model call, no
//! network, so it runs offline and costs nothing per query:
//!
//!   coverage  share of the query's significant terms present in the chunk
//!   density   how often those terms appear per 100 content words (demotes
//!             name-dropping in long documents)
//!   vector    the original similarity, so ranking stays topic-aware
//!   phrase    the whole query appearing verbatim (a very strong signal)
//!
//! It then caps each source's representation in the top-k, because otherwise a
//! single 60-page PDF pushes every other source out of the model's context.
//!
//! Weights are deliberately not tuned to a benchmark grid. They are ordered by
//! how much each signal actually discriminates, and `crate::eval` is there to
//! prove whether this rerank helps or hurts on the corpus it ships with.

use crate::models::ChunkHit;
use crate::repo;

/// Share of query terms that must appear. Dominant signal.
const W_COVERAGE: f32 = 0.40;
/// Term frequency per content word. Catches long, name-dropping documents.
const W_DENSITY: f32 = 0.20;
/// Original similarity — keeps the ranking topic-aware.
const W_VECTOR: f32 = 0.25;
/// The query verbatim inside the chunk. Rare but decisive.
const W_PHRASE: f32 = 0.15;
/// Saturation point for density normalisation (10 hits per 100 words).
const DENSITY_FULL: f32 = 10.0;
/// At most this many chunks from any single source in the result.
pub const MAX_PER_SOURCE: usize = 2;

/// Score a chunk against the query. Higher is better; range ~[0, 1].
pub fn score(query: &str, terms: &[String], hit: &ChunkHit) -> f32 {
    if terms.is_empty() {
        return hit.score.clamp(0.0, 1.0);
    }
    let query_lower = query.trim().to_lowercase();
    let text_lower = hit.text.to_lowercase();

    let matched = terms
        .iter()
        .filter(|t| text_lower.contains(t.as_str()))
        .count();
    let coverage = matched as f32 / terms.len() as f32;

    let words = hit
        .text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .count()
        .max(1);
    let per_100 = matched as f32 / words as f32 * 100.0;
    let density = (per_100 / DENSITY_FULL).clamp(0.0, 1.0);

    let phrase = if !query_lower.is_empty() && text_lower.contains(&query_lower) {
        1.0
    } else {
        0.0
    };
    let vector = hit.score.clamp(0.0, 1.0);

    (W_COVERAGE * coverage
        + W_DENSITY * density
        + W_VECTOR * vector
        + W_PHRASE * phrase)
        .clamp(0.0, 1.0)
}

/// Rerank `hits` for `query` and return at most `k` results.
///
/// Order is best score first, taken in rounds across sources so no single source
/// exceeds [`MAX_PER_SOURCE`] until every other source has had its turn.
pub fn rerank(query: &str, hits: &[ChunkHit], k: usize) -> Vec<ChunkHit> {
    if k == 0 || hits.is_empty() {
        return Vec::new();
    }
    let terms = repo::query_terms(query);
    if terms.is_empty() {
        return hits.iter().take(k).cloned().collect();
    }

    let mut scored: Vec<(f32, &ChunkHit)> =
        hits.iter().map(|h| (score(query, &terms, h), h)).collect();
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.id.cmp(&b.1.id))
    });

    // Bucket by source, remembering each source's first appearance so the
    // round-robin reads from best to worst rather than in hash order.
    let mut order: Vec<String> = Vec::new();
    let mut buckets: std::collections::HashMap<&str, Vec<&ChunkHit>> =
        std::collections::HashMap::new();
    for (_s, h) in scored {
        let sid = h.source_id.as_str();
        if !buckets.contains_key(sid) {
            order.push(h.source_id.clone());
        }
        buckets.entry(sid).or_default().push(h);
    }

    let mut taken: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut out: Vec<ChunkHit> = Vec::with_capacity(k.min(hits.len()));
    loop {
        let mut progressed = false;
        for sid in &order {
            if out.len() >= k {
                break;
            }
            let used = taken.entry(sid.as_str()).or_insert(0);
            if *used >= MAX_PER_SOURCE {
                continue;
            }
            if let Some(next) = buckets.get_mut(sid.as_str()).and_then(|b| {
                if b.is_empty() {
                    None
                } else {
                    Some(b.remove(0))
                }
            }) {
                out.push(next.clone());
                *used += 1;
                progressed = true;
            }
        }
        if out.len() >= k || !progressed {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ChunkHit;

    fn hit(id: &str, source: &str, text: &str, score: f32) -> ChunkHit {
        ChunkHit {
            id: id.into(),
            source_id: source.into(),
            source_name: format!("{source}.md"),
            text: text.into(),
            loc: None,
            char_start: None,
            char_end: None,
            score,
        }
    }

    const DENSE: &str = "Bayesian networks encode conditional independence with a \
        directed acyclic graph. Each node is a random variable and each edge is a \
        conditional dependence. Inference sums out hidden variables to answer \
        conditional probability queries exactly.";
    const FLUFF: &str = "This reading list collects material from across the seminar: \
        linear algebra, optimisation, distributed systems, compilers, compilers again, \
        topology, number theory, linguistics, signal processing, group theory, \
        queueing theory, cryptography, and near the end a short aside noting that \
        Bayesian networks use a directed acyclic graph and nothing more about it.";

    #[test]
    fn a_dense_answer_beats_a_name_dropping_list() {
        let hits = vec![
            hit("fluff", "reading-list", FLUFF, 0.72),
            hit("dense", "bayes-notes", DENSE, 0.61),
        ];
        let out = rerank("bayesian networks conditional independence", &hits, 5);
        assert_eq!(out[0].id, "dense", "density must beat raw similarity");
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn no_source_floods_the_top_k() {
        // Three sources with four matching chunks each, asked for five results.
        // Round-robin gives A, B, C, A, B -- never more than two from any one.
        let mut hits = Vec::new();
        for s in ["a", "b", "c"] {
            for i in 0..4 {
                hits.push(hit(
                    &format!("{s}-{i}"),
                    s,
                    if i % 2 == 0 { DENSE } else { FLUFF },
                    0.9 - s.as_bytes()[0] as f32 * 0.01 - i as f32 * 0.001,
                ));
            }
        }
        let out = rerank("bayesian networks conditional independence", &hits, 5);
        assert_eq!(out.len(), 5);
        for s in ["a", "b", "c"] {
            let n = out.iter().filter(|h| h.source_id == s).count();
            assert!(n <= MAX_PER_SOURCE, "{n} chunks from source {s}");
        }
        // All three sources are represented.
        assert_eq!(
            out.iter().map(|h| h.source_id.as_str()).collect::<std::collections::HashSet<_>>().len(),
            3
        );
    }

    #[test]
    fn respects_k_and_handles_empty_input() {
        let hits = vec![hit("a", "s", DENSE, 0.8)];
        assert_eq!(rerank("bayesian networks", &hits, 0).len(), 0);
        assert_eq!(rerank("bayesian networks", &[], 5).len(), 0);
        assert_eq!(rerank("bayesian networks", &hits, 1).len(), 1);
        // A query with no significant terms falls back to the original order.
        assert_eq!(rerank("the and of", &hits, 1)[0].id, "a");
    }

    /// The reranker blends signals rather than letting one dominate, so the
    /// honest claim is narrower: *all else equal*, a verbatim phrase wins. A
    /// candidate that already has much better vector similarity can still
    /// outrank it -- that is the point of blending.
    #[test]
    fn exact_phrase_breaks_a_tie() {
        let q = "directed acyclic graph";
        let with_phrase = hit("p", "s1", "the answer: directed acyclic graph and more", 0.5);
        let without = hit("n", "s2", "a graph that is acyclic and directed in shape", 0.5);
        let out = rerank(q, &[without, with_phrase], 2);
        assert_eq!(out[0].id, "p", "equal otherwise, a verbatim phrase should win");
    }
}
