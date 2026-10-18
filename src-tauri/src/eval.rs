//! Retrieval evaluation harness.
//!
//! Before this module there was no way to answer "does retrieval work?" without
//! hand-clicking a query and eyeballing the results. A retrieval change — a new
//! embedder, a different chunk size, a reranker — was therefore unfalsifiable:
//! you could not tell an improvement from a regression.
//!
//! This builds a **synthetic corpus** with known ground truth, runs the real
//! retrieval path over it (embed → store → sqlite-vec cosine → optional rerank),
//! and reports Recall@k and MRR for four configurations:
//!
//!   vector          cosine only (what the app did before)
//!   rerank          cosine + crate::rerank
//!   hybrid          cosine ∪ keyword, fused by reciprocal rank
//!   hybrid+rerank   both, then reranked (the proposed default)
//!
//! The corpus is deterministic (no sampling, no clock), so the same numbers come
//! back on every machine. That is the point: the harness is cheap enough to run
//! in CI, and `cargo test` fails if a change regresses the numbers.
//!
//! The corpus is honest about its limits. It is synthetic text about textbook
//! topics, so it measures *the pipeline*, not real-world recall. Real documents
//! would need a labelled fixture — which this is a stepping stone to.

use crate::embed::Embedder;
use crate::error::Result;
use crate::repo;
use rusqlite::{params, Connection};
use serde::Serialize;

/// Number of results considered a success.
pub const DEFAULT_K: usize = 5;

/// The eval corpus, inserted under a subject titled `(eval)`. Rows are deleted
/// and rewritten on every run, so the harness is idempotent.
/// `origin = "eval"` marks them for cleanup.
const SUBJECT_NAME: &str = "(eval) 检索评测";

