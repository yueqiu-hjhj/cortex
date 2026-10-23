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
//!
//! A fourth addition is a **language axis**. "Chinese retrieval works here" was
//! an unfalsifiable claim: the corpus was English-only, so no change could make
//! it worse. Every case now carries a language and every report slices the
//! metrics by it, but the number that actually guards the Chinese path is
//! [`LangSlice::keyword_hit_rate`] — the share of cases where the keyword
//! channel ALONE ranked the right source. Han text has no spaces, so the
//! keyword leg tokenises it into overlapping bigrams ([`query_terms`]); replace
//! that with whitespace or whole-run tokenisation — what a default FTS5 setup
//! does — and Chinese cases stop matching *silently*. Recall under a hybrid
//! mode drifts by however much the vector leg happens to be worth; the keyword
//! hit rate drops to zero, which cannot be mistaken for tuning noise.
//!
//! The offline [`crate::embed::StubEmbedder`] hashes alphanumeric runs, so a
//! Han clause is one token there — the same fault. That is why the zh numbers
//! under the stub measure the keyword leg almost exclusively, and why the
//! `vector` row for Chinese looks so much worse than `hybrid`: it is a
//! deliberate lower bound, not a bug in this harness. A configured
//! multilingual embedder (`multilingual-e5-small`) lifts the vector leg too,
//! and `retrieval_eval_real` then reports the real library's numbers.

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

/// (source name, chunks). Sixteen real topics plus three long "reading list"
/// documents that name-drop many topics once — the shape that fools pure
/// similarity ranking.
///
/// Topics are split by language on purpose: the twelve English notes cover
/// classic ML, the six Chinese notes cover topics the English set does NOT
/// contain (SVM, trees, ensembles, RL, GNN, time series), and the mixed-script
/// notes do what Chinese study notes actually do — Chinese prose with English
/// technical terms left untranslated. Disjoint topics mean each query has
/// exactly one right answer, so a per-language recall number cannot be
/// inflated by a lucky cross-language match on a duplicate document.
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
        // ---- Chinese notes. Topics disjoint from the English set above, so a
        // zh query has exactly one correct source.
        (
            "svm-zh.md",
            vec![
                "支持向量机寻找一个能把两类样本分开、并且间隔最大的超平面。最终决定这个平面\
                 的只有落在间隔边界上的支持向量，其余样本删掉也不会改变结果。",
                "核技巧把样本隐式地映射到高维空间：只要在高维里线性可分，就不必显式算出映射\
                 后的坐标。软间隔用松弛变量允许少量样本越界，惩罚系数控制这种容忍的代价。",
            ],
        ),
        (
            "trees-zh.md",
            vec![
                "决策树每次分裂都选让信息增益最大的特征。熵衡量标签的不确定性，一次划分之后\
                 熵下降得越多，说明这次分裂越有价值。",
                "预剪枝在树长深之前就停止分裂，后剪枝先让树长满、再自底向上砍掉收益为负的\
                 子树。剪枝效果要用验证集评估，训练集上的增益会高估。",
            ],
        ),
        (
            "ensemble-zh.md",
            vec![
                "Bagging 对训练集做有放回的重采样，训练多个基学习器再投票，主要降低方差。\
                 随机森林在每次分裂时限制候选特征，让树与树之间更不相关。",
                "Boosting 串行地训练弱学习器，每个新模型更关注前一轮分错的样本，主要降低\
                 偏差。学习率缩小每棵树的贡献，用来抑制过拟合。",
            ],
        ),
        (
            "rl-zh.md",
            vec![
                "Q 学习按贝尔曼方程迭代更新动作价值，不需要环境模型。折扣因子决定未来奖励\
                 有百分之多少计入当前这一步的价值。",
                "探索与利用的取舍决定了智能体会不会困在次优策略里。ε 贪心以一定概率随机\
                 选动作，衰减的 ε 让前期多探索、后期多利用。",
            ],
        ),
        (
            "gnn-zh.md",
            vec![
                "图神经网络用消息传递聚合邻居的特征来更新节点表示。邻接矩阵的归一化方式\
                 决定了聚合时每个邻居占多大权重。",
                "层数太深会出现过平滑：所有节点的表示收敛到同一个向量，彼此再也分不开。\
                 残差连接和随机丢边都能缓解这个问题。",
            ],
        ),
        (
            "timeseries-zh.md",
            vec![
                "ARIMA 把序列拆成自回归、差分和移动平均三部分。差分用来把非平稳序列变成\
                 平稳序列，差分阶数由单位根检验确定。",
                "自相关函数衡量序列与自身滞后版本的相似度，拖尾和截尾的形状用来给自回归与\
                 移动平均的阶数定阶。季节性要另外用 SARIMA 建模。",
            ],
        ),
        // Mixed script: Chinese prose, English terms left as they are. This is
        // how a Chinese student's own notes on an English paper actually read.
        (
            "mix-vit.md",
            vec![
                "ViT 把图像切成 patch，当成 token 序列送进 transformer，因此几乎没有卷积\
                 自带的那种 inductive bias，在小数据集上往往打不过 CNN。",
                "CNN 的平移等变性来自权重共享和局部感受野。ViT 要补上缺少的先验，就得靠\
                 大规模预训练或者很强的数据增强。",
            ],
        ),
        (
            "mix-moe.md",
            vec![
                "Mixture of Experts 把前馈层换成若干个 expert，router 为每个 token 选 top-k\
                 个 expert，于是参数量很大而单次计算量很小。",
                "负载不均衡会让少数 expert 被打爆、其余长期闲置。训练时用 auxiliary loss\
                 惩罚路由分布过于集中，把 token 摊开。",
            ],
        ),
        (
            "mix-diffusion.md",
            vec![
                "Diffusion 模型的前向过程逐步加高斯噪声，反向过程训练一个网络预测每一步加\
                 进去的噪声，采样就是从纯噪声开始逐步去噪。",
                "classifier-free guidance 把有条件与无条件的预测做外推，guidance scale 越大\
                 越贴着提示词，但多样性和真实感都会下降。",
            ],
        ),
        // The Chinese counterpart of reading-list.md: name-drops every zh topic
        // once, so the same crowding effect has to be handled in the Chinese
        // path too — a decoy that only exists in English would make zh recall
        // look better than the English path for no reason worth having.
        (
            "reading-list-zh.md",
            vec![
                "中文读书笔记 第一部分：支持向量机、核技巧、决策树与信息增益、集成学习、\
                 随机森林为什么降方差。",
                "中文读书笔记 第二部分：Q 学习、探索与利用、图神经网络的消息传递、时间序列\
                 的平稳性检验、ViT 的归纳偏置。",
                "中文读书笔记 第三部分：ARIMA 定阶、过平滑、软间隔、boosting 的学习率、\
                 混合专家的路由。",
            ],
        ),
    ]
}

