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
//!
//! Three additions make the numbers decidable rather than decorative (all
//! KnowNote borrows):
//!
//! - **Stratified report** — every case carries a kind (factual / conceptual /
//!   unanswerable) and every metric is reported per stratum as well as in
//!   aggregate. A change that lifts factual recall while quietly wrecking
//!   conceptual questions is not an improvement, and an average hides exactly
//!   that.
//! - **Abstention set** — unanswerable queries the corpus cannot answer.
//!   Retrieval has no right answer here; what matters is that it scores them
//!   clearly BELOW the answerable strata, because that gap is what makes the
//!   零命中 badge honest. [`SEPARATION_FLOOR`] is the collapse threshold.
//! - **Adoption rule** — every run is persisted (0080) and judged against the
//!   previous run of the same kind: adopt only with headroom
//!   ([`IMPROVE_MARGIN`]) and no regression beyond [`REGRESS_TOLERANCE`];
//!   "no measurable change" is its own verdict. The rule judges
//!   [`VERDICT_MODE`] — the shipping config — not whichever mode happens to
//!   look best today.

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

/// Queries with the source whose chunk should rank first, plus a stratum.
/// Strata make the report stratified (KnowNote borrow): a change that lifts
/// factual recall while quietly wrecking conceptual questions is not an
/// improvement, and an aggregate number hides exactly that.
///
///   factual      definitional lookups — the easiest shape
///   conceptual   mechanism/why questions — paraphrase-shaped, the shape real
///                studying produces
///   unanswerable topics the corpus does NOT contain — the abstention set.
///                Retrieval has no right answer here; what matters is that it
///                scores them clearly BELOW the answerable strata, because
///                that gap is what makes the 零命中 badge honest.
fn cases() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("how do bayesian networks encode conditional independence", "bayes-notes.md", "conceptual"),
        ("what does d-separation tell you about conditional independence", "bayes-notes.md", "conceptual"),
        ("explain gradient descent and the learning rate", "gradient-descent.md", "conceptual"),
        ("how does momentum damp oscillation in gradient descent", "gradient-descent.md", "conceptual"),
        ("why does L2 regularisation penalise large weights", "regularisation.md", "conceptual"),
        ("how does dropout reduce overfitting during training", "regularisation.md", "conceptual"),
        ("how does multi head attention work in the transformer", "transformer.md", "conceptual"),
        ("why are positional encodings needed for attention", "transformer.md", "conceptual"),
        ("how does k means clustering assign points to centroids", "kmeans.md", "conceptual"),
        ("what is the difference between underfitting and overfitting", "bias-variance.md", "conceptual"),
        ("how does the chain rule give backpropagation its gradients", "backprop.md", "conceptual"),
        ("what makes vanishing gradients slow early layers", "backprop.md", "conceptual"),
        ("what does the prior and likelihood give in bayes theorem", "bayes-theorem.md", "factual"),
        ("which axis captures the largest variance in principal component analysis", "dimensionality.md", "factual"),
        ("why subtract the maximum logit before softmax", "softmax.md", "factual"),
        ("how does weight sharing help convolutional layers", "convolution.md", "conceptual"),
        ("what property does a markov chain transition matrix have", "markov.md", "factual"),
        // Abstention set: nothing in a machine-learning textbook corpus answers
        // these. They exist to catch a retrieval change that makes everything
        // look relevant.
        ("how do i fix a flat bicycle tire", "", "unanswerable"),
        ("what is the boiling point of mercury at sea level", "", "unanswerable"),
        ("who won the 2018 football world cup final", "", "unanswerable"),
        ("how do i renew my passport before it expires", "", "unanswerable"),
        ("what causes ocean tides to rise and fall", "", "unanswerable"),
        ("how do i feed a sourdough starter", "", "unanswerable"),
    ]
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvalRow {
    pub query: String,
    pub expect: String,
    /// Top-k source names, best first.
    pub got: Vec<String>,
    /// 1-based rank of the expected source (None = not in the top k).
    pub rank: Option<usize>,
    /// Stratum this case belongs to (factual / conceptual / unanswerable /
    /// real). Unanswerable rows carry an empty `expect` and never a rank.
    pub kind: String,
    /// Score of the top-1 hit. The abstention signal: unanswerable queries
    /// must score clearly below answerable ones.
    pub top1: f32,
}

