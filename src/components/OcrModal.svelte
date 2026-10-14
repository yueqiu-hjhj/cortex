<script lang="ts">
  import { tick, untrack } from "svelte";
  import { app } from "../lib/store.svelte";
  import { extractCards } from "../lib/ocr";
  import Icon from "./Icon.svelte";

  let dialog: HTMLDialogElement;
  let loading = $state(false);
  let error = $state("");
  let subjectId = $state("");
  let topicId = $state("");
  let title = $state("截图闪卡");
  let generation = 0;
  const topics = $derived(app.subjects.find((s) => s.id === subjectId)?.topics ?? []);
  const valid = $derived(app.ocrCards.length > 0 && app.ocrCards.every((c) => c.front.trim() && c.back.trim()));

  async function extract(image: NonNullable<typeof app.ocrImage>) {
    const request = ++generation;
    loading = true;
    error = "";
    try {
      const cards = await extractCards(image.mime, image.base64);
      if (request === generation && app.ocrOpen) app.ocrCards = cards;
    } catch (e) {
      if (request === generation) error = String(e);
    } finally {
      if (request === generation) loading = false;
    }
  }

  $effect(() => {
    const image = app.ocrOpen ? app.ocrImage : null;
    if (!image) { dialog?.close(); return; }
    untrack(() => {
      subjectId = app.activeSubjectId ?? app.subjects[0]?.id ?? "";
      topicId = "";
      title = "截图闪卡";
      void tick().then(() => { if (app.ocrOpen && !dialog.open) dialog.showModal(); });
      void extract(image);
    });
    return () => { generation++; };
  });

  function move(index: number, delta: number) {
    const cards = [...app.ocrCards];
    const other = index + delta;
    if (other < 0 || other >= cards.length) return;
    [cards[index], cards[other]] = [cards[other], cards[index]];
    app.ocrCards = cards;
  }

  async function save() {
    error = "";
    try { await app.saveOcr(subjectId, topicId || null, title); }
    catch (e) { error = String(e); }
  }
</script>