/// Language tags for cases. Reported in this order: the Chinese numbers lead
/// because they are the ones this project claims an edge on.
const ZH: &str = "zh";
const EN: &str = "en";
/// Chinese prose with English terms left untranslated — the shape of a
/// Chinese student's notes on an English paper, and the shape that a
/// word-based tokeniser mishandles worst (half the tokens have spaces, half
/// don't).
const MIX: &str = "mix";
const LANGS: [&str; 3] = [ZH, EN, MIX];

/// Queries with the source whose chunk should rank first, plus a stratum and a
/// language. Strata make the report stratified (KnowNote borrow): a change that
/// lifts factual recall while quietly wrecking conceptual questions is not an
/// improvement, and an aggregate number hides exactly that.
///
///   factual      definitional lookups — the easiest shape
///   conceptual   mechanism/why questions — paraphrase-shaped, the shape real
///                studying produces
///   unanswerable topics the corpus does NOT contain — the abstention set.
///                Retrieval has no right answer here; what matters is that it
///                scores them clearly BELOW the answerable strata, because
///                that gap is what makes the 零命中 badge honest.
fn cases() -> Vec<(&'static str, &'static str, &'static str, &'static str)> {
    vec![
        ("how do bayesian networks encode conditional independence", "bayes-notes.md", "conceptual", EN),
        ("what does d-separation tell you about conditional independence", "bayes-notes.md", "conceptual", EN),
        ("explain gradient descent and the learning rate", "gradient-descent.md", "conceptual", EN),
        ("how does momentum damp oscillation in gradient descent", "gradient-descent.md", "conceptual", EN),
        ("why does L2 regularisation penalise large weights", "regularisation.md", "conceptual", EN),
        ("how does dropout reduce overfitting during training", "regularisation.md", "conceptual", EN),
        ("how does multi head attention work in the transformer", "transformer.md", "conceptual", EN),
        ("why are positional encodings needed for attention", "transformer.md", "conceptual", EN),
        ("how does k means clustering assign points to centroids", "kmeans.md", "conceptual", EN),
        ("what is the difference between underfitting and overfitting", "bias-variance.md", "conceptual", EN),
        ("how does the chain rule give backpropagation its gradients", "backprop.md", "conceptual", EN),
        ("what makes vanishing gradients slow early layers", "backprop.md", "conceptual", EN),
        ("what does the prior and likelihood give in bayes theorem", "bayes-theorem.md", "factual", EN),
        ("which axis captures the largest variance in principal component analysis", "dimensionality.md", "factual", EN),
        ("why subtract the maximum logit before softmax", "softmax.md", "factual", EN),
        ("how does weight sharing help convolutional layers", "convolution.md", "conceptual", EN),
        ("what property does a markov chain transition matrix have", "markov.md", "factual", EN),
        // ---- Chinese. Paraphrased the way a student asks themselves, and
        // deliberately NOT quoting a whole clause from the note: a query that
        // shares a long verbatim substring would be found by any tokeniser and
        // would prove nothing about the Chinese path.
        ("支持向量机到底靠哪些样本决定分类边界", "svm-zh.md", "conceptual", ZH),
        ("核技巧是怎么绕开显式计算高维映射的", "svm-zh.md", "conceptual", ZH),
        ("软间隔里的松弛变量和惩罚系数各自管什么", "svm-zh.md", "factual", ZH),
        ("决策树按什么准则挑选划分特征", "trees-zh.md", "conceptual", ZH),
        ("预剪枝和后剪枝分别在哪一步停下来", "trees-zh.md", "conceptual", ZH),
        ("bagging 为什么主要是降方差而不是降偏差", "ensemble-zh.md", "conceptual", ZH),
        ("boosting 怎么让新模型盯着上一轮分错的样本", "ensemble-zh.md", "conceptual", ZH),
        ("Q 学习里的折扣因子影响的是什么", "rl-zh.md", "conceptual", ZH),
        ("ε 贪心如何权衡探索和利用", "rl-zh.md", "conceptual", ZH),
        ("图神经网络靠什么把邻居特征聚合起来", "gnn-zh.md", "conceptual", ZH),
        ("层数太深为什么会让节点表示分不开", "gnn-zh.md", "conceptual", ZH),
        ("差分这一步在 ARIMA 里解决什么问题", "timeseries-zh.md", "conceptual", ZH),
        ("自相关函数的拖尾和截尾怎么用来定阶", "timeseries-zh.md", "factual", ZH),
        // ---- Mixed script: enough Chinese to need bigrams, enough English to
        // need word tokens. A tokeniser that only does one of the two fails
        // here while still passing the en and zh strata.
        ("ViT 为什么缺少卷积那种 inductive bias", "mix-vit.md", "conceptual", MIX),
        ("图像切成 patch 当 token 之后平移等变性去哪了", "mix-vit.md", "conceptual", MIX),
        ("Mixture of Experts 的 router 怎么给 token 挑 expert", "mix-moe.md", "conceptual", MIX),
        ("expert 负载不均为什么需要 auxiliary loss 来摊平", "mix-moe.md", "conceptual", MIX),
        ("diffusion 的反向过程在预测什么东西", "mix-diffusion.md", "conceptual", MIX),
        ("classifier-free guidance 的 scale 调大会牺牲什么", "mix-diffusion.md", "conceptual", MIX),
        // Abstention set: nothing in a machine-learning textbook corpus answers
        // these. They exist to catch a retrieval change that makes everything
        // look relevant.
        ("how do i fix a flat bicycle tire", "", "unanswerable", EN),
        ("what is the boiling point of mercury at sea level", "", "unanswerable", EN),
        ("who won the 2018 football world cup final", "", "unanswerable", EN),
        ("how do i renew my passport before it expires", "", "unanswerable", EN),
        ("what causes ocean tides to rise and fall", "", "unanswerable", EN),
        ("how do i feed a sourdough starter", "", "unanswerable", EN),
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
    /// Language of the query: zh / en / mix. Empty in reports stored before
    /// the language axis existed.
    #[serde(default)]
    pub lang: String,
    /// 1-based rank of the expected source in the KEYWORD channel alone, before
    /// fusion. `None` in two different situations that must stay distinct to
    /// the reader: the mode never ran the keyword leg (plain `vector`), or the
    /// leg ran and missed (a dead Han tokeniser looks exactly like this).
    /// [`LangSlice::keyword_hit_rate`] aggregates the latter; the mode name
    /// tells you which one you are looking at.
    #[serde(default)]
    pub kw_rank: Option<usize>,
}

/// One stratum's slice of a report. Aggregates lie when strata move in
/// opposite directions, so every metric is reported per stratum too.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Stratum {
    /// The slice label — the case kind for [`EvalReport::strata`], the language
    /// for [`EvalReport::langs`].
    pub kind: String,
    pub cases: usize,
    pub recall_at_k: f32,
    pub mrr: f32,
    /// Mean top-1 score inside this stratum.
    pub mean_top1: f32,
}