/// One stratum's slice of a report. Aggregates lie when strata move in
/// opposite directions, so every metric is reported per stratum too.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Stratum {
    pub kind: String,
    pub cases: usize,
    pub recall_at_k: f32,
    pub mrr: f32,
    /// Mean top-1 score inside this stratum.
    pub mean_top1: f32,
}

/// How far apart the answerable strata and the unanswerable stratum sit.
/// This gap is the mechanical basis of the 零命中 / abstention badge: when it
/// collapses, retrieval can no longer tell "not in your library" from "in it",
/// and every ungrounded answer starts looking grounded.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Separation {
    pub answerable_mean_top1: f32,
    pub unanswerable_mean_top1: f32,
    /// Scale-free gap: (answerable − unanswerable) / answerable. Scale-free so
    /// cosine scores and RRF rank scores can be compared under one rule.
    pub ratio: f32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvalReport {
    pub mode: String,
    pub k: usize,
    pub evaluated: usize,
    /// Share of queries whose expected source appears in the top k.
    pub recall_at_k: f32,
    /// Mean reciprocal rank of the expected source.
    pub mrr: f32,
    pub rows: Vec<EvalRow>,
    /// Per-stratum breakdown (same metrics, sliced by case kind).
    pub strata: Vec<Stratum>,
    /// Answerable-vs-unanswerable score separation, when the run has both.
    pub separation: Option<Separation>,
}

pub const MODES: [&str; 4] = ["vector", "rerank", "hybrid", "hybrid+rerank"];

/// The corpus with vectors already computed. Splitting "embed" from "write"
/// keeps every slow (ONNX / network) call inside one blocking hop in the command
/// layer, and makes the scoring path testable without an embedder.
pub struct Seed {
    pub sources: Vec<(String, Vec<(String, Vec<f32>)>)>,
    /// Embedding space of the embedder that produced the vectors (0081). Scored
    /// queries must live in the same space or retrieval silently sees nothing.
    pub space: String,
}

/// Embed the corpus. Deterministic given the same embedder.
pub fn embed_corpus(embedder: &dyn Embedder) -> Result<Seed> {
    let mut sources = Vec::with_capacity(corpus().len());
    for (name, chunks) in corpus() {
        let texts: Vec<String> = chunks.into_iter().map(str::to_string).collect();
        let vecs = embedder.embed(&texts)?;
        sources.push((name.to_string(), texts.into_iter().zip(vecs).collect()));
    }
    Ok(Seed {
        sources,
        space: embedder.space().to_string(),
    })
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
                &seed.space,
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
    /// Stratum: factual / conceptual / unanswerable (synthetic) or real.
    pub kind: String,
    /// Embedding space of the embedder that produced `vec` (0081).
    pub space: String,
}

/// Embed every eval query in one call -- again so the slow part happens in the
/// command layer's blocking hop, not once per mode.
pub fn embed_queries(embedder: &dyn Embedder) -> Result<Vec<EvalQuery>> {
    let queries: Vec<(String, String, String)> = cases()
        .into_iter()
        .map(|(q, e, k)| (q.to_string(), e.to_string(), k.to_string()))
        .collect();
    let vecs = embedder.embed(&queries.iter().map(|(q, _, _)| q.clone()).collect::<Vec<_>>())?;
    let space = embedder.space().to_string();
    Ok(queries
        .into_iter()
        .zip(vecs)
        .map(|((query, expect, kind), vec)| EvalQuery {
            query,
            expect,
            vec,
            kind,
            space: space.clone(),
        })
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
        // Search in the query's own embedding space: after a model switch the
        // eval corpus is seeded with the new embedder, and mixing spaces would
        // score garbage against garbage (or nothing at all).
        let mut hits = repo::search_chunks_in_space(conn, None, &q.vec, k * 4, &q.space)?;
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
        let top1 = hits.first().map(|h| h.score).unwrap_or(0.0);
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
            kind: q.kind.clone(),
            top1,
        });
    }

    let n = rows.len().max(1);
    let strata = strata_of(&rows);
    let separation = separation_of(&strata);
    Ok(EvalReport {
        mode: mode.to_string(),
        k,
        evaluated: rows.len(),
        recall_at_k: hit_count as f32 / n as f32,
        mrr: rr_sum / n as f32,
        rows,
        strata,
        separation,
    })
}

