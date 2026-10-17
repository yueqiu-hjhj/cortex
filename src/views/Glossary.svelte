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
  import * as api from "../lib/api";
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

  // AI suggest dialog state.
  let aiOpen = $state(false);
  let aiText = $state("");
  let aiCandidates = $state<[string, string][]>([]);
  let aiSelected = $state<Record<number, boolean>>({});
  let aiBusy = $state(false);
  let aiErr = $state<string | null>(null);
  function openAi() { aiText = ""; aiCandidates = []; aiSelected = {}; aiErr = null; aiOpen = true; }
  function closeAi() { aiOpen = false; }
  async function runSuggest() {
    if (aiBusy) return;
    aiBusy = true; aiErr = null; aiCandidates = []; aiSelected = {};
    try {
      const out = await api.aiSuggestGlossary(aiText, 20);
      aiCandidates = out ?? [];
      aiSelected = Object.fromEntries(aiCandidates.map((_, i) => [i, true]));
      if (aiCandidates.length === 0) aiErr = "AI 没抽到术语";
    } catch (e) { aiErr = String(e); }
    finally { aiBusy = false; }
  }
  function acceptAi() {
    if (!sid) return;
    let added = 0;
    aiCandidates.forEach(([t, d], i) => {
      if (!aiSelected[i]) return;
      gl.addTerm(sid, t, d);
      added++;
    });
    if (added) {
      app.pushToast({ kind: "success", title: `已加 ${added} 个术语`, body: "可在 All 里翻看" });
      reload();
    }
    closeAi();
  }

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
      <div class="eyebrow">术语表</div>
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
      <button class="btn btn--sm" type="button" onclick={openAi} title="粘贴文本或来源内容，让 AI 抽出候选术语">
        <Icon name="bolt" size={12} /> AI Suggest
      </button>
      <div class="seg" role="tablist" aria-label="Filter">
        <button
          class="seg-btn{filter === "all" ? " on" : ""}"
          role="tab"
          aria-selected={filter === "all"}
          onclick={() => (filter = "all")}
          type="button"
        >全部 <span class="seg-count">{counts.total}</span></button>
        <button
          class="seg-btn{filter === "fuzzy" ? " on" : ""}"
          role="tab"
          aria-selected={filter === "fuzzy"}
          onclick={() => (filter = "fuzzy")}
          type="button"
        >模糊 <span class="seg-count">{counts.fuzzy}</span></button>
      </div>
    </div>
  </header>

  {#if !sid}
    <div class="gl-empty">
      <div class="gl-empty-glyph">📖</div>
      <div class="gl-empty-title">未选择科目</div>
      <div class="gl-empty-body">术语表按科目区分。请先打开一个科目再回来。</div>
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
      <div class="gl-hint">正在加载…</div>
    {:else if terms.length === 0}
      <div class="gl-empty">
        <div class="gl-empty-glyph">📝</div>
        <div class="gl-empty-title">还没有术语</div>
        <div class="gl-empty-body">在上方添加术语和定义，开始建立这个术语表。</div>
      </div>
    {:else if visible.length === 0}
      <div class="gl-empty">
        <div class="gl-empty-glyph">✓</div>
        <div class="gl-empty-title">没有模糊术语</div>
        <div class="gl-empty-body">全部术语要么已掌握要么是新的。标记几个为「仍然模糊」来建立复习队列。</div>
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
                <span class="gl-hint">点击翻面</span>
              </div>
              <div class="gl-face gl-face-back">
                <span class="gl-def">{#if t.definition}{t.definition}{:else}<em class="faint">暂无定义</em>{/if}</span>
                <div class="gl-actions" role="group">
                  <button
                    type="button"
                    class="btn btn--ok btn--sm"
                    onclick={(e) => { e.stopPropagation(); grade(t.id, "known"); }}
                    title="Mark as known"
                  >已掌握</button>
                  <button
                    type="button"
                    class="btn btn--fuzzy btn--sm"
                    onclick={(e) => { e.stopPropagation(); grade(t.id, "fuzzy"); }}
                    title="Mark as still fuzzy"
                  >仍然模糊</button>
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

{#if aiOpen}
  <div class="gl-ai-backdrop" onclick={closeAi} role="presentation"></div>
  <div class="gl-ai-dialog" role="dialog" aria-modal="true" aria-label="AI Suggest terms">
    <header class="gl-ai-head">
      <h2>AI Suggest · 术语候选</h2>
      <button class="btn btn--icon btn--sm btn--ghost" type="button" onclick={closeAi} aria-label="Close">
        <Icon name="x" size={14} />
      </button>
    </header>
    <div class="gl-ai-body">
      <label class="mono faint" for="gl-ai-text">粘贴一段文本（笔记 / 教材 / 来源内容）</label>
      <textarea id="gl-ai-text" class="input" rows="6" bind:value={aiText} disabled={aiBusy} placeholder="把要抽术语的段落粘在这里…"></textarea>
      <div class="gl-ai-actions">
        <button class="btn btn--primary btn--sm" type="button" onclick={runSuggest} disabled={aiBusy || !aiText.trim()}>
          {aiBusy ? "正在抽…" : "提取候选"}
        </button>
        {#if aiCandidates.length > 0}
          <span class="mono faint">{aiCandidates.length} 候选 · 已选 {Object.values(aiSelected).filter(Boolean).length}</span>
        {/if}
      </div>
      {#if aiErr}
        <div class="gl-ai-err mono">{aiErr}</div>
      {/if}
      {#if aiCandidates.length > 0}
        <ul class="gl-ai-list">
          {#each aiCandidates as c, i (i)}
            <li class="gl-ai-item">
              <label>
                <input type="checkbox" bind:checked={aiSelected[i]} />
                <span class="gl-ai-term read">{c[0]}</span>
                <span class="gl-ai-def mono faint">{c[1]}</span>
              </label>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
    <footer class="gl-ai-foot">
      <button class="btn btn--ghost btn--sm" type="button" onclick={closeAi}>取消</button>
      <button class="btn btn--primary btn--sm" type="button" onclick={acceptAi} disabled={aiBusy || Object.values(aiSelected).filter(Boolean).length === 0}>
        入 {Object.values(aiSelected).filter(Boolean).length} 个术语
      </button>
    </footer>
  </div>
{/if}


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

  /* ── AI Suggest dialog ── */
  .gl-ai-backdrop {
    position: fixed; inset: 0; z-index: 80;
    background: color-mix(in srgb, var(--bg) 60%, transparent);
    backdrop-filter: blur(2px);
  }
  .gl-ai-dialog {
    position: fixed; z-index: 81;
    top: 50%; left: 50%; transform: translate(-50%, -50%);
    width: min(640px, calc(100vw - 32px));
    max-height: calc(100vh - 64px);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    box-shadow: 0 16px 60px color-mix(in srgb, black 40%, transparent);
    display: flex; flex-direction: column;
  }
  .gl-ai-head {
    display: flex; align-items: center; justify-content: space-between;
    padding: 14px 18px; border-bottom: 1px solid var(--border);
  }
  .gl-ai-head h2 { font-size: var(--t-md); font-weight: 600; color: var(--fg-bright); margin: 0; }
  .gl-ai-body { padding: 16px 18px; display: flex; flex-direction: column; gap: 10px; overflow: auto; }
  .gl-ai-body .input { width: 100%; resize: vertical; min-height: 96px; }
  .gl-ai-actions { display: flex; align-items: center; gap: 12px; }
  .gl-ai-err { color: var(--err); font-size: var(--t-xs); }
  .gl-ai-list { list-style: none; padding: 0; margin: 6px 0 0; display: flex; flex-direction: column; gap: 6px; max-height: 360px; overflow: auto; }
  .gl-ai-item label {
    display: grid;
    grid-template-columns: 18px 1fr;
    column-gap: 8px;
    row-gap: 2px;
    align-items: start;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    cursor: pointer;
    background: var(--surface);
  }
  .gl-ai-item label:hover { border-color: var(--accent); }
  .gl-ai-item input[type="checkbox"] { margin-top: 3px; }
  .gl-ai-term { grid-column: 2; font-size: var(--t-sm); color: var(--fg-bright); font-weight: 600; }
  .gl-ai-def  { grid-column: 2; font-size: var(--t-xs); color: var(--fg-muted); line-height: 1.5; }
  .gl-ai-foot { padding: 12px 18px; border-top: 1px solid var(--border); display: flex; justify-content: flex-end; gap: 8px; }
</style>