/// (source name, chunks). Twelve real topics plus two long "reading list"
/// documents that name-drop many topics once — the shape that fools pure
/// similarity ranking.
fn corpus() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        (
            "bayes-notes.md",
            vec![
                "Bayesian networks represent joint distributions compactly using a \
                 directed acyclic graph. Each node is a random variable and each edge \
                 encodes a conditional dependence between variables.",
                "Conditional independence is read off the graph structure by d-separation. \
                 Exact inference sums out hidden variables, so conditional probability \
                 queries are answered algebraically rather than by sampling.",
            ],
        ),
        (
            "gradient-descent.md",
            vec![
                "Gradient descent follows the negative gradient of a loss function. The \
                 learning rate scales each step, and momentum accumulates a running \
                 average of past gradients to damp oscillation.",
                "Stochastic gradient descent estimates the gradient from a single \
                 minibatch, trading gradient accuracy for throughput. Adaptive methods \
                 rescale the step per parameter using accumulated second moments.",
            ],
        ),
        (
            "regularisation.md",
            vec![
                "Regularisation constrains model capacity to reduce overfitting. L2 \
                 regularisation penalises large weights, while L1 drives weights to \
                 exactly zero and yields sparse solutions.",
                "Dropout randomly silences a fraction of activations during training, so \
                 the network cannot rely on any single unit. Early stopping halts \
                 training when validation loss stops improving.",
            ],
        ),
        (
            "transformer.md",
            vec![
                "The transformer computes attention between every pair of positions, so \
                 long range dependencies cost the same as short ones. Multi head \
                 attention runs several projections in parallel and concatenates them.",
                "Positional encodings inject order information because attention itself \
                 is permutation invariant. Layer normalisation stabilises training, and \
                 residual connections let gradients reach earlier layers.",
            ],
        ),
        (
            "kmeans.md",
            vec![
                "K means clustering partitions points into k clusters by minimising \
                 within cluster variance. Centroids move to the mean of their members \
                 after each reassignment step.",
                "The algorithm alternates between assigning points to the nearest \
                 centroid and recomputing centroids. It converges to a local optimum, so \
                 restarts with different seeds are standard practice.",
            ],
        ),
        (
            "bias-variance.md",
            vec![
                "Bias is the error from an overly simple model, and variance is the error \
                 from sensitivity to the training sample. Total error decomposes into \
                 the two plus irreducible noise.",
                "Underfitting shows high bias and low variance, while overfitting shows \
                 low bias and high variance. Collecting more data reduces variance but \
                 does nothing for bias.",
            ],
        ),
        (
            "backprop.md",
            vec![
                "Backpropagation applies the chain rule backwards through the \
                 computation graph. At each node the local gradient is multiplied by \
                 the gradient flowing in from above.",
                "Gradients for shared parameters accumulate across every path that uses \
                 them. Vanishing gradients make early layers learn slowly, which \
                 residual connections and normalisation mitigate.",
            ],
        ),
        (
            "bayes-theorem.md",
            vec![
                "Bayes theorem inverts a conditional probability: the posterior equals \
                 the likelihood times the prior divided by the evidence. It is the \
                 foundation of probabilistic inference.",
                "Conjugate priors keep the posterior in the same family as the prior, so \
                 updates stay closed form. Naive Bayes assumes features are independent \
                 given the class.",
            ],
        ),
        (
            "dimensionality.md",
            vec![
                "Principal component analysis projects data onto directions of maximum \
                 variance. The first component captures the largest spread, and later \
                 components are orthogonal to the earlier ones.",
                "Choosing the number of components trades reconstruction error against \
                 compression. Explained variance ratio tells you how much information \
                 each component keeps.",
            ],
        ),
        (
            "softmax.md",
            vec![
                "Softmax turns a vector of logits into a probability distribution. \
                 Subtracting the maximum logit before exponentiating prevents numeric \
                 overflow without changing the result.",
                "Cross entropy loss compares the predicted distribution with the one hot \
                 target. Temperature sharpens or flattens the distribution for sampling.",
            ],
        ),
        (
            "convolution.md",
            vec![
                "Convolutional layers share weights across positions, so a filter that \
                 detects an edge works anywhere in the image. Pooling downsamples \
                 feature maps and adds translation tolerance.",
                "Receptive field grows with depth, so late layers see large regions. \
                 Strided convolutions replace pooling when the downsampling factor \
                 needs to be learned.",
            ],
        ),
        (
            "markov.md",
            vec![
                "A Markov chain moves between states with probabilities that depend only \
                 on the current state. The transition matrix holds those probabilities.",
                "A stationary distribution is unchanged by one more step. Stationarity is \
                 reached when the chain is irreducible and aperiodic, and mixing time \
                 measures how fast that happens.",
            ],
        ),
        // One big "reading list" split into six chunks. Each chunk name-drops
        // many topics, so any query hits several of them — the classic way a
        // single catch-all document crowds every specific note out of the top-k.
        // This is what makes corpus-vs-baseline discrimination real: without a
        // diversity cap, the decoy fills the whole context window.
        (
            "reading-list.md",
            vec![
                "Reading list part one. Bayesian networks, gradient descent,                  regularisation, the transformer paper, k means clustering.",
                "Reading list part two. Bias and variance, backpropagation, bayes                  theorem, principal component analysis, softmax and cross entropy.",
                "Reading list part three. Convolution, markov chains, bayesian \
                 networks again, regularisation survey, the transformer ablations.",
                "Reading list part four. K means clustering chapter nine, bias and \
                 variance, gradient descent variants, softmax temperature, \
                 convolution receptive fields.",
                "Reading list part five. Backpropagation notes, bayes theorem and \
                 conjugate priors, principal component analysis, markov chain \
                 monte carlo, regularisation of deep nets.",
                "Reading list part six. Transformer survey, convolution history, \
                 clustering comparison, bayesian inference, optimisation \
                 schedules, feature selection notes.",
            ],
        ),
        (
            "seminar-notes.md",
            vec![
                "Loose seminar notes. Somebody asked about bayesian networks and whether \
                 conditional independence still holds with latent confounders. Somebody \
                 else asked about transformer attention and positional encodings. A long \
                 digression about the reading room booking system follows, then a \
                 reminder that the deadline for the problem set is next Tuesday, then \
                 notes about coffee, parking, and the department printer.",
            ],
        ),
    ]
}