/// Per-stratum metrics. Strata are reported in a stable order (unanswerable
/// last) so the UI table doesn't reshuffle between runs.
fn strata_of(rows: &[EvalRow]) -> Vec<Stratum> {
    let mut kinds: Vec<String> = rows.iter().map(|r| r.kind.clone()).collect();
    kinds.sort();
    kinds.dedup();
    kinds.sort_by_key(|k| if k == "unanswerable" { 1 } else { 0 });
    kinds
        .iter()
        .map(|kind| {
            let sub: Vec<&EvalRow> = rows.iter().filter(|r| &r.kind == kind).collect();
            let n = sub.len().max(1);
            Stratum {
                kind: kind.clone(),
                cases: sub.len(),
                recall_at_k: sub.iter().filter(|r| r.rank.is_some()).count() as f32 / n as f32,
                mrr: sub
                    .iter()
                    .map(|r| r.rank.map(|x| 1.0 / x as f32).unwrap_or(0.0))
                    .sum::<f32>()
                    / n as f32,
                mean_top1: sub.iter().map(|r| r.top1).sum::<f32>() / n as f32,
            }
        })
        .collect()
}

/// Answerable-vs-unanswerable separation, when the run contains both. `None`
/// for real-corpus runs (every case is answerable by construction).
fn separation_of(strata: &[Stratum]) -> Option<Separation> {
    let un = strata.iter().find(|s| s.kind == "unanswerable")?;
    let ans: Vec<&Stratum> = strata.iter().filter(|s| s.kind != "unanswerable").collect();
    if ans.is_empty() {
        return None;
    }
    let mean = ans.iter().map(|s| s.mean_top1).sum::<f32>() / ans.len() as f32;
    if mean <= 0.0 {
        return None;
    }
    Some(Separation {
        answerable_mean_top1: mean,
        unanswerable_mean_top1: un.mean_top1,
        ratio: (mean - un.mean_top1) / mean,
    })
}

/// Score every mode, in [`MODES`] order, so the UI can render one table.
pub fn score_all(conn: &Connection, k: usize, queries: &[EvalQuery]) -> Result<Vec<EvalReport>> {
    MODES.iter().map(|m| score(conn, m, k, queries)).collect()
}

/// What a run returns: the per-mode table plus, when a previous run of the
/// same kind exists, the mechanical adoption verdict against it. The verdict
/// is the point of persisting runs — "did my change help?" becomes a
/// repeatable question instead of a vibe.
#[derive(Debug, Clone, Serialize)]
pub struct EvalOutcome {
    pub reports: Vec<EvalReport>,
    pub verdict: Option<EvalVerdict>,
}

/// Persist one run and judge it against the previous run of the same kind.
/// `kind` separates synthetic runs (comparable across time — same corpus)
/// from real-corpus runs (whose numbers also move when the library does).
pub fn record_and_judge(
    conn: &Connection,
    kind: &str,
    reports: &[EvalReport],
) -> Result<Option<EvalVerdict>> {
    let at = crate::repo::save_eval_run(conn, kind, reports)?;
    let Some((prev_at, prev_reports)) = crate::repo::previous_eval_run(conn, kind, at)? else {
        return Ok(None); // first run of this kind: nothing to compare against
    };
    let base = prev_reports.iter().find(|r| r.mode == VERDICT_MODE);
    let cand = reports.iter().find(|r| r.mode == VERDICT_MODE);
    Ok(base.zip(cand).map(|(b, c)| {
        let mut v = verdict(prev_at, b, c);
        if kind == "real" {
            v.note.push_str("（real 语料的对比同时反映了你库里的增删改——数字变化不全是检索改动的功劳或责任。）");
        }
        v
    }))
}

// ------------------------------------- adoption rule (KnowNote borrow)

/// Margins for the adoption rule. A retrieval change is ADOPTed only with
/// headroom: some metric improves by at least [`IMPROVE_MARGIN`] and NOTHING
/// regresses by more than [`REGRESS_TOLERANCE`]. "No measurable change" is its
/// own verdict — a change that moves nothing should be reverted on
/// complexity grounds alone, not celebrated.
pub const IMPROVE_MARGIN: f32 = 0.02;
pub const REGRESS_TOLERANCE: f32 = 0.01;
/// Below this scale-free separation, retrieval can no longer tell "not in the
/// library" from "in it" — the 零命中 badge becomes decoration.
pub const SEPARATION_FLOOR: f32 = 0.10;
/// The config the verdict judges: the shipping pipeline. The other modes stay
/// in the table for humans, not for the rule.
pub const VERDICT_MODE: &str = "hybrid+rerank";

