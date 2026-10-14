<script lang="ts">
  // Per-subject glossary — manual term + definition cards with a 2-tier
  // retention status (known / fuzzy). Backed by localStorage (see
  // src/lib/glossary.ts); the SQLite DB is deliberately NOT involved so this
  // stays a fast, per-device study aid with no sync story to maintain.
  //
  // UX: each card flips term ↔ definition on click. The back face carries the
  // "Know it / Still fuzzy" grading buttons; the front face shows the term
  // and its current status. A filter toggle exposes the fuzzy-only review
  // queue without a separate "study mode" screen.
  import { app } from "../lib/store.svelte";
  import Icon from "../components/Icon.svelte";
  import * as gl from "../lib/glossary";
  import type { GlossaryTerm } from "../lib/glossary";
  import { isMobile } from "../lib/platform";

  const sid = $derived(app.activeSubjectId);
  const subjectName = $derived(app.activeSubject?.name ?? "");

  // Re-read on mount and whenever the subject changes. localStorage writes
  // also bump this via a key so multiple windows stay in sync.
  let terms = $state<GlossaryTerm[]>([]);
  let loaded = $state(false);

  function reload() {
    if (!sid) { terms = []; loaded = true; return; }
    terms = gl.loadGlossary(sid);
    loaded = true;
  }
  $effect(() => { reload(); });

  // Filter: "all" shows everything, "fuzzy" is the review queue.
  type Filter = "all" | "fuzzy";
  let filter = $state<Filter>("all");
  const visible = $derived(filter === "all" ? terms : terms.filter((t) => t.status === "fuzzy"));

  // Counts (cheap; recomputed from terms on any change).
  const counts = $derived({
    total: terms.length,
    known: terms.filter((t) => t.status === "known").length,
    fuzzy: terms.filter((t) => t.status === "fuzzy").length,
    new: terms.filter((t) => t.status === "new").length,
  });

  // Add form.
  let newTerm = $state("");
  let newDef = $state("");
  let adding = $state(false);
  async function add() {
    if (!sid) return;
    const t = newTerm.trim();
    if (!t) return;
    adding = true;
    try {
      gl.addTerm(sid, t, newDef);
      newTerm = ""; newDef = "";
      reload();
    } catch (e) {
      app.pushToast({ kind: "error", title: "Couldn't add term", body: String(e) });
    } finally { adding = false; }
  }

  // Card flip state — tracked per term id so multiple cards can flip independently.
  let flipped = $state<Record<string, boolean>>({});
  function toggle(id: string) { flipped[id] = !flipped[id]; }

  function grade(id: string, status: "known" | "fuzzy") {
    if (!sid) return;
    gl.setStatus(sid, id, status);
    reload();
    // Auto-advance: in fuzzy-only review, un-flip so the next card is ready.
    flipped[id] = false;
  }

  function remove(id: string) {
    if (!sid) return;
    gl.deleteTerm(sid, id);
    reload();
  }
</script>