/// Queries with the source whose chunk should rank first. Some queries
/// deliberately overlap the decoy documents' vocabulary.
fn cases() -> Vec<(&'static str, &'static str)> {
    vec![
        ("how do bayesian networks encode conditional independence", "bayes-notes.md"),
        ("what does d-separation tell you about conditional independence", "bayes-notes.md"),
        ("explain gradient descent and the learning rate", "gradient-descent.md"),
        ("how does momentum damp oscillation in gradient descent", "gradient-descent.md"),
        ("why does L2 regularisation penalise large weights", "regularisation.md"),
        ("how does dropout reduce overfitting during training", "regularisation.md"),
        ("how does multi head attention work in the transformer", "transformer.md"),
        ("why are positional encodings needed for attention", "transformer.md"),
        ("how does k means clustering assign points to centroids", "kmeans.md"),
        ("what is the difference between underfitting and overfitting", "bias-variance.md"),
        ("how does the chain rule give backpropagation its gradients", "backprop.md"),
        ("what makes vanishing gradients slow early layers", "backprop.md"),
        ("what does the prior and likelihood give in bayes theorem", "bayes-theorem.md"),
        ("which axis captures the largest variance in principal component analysis", "dimensionality.md"),
        ("why subtract the maximum logit before softmax", "softmax.md"),
        ("how does weight sharing help convolutional layers", "convolution.md"),
        ("what property does a markov chain transition matrix have", "markov.md"),
    ]
}