/// One language's slice of a report, plus the number that only exists per
/// language: how often the keyword channel on its own found the right source.
///
/// Recall tells you the pipeline as a whole still works. It does not tell you
/// WHICH leg did the work, and for Chinese one leg is load-bearing: the vector
/// leg needs a multilingual model, so on the offline stub (and for anyone who
/// never downloaded one) recall for Chinese rests on bigram keyword matching.
/// `keyword_hit_rate` is that dependency made visible — and it is the only
/// number in this harness that goes to exactly zero when Han tokenisation
/// breaks, instead of drifting.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LangSlice {
    /// zh / en / mix.
    pub lang: String,
    /// Every case in this language, answerable or not.
    pub cases: usize,
    /// Cases in this language with an expected source. The recall/MRR below
    /// divide by THIS, not by `cases` — an unanswerable case has no right
    /// answer, so counting it against the language that happens to hold the
    /// abstention set would report "Chinese retrieves better than English" for
    /// a reason that has nothing to do with either language.
    pub answerable: usize,
    /// Recall over the answerable cases of this language.
    pub recall_at_k: f32,
    pub mrr: f32,
    /// Cases the keyword channel alone ranked correctly.
    pub keyword_hits: usize,
    /// `keyword_hits` over `answerable`. 0.0 for the modes that never run the
    /// keyword leg.
    pub keyword_hit_rate: f32,
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
    /// Embedding space used for this run. Empty for pre-config-identity
    /// reports, which must never be treated as a comparable baseline.
    #[serde(default)]
    pub space: String,
    /// Canonical subject scope for real-corpus runs, or `synthetic`.
    #[serde(default)]
    pub scope: String,
    /// Identity of the measurement fixture: a fingerprint of the synthetic
    /// corpus plus its cases, or `real` for a run over the user's own library.
    /// Two runs are only comparable when this matches — otherwise changing the
    /// corpus would silently be read as a retrieval improvement. Empty for
    /// reports stored before this field existed, which therefore never match.
    #[serde(default)]
    pub corpus: String,
    pub evaluated: usize,
    /// Share of queries whose expected source appears in the top k.
    pub recall_at_k: f32,
    /// Mean reciprocal rank of the expected source.
    pub mrr: f32,
    pub rows: Vec<EvalRow>,
    /// Per-stratum breakdown (same metrics, sliced by case kind).
    pub strata: Vec<Stratum>,
    /// Per-language breakdown, in [`LANGS`] order. Empty for reports stored
    /// before the language axis existed.
    #[serde(default)]
    pub langs: Vec<LangSlice>,
    /// Answerable-vs-unanswerable score separation, when the run has both.
    pub separation: Option<Separation>,
}