<div class="glossary">
  <header class="gl-head">
    <div>
      <div class="eyebrow">Glossary</div>
      <h1 class="gl-title">
        {subjectName ? subjectName + " · Glossary" : "Glossary"}
      </h1>
      <div class="mono faint" style:font-size="var(--t-xs)">
        {#if sid}
          {counts.total} term{counts.total === 1 ? "" : "s"}
          · {counts.known} known · {counts.fuzzy} fuzzy · {counts.new} new
        {:else}
          Pick a subject to manage its terms
        {/if}
      </div>
    </div>
    <div class="gl-head-actions">
      <div class="seg" role="tablist" aria-label="Filter">
        <button
          class="seg-btn{filter === "all" ? " on" : ""}"
          role="tab"
          aria-selected={filter === "all"}
          onclick={() => (filter = "all")}
          type="button"
        >All <span class="seg-count">{counts.total}</span></button>
        <button
          class="seg-btn{filter === "fuzzy" ? " on" : ""}"
          role="tab"
          aria-selected={filter === "fuzzy"}
          onclick={() => (filter = "fuzzy")}
          type="button"
        >Fuzzy <span class="seg-count">{counts.fuzzy}</span></button>
      </div>
    </div>
  </header>

  {#if !sid}
    <div class="gl-empty">
      <div class="gl-empty-glyph">📖</div>
      <div class="gl-empty-title">No subject selected</div>
      <div class="gl-empty-body">Glossaries are per-subject. Open a subject first, then come back here.</div>
      <button class="btn btn--primary btn--sm" type="button" onclick={() => app.setView("dashboard")}>
        Go to dashboard
      </button>
    </div>
  {:else}
    <!-- Add form: term + definition, two fields only (per the agreed scope). -->
    <form class="gl-add" onsubmit={(e) => { e.preventDefault(); void add(); }}>
      <input
        class="input gl-add-term"
        type="text"
        placeholder="Term"
        bind:value={newTerm}
        disabled={adding}
        maxlength={120}
      />
      <textarea
        class="input gl-add-def"
        placeholder="Definition"
        bind:value={newDef}
        disabled={adding}
        rows={2}
        maxlength={500}
      ></textarea>
      <button class="btn btn--primary btn--sm" type="submit" disabled={adding || !newTerm.trim()}>
        <Icon name="plus" size={12} /> Add
      </button>
    </form>

    {#if !loaded}
      <div class="gl-hint">Loading…</div>
    {:else if terms.length === 0}
      <div class="gl-empty">
        <div class="gl-empty-glyph">📝</div>
        <div class="gl-empty-title">No terms yet</div>
        <div class="gl-empty-body">Add a term and its definition above to start building this glossary.</div>
      </div>
    {:else if visible.length === 0}
      <div class="gl-empty">
        <div class="gl-empty-glyph">✓</div>
        <div class="gl-empty-title">No fuzzy terms</div>
        <div class="gl-empty-body">Everything is known or new. Mark a few as "Still fuzzy" to build a review queue.</div>
      </div>
    {:else}
      <div class="gl-grid" class:gl-grid--review={filter === "fuzzy"}>
        {#each visible as t (t.id)}
          {@const isFlipped = !!flipped[t.id]}
          <div
            class="gl-card{isFlipped ? " flipped" : ""} status-{t.status}"
            role="button"
            tabindex="0"
            onclick={() => toggle(t.id)}
            onkeydown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); toggle(t.id); } }}
            aria-label={"Flip card: " + t.term}
          >
            <div class="gl-card-inner">
              <div class="gl-face gl-face-front">
                <span class={"gl-status dot-" + t.status} title={t.status}></span>
                <span class="gl-term">{t.term}</span>
                <span class="gl-hint">click to flip</span>
              </div>
              <div class="gl-face gl-face-back">
                <span class="gl-def">{#if t.definition}{t.definition}{:else}<em class="faint">no definition</em>{/if}</span>
                <div class="gl-actions" role="group">
                  <button
                    type="button"
                    class="btn btn--ok btn--sm"
                    onclick={(e) => { e.stopPropagation(); grade(t.id, "known"); }}
                    title="Mark as known"
                  >Know it</button>
                  <button
                    type="button"
                    class="btn btn--fuzzy btn--sm"
                    onclick={(e) => { e.stopPropagation(); grade(t.id, "fuzzy"); }}
                    title="Mark as still fuzzy"
                  >Still fuzzy</button>
                  <span class="grow"></span>
                  <button
                    type="button"
                    class="btn btn--ghost btn--sm gl-del"
                    onclick={(e) => { e.stopPropagation(); remove(t.id); }}
                    aria-label={"Delete " + t.term}
                    title="Delete"
                  >
                    <Icon name="x" size={12} />
                  </button>
                </div>
              </div>
            </div>
          </div>
        {/each}
      </div>
    {/if}
  {/if}
</div>

<style>
  .glossary {
    padding: 18px 22px 32px;
    max-width: 1100px;
  }
  .gl-head {
    display: flex; align-items: flex-end; justify-content: space-between;
    gap: 16px; flex-wrap: wrap; margin: 0 0 18px;
  }
  .gl-title { margin: 4px 0 6px; font-size: var(--t-lg); color: var(--fg-bright); font-weight: 600; }
  .eyebrow { font-size: var(--t-2xs); color: var(--fg-faint); text-transform: uppercase; letter-spacing: 0.1em; }
  .mono { font-family: var(--font-mono); }

  .seg {
    display: inline-flex; padding: 3px; gap: 2px;
    background: var(--surface-2); border: 1px solid var(--border); border-radius: 9px;
  }
  .seg-btn {
    display: inline-flex; align-items: center; gap: 6px;
    height: 28px; padding: 0 10px;
    background: transparent; border: 0; border-radius: 7px;
    font-size: var(--t-2xs); color: var(--fg-muted); cursor: pointer;
    transition: background 0.1s ease, color 0.1s ease;
  }
  .seg-btn:hover { color: var(--fg-bright); }
  .seg-btn.on { background: var(--surface); color: var(--fg-bright); box-shadow: 0 1px 0 var(--border); }
  .seg-count { font-family: var(--font-mono); font-size: 10px; color: var(--fg-faint); }
  .seg-btn.on .seg-count { color: var(--fg-muted); }

  .gl-add {
    display: grid; grid-template-columns: 1fr 2fr auto; gap: 8px; align-items: stretch;
    margin: 0 0 18px;
  }
  .gl-add-term, .gl-add-def {
    font-size: var(--t-xs); color: var(--fg);
    background: var(--surface); border: 1px solid var(--border); border-radius: 8px;
    padding: 8px 10px;
  }
  .gl-add-def { font-family: var(--font-cn); line-height: 1.5; resize: vertical; min-height: 38px; }
  .gl-add-term:focus, .gl-add-def:focus { outline: none; border-color: var(--accent); }

  @media (max-width: 720px) {
    .gl-add { grid-template-columns: 1fr; }
  }

  .gl-hint, .gl-empty { padding: 24px; color: var(--fg-faint); text-align: center; }
  .gl-empty {
    display: flex; flex-direction: column; align-items: center; gap: 8px;
    border: 1px dashed var(--border-strong); border-radius: 12px; background: var(--surface);
  }
  .gl-empty-glyph { font-size: 28px; line-height: 1; }
  .gl-empty-title { font-size: var(--t-sm); font-weight: 600; color: var(--fg-bright); }
  .gl-empty-body { font-size: var(--t-xs); color: var(--fg-muted); max-width: 320px; }

  /* ── Card grid ── */
  .gl-grid {
    display: grid; grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: 12px;
  }
  .gl-grid--review { grid-template-columns: repeat(auto-fill, minmax(260px, 1fr)); }

  /* ── Flip card (P0 easing + preserve-3d) ── */
  .gl-card {
    /* The outer button is the perspective container so the inner transform is
       readable in 3D. */
    perspective: 800px;
    background: transparent; border: 0; padding: 0; cursor: pointer;
    text-align: left; color: inherit;
    min-height: 130px;
    font: inherit;
  }
  .gl-card:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: 12px; }
  .gl-card-inner {
    position: relative;
    width: 100%; height: 100%; min-height: 130px;
    transform-style: preserve-3d;
    transition: transform 0.5s var(--ease-out-back);
  }
  .gl-card.flipped .gl-card-inner { transform: rotateY(180deg); }
  .gl-face {
    position: absolute; inset: 0;
    display: flex; flex-direction: column; gap: 8px;
    padding: 14px;
    background: var(--surface); border: 1px solid var(--border); border-radius: 12px;
    box-shadow: 0 1px 0 var(--border);
    -webkit-backface-visibility: hidden; backface-visibility: hidden;
  }
  .gl-face-front { justify-content: center; align-items: flex-start; }
  .gl-face-back { transform: rotateY(180deg); }

  .gl-card.status-known { --card-accent: var(--ok); }
  .gl-card.status-fuzzy { --card-accent: var(--warn); }
  .gl-card.status-new   { --card-accent: var(--accent); }
  .gl-face { border-top: 3px solid var(--card-accent, var(--border)); }

  .gl-status {
    width: 8px; height: 8px; border-radius: 50%;
    background: var(--card-accent, var(--fg-faint));
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--card-accent, var(--fg-faint)) 22%, transparent);
  }
  .gl-term { font-size: var(--t-md); font-weight: 600; color: var(--fg-bright); line-height: 1.3; }
  .gl-hint { font-size: var(--t-2xs); color: var(--fg-faint); }
  .gl-def { font-size: var(--t-xs); color: var(--fg); line-height: 1.5; flex: 1; }

  .gl-actions { display: flex; align-items: center; gap: 6px; }
  .grow { flex: 1; }
  .faint { color: var(--fg-faint); }

  .btn--ok    { background: color-mix(in srgb, var(--ok) 18%, transparent); color: var(--ok); border: 1px solid color-mix(in srgb, var(--ok) 40%, transparent); }
  .btn--ok:hover    { background: color-mix(in srgb, var(--ok) 30%, transparent); }
  .btn--fuzzy { background: color-mix(in srgb, var(--warn) 18%, transparent); color: var(--warn); border: 1px solid color-mix(in srgb, var(--warn) 40%, transparent); }
  .btn--fuzzy:hover { background: color-mix(in srgb, var(--warn) 30%, transparent); }
</style>