#[derive(Debug, Clone, Serialize)]
pub struct EvalRow {
    pub query: String,
    pub expect: String,
    /// Top-k source names, best first.
    pub got: Vec<String>,
    /// 1-based rank of the expected source (None = not in the top k).
    pub rank: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvalReport {
    pub mode: String,
    pub k: usize,
    pub evaluated: usize,
    /// Share of queries whose expected source appears in the top k.
    pub recall_at_k: f32,
    /// Mean reciprocal rank of the expected source.
    pub mrr: f32,
    pub rows: Vec<EvalRow>,
}

pub const MODES: [&str; 4] = ["vector", "rerank", "hybrid", "hybrid+rerank"];

/// The corpus with vectors already computed. Splitting "embed" from "write"
/// keeps every slow (ONNX / network) call inside one blocking hop in the command
/// layer, and makes the scoring path testable without an embedder.
pub struct Seed {
    pub sources: Vec<(String, Vec<(String, Vec<f32>)>)>,
}

/// Embed the corpus. Deterministic given the same embedder.
pub fn embed_corpus(embedder: &dyn Embedder) -> Result<Seed> {
    let mut sources = Vec::with_capacity(corpus().len());
    for (name, chunks) in corpus() {
        let texts: Vec<String> = chunks.into_iter().map(str::to_string).collect();
        let vecs = embedder.embed(&texts)?;
        sources.push((name.to_string(), texts.into_iter().zip(vecs).collect()));
    }
    Ok(Seed { sources })
}

/// Write the corpus into the DB (replacing any previous eval rows).
///
/// The subject is named `(eval) ...` and its sources carry `origin = "eval"`,
/// which is what the cleanup matches on -- there is no flag column on
/// `subjects`, so the name is the marker, and it is prefixed so a user can
/// spot it.
pub fn seed(conn: &Connection, seed: &Seed) -> Result<()> {
    conn.execute(
        "DELETE FROM sources WHERE subject_id IN \
         (SELECT id FROM subjects WHERE name = ?1)",
        params![SUBJECT_NAME],
    )?;
    conn.execute(
        "DELETE FROM subjects WHERE name = ?1",
        params![SUBJECT_NAME],
    )?;

    let subject_id = repo::insert_subject(conn, SUBJECT_NAME, Some("eval"), Some("\u{25c6}"), None)?;
    for (name, chunks) in &seed.sources {
        let source_id = repo::insert_source(conn, &subject_id, None, name, "txt", Some("eval"))?;
        for (i, (text, vec)) in chunks.iter().enumerate() {
            repo::insert_chunk(
                conn,
                &source_id,
                &subject_id,
                None,
                i as i64,
                text,
                None,
                vec.len() as i64,
                &crate::vector::f32s_to_blob(vec),
            )?;
        }
        let joined = chunks
            .iter()
            .map(|(t, _)| t.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        repo::finalize_source(conn, &source_id, "ready", Some("eval"), Some(&joined), None)?;
    }
    Ok(())
}

/// A query with its embedding, ready to score.
pub struct EvalQuery {
    pub query: String,
    pub expect: String,
    pub vec: Vec<f32>,
}

/// Embed every eval query in one call -- again so the slow part happens in the
/// command layer's blocking hop, not once per mode.
pub fn embed_queries(embedder: &dyn Embedder) -> Result<Vec<EvalQuery>> {
    let queries: Vec<(String, String)> = cases()
        .into_iter()
        .map(|(q, e)| (q.to_string(), e.to_string()))
        .collect();
    let vecs = embedder.embed(&queries.iter().map(|(q, _)| q.clone()).collect::<Vec<_>>())?;
    Ok(queries
        .into_iter()
        .zip(vecs)
        .map(|((query, expect), vec)| EvalQuery { query, expect, vec })
        .collect())
}

/// Score one mode against pre-embedded queries.
///
/// `mode` must be one of [`MODES`]; anything else is rejected so a typo cannot
/// silently report the baseline as an improvement.
pub fn score(conn: &Connection, mode: &str, k: usize, queries: &[EvalQuery]) -> Result<EvalReport> {
    if !MODES.contains(&mode) {
        return Err(crate::error::Error::Other(format!(
            "unknown eval mode '{mode}' (expected one of: {})",
            MODES.join(", ")
        )));
    }
    let k = k.clamp(1, 20);
    let mut rows = Vec::new();
    let mut hit_count = 0usize;
    let mut rr_sum = 0.0f32;

    for q in queries {
        let mut hits = repo::search_chunks(conn, None, &q.vec, k * 4)?;
        if mode.starts_with("hybrid") {
            let keyword = repo::keyword_search_chunks(conn, None, None, &q.query, k * 4)?;
            hits = repo::rrf_merge(&[hits, keyword], k * 4);
        }
        if mode.ends_with("rerank") {
            hits = crate::rerank::rerank(&q.query, &hits, k);
        } else {
            hits.truncate(k);
        }

        let got: Vec<String> = hits.iter().map(|h| h.source_name.clone()).collect();
        let rank = got.iter().position(|s| s == &q.expect).map(|i| i + 1);
        if rank.is_some() {
            hit_count += 1;
        }
        if let Some(r) = rank {
            rr_sum += 1.0 / r as f32;
        }
        rows.push(EvalRow {
            query: q.query.clone(),
            expect: q.expect.clone(),
            got,
            rank,
        });
    }

    let n = rows.len().max(1);
    Ok(EvalReport {
        mode: mode.to_string(),
        k,
        evaluated: rows.len(),
        recall_at_k: hit_count as f32 / n as f32,
        mrr: rr_sum / n as f32,
        rows,
    })
}

/// Score every mode, in [`MODES`] order, so the UI can render one table.
pub fn score_all(conn: &Connection, k: usize, queries: &[EvalQuery]) -> Result<Vec<EvalReport>> {
    MODES.iter().map(|m| score(conn, m, k, queries)).collect()
}

// ------------------------------------------------------- real-corpus eval

/// Hard cap on real-corpus queries per run. Embedding a query is a network /
/// ONNX round-trip; 40 keeps a run seconds-long while sampling every source.
const MAX_REAL_QUERIES: usize = 40;
/// Chunks shorter than this make keyword queries that are either empty or
/// noise ("那这一章呢").
const MIN_CHUNK_CHARS: usize = 40;

/// Whether `c` is a CJK ideograph (used to pick the token unit: bigrams for
/// Han runs, whole words for Latin).
fn is_han(c: char) -> bool {
    matches!(c as u32,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F)
}

/// Extract a keyword query from a chunk's own text. Deterministic, no LLM:
/// Latin words (≥3 chars) and CJK bigrams, ranked by frequency in the chunk
/// with first-occurrence tie-break. A chunk whose text yields fewer than two
/// distinct terms is skipped by the caller — a one-term query is noise.
///
/// This is the honest ground truth for a personal corpus: nobody has labelled
/// (query → source) pairs for their own notes, but "the words this chunk uses
/// should find that chunk's source back" is a fair, checkable proxy.
pub fn query_terms(text: &str) -> Vec<String> {
    let mut freq: Vec<(String, usize)> = Vec::new();
    let bump = |freq: &mut Vec<(String, usize)>, t: String| {
        if let Some(e) = freq.iter_mut().find(|(s, _)| *s == t) {
            e.1 += 1;
        } else {
            freq.push((t, 1));
        }
    };
    for run in text.split(|c: char| !c.is_alphanumeric()) {
        if run.is_empty() {
            continue;
        }
        let chars: Vec<char> = run.chars().collect();
        let han = chars.iter().filter(|c| is_han(**c)).count();
        if han > 0 && han == chars.len() {
            // CJK run: overlapping bigrams are the token unit (same trick the
            // keyword search path uses).
            for w in chars.windows(2) {
                let g: String = w.iter().collect();
                bump(&mut freq, g);
            }
        } else if chars.len() >= 3 {
            bump(&mut freq, run.to_lowercase());
        }
    }
    let mut ranked: Vec<(String, usize)> = freq.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.truncate(6);
    ranked.into_iter().map(|(t, _)| t).collect()
}

/// Build a real-corpus eval set from the user's own indexed chunks.
///
/// Query = the top keyword terms of one chunk; expected source = the source
/// that chunk lives in. The corpus is NOT re-embedded — chunks already carry
/// vectors from ingest — so the only slow part is embedding the query strings.
/// Deterministic: chunks are ordered by (source_id, ord) and sampled with a
/// fixed stride when there are more than [`MAX_REAL_QUERIES`].
///
/// `subject_ids` scopes the pool to those subjects; `None` means every subject
/// except the synthetic `(eval)` one. Returns an empty vec when the pool has
/// no usable chunks (nothing indexed yet).
pub fn real_queries(
    conn: &Connection,
    subject_ids: Option<&[String]>,
    embedder: &dyn Embedder,
) -> Result<Vec<EvalQuery>> {
    let candidates = real_query_candidates(conn, subject_ids)?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let queries: Vec<String> = candidates.iter().map(|(q, _)| q.clone()).collect();
    let vecs = embedder.embed(&queries)?;
    Ok(candidates
        .into_iter()
        .zip(vecs)
        .map(|((query, expect), vec)| EvalQuery { query, expect, vec })
        .collect())
}

/// The SQL half of [`real_queries`]: (query text, expected source name)
/// candidates, deterministic order, stride-sampled. Kept separate from the
/// embedding step so the command layer can do its slow network/ONNX hop with
/// no DB connection held.
pub fn real_query_candidates(
    conn: &Connection,
    subject_ids: Option<&[String]>,
) -> Result<Vec<(String, String)>> {
    // Scope + deterministic order in one SQL pass. IN-list is built with
    // positional placeholders — subject ids come from the local DB, but they
    // still go through bound params, never string interpolation. (SUBJECT_NAME
    // is a compile-time constant with no quotes, so inline is safe.)
    let mut sql = format!(
        "SELECT s.name, c.text FROM chunks c \
           JOIN sources s ON s.id = c.source_id \
           JOIN subjects sub ON sub.id = s.subject_id \
          WHERE length(c.text) >= {MIN_CHUNK_CHARS} \
            AND sub.name != '{SUBJECT_NAME}'"
    );
    let mut bound: Vec<String> = Vec::new();
    if let Some(ids) = subject_ids.filter(|v| !v.is_empty()) {
        let list = ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&format!(" AND s.subject_id IN ({list})"));
        bound.extend(ids.iter().cloned());
    }
    sql.push_str(" ORDER BY s.id, c.ord");

    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<(String, String)> = stmt
        .query_map(rusqlite::params_from_iter(bound.iter()), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut candidates: Vec<(String, String)> = rows
        .into_iter()
        .filter_map(|(source, text)| {
            let terms = query_terms(&text);
            if terms.len() < 2 {
                None
            } else {
                Some((terms.join(" "), source))
            }
        })
        .collect();

    if candidates.len() > MAX_REAL_QUERIES {
        let stride = candidates.len() as f64 / MAX_REAL_QUERIES as f64;
        candidates = (0..MAX_REAL_QUERIES)
            .map(|i| candidates[(i as f64 * stride) as usize].clone())
            .collect();
    }
    Ok(candidates)
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::AppState;
    use crate::embed::StubEmbedder;

    /// Seed the corpus and embed the queries, returning everything the scoring
    /// path needs. Mirrors what the command layer does.
    fn prepare() -> (AppState, Vec<EvalQuery>) {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let seeded = embed_corpus(&StubEmbedder).unwrap();
        seed(&c, &seeded).unwrap();
        let queries = embed_queries(&StubEmbedder).unwrap();
        drop(c);
        (st, queries)
    }

    #[test]
    fn all_modes_are_reported_and_the_corpus_is_seeded() {
        let (st, queries) = prepare();
        let c = st.db.lock().unwrap();
        let reports = score_all(&c, DEFAULT_K, &queries).unwrap();
        assert_eq!(reports.len(), 4);
        for r in &reports {
            assert_eq!(r.evaluated, cases().len());
            assert_eq!(r.rows.len(), cases().len());
            assert!(r.recall_at_k >= 0.0 && r.recall_at_k <= 1.0);
            assert!(r.mrr >= 0.0 && r.mrr <= 1.0, "mrr out of range for {}", r.mode);
        }
    }

    /// The offline (stub) embedder is a bag of hashed tokens, so cosine
    /// similarity *is* term overlap -- the same signal the reranker uses. Every
    /// mode therefore ties on this corpus, and claiming an improvement here
    /// would be measuring the embedder's redundancy rather than the reranker.
    ///
    /// What this test does guarantee is the property that matters in CI:
    /// adding the rerank or hybrid channel must never make recall worse. Real
    /// separation shows up when a semantic embedder (ONNX / Gemini) is
    /// configured and the query paraphrases the document -- at which point
    /// `retrieval_eval` reports the difference against the live index.
    #[test]
    fn rerank_and_hybrid_never_regress_vector() {
        let (st, queries) = prepare();
        let c = st.db.lock().unwrap();
        let reports = score_all(&c, DEFAULT_K, &queries).unwrap();
        let get = |mode: &str| reports.iter().find(|r| r.mode == mode).unwrap();
        let vector = get("vector");
        for mode in ["rerank", "hybrid", "hybrid+rerank"] {
            let r = get(mode);
            assert!(
                r.recall_at_k + 1e-4 >= vector.recall_at_k,
                "{mode} regressed recall: {} -> {}",
                vector.recall_at_k,
                r.recall_at_k
            );
            assert!(
                r.mrr + 1e-4 >= vector.mrr,
                "{mode} regressed mrr: {} -> {}",
                vector.mrr,
                r.mrr
            );
        }
    }

    #[test]
    fn seeding_twice_does_not_duplicate_the_corpus() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let seeded = embed_corpus(&StubEmbedder).unwrap();
        seed(&c, &seeded).unwrap();
        seed(&c, &seeded).unwrap();
        let n: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM chunks WHERE subject_id IN \
                 (SELECT id FROM subjects WHERE name = ?1)",
                params![SUBJECT_NAME],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            n as usize,
            corpus().iter().map(|(_, ch)| ch.len()).sum::<usize>()
        );
    }

