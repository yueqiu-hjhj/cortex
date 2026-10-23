# Cortex design contract

The load-bearing invariants of this codebase. Each one states a promise the
system makes, and where it is *enforced* — a test that fails if the promise
breaks. This is the document to read before changing any of the subsystems
below; if your change makes one of these tests fail, the change is wrong (or
the contract needs a deliberate, documented amendment — not a quiet edit).

The enforcement half of this contract is the gate: `npm run verify`
(svelte-check → vite build → cargo check → cargo test --lib → clippy
`--all-targets -D warnings`). Nothing lands that isn't green.

## 1. Local-first, personal, single-user

All content, embeddings and generated material live in one SQLite database in
the OS app-data dir. The only outbound calls are to the AI provider you
configured and (optionally) your own SearXNG. No accounts, no telemetry, no
multi-device sync (see README "Deliberately not building").

## 2. Answers explain themselves (0077)

Every chat answer ends in exactly one of five terminal states —
`completed / truncated / blocked / aborted / failed` — classified from the
provider's `finishReason` (`llm::classify_answer`, per-vocabulary tables for
OpenAI/Gemini/Anthropic, case-insensitive).

- An **unknown** finish reason with text is `completed` — the classifier never
  downgrades a real answer.
- A **length-capped** half answer keeps its partial text and invites "继续".
- An **empty** reply is `failed`, not an opaque error.
- `aborted` is produced *only* by the frontend's stop button — the classifier
  never emits it.

Enforced by: `llm.rs` classifier vocabulary test; `repo.rs` state-roundtrip
test (NULL for pre-0077 rows).

## 3. Citations are frozen snapshots, re-verifiable (0078)

Every citation freezes the chunk's full text at answer time (`Citation.quote`,
inside the citations JSON — no schema column). `citesnap::verify_message`
re-checks each frozen quote against the source's *current* text on demand:

- `Exact` — verbatim at the recorded span.
- `Moved` — verbatim but re-indexed elsewhere (whitespace-only drift falls
  back to a normalized match and reports Moved with an approximate span,
  never lies about drift).
- `Drifted` / `SourceGone` — honest failure states.
- Pre-0078 citations carry no quote and verify as `None` (uncheckable),
  **never** as drifted.

Complementary to citecheck ("did the model invent this?" → strip before
display): this answers "does the frozen quote still match the source today?".

**Drift is repairable, not just reportable** (`citesnap::plan_reanchor` →
`refreeze_citation`). A diagnosis the user can only stare at is half a
feature; the repair has to be as honest as the check:

- A quote still verbatim in the source is only **relocated** — the span
  changes, the quotation does not. A re-chunk that also added sentences must
  never smuggle them into a quote the model never made.
- Otherwise the cited **chunk** is the last honest handle: its current text
  becomes the new snapshot, because the citation always meant that chunk.
- Otherwise **nothing is written**. No prefix matching, no hunting for a
  look-alike passage — anchoring to something merely similar manufactures
  evidence, which is the exact failure citecheck exists to prevent.
- The original quote survives as `superseded_quote` (the *first* repair wins;
  a later one must not overwrite the original with an intermediate state) and
  `reanchored_at` dates the repair. Only the `citations` column is rewritten —
  text, answer state and fabrication count stay exactly as the model left
  them, so a repair can never masquerade as a new answer.

**One ledger, one set of numbers** (`citesnap::audit_subject`). The check is
subject-wide and spans *all* threads, including the ones a rollover archived —
the oldest answers are the likeliest to have drifted, so an audit that read
only the current thread would systematically miss them. Three consequences
are load-bearing:

- Uncheckable is not healthy: citations with no frozen quote are counted
  separately (`unchecked`) and never folded into `exact`.
- The audit persists nothing. A stored copy would become a second truth that
  drifts from the citations it describes; the UI shows the time of the run
  instead.
- `answer_health` derives its snapshot numbers *from* this audit rather than
  re-scanning, so the evidence badge, the Lab verdict and the startup check
  can never disagree about how many quotes drifted.

Batch repair (`repair_drifted`) is narrow on purpose: only `Drifted` rows are
touched, each through the same `plan_reanchor` the single-citation button
uses. `Moved` and `Exact` rows are left alone — rewriting a healthy citation
would erase the difference between "verified fine" and "had to be fixed",
which is the only reason the ledger distinguishes them.

Enforced by: `citesnap.rs` (span math, moved/drifted/source-gone, whitespace
fallback, legacy no-quote, DB roundtrip with re-index; relocate-without-
rewriting, chunk refresh, refusal on `text_gone`/`source_gone`/`no_snapshot`,
original quote preserved across repeated repairs, write-back touches only the
citations column; archived-thread coverage, uncheckable-not-healthy counting,
audit writes nothing, drifted-only + idempotent batch repair, refusal reported
rather than fabricated).