pub const MODES: [&str; 4] = ["vector", "rerank", "hybrid", "hybrid+rerank"];
pub const SYNTHETIC_SCOPE: &str = "synthetic";

/// Identity of the synthetic fixture: a fingerprint of every corpus chunk and
/// every case. Any edit to [`corpus`] or [`cases`] changes it, which is the
/// point — numbers measured on one fixture must never be compared against a
/// different one, or adding (say) harder Chinese documents would be read as a
/// retrieval regression.
///
/// FNV-1a rather than `std::hash::DefaultHasher` because this string is
/// persisted and compared across app versions: SipHash's output is not
/// contractual, and a fingerprint that changed under a toolchain upgrade would
/// silently disable the adoption rule instead of failing loudly.
fn fixture_id() -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    fn eat(h: &mut u64, s: &str) {
        for b in s.as_bytes() {
            *h ^= *b as u64;
            *h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    for (name, chunks) in corpus() {
        eat(&mut h, name);
        for c in chunks {
            eat(&mut h, c);
        }
    }
    for (q, e, k, l) in cases() {
        eat(&mut h, q);
        eat(&mut h, e);
        eat(&mut h, k);
        eat(&mut h, l);
    }
    format!("syn:{:012x}", h & 0xffff_ffff_ffff)
}

/// Canonical identity for a real-corpus subject selection. None and an empty
/// list both mean "all real subjects"; selected ids are sorted so checkbox
/// order cannot split otherwise-identical runs into different configurations.
pub fn real_scope_key(subject_ids: Option<&[String]>) -> String {
    let mut ids: Vec<&str> = subject_ids
        .unwrap_or_default()
        .iter()
        .map(String::as_str)
        .filter(|id| !id.is_empty())
        .collect();
    if ids.is_empty() {
        return "all".to_string();
    }
    ids.sort_unstable();
    ids.dedup();
    format!(
        "subjects:{}",
        serde_json::to_string(&ids).unwrap_or_else(|_| "[]".to_string())
    )
}

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
    /// Language: zh / en / mix. For real-corpus runs it is derived from the
    /// sampled query terms, so a library that is half Chinese shows up in the
    /// per-language table as half Chinese.
    pub lang: String,
    /// Embedding space of the embedder that produced `vec` (0081).
    pub space: String,
}