    #[test]
    fn unknown_mode_is_rejected() {
        let (st, queries) = prepare();
        let c = st.db.lock().unwrap();
        assert!(score(&c, "quantum", DEFAULT_K, &queries).is_err());
    }

    #[test]
    fn the_expected_source_is_ranked_first_on_a_clean_query() {
        let (st, queries) = prepare();
        let c = st.db.lock().unwrap();
        let r = score(&c, "vector", DEFAULT_K, &queries).unwrap();
        let top = r.rows.iter().filter(|row| row.rank == Some(1)).count();
        assert_eq!(top, r.evaluated, "every clean query should be rank 1");
    }

    // ------------------------------------------------ real-corpus queries

    #[test]
    fn query_terms_handle_cjk_bigrams_and_latin_words() {
        // Short CJK run: overlapping bigrams, no single chars.
        let t = query_terms("对比矩阵");
        assert_eq!(t, vec!["对比".to_string(), "比矩".to_string(), "矩阵".to_string()]);
        // Mixed: Latin words lowercased (≥3 chars), short Latin dropped.
        let t = query_terms("对比矩阵 is Matrix");
        assert!(t.contains(&"matrix".to_string()));
        assert!(t.contains(&"对比".to_string()) && t.contains(&"矩阵".to_string()));
        assert!(!t.contains(&"is".to_string()));
    }