## 4. Retrieval evidence is precise about what counts (0044/0069)

- Only **inline-cited** chunks are evidence. Retrieved-but-uncited chunks are
  context, not proof — `backrefs::parse_citation_edges` takes cited edges only.
- A ⟦…⟧ token naming a source that was never retrieved is a **fabrication**:
  citecheck strips it before the user sees it, and the count rides along with
  the message.
- Zero-hit answers are flagged `abstained` — general knowledge by
  construction, badged honestly instead of dressed up.

Enforced by: `backrefs.rs` edge tests; `repo.rs` fabrication-count roundtrip.

## 4b. The context budget is shaped by the question (pinpoint / balanced / synthesis)

One prompt budget is split four ways — subject overview → topic digests →
source digests → raw passages. A fixed 10/25/25/40 spent 60% of it on layers a
pinpoint question never reads, and starved the digests a synthesis question is
made of. `summary_tree::context_intent` classifies the question (two small
bilingual word lists; when the question carries no intent words, the retrieval
shape decides: hits spread across 3+ sources mean the question pulls threads
together, hits from one source mean it is about that passage) and
`budget_split` moves the proportions:

| intent | overview | topics | sources | raw |
|---|---|---|---|---|
| synthesis | 15% (cap 2.4k) | 35% | 30% | ~20% |
| balanced | 10% (cap 1.4k) | 25% | 25% | ~40% |
| pinpoint | 5% (cap 0.8k) | 10% | 15% | ~70% |

Nothing is withheld in any mode — every layer still appears, so a pinpoint
answer is never blind to the topic and a synthesis answer still carries the
passages it cites. The split is a pure function, and the badge the UI shows
(`RetrievalMeta.intent`) is computed by the same function on the same hits, so
the badge can never disagree with what the model actually read.

Enforced by: `summary_tree.rs` (intent words + tie + shape fallback; the four
shares always sum to the budget and raw is ordered pinpoint > balanced >
synthesis; end-to-end: same library and hits, two questions, digest bytes move
one way and passage bytes the other).

## 5. Eval is decidable, not decorative (0080)

The retrieval eval reports **per stratum** (factual / conceptual /
unanswerable) plus an abstention set, and every run is persisted and judged
against the previous run of the same kind with a mechanical rule:

- adopt only with headroom (`IMPROVE_MARGIN` 0.02) and no regression beyond
  tolerance (`REGRESS_TOLERANCE` 0.01);
- the judged mode is the shipping config (`hybrid+rerank`);
- a collapsed answerable/unanswerable separation (`SEPARATION_FLOOR` 0.10)
  regresses regardless of recall;
- timestamps of the same kind are strictly monotonic.

Enforced by: `eval.rs` (strata partition, unanswerable-never-ranks,
separation, adoption headroom/regression/neutral, collapsed separation,
persist-and-judge incl. same-ms monotonicity).

## 6. Vectors are only ever compared inside one embedding space (0081)

Cosine similarity is meaningless across embed models — and the old
`length(embedding)` guard cannot catch a same-dimension switch (two 384-dim
models produce incomparable vectors with plausible-looking scores). So:

- every chunk carries its space (`Embedder::space()`, e.g. `onnx:384`,
  `gemini:text-embedding-004:768` — the model is part of the identity);
- vector search considers only the query's own space **plus never-stamped
  rows**, which stay searchable everywhere so the migration is lossless;
- the first ingest after the migration adopts unlabeled rows — into the
  *previous* cohort on a model switch, never the new one;
- a library split across spaces fails the startup selfcheck loudly
  (`Embedding spaces`) instead of silently halving retrieval.

Enforced by: `repo.rs` cross-space exclusion + adopt-once tests;
`selfcheck.rs` single/split tests.

## 7. A conversation that outgrows its budget rolls over — never silently (0082)

Two watermarks race against `chat_rollover_tokens` (default 12k estimated
tokens, `0` disables), and the decision happens once the turn's real volume is
known — the watermark that matters is the prompt the model is about to read,
not the length of the transcript:

- **transcript** — the thread itself outgrew the budget. Rolling over always
  helps: the fresh thread starts from a summary;
- **prompt** — system + history window + context + question + reply headroom
  already meet it in a *young* thread (a fat context block plus a full history
  window can do that with a short transcript). This watermark only fires when
  there IS a history window to shed: a just-rolled thread's window is empty by
  construction, so without that guard a giant context alone would roll the
  thread on every turn, forever — trimming an oversized context is
  `context_char_budget`'s job (`chat_context_chars`, default 18k chars), not
  the thread's.

Either way, the next answer summarizes the thread and continues in a fresh one:

- the old thread keeps its full history (`chat_threads.rolled_from` lineage);
- the summary is persisted as the new thread's first system message **and**
  prepended to the system prompt — without that the rollover would be theater,
  since chat answers are retrieval-stateless;