<dialog bind:this={dialog} aria-labelledby="ocr-title"
  oncancel={(e) => { e.preventDefault(); app.closeOcr(); }}
  onkeydown={(e) => e.stopPropagation()}>
  {#if app.ocrImage}
    <header>
      <img src={app.ocrImage.url} alt="待提取截图" />
      <div class="heading">
        <h2 id="ocr-title">截图闪卡</h2>
        <label>卡组名称<input class="input" bind:value={title} maxlength="120" disabled={app.ocrSaving} /></label>
      </div>
      <button class="icon-button" title="关闭" aria-label="关闭" disabled={app.ocrSaving} onclick={() => app.closeOcr()}><Icon name="x" /></button>
    </header>
    <div class="cards" aria-busy={loading}>
      {#if loading}
        <p class="status" role="status"><span class="spinner"></span> 正在提取问答…</p>
      {:else}
        {#each app.ocrCards as card, i}
          <div class="draft">
            <span class="number">{i + 1}</span>
            <label>问题<textarea class="input" rows="2" bind:value={card.front} disabled={app.ocrSaving}></textarea></label>
            <label>答案<textarea class="input" rows="3" bind:value={card.back} disabled={app.ocrSaving}></textarea></label>
            <div class="actions">
              <button class="icon-button" aria-label="上移第 {i + 1} 张" title="上移" disabled={app.ocrSaving || i === 0} onclick={() => move(i, -1)}><span class="up"><Icon name="arrowR" /></span></button>
              <button class="icon-button" aria-label="下移第 {i + 1} 张" title="下移" disabled={app.ocrSaving || i === app.ocrCards.length - 1} onclick={() => move(i, 1)}><span class="down"><Icon name="arrowR" /></span></button>
              <button class="icon-button" aria-label="删除第 {i + 1} 张" title="删除" disabled={app.ocrSaving} onclick={() => { app.ocrCards = app.ocrCards.filter((_, n) => n !== i); }}><Icon name="x" /></button>
            </div>
          </div>
        {/each}
        <button class="btn btn--sm" disabled={app.ocrSaving || app.ocrCards.length >= 50} onclick={() => { app.ocrCards = [...app.ocrCards, { front: "", back: "" }]; }}><Icon name="plus" size={14} /> 添加一张</button>
      {/if}
    </div>
    {#if error}
      <div class="error" role="alert">{error}</div>
      {#if !loading && app.ocrCards.length === 0}
        <button class="btn btn--sm retry" onclick={() => { if (app.ocrImage) void extract(app.ocrImage); }}>重试</button>
      {/if}
    {/if}
    <footer>
      <label>学科<select class="input" bind:value={subjectId} disabled={app.ocrSaving} onchange={() => { topicId = ""; }}>
        <option value="" disabled>选择学科</option>
        {#each app.subjects as subject}<option value={subject.id}>{subject.name}</option>{/each}
      </select></label>
      <label>主题<select class="input" bind:value={topicId} disabled={app.ocrSaving}>
        <option value="">不指定主题</option>
        {#each topics as topic}<option value={topic.id}>{topic.name}</option>{/each}
      </select></label>
      <button class="btn save" disabled={loading || app.ocrSaving || !subjectId || !valid} onclick={save}>
        {#if app.ocrSaving}<span class="spinner"></span> 保存中…{:else}<Icon name="cards" size={14} /> 入 {app.ocrCards.length} 张卡{/if}
      </button>
    </footer>
  {/if}
</dialog>

<style>
  dialog { width: min(860px, calc(100vw - 32px)); max-height: calc(100dvh - 32px); padding: 0; border: 1px solid var(--border-strong); border-radius: 8px; background: var(--surface); color: var(--fg); overflow-y: auto; }
  dialog::backdrop { background: #0008; }
  header { display: flex; gap: 18px; padding: 20px; border-bottom: 1px solid var(--border); align-items: flex-start; }
  header img { width: 30%; height: 140px; object-fit: contain; background: var(--bg); border-radius: 6px; }
  .heading { flex: 1; min-width: 0; }
  h2 { font-size: 18px; margin: 0 0 16px; }
  label { display: flex; flex-direction: column; gap: 6px; min-width: 0; font-size: 12px; }
  .input { width: 100%; box-sizing: border-box; }
  textarea { resize: vertical; min-height: 60px; }
  .cards { padding: 20px; }
  .draft { display: grid; grid-template-columns: 24px 1fr 1fr 32px; gap: 12px; padding: 12px 0; border-bottom: 1px solid var(--border); margin-bottom: 12px; }
  .number { color: var(--fg-muted); padding-top: 25px; }
  .actions { display: flex; flex-direction: column; gap: 4px; padding-top: 20px; }
  .icon-button { width: 30px; height: 30px; display: grid; place-items: center; border: 1px solid var(--border); background: transparent; color: inherit; border-radius: 4px; cursor: pointer; }
  button:disabled { opacity: .45; cursor: default; }
  .up { transform: rotate(-90deg); } .down { transform: rotate(90deg); }
  footer { display: grid; grid-template-columns: 1fr 1fr auto; gap: 12px; padding: 20px; border-top: 1px solid var(--border); align-items: end; }
  .save { background: var(--ok, #2dd5b7); color: var(--bg); min-height: 36px; }
  .error { padding: 0 20px 16px; color: var(--err); overflow-wrap: anywhere; }
  .retry { margin: 0 20px 16px; }
  .status { display: flex; align-items: center; gap: 8px; }
  .spinner { width: 14px; height: 14px; border: 2px solid currentColor; border-right-color: transparent; border-radius: 50%; animation: spin .8s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  @media (max-width: 600px) {
    header { padding: 14px; gap: 10px; } header img { height: 100px; }
    .cards { padding: 14px; } .draft { grid-template-columns: 24px 1fr 32px; }
    .draft label { grid-column: 2; } .number { grid-row: 1 / 3; }
    .actions { grid-column: 3; grid-row: 1 / 3; }
    footer { padding: 14px; grid-template-columns: 1fr 1fr; } .save { grid-column: 1 / -1; }
  }
  @media (prefers-reduced-motion: reduce) { .spinner { animation: none; } }
</style>
