<script lang="ts">
  // Vocabulary workbench - corpus-aware English term dictionary.
  //
  // Per subject: a list of terms with frequency, definition, in-context
  // examples, and a 3-state mastery (new / learning / known). The user
  // can:
  //   - browse the existing terms (sorted by frequency)
  //   - add a term manually
  //   - extract the top-N most frequent English tokens from the subject's
  //     source corpus (vocab_extract on the backend)
  //   - edit definition / examples / status inline
  //   - delete a term
  //
  // The list re-loads on subject change. State and "change" are deliberate
  // - the whole point is to study a subject-specific corpus.

  import { app } from "../lib/store.svelte";
  import * as api from "../lib/api";
  import type { VocabTerm, MineCandidate } from "../lib/api";
  import Icon from "../components/Icon.svelte";

  let terms = $state<VocabTerm[]>([]);
  let loaded = $state(false);
  let loading = $state(false);
  let error = $state<string | null>(null);

  // ── 语料采词：模型挑词，原句逐字校验，保存即进复习轮转 ──
  let mineKind = $state<"text" | "source" | "note">("text");
  let mineId = $state("");
  let mineText = $state("");
  let candidates = $state<MineCandidate[]>([]);
  let dropped = $state(0);
  let picked = $state<Record<string, boolean>>({});
  let mining = $state(false);
  let sources = $state<api.Source[]>([]);
  let notes = $state<api.Note[]>([]);

  const sid = $derived(app.activeSubjectId);

  let newTerm = $state("");
  let editingId = $state<string | null>(null);
  let editDefinition = $state("");
  let editExamples = $state("");
  let editStatus = $state<"new" | "learning" | "known">("new");

  $effect(() => { void loadTerms(); });
  $effect(() => { void loadMaterial(); });

  async function loadMaterial() {
    if (!sid) { sources = []; notes = []; return; }
    const s = sid;
    try {
      const [ss, ns] = await Promise.all([api.listSources(s), api.listNotes(s)]);
      if (s !== sid) return;
      sources = ss.filter((x) => (x.content ?? "").trim().length > 40);
      notes = ns.filter((n) => n.body.trim().length > 40);
      if (mineKind === "source" && sources.length && !mineId) mineId = sources[0].id;
      if (mineKind === "note" && notes.length && !mineId) mineId = notes[0].id;
    } catch { /* pickers just stay empty */ }
  }

  async function runMine() {
    if (!sid || mining) return;
    let text = mineText;
    if (mineKind === "source") {
      const src = sources.find((x) => x.id === mineId);
      if (!src) return;
      text = src.content ?? "";
      // 大源只取前 6000 字——和后端采词窗口一致
      text = text.slice(0, 6000);
    } else if (mineKind === "note") {
      const note = notes.find((n) => n.id === mineId);
      if (!note) return;
      text = note.body.slice(0, 6000);
    }
    if (text.trim().length < 40) { error = "材料太短，采不出东西"; return; }
    mining = true; error = null;
    try {
      const out = await api.vocabMine(text);
      candidates = out.candidates;
      dropped = out.dropped;
      picked = {};
      for (const c of out.candidates) picked[c.normalized] = true;
      if (out.candidates.length === 0) {
        error = out.dropped > 0
          ? `模型给了 ${out.dropped} 个词，但原句都对不上材料——逐字校验全挡掉了，换段材料或换个模型`
          : "没采到值得学的词";
      }
    } catch (e) { error = String(e); }
    finally { mining = false; }
  }

  async function saveMined() {
    if (!sid || candidates.length === 0) return;
    loading = true; error = null;
    try {
      const chosen = candidates.filter((c) => picked[c.normalized]);
      if (chosen.length === 0) { error = "一个都没选"; loading = false; return; }
      const out = await api.vocabSaveMined(sid, chosen);
      app.pushToast({
        kind: "success",
        title: `已保存 ${out.saved} 个词`,
      });
      candidates = [];
      await loadTerms();
    } catch (e) { error = String(e); }
    finally { loading = false; }
  }

  const pickedCount = $derived(Object.values(picked).filter(Boolean).length);

  async function loadTerms() {
    if (!sid) { terms = []; loaded = true; return; }
    const subject = sid;
    error = null; loaded = false;
    try {
      const rows = await api.vocabList(subject);
      if (sid === subject) terms = rows;
    } catch (e) { error = String(e); }
    finally { if (sid === subject) loaded = true; }
  }

  async function addTerm() {
    const t = newTerm.trim();
    if (!t || !sid) return;
    try {
      await api.vocabAdd(sid, t);
      newTerm = "";
      await loadTerms();
    } catch (e) { error = String(e); }
  }

  async function extractFromCorpus() {
    if (!sid) return;
    loading = true; error = null;
    try {
      const n = await api.vocabExtract(sid, 200);
      app.pushToast({ kind: "success", title: `Extracted ${n} terms` });
      await loadTerms();
    } catch (e) { error = String(e); }
    finally { loading = false; }
  }

  function startEdit(t: VocabTerm) {
    editingId = t.id;
    editDefinition = t.definition ?? "";
    editExamples = t.examples ?? "";
    editStatus = (t.status as "new" | "learning" | "known") ?? "new";
  }

  async function saveEdit() {
    if (!editingId) return;
    try {
      await api.vocabUpdate(
        editingId,
        editDefinition.trim(),
        editExamples.trim(),
        editStatus,
      );
      editingId = null;
      await loadTerms();
    } catch (e) { error = String(e); }
  }

  async function del(t: VocabTerm) {
    if (!await app.confirm({ title: "删除术语？", body: t.term, danger: true, okLabel: "删除" })) return;
    try {
      const undoId = await api.vocabDelete(t.id);
      app.destructiveDone({ title: "术语已删除", body: t.term, undoId });
      await loadTerms();
    } catch (e) { error = String(e); }
  }

  const counts = $derived({
    total: terms.length,
    known: terms.filter((t) => t.status === "known").length,
    learning: terms.filter((t) => t.status === "learning").length,
    new: terms.filter((t) => t.status === "new").length,
  });

  function statusColor(s: string): string {
    if (s === "known") return "var(--ok)";
    if (s === "learning") return "var(--warn)";
    return "var(--fg-faint)";
  }