- this turn's question travels with the answer (backrefs pairs Q→A within a
  thread); the move only fires when the question is genuinely the newest row,
  so a concurrent rollover can never steal an older one;
- the UI shows a banner (naming which watermark fired) and the summary inline —
  a rollover the user doesn't notice is a conversation that silently lost its
  thread.

**The summary is repairable.** It *is* the conversation's memory for every
turn that follows, so a wrong summary keeps being wrong. It is therefore shown
as a readable card in the transcript (not just fed to the model), and
`regenerate_rollover_summary` re-summarizes it from the archived thread it
stands in for. The rewrite replaces that one system message and nothing else —
the lineage row, the carried question and every other message are untouched;
the `ROLLOVER_PREFIX` is re-applied rather than trusted, so a rewrite can never
quietly turn the preamble into an ordinary system message. Takes effect from
the next turn, because the current prompt was already assembled.

Enforced by: `repo.rs` (archive-nothing/carry-question/lineage, no-steal,
budget + off switch, head+tail window, inherited-summary visibility,
rewrite-in-place, two-watermark policy incl. the no-shed no-roll guard,
context-budget floor).

## 8. Recent turns are fed back into the prompt — windowed, never duplicated

The transcript was stored but never shown to the model, so "它 / 刚才 / 那个"
only ever resolved inside a single turn. `repo::recent_chat_history` closes
that:

- the window is token-budgeted (`chat_history_tokens`, default 4k, `0`
  disables) and sheds the **oldest** turns first — the tail is what "刚才"
  resolves to, so it is the last thing to go;
- the current question is excluded: the panel persists it *before* asking, so
  without the exclusion it would sit in the prompt twice. Only when it is
  genuinely the newest row — a same-text older question is real history;
- system messages are skipped (the rollover summary rides in separately);
- one over-budget message is tail-clipped, not dropped — "继续" after a
  truncated answer must still see what it continues;
- the window is context, never a source (never citable), and the count of
  earlier questions is persisted per answer (`RetrievalMeta.history_turns`)
  and shown as a chip — the user can see when the model is flying on history
  alone.

Enforced by: `repo.rs` (sheds-oldest-first, current-question exclusion,
system-skip, tail-clip, off switch, empty-after-rollover).

## 9. Chat must not silently fall back to a reasoning model

`DEFAULT_CHAT_MODEL` is a fast non-reasoning model on purpose: reasoning
models make the tutor feel slow. `model_spec_for(c, "chat")` plus
`guard_offline_llm` enforce this at the command boundary.

## 10. Windows CRLF fidelity

Several files (`cortex-gateway.rs`, `embed.rs`, `eval.rs`, `notes.rs`,
`selfcheck.rs`, …) are CRLF. An edit that flips line endings produces a
whole-file diff that buries the real change — restore byte-level CRLF before
committing. (This has bitten twice; both times the fix was `amend`.)

## 11. Migrations are append-only

Schema changes are new numbered files in `src-tauri/migrations/`, registered
in `db.rs`; `user_version` is the count of applied migrations. Never edit a
shipped migration — the selfcheck compares `user_version` against
`migration_count()` and the app refuses to look healthy otherwise.

## 12. Answer health is one mechanical page, not a dashboard of vibes

Five honest signals were already recorded per answer — terminal state (0077),
retrieval accounting (0044), stripped fabrications (0069), rollover lineage
(0082), frozen-quote drift (0078) — and never read together.
`answer_health::health` aggregates them for a window and a subject, and its
verdict lines are fixed thresholds over counts (`>15%` failed+blocked,
`>40%` abstained, any fabrication, `>10%` snapshot drift). No model explains
the numbers: a health report that has to be interpreted by the thing it is
measuring is not a health report.

Two rules keep it honest:

- **No answers means "no answers"**, not a score of zero — an empty window
  reports that and stops, because a clean sheet and an unmeasured sheet look
  identical in numbers.
- Drift is computed by `citesnap::audit_subject`, the very call the citation
  ledger makes, so the badge, the ledger and the verdict cannot disagree.

A third rule governs where it runs. The startup check uses only
`answer_health::quick_verdicts` — the `stats` half, no source lookups.
Verifying every frozen quote is a full-library read, and a check that slows
every launch is a check the user switches off; snapshot drift therefore stays
in the Lab, where it is asked for.

Data quality fails nothing. `Check.warn` exists so a check can **pass and
still be surfaced**: a library whose answers are failing is not a reason to
refuse to start, but it is not something to leave in the dark either.

Enforced by: `answer_health.rs` (empty window; state counts only for states
that occurred; failure / abstention / fabrication / truncation each raise
their own line; drift counted against rewritten source with reanchored
flagged; window + subject scoping; rolled threads counted once) and
`selfcheck.rs` (an unhealthy window warns while `ok` stays true; an empty
library raises no warning).