/// One metric's verdict: adopt (improved with headroom) / regress (worse
/// beyond tolerance) / neutral (inside the noise band).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MetricVerdict {
    pub metric: String,
    pub baseline: f32,
    pub candidate: f32,
    pub delta: f32,
    pub verdict: String,
}

/// The mechanical verdict of one candidate run against the stored baseline.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvalVerdict {
    /// created_at of the baseline run this judges against.
    pub baseline_at: i64,
    pub mode: String,
    pub metrics: Vec<MetricVerdict>,
    /// adopt | regress | neutral
    pub overall: String,
    pub note: String,
}

fn cmp_metric(metric: &str, baseline: f32, candidate: f32) -> MetricVerdict {
    let delta = candidate - baseline;
    let verdict = if delta >= IMPROVE_MARGIN {
        "adopt"
    } else if delta <= -REGRESS_TOLERANCE {
        "regress"
    } else {
        "neutral"
    };
    MetricVerdict {
        metric: metric.to_string(),
        baseline,
        candidate,
        delta,
        verdict: verdict.to_string(),
    }
}

/// Judge one candidate report against the baseline. Both must be for
/// [`VERDICT_MODE`]; the caller picks the mode. A collapsed separation is a
/// regression regardless of the recall numbers — a retriever that scores
/// unanswerable queries as high as answerable ones has lost the ability to
/// abstain, which is a correctness failure, not a tuning trade-off.
pub fn verdict(baseline_at: i64, baseline: &EvalReport, candidate: &EvalReport) -> EvalVerdict {
    let mut metrics = vec![
        cmp_metric("recall_at_k", baseline.recall_at_k, candidate.recall_at_k),
        cmp_metric("mrr", baseline.mrr, candidate.mrr),
    ];
    if let (Some(b), Some(c)) = (&baseline.separation, &candidate.separation) {
        metrics.push(cmp_metric("separation_ratio", b.ratio, c.ratio));
    }
    let collapsed = candidate
        .separation
        .as_ref()
        .is_some_and(|s| s.ratio < SEPARATION_FLOOR);
    let overall = if collapsed || metrics.iter().any(|m| m.verdict == "regress") {
        "regress"
    } else if metrics.iter().any(|m| m.verdict == "adopt") {
        "adopt"
    } else {
        "neutral"
    };
    // Permille, not {:.3} — this toolchain's rustc has no fixed-point format
    // trait (same reason fmt1 exists elsewhere in the codebase).
    let note = if collapsed {
        let ratio = candidate.separation.as_ref().map(|s| s.ratio).unwrap_or(0.0);
        format!(
            "分数分离度跌到 {}‰ 以下（阈值 {}‰）：检索已经分不清「库里没有」和「库里有」，零命中徽章失去意义——按回退处理。",
            (ratio * 1000.0).round() as i64,
            (SEPARATION_FLOOR * 1000.0).round() as i64
        )
    } else {
        format!(
            "规则：任一指标改善 ≥ {}‰ 且无指标回退 > {}‰ → adopt；有回退 → regress；其余 → neutral（无可测变化的改动应按复杂度理由回退，而不是庆祝）。",
            (IMPROVE_MARGIN * 1000.0).round() as i64,
            (REGRESS_TOLERANCE * 1000.0).round() as i64
        )
    };
    EvalVerdict {
        baseline_at,
        mode: VERDICT_MODE.to_string(),
        metrics,
        overall: overall.to_string(),
        note,
    }
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
    // Query space = the embedder used right now, mirroring chat retrieval: after
    // a model switch without re-ingest, real eval sees the same rows chat sees.
    let space = embedder.space().to_string();
    Ok(candidates
        .into_iter()
        .zip(vecs)
        .map(|((query, expect), vec)| EvalQuery {
            query,
            expect,
            vec,
            kind: "real".to_string(),
            space: space.clone(),
        })
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
        // Unanswerable cases have no expected source and never rank — the
        // stub embedder is a hashed bag of terms, so every clean (answerable)
        // query should still put its source first.
        let answerable = r.rows.iter().filter(|row| !row.expect.is_empty()).count();
        let top = r.rows.iter().filter(|row| row.rank == Some(1)).count();
        assert_eq!(top, answerable, "every clean query should be rank 1");
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
            repo::insert_chunk(&c, &src1, &s1, None, i as i64, text, None, v.len() as i64, &crate::vector::f32s_to_blob(&v), "").unwrap();
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

    /// A hand-built report for the verdict tests — the adoption rule is pure
    /// arithmetic over two reports, so it must be testable without retrieval.
    fn report(mode: &str, recall: f32, mrr: f32, sep: Option<f32>) -> EvalReport {
        EvalReport {
            mode: mode.to_string(),
            k: DEFAULT_K,
            evaluated: 23,
            recall_at_k: recall,
            mrr,
            rows: Vec::new(),
            strata: Vec::new(),
            separation: sep.map(|ratio| Separation {
                answerable_mean_top1: 0.8,
                unanswerable_mean_top1: 0.8 * (1.0 - ratio),
                ratio,
            }),
        }
    }

    /// Every case lands in exactly one stratum, unanswerable cases never rank
    /// anything, and the answerable/unanswerable score gap exists — the gap is
    /// what makes the 零命中 badge honest.
    #[test]
    fn strata_are_stratified_and_unanswerable_never_ranks() {
        let (st, queries) = prepare();
        let c = st.db.lock().unwrap();
        let r = &score_all(&c, DEFAULT_K, &queries).unwrap()[0];
        let kinds: Vec<&str> = r.strata.iter().map(|s| s.kind.as_str()).collect();
        assert!(kinds.contains(&"factual"), "strata: {kinds:?}");
        assert!(kinds.contains(&"conceptual"), "strata: {kinds:?}");
        assert!(kinds.contains(&"unanswerable"), "strata: {kinds:?}");
        let total: usize = r.strata.iter().map(|s| s.cases).sum();
        assert_eq!(total, r.evaluated, "strata must partition the cases");
        for row in r.rows.iter().filter(|x| x.kind == "unanswerable") {
            assert!(row.rank.is_none(), "unanswerable case ranked something: {row:?}");
            assert!(row.expect.is_empty());
        }
        let sep = r.separation.as_ref().expect("synthetic corpus has both strata");
        assert!(sep.ratio > 0.0, "answerable queries must outscore unanswerable ones");
        assert!(sep.answerable_mean_top1 > sep.unanswerable_mean_top1);
    }

    /// The adoption rule: headroom to adopt, any regression rejects, noise is
    /// its own verdict.
    #[test]
    fn the_adoption_rule_needs_headroom_and_rejects_any_regression() {
        let base = report(VERDICT_MODE, 0.60, 0.50, Some(0.30));
        // Everything improves beyond the margins → adopt.
        let v = verdict(1, &base, &report(VERDICT_MODE, 0.65, 0.52, Some(0.30)));
        assert_eq!(v.overall, "adopt");
        // Recall drops beyond tolerance while mrr improves → the drop wins.
        let v = verdict(1, &base, &report(VERDICT_MODE, 0.58, 0.60, Some(0.30)));
        assert_eq!(v.overall, "regress");
        // Both inside the noise band → neutral, explicitly not a celebration.
        let v = verdict(1, &base, &report(VERDICT_MODE, 0.61, 0.51, Some(0.30)));
        assert_eq!(v.overall, "neutral");
    }

    /// A collapsed separation is a regression even with perfect recall: a
    /// retriever that can't tell "not in the library" from "in it" has lost
    /// the ability to abstain.
    #[test]
    fn collapsed_separation_regresses_even_with_perfect_recall() {
        let base = report(VERDICT_MODE, 0.60, 0.50, Some(0.30));
        let v = verdict(1, &base, &report(VERDICT_MODE, 1.0, 1.0, Some(0.05)));
        assert_eq!(v.overall, "regress");
        assert!(v.note.contains("分离度"), "note should name the cause: {}", v.note);
    }

    /// record_and_judge: first run has no baseline; the second is judged
    /// against the first, and the baseline is strictly the PREVIOUS run.
    #[test]
    fn record_and_judge_compares_against_the_previous_run() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let first = record_and_judge(&c, "synthetic", std::slice::from_ref(&report(VERDICT_MODE, 0.60, 0.50, Some(0.30)))).unwrap();
        assert!(first.is_none(), "the first run has nothing to be judged against");
        let second = record_and_judge(&c, "synthetic", std::slice::from_ref(&report(VERDICT_MODE, 0.66, 0.52, Some(0.30)))).unwrap();
        let v = second.expect("second run judges against the first");
        assert_eq!(v.overall, "adopt");
        assert_eq!(v.mode, VERDICT_MODE);
        // A real run's baseline is separate: synthetic history must not leak in.
        assert!(record_and_judge(&c, "real", std::slice::from_ref(&report(VERDICT_MODE, 0.5, 0.5, None))).unwrap().is_none());
    }
}