    #[test]
    fn query_terms_rank_by_frequency_and_cap_at_six() {
        let t = query_terms("alpha beta alpha beta alpha gamma delta epsilon zeta eta theta iota");
        assert_eq!(t.first().map(String::as_str), Some("alpha"));
        assert_eq!(t.len(), 6, "terms are capped at six");
    }

    #[test]
    fn real_queries_expect_the_chunk_own_source() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        // Two subjects with real-ish text; the synthetic eval subject never seeded.
        let s1 = repo::insert_subject(&c, "线代", Some("math"), Some("◆"), None).unwrap();
        let src1 = repo::insert_source(&c, &s1, None, "特征值讲义.md", "md", None).unwrap();
        for (i, text) in [
            "特征值和特征向量描述线性变换的不变方向，对角化把矩阵分解成特征值与特征向量的乘积。",
            "正定矩阵的所有特征值都是正数，因此可以做 Cholesky 分解，数值上非常稳定。",
        ]
        .into_iter()
        .enumerate()
        {
            let e = StubEmbedder;
            let v = e.embed(&[text.to_string()]).unwrap().pop().unwrap();
            repo::insert_chunk(&c, &src1, &s1, None, i as i64, text, None, v.len() as i64, &crate::vector::f32s_to_blob(&v)).unwrap();
        }
        let qs = real_queries(&c, None, &StubEmbedder).unwrap();
        assert_eq!(qs.len(), 2);
        for q in &qs {
            assert_eq!(q.expect, "特征值讲义.md");
            assert!(q.query.split(' ').count() >= 2);
        }
        // Scoped to a non-existent subject → nothing.
        let none = real_queries(&c, Some(&["nope".into()]), &StubEmbedder).unwrap();
        assert!(none.is_empty());
    }
}