</script>

<div class="vw">
  <header class="vw-head">
    <div>
      <div class="eyebrow">词汇本</div>
      <h1 class="vw-title">{app.activeSubject?.name ?? "未选中科目"}</h1>
      <p class="vw-sub mono">词频 · 术语定义 · 语境例句 · 掌握度</p>
    </div>
    <div class="vw-tools">
      <input
        class="ipt"
        placeholder="添加术语"
        bind:value={newTerm}
        onkeydown={(e) => e.key === "Enter" && void addTerm()}
        disabled={!sid}
      />
      <button class="btn" disabled={!sid} onclick={() => void addTerm()}>
        <Icon name="plus" size={12} /> 添加
      </button>
      <button class="btn primary" disabled={!sid || loading} onclick={() => void extractFromCorpus()}>
        <Icon name="bolt" size={12} /> {loading ? "提取中..." : "从语料库提取"}
      </button>
    </div>
  </header>

  {#if !app.subjects.length}
    <div class="vw-empty">
      <div class="vw-empty-big">还没有科目</div>
      <div class="vw-empty-sub mono">创建科目并导入语料后才能提取术语。</div>
    </div>
  {:else if !sid}
    <div class="vw-empty">
      <div class="vw-empty-big">请选择一个科目</div>
    </div>
  {:else if error}
    <div class="err mono">{error}</div>
  {:else if !loaded}
    <div class="vw-empty"><div class="vw-empty-big">加载中...</div></div>
  {:else}
    <div class="kpis">
      <div class="kpi"><i>总词</i><b>{counts.total}</b></div>
      <div class="kpi"><i>已掌握</i><b>{counts.known}</b></div>
      <div class="kpi"><i>学习中</i><b>{counts.learning}</b></div>
      <div class="kpi"><i>未学</i><b>{counts.new}</b></div>
    </div>

    {#if sid}
      <section class="vw-mine">
        <div class="vw-mine-head">
          <h2 class="vw-h2">从材料采词</h2>
          <span class="vw-hint">模型挑词、写释义；原句必须逐字来自材料——对不上的会被挡掉</span>
        </div>
        <div class="vw-mine-row">
          <select class="ipt sel" bind:value={mineKind}>
            <option value="text">粘贴文本</option>
            <option value="source">来源全文</option>
            <option value="note">笔记</option>
          </select>
          {#if mineKind === "source"}
            <select class="ipt sel grow" bind:value={mineId}>
              {#each sources as s (s.id)}
                <option value={s.id}>{s.name}</option>
              {/each}
            </select>
          {:else if mineKind === "note"}
            <select class="ipt sel grow" bind:value={mineId}>
              {#each notes as n (n.id)}
                <option value={n.id}>{n.title}</option>
              {/each}
            </select>
          {:else}
            <textarea
              class="ipt area grow"
              placeholder="粘一段英文材料（≥40 字）…"
              bind:value={mineText}
            ></textarea>
          {/if}
          <button class="btn primary" disabled={mining || !sid} onclick={() => void runMine()}>
            <Icon name="bolt" size={12} /> {mining ? "采词中…" : "采词"}
          </button>
        </div>

        {#if candidates.length > 0}
          <div class="vw-cands">
            <div class="vw-cands-head">
              <span class="mono">
                {candidates.length} 个候选 · 已选 {pickedCount}
                {#if dropped > 0}<span title="模型给的原句不在材料里，已挡掉"> · 挡掉 {dropped} 个对不上原句的</span>{/if}
              </span>
              <button class="btn primary" disabled={loading || pickedCount === 0} onclick={() => void saveMined()}>
                保存 {pickedCount} 个为闪卡
              </button>
            </div>
            {#each candidates as c (c.normalized)}
              <label class="vw-cand">
                <input type="checkbox" bind:checked={picked[c.normalized]} />
                <span class="vw-cand-word">{c.word}</span>
                <span class="vw-cand-def">{c.definition}</span>
                <span class="vw-cand-sent" title={c.sentence}>{c.sentence}</span>
              </label>
            {/each}
          </div>
        {/if}
      </section>
    {/if}

    {#if terms.length === 0}
      <div class="vw-empty">
        <div class="vw-empty-big">还没有术语</div>
        <div class="vw-empty-sub mono">点“从语料库提取”从该科目的源文本里扫词频，或手动添加。</div>
      </div>
    {:else}
      <div class="t">
        <div class="t-head mono">
          <span>术语</span>
          <span>词频</span>
          <span>定义</span>
          <span>例句</span>
          <span>状态</span>
          <span></span>
        </div>
        {#each terms as t (t.id)}
          {@const editing = editingId === t.id}
          <div class="t-row" class:editing>
            <span class="t-term read">{t.term}</span>
            <span class="t-freq mono">{t.freq}</span>
            {#if editing}
              <input class="ipt small" placeholder="定义" bind:value={editDefinition} />
            {:else}
              <span class="t-def mono faint">{t.definition || ""}</span>
            {/if}
            {#if editing}
              <textarea class="ipt small area" placeholder="例句（一行一个）" bind:value={editExamples}></textarea>
            {:else}
              <span class="t-ex mono faint" title={t.examples ?? ""}>{(t.examples ?? "").split("\n").filter(Boolean).length}</span>
            {/if}
            {#if editing}
              <select class="ipt small sel" bind:value={editStatus}>
                <option value="new">未学</option>
                <option value="learning">学习中</option>
                <option value="known">已掌握</option>
              </select>
            {:else}
              <span class="chip mono" style:color={statusColor(t.status)} style:border-color={statusColor(t.status)}>
                {t.status === "known" ? "已掌握" : t.status === "learning" ? "学习中" : "未学"}
              </span>
            {/if}
            <span class="t-actions">
              {#if editing}
                <button class="fb-icon" onclick={() => void saveEdit()} title="保存"><Icon name="check" size={12} /></button>
                <button class="fb-icon" onclick={() => (editingId = null)} title="取消"><Icon name="x" size={12} /></button>
              {:else}
                <button class="fb-icon" onclick={() => startEdit(t)} title="编辑"><Icon name="pencil" size={11} /></button>
                <button class="fb-icon" onclick={() => void del(t)} title="删除"><Icon name="x" size={11} /></button>
              {/if}
            </span>
          </div>
        {/each}
      </div>
    {/if}
  {/if}
</div>

<style>
  .vw {
    flex: 1; min-height: 0; overflow: auto;
    padding: 22px 28px 32px;
    display: flex; flex-direction: column; gap: 14px;
  }
  .eyebrow {
    font-family: var(--font-mono);
    font-size: 10.5px;
    color: var(--fg-faint);
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }
  .faint { color: var(--fg-faint); }
  .mono { font-family: var(--font-mono); }
  .read { font-family: var(--font-read); }

  .vw-head {
    display: flex; align-items: flex-start; justify-content: space-between; gap: 16px;
    border-bottom: 1px solid var(--border); padding-bottom: 14px; flex-wrap: wrap;
  }
  .vw-title {
    margin: 4px 0 0;
    font-family: var(--font-display);
    font-size: 22px; font-weight: 600; letter-spacing: -0.01em;
    color: var(--fg-bright);
  }
  .vw-sub { margin: 6px 0 0; font-size: 11.5px; color: var(--fg-muted); }
  .vw-tools { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .ipt {
    font-family: var(--font-mono); font-size: 12px; color: var(--fg);
    background: var(--surface-2); border: 1px solid var(--border);
    border-radius: 7px; padding: 6px 9px; outline: none; min-width: 200px;
  }
  .ipt:focus { border-color: var(--accent-dim); color: var(--fg-bright); }
  .ipt:disabled { opacity: 0.5; }
  .ipt.small { min-width: 0; padding: 4px 7px; font-size: 11px; }
  .ipt.area { min-height: 32px; max-height: 80px; resize: vertical; }
  .ipt.sel { min-width: 90px; }

  .btn {
    display: inline-flex; align-items: center; gap: 6px;
    font-family: var(--font-mono); font-size: 11.5px;
    padding: 7px 12px; border-radius: 7px; cursor: pointer;
    border: 1px solid var(--border); background: var(--surface-2); color: var(--fg);
  }
  .btn:hover:not(:disabled) { color: var(--fg-bright); border-color: var(--border-strong); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn.primary {
    background: color-mix(in srgb, var(--accent) 22%, transparent);
    border-color: color-mix(in srgb, var(--accent) 45%, transparent);
    color: var(--fg-bright);
  }
  .btn.primary:hover:not(:disabled) { background: color-mix(in srgb, var(--accent) 32%, transparent); }

  .err {
    padding: 10px 12px; border-radius: 7px;
    background: color-mix(in srgb, var(--err) 10%, var(--surface));
    border: 1px solid color-mix(in srgb, var(--err) 35%, transparent);
    color: var(--err); font-size: 11.5px;
  }

  .kpis { display: flex; gap: 8px; flex-wrap: wrap; }
  .kpi {
    display: inline-flex; align-items: baseline; gap: 5px;
    font-family: var(--font-mono); font-size: 11px; color: var(--fg-faint);
    background: var(--surface); border: 1px solid var(--border);
    border-radius: 8px; padding: 6px 12px;
  }
  .kpi b { font-size: 17px; color: var(--fg-bright); font-weight: 600; }
  .kpi i { font-style: normal; color: var(--fg-muted); }

  .vw-empty {
    border: 1px dashed var(--border); border-radius: 8px;
    background: var(--surface);
    padding: 48px 24px; text-align: center;
  }
  .vw-h2 {
    margin: 0;
    font-family: var(--font-mono);
    font-size: 10.5px; font-weight: 500;
    color: var(--fg-faint);
    letter-spacing: 0.06em; text-transform: uppercase;
  }
  .vw-hint { font-size: 10.5px; color: var(--fg-faint); }
  .vw-mine {
    border: 1px solid var(--border); border-radius: 10px;
    background: var(--surface);
    padding: 12px 14px;
    display: flex; flex-direction: column; gap: 10px;
  }
  .vw-mine-head { display: flex; align-items: baseline; gap: 10px; flex-wrap: wrap; }
  .vw-mine-row { display: flex; align-items: flex-start; gap: 8px; flex-wrap: wrap; }
  .vw-mine-row .grow { flex: 1; min-width: 220px; }
  .vw-cands {
    border-top: 1px dashed var(--border);
    padding-top: 10px;
    display: flex; flex-direction: column; gap: 6px;
  }
  .vw-cands-head {
    display: flex; align-items: center; justify-content: space-between; gap: 10px;
    font-size: 10.5px; color: var(--fg-faint);
  }
  .vw-cand {
    display: grid;
    grid-template-columns: 18px 130px minmax(120px, 1fr) minmax(160px, 1.4fr);
    align-items: baseline; gap: 8px;
    padding: 6px 8px;
    border-radius: 6px;
    cursor: pointer;
  }
  .vw-cand:hover { background: var(--surface-2); }
  .vw-cand input { accent-color: var(--accent); margin: 0; }
  .vw-cand-word { font-size: 12.5px; color: var(--fg-bright); }
  .vw-cand-def { font-size: 11.5px; color: var(--fg-muted); }
  .vw-cand-sent {
    font-size: 11px; color: var(--fg-faint);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  @media (max-width: 760px) {
    .vw-cand { grid-template-columns: 18px 1fr; }
    .vw-cand-sent { grid-column: 2; white-space: normal; }
  }
  .vw-empty-big {
    font-family: var(--font-display); font-size: 18px; font-weight: 600;
    color: var(--fg-bright);
  }
  .vw-empty-sub { margin: 8px auto 0; max-width: 460px; font-size: 11.5px; color: var(--fg-muted); }

  .t { border: 1px solid var(--border); border-radius: 8px; background: var(--surface); overflow: hidden; }
  .t-head, .t-row {
    display: grid;
    grid-template-columns: 160px 60px minmax(160px, 1fr) minmax(120px, 1fr) 90px 60px;
    align-items: center; gap: 10px;
    padding: 9px 14px;
  }
  .t-head {
    font-size: 10px; color: var(--fg-faint); text-transform: uppercase; letter-spacing: 0.04em;
    border-bottom: 1px solid var(--border);
  }
  .t-row { border-bottom: 1px solid color-mix(in srgb, var(--border) 55%, transparent); }
  .t-row:last-child { border-bottom: none; }
  .t-row:hover:not(.editing) { background: var(--surface-2); }
  .t-row.editing { background: color-mix(in srgb, var(--accent) 8%, var(--surface)); }
  .t-term { font-size: 13px; color: var(--fg-bright); }
  .t-freq { font-size: 12px; color: var(--fg-muted); text-align: right; }
  .t-def { font-size: 12px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .t-ex { font-size: 12px; }
  .chip {
    justify-self: start;
    font-size: 10.5px; padding: 2px 8px; border-radius: 999px;
    border: 1px solid currentColor;
    background: color-mix(in srgb, currentColor 10%, transparent);
  }
  .t-actions { display: flex; gap: 4px; justify-content: flex-end; }
  .fb-icon {
    background: none; border: 1px solid transparent; cursor: pointer;
    color: var(--fg-muted); padding: 4px; border-radius: 4px;
    display: inline-flex; align-items: center; justify-content: center;
  }
  .fb-icon:hover { color: var(--fg-bright); border-color: var(--border); background: var(--surface-2); }
</style>