/// Embed every eval query in one call -- again so the slow part happens in the
/// command layer's blocking hop, not once per mode.
pub fn embed_queries(embedder: &dyn Embedder) -> Result<Vec<EvalQuery>> {
    let queries: Vec<(String, String, String, String)> = cases()
        .into_iter()
        .map(|(q, e, k, l)| (q.to_string(), e.to_string(), k.to_string(), l.to_string()))
        .collect();
    let vecs = embedder.embed(&queries.iter().map(|(q, _, _, _)| q.clone()).collect::<Vec<_>>())?;
    let space = embedder.space().to_string();
    Ok(queries
        .into_iter()
        .zip(vecs)
        .map(|((query, expect, kind, lang), vec)| EvalQuery {
            query,
            expect,
            vec,
            kind,
            lang,
            space: space.clone(),
        })
        .collect())
}

/// Score one mode against pre-embedded queries.
///
/// `mode` must be one of [`MODES`]; anything else is rejected so a typo cannot
/// silently report the baseline as an improvement.
pub fn score(
    conn: &Connection,
    mode: &str,
    k: usize,
    scope: &str,
    queries: &[EvalQuery],
) -> Result<EvalReport> {
    if !MODES.contains(&mode) {
        return Err(crate::error::Error::Other(format!(
            "unknown eval mode '{mode}' (expected one of: {})",
            MODES.join(", ")
        )));
    }
    if scope.is_empty() {
        return Err(crate::error::Error::Other(
            "eval scope must not be empty".into(),
        ));
    }
    let space = queries.first().map(|q| q.space.as_str()).unwrap_or("");
    if queries.iter().any(|q| q.space.as_str() != space) {
        return Err(crate::error::Error::Other(
            "eval queries span multiple embedding spaces".into(),
        ));
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
        // Rank of the expected source in the keyword channel ALONE, captured
        // before fusion overwrites the ranking. This is the number that shows
        // whether the leg contributed anything, as opposed to whether the
        // pipeline as a whole happened to get there.
        let mut kw_rank = None;
        if mode.starts_with("hybrid") {
            let keyword = repo::keyword_search_chunks(conn, None, None, &q.query, k * 4)?;
            kw_rank = keyword
                .iter()
                .take(k)
                .map(|h| h.source_name.as_str())
                .position(|s| s == q.expect.as_str())
                .map(|i| i + 1);
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
            lang: q.lang.clone(),
            kw_rank,
        });
    }

    let n = rows.len().max(1);
    let strata = strata_of(&rows);
    let langs = langs_of(&rows, k);
    let separation = separation_of(&strata);
    Ok(EvalReport {
        mode: mode.to_string(),
        k,
        space: space.to_string(),
        scope: scope.to_string(),
        corpus: if scope == SYNTHETIC_SCOPE {
            fixture_id()
        } else {
            "real".to_string()
        },
        evaluated: rows.len(),
        recall_at_k: hit_count as f32 / n as f32,
        mrr: rr_sum / n as f32,
        rows,
        strata,
        langs,
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

/// Per-language metrics, in [`LANGS`] order so the table does not reshuffle
/// between runs. Unknown tags sort last, alphabetically.
fn langs_of(rows: &[EvalRow], k: usize) -> Vec<LangSlice> {
    let mut seen: Vec<String> = rows.iter().map(|r| r.lang.clone()).collect();
    seen.sort();
    seen.dedup();
    seen.sort_by_key(|l| LANGS.iter().position(|x| x == l).unwrap_or(LANGS.len()));
    seen.iter()
        .map(|lang| {
            let all: Vec<&EvalRow> = rows.iter().filter(|r| &r.lang == lang).collect();
            let scored: Vec<&&EvalRow> = all.iter().filter(|r| !r.expect.is_empty()).collect();
            let n = scored.len().max(1);
            let keyword_hits = scored
                .iter()
                .filter(|r| r.kw_rank.is_some_and(|x| x <= k))
                .count();
            LangSlice {
                lang: lang.clone(),
                cases: all.len(),
                answerable: scored.len(),
                recall_at_k: scored.iter().filter(|r| r.rank.is_some()).count() as f32 / n as f32,
                mrr: scored
                    .iter()
                    .map(|r| r.rank.map(|x| 1.0 / x as f32).unwrap_or(0.0))
                    .sum::<f32>()
                    / n as f32,
                keyword_hits,
                keyword_hit_rate: keyword_hits as f32 / n as f32,
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
pub fn score_all(
    conn: &Connection,
    k: usize,
    scope: &str,
    queries: &[EvalQuery],
) -> Result<Vec<EvalReport>> {
    MODES
        .iter()
        .map(|m| score(conn, m, k, scope, queries))
        .collect()
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

/// Two reports belong to the same comparison bucket only when every retrieval
/// input that changes the metric is identical — including the fixture itself
/// ([`EvalReport::corpus`]). Old reports have empty config fields and therefore
/// never match a new run.
pub(crate) fn same_eval_config(a: &EvalReport, b: &EvalReport) -> bool {
    a.mode == b.mode
        && a.k == b.k
        && !a.space.is_empty()
        && a.space == b.space
        && !a.scope.is_empty()
        && a.scope == b.scope
        && !a.corpus.is_empty()
        && a.corpus == b.corpus
}

/// Persist one run and judge it against the previous run with the same
/// embedding space, k, and subject scope. `kind` still separates synthetic
/// runs (comparable across time -- same corpus) from real-corpus runs (whose
/// numbers also move when the library does).
pub fn record_and_judge(
    conn: &Connection,
    kind: &str,
    reports: &[EvalReport],
) -> Result<Option<EvalVerdict>> {
    let Some(cand) = reports.iter().find(|r| r.mode == VERDICT_MODE) else {
        crate::repo::save_eval_run(conn, kind, reports)?;
        return Ok(None);
    };
    let at = crate::repo::save_eval_run(conn, kind, reports)?;
    let Some((prev_at, prev_reports)) =
        crate::repo::previous_eval_run(conn, kind, at, cand)?
    else {
        return Ok(None); // first run with this config: nothing to compare against
    };
    let base = prev_reports
        .iter()
        .find(|r| r.mode == VERDICT_MODE && same_eval_config(r, cand));
    Ok(base.map(|b| {
        let c = cand;
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

/// Language of a sampled keyword query, read off its own terms: Han bigrams
/// (some or all) → zh, Latin words → en, both → mix. Derived from what the
/// retriever actually has to tokenise, not from what the document claims to be.
fn query_lang(terms: &[String]) -> &'static str {
    let han = terms.iter().any(|t| t.chars().all(is_han));
    let latin = terms.iter().any(|t| !t.chars().all(is_han));
    match (han, latin) {
        (true, false) => ZH,
        (false, true) => EN,
        _ => MIX,
    }
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
    let queries: Vec<String> = candidates.iter().map(|(q, _, _)| q.clone()).collect();
    let vecs = embedder.embed(&queries)?;
    // Query space = the embedder used right now, mirroring chat retrieval: after
    // a model switch without re-ingest, real eval sees the same rows chat sees.
    let space = embedder.space().to_string();
    Ok(candidates
        .into_iter()
        .zip(vecs)
        .map(|((query, expect, lang), vec)| EvalQuery {
            query,
            expect,
            vec,
            kind: "real".to_string(),
            lang,
            space: space.clone(),
        })
        .collect())
}

/// The SQL half of [`real_queries`]: (query text, expected source name,
/// language) candidates, deterministic order, stride-sampled. Kept separate
/// from the embedding step so the command layer can do its slow network/ONNX
/// hop with no DB connection held.
pub fn real_query_candidates(
    conn: &Connection,
    subject_ids: Option<&[String]>,
) -> Result<Vec<(String, String, String)>> {
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

    let mut candidates: Vec<(String, String, String)> = rows
        .into_iter()
        .filter_map(|(source, text)| {
            let terms = query_terms(&text);
            if terms.len() < 2 {
                None
            } else {
                let lang = query_lang(&terms).to_string();
                Some((terms.join(" "), source, lang))
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
        let reports = score_all(&c, DEFAULT_K, SYNTHETIC_SCOPE, &queries).unwrap();
        assert_eq!(reports.len(), 4);
        for r in &reports {
            assert_eq!(r.evaluated, cases().len());
            assert_eq!(r.rows.len(), cases().len());
            assert_eq!(r.scope, SYNTHETIC_SCOPE);
            assert_eq!(r.space, StubEmbedder.space());
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
        let reports = score_all(&c, DEFAULT_K, SYNTHETIC_SCOPE, &queries).unwrap();
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
        assert!(score(&c, "quantum", DEFAULT_K, SYNTHETIC_SCOPE, &queries).is_err());
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
            // All-Han → zh, but the Cholesky chunk mixes an English term in,
            // and that must be tagged mix rather than averaged into zh.
            assert_ne!(q.lang, EN, "a Han query must never be tagged English");
        }
        assert!(qs.iter().any(|q| q.lang == MIX), "the Cholesky chunk carries a Latin term");
        assert!(qs.iter().any(|q| q.lang == ZH));
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
            space: StubEmbedder.space().to_string(),
            scope: SYNTHETIC_SCOPE.to_string(),
            corpus: fixture_id(),
            evaluated: cases().len(),
            recall_at_k: recall,
            mrr,
            rows: Vec::new(),
            strata: Vec::new(),
            langs: Vec::new(),
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
        let r = &score_all(&c, DEFAULT_K, SYNTHETIC_SCOPE, &queries).unwrap()[0];
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
    /// against the first matching config, and the baseline is strictly before
    /// the candidate.
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

    #[test]
    fn record_and_judge_skips_mismatched_or_legacy_configs() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let baseline = report(VERDICT_MODE, 0.60, 0.50, Some(0.30));
        assert!(record_and_judge(&c, "synthetic", std::slice::from_ref(&baseline))
            .unwrap()
            .is_none());
        let baseline_at: i64 = c
            .query_row("SELECT MIN(created_at) FROM eval_runs", [], |r| r.get(0))
            .unwrap();

        let mut different_k = report(VERDICT_MODE, 0.90, 0.90, Some(0.30));
        different_k.k = DEFAULT_K + 1;
        assert!(record_and_judge(&c, "synthetic", std::slice::from_ref(&different_k))
            .unwrap()
            .is_none());

        let mut different_space = report(VERDICT_MODE, 0.90, 0.90, Some(0.30));
        different_space.space = "other:768".into();
        assert!(record_and_judge(&c, "synthetic", std::slice::from_ref(&different_space))
            .unwrap()
            .is_none());

        let mut different_scope = report(VERDICT_MODE, 0.90, 0.90, Some(0.30));
        different_scope.scope = real_scope_key(Some(&["subject-a".into()]));
        assert!(record_and_judge(&c, "synthetic", std::slice::from_ref(&different_scope))
            .unwrap()
            .is_none());

        let mut legacy = report(VERDICT_MODE, 0.90, 0.90, Some(0.30));
        legacy.space.clear();
        legacy.scope.clear();
        crate::repo::save_eval_run(&c, "synthetic", std::slice::from_ref(&legacy)).unwrap();

        let matching = record_and_judge(
            &c,
            "synthetic",
            std::slice::from_ref(&report(VERDICT_MODE, 0.66, 0.52, Some(0.30))),
        )
        .unwrap()
        .expect("the matching baseline is still available");
        assert_eq!(matching.baseline_at, baseline_at);
    }

    // ---------------------------------------------------- language axis

    /// A case pointing at a source that does not exist would silently score
    /// zero forever — an air-gap in the harness that no other test would catch.
    #[test]
    fn every_expected_source_exists_in_the_corpus() {
        let names: Vec<&str> = corpus().into_iter().map(|(n, _)| n).collect();
        for (q, expect, kind, lang) in cases() {
            assert_eq!(expect.is_empty(), kind == "unanswerable", "{q} ({lang})");
            if !expect.is_empty() {
                assert!(names.contains(&expect), "{q} expects missing source {expect}");
            }
        }
    }

    #[test]
    fn the_corpus_covers_three_languages() {
        let mut by_lang = std::collections::BTreeMap::new();
        for (_, _, _, lang) in cases() {
            *by_lang.entry(lang).or_insert(0) += 1;
        }
        for lang in LANGS {
            let n = by_lang.get(lang).copied().unwrap_or(0);
            assert!(n >= 6, "language `{lang}` has only {n} cases; a slice that thin cannot move a rate");
        }
    }

    /// The number this whole axis exists for: under the offline embedder,
    /// Chinese recall comes from the keyword channel, and the keyword channel
    /// is the bigram tokeniser. Break it (whitespace tokens, whole-run tokens,
    /// a default FTS5 setup) and Chinese retrieval loses the gap below while
    /// English does not move — which is what makes this a language problem
    /// rather than a tuning problem.
    #[test]
    fn chinese_recall_rides_on_the_keyword_leg() {
        let (st, queries) = prepare();
        let c = st.db.lock().unwrap();
        let reports = score_all(&c, DEFAULT_K, SYNTHETIC_SCOPE, &queries).unwrap();
        let lang = |mode: &str, l: &str| {
            reports
                .iter()
                .find(|r| r.mode == mode)
                .and_then(|r| r.langs.iter().find(|s| s.lang == l))
                .cloned()
                .unwrap_or_else(|| panic!("no {l} slice in {mode}"))
        };
        let vector = lang("vector", ZH);
        let best = lang(VERDICT_MODE, ZH);
        assert!(
            vector.recall_at_k <= 0.5,
            "the offline vector leg suddenly reads Han clauses ({}%) — if that is a real fix, \
             update the module doc that says it cannot",
            (vector.recall_at_k * 100.0).round() as i64
        );
        assert_eq!(
            best.recall_at_k, 1.0,
            "Chinese recall regressed under {VERDICT_MODE}: {} of {} found",
            (best.recall_at_k * best.answerable as f32).round() as i64,
            best.answerable
        );
        assert!(
            best.recall_at_k - vector.recall_at_k >= 0.5,
            "the keyword leg no longer carries Chinese: {}% -> {}%",
            (vector.recall_at_k * 100.0).round() as i64,
            (best.recall_at_k * 100.0).round() as i64
        );
        assert_eq!(
            best.keyword_hits, best.answerable,
            "the keyword channel alone must find every Chinese case; {} of {}",
            best.keyword_hits, best.answerable
        );
        // English is unaffected by the same leg on this corpus, so the gap
        // above is a language effect and not the harness getting easier.
        assert_eq!(lang(VERDICT_MODE, EN).keyword_hit_rate, 1.0);
        assert_eq!(lang(VERDICT_MODE, MIX).keyword_hit_rate, 1.0);
    }

    /// `keyword_hit_rate` is meaningless for modes that never run the keyword
    /// leg, so it must read as "not measured" rather than as a bad score. The
    /// UI keys the column off the mode name; this test pins that the raw value
    /// is zero and every row's `kw_rank` is empty, so nothing can quietly
    /// present 0% as a measurement.
    #[test]
    fn keyword_hit_rate_is_zero_in_modes_without_a_keyword_leg() {
        let (st, queries) = prepare();
        let c = st.db.lock().unwrap();
        let r = score(&c, "vector", DEFAULT_K, SYNTHETIC_SCOPE, &queries).unwrap();
        for s in &r.langs {
            assert_eq!(s.keyword_hits, 0, "vector mode ran the keyword leg?");
            assert_eq!(s.keyword_hit_rate, 0.0);
        }
        assert!(r.rows.iter().all(|row| row.kw_rank.is_none()));
    }

    /// On the offline stub, `StubEmbedder` hashes alphanumeric runs — so a Han
    /// clause is ONE token, the same fault a default FTS5 tokeniser has. English
    /// clean queries still rank first through the vector leg; Chinese ones need
    /// the keyword leg. Both halves of this test are load-bearing: the first
    /// proves the harness is not broken, the second documents the limit the
    /// language table is measuring.
    #[test]
    fn the_offline_vector_leg_cannot_read_han_clauses() {
        let (st, queries) = prepare();
        let c = st.db.lock().unwrap();
        let r = score(&c, "vector", DEFAULT_K, SYNTHETIC_SCOPE, &queries).unwrap();
        let en: Vec<&EvalRow> = r
            .rows
            .iter()
            .filter(|row| row.lang == EN && !row.expect.is_empty())
            .collect();
        assert!(
            en.iter().all(|row| row.rank == Some(1)),
            "every clean English query should rank first under the stub"
        );
        let zh_missed = r.rows.iter().filter(|row| row.lang == ZH && row.rank.is_none()).count();
        assert!(
            zh_missed > 0,
            "if the stub starts reading Han clauses, the module doc's caveat is stale"
        );
    }

    #[test]
    fn the_language_table_partitions_the_cases() {
        let (st, queries) = prepare();
        let c = st.db.lock().unwrap();
        let r = &score_all(&c, DEFAULT_K, SYNTHETIC_SCOPE, &queries).unwrap()[0];
        let langs: Vec<&str> = r.langs.iter().map(|s| s.lang.as_str()).collect();
        assert_eq!(langs, LANGS.to_vec(), "language slices are reported in LANGS order");
        assert_eq!(
            r.langs.iter().map(|s| s.cases).sum::<usize>(),
            r.evaluated,
            "every case belongs to exactly one language"
        );
    }

    /// A corpus edit changes what the numbers MEAN, so it must break the
    /// comparison instead of being read as a retrieval improvement.
    #[test]
    fn changing_the_fixture_invalidates_the_baseline() {
        let base = report(VERDICT_MODE, 0.60, 0.50, Some(0.30));
        let mut harder = report(VERDICT_MODE, 0.40, 0.40, Some(0.30));
        harder.corpus = "syn:000000000001".into();
        assert!(!same_eval_config(&base, &harder));

        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        assert!(record_and_judge(&c, "synthetic", std::slice::from_ref(&base))
            .unwrap()
            .is_none());
        assert!(
            record_and_judge(&c, "synthetic", std::slice::from_ref(&harder))
                .unwrap()
                .is_none(),
            "a run on a different fixture must not be judged against this one"
        );
    }

    #[test]
    fn query_lang_reads_the_terms_not_the_document() {
        assert_eq!(query_lang(&["支持".into(), "向量".into()]), ZH);
        assert_eq!(query_lang(&["gradient".into(), "descent".into()]), EN);
        assert_eq!(query_lang(&["支持".into(), "vector".into()]), MIX);
    }

    #[test]
    fn real_scope_identity_ignores_selection_order_and_duplicates() {
        assert_eq!(real_scope_key(None), "all");
        assert_eq!(real_scope_key(Some(&[])), "all");
        let a = real_scope_key(Some(&["b".into(), "a".into(), "a".into()]));
        let b = real_scope_key(Some(&["a".into(), "b".into()]));
        assert_eq!(a, b);
        assert_ne!(a, real_scope_key(Some(&["a".into()])));
    }
}
