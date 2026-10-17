<script lang="ts">
  // Literature Reader â focused paper reading surface.
  //
  // Three entry modes:
  //   1. URL: paste a paper URL, the backend fetches it (via fetch_page) and
  //      we render the readable text. Best for arXiv abstracts, blog posts,
  //      and HTML papers.
  //   2. Paste: drop a chunk of text in directly (PDF excerpts, OCR dumps).
  //   3. Library: open an existing text-kind source from the subject library.
  //
  // Selection toolbar: when the user selects text inside the article body, a
  // small floating bar appears with Translate / Explain / Summarize / Copy.
  // Each action calls literature_explain; the result streams into a right-side
  // panel. Offline users (no LLM) get the Copy option only.
  //
  // Repair: a "Repair layout" button re-passes the text through
  // literature_repair (merge hyphenated line breaks, fold soft wraps, etc.)
  // to undo the worst PDF extraction damage.
  //
  // Save to project: turns the paper into a real cortex source under a chosen
  // subject via literature_save_source, so the text shows up in search,
  // cheatsheet, and the knowledge tree.

  import { app } from "../lib/store.svelte";
  import * as api from "../lib/api";
  import Icon from "../components/Icon.svelte";
  import DatePicker from "../components/DatePicker.svelte";
  import { tick } from "svelte";

  type Mode = "translate:zh" | "translate:en" | "explain" | "summarize" | "keypoints" | "define";

  let mode = $state<Mode>("translate:zh");
  let input = $state("");
  let inputKind = $state<"url" | "paste">("url");
  let title = $state("");
  let finalUrl = $state<string | null>(null);
  let rawText = $state("");
  let repaired = $state(false);
  let loading = $state(false);
  let error = $state<string | null>(null);

  let selection = $state<{ text: string; x: number; y: number } | null>(null);
  let selectionAction = $state<null | { mode: Mode; prompt: string; result: string; loading: boolean }>(null);

  let saveOpen = $state(false);
  let saveSubjectId = $state<string | null>(app.activeSubjectId);
  let saving = $state(false);
  let saveError = $state<string | null>(null);

  $effect(() => {
    if (!saveSubjectId && app.subjects.length) {
      saveSubjectId = app.activeSubjectId ?? app.subjects[0].id;
    }
  });

  async function openUrl() {
    const u = input.trim();
    if (!u) return;
    loading = true; error = null;
    try {
      const page = await api.fetchPage(u);
      finalUrl = page.final_url || u;
      title = page.title || finalUrl;
      rawText = page.text;
      repaired = false;
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  async function repairLayout() {
    if (!rawText) return;
    loading = true;
    try {
      rawText = await api.literatureRepair(rawText);
      repaired = true;
    } catch (e) { error = String(e); }
    finally { loading = false; }
  }

  function onMouseUp() {
    const sel = window.getSelection();
    if (!sel || sel.isCollapsed) { selection = null; return; }
    const text = sel.toString().trim();
    if (text.length < 2) { selection = null; return; }
    const range = sel.getRangeAt(0);
    const rect = range.getBoundingClientRect();
    selection = { text, x: rect.left + rect.width / 2, y: rect.top - 8 };
  }

  async function runSelection(m: Mode) {
    if (!selection) return;
    selectionAction = { mode: m, prompt: selection.text, result: "", loading: true };
    try {
      const r = await api.literatureExplain(selection.text, m);
      if (selectionAction) selectionAction.result = r;
    } catch (e) {
      if (selectionAction) selectionAction.result = `(${String(e)})`;
    } finally {
      if (selectionAction) selectionAction.loading = false;
    }
  }

  function closeSelectionAction() {
    selectionAction = null;
  }

  async function copySelection() {
    if (!selection) return;
    try { await navigator.clipboard.writeText(selection.text); } catch {}
    selection = null;
  }

  async function saveAsSource() {
    if (!saveSubjectId || !title.trim() || !rawText.trim()) return;
    saving = true; saveError = null;
    try {
      const id = await api.literatureSaveSource(
        saveSubjectId,
        title.trim(),
        rawText,
        finalUrl,
        null,
      );
      app.pushToast({
        kind: "success",
        title: "已保存到项目",
        body: title.trim(),
        action: { label: "打开", run: async () => { const full = await api.getSource(id).catch(() => null); if (full) app.openSource(full); } },
      });
      saveOpen = false;
    } catch (e) {
      saveError = String(e);
    } finally {
      saving = false;
    }
  }

  const charCount = $derived(rawText.length);
  const wordCount = $derived(rawText.trim() ? rawText.trim().split(/\s+/).length : 0);
  const canSave = $derived(!!saveSubjectId && title.trim().length > 0 && rawText.trim().length > 0);

  const modeLabel: Record<Mode, string> = {
    "translate:zh": "ç¿»è¯æä¸­æ",
    "translate:en": "Translate to English",
    "explain": "AI è®²è§£",
    "summarize": "æè¦",
    "keypoints": "æ°å¾",
    "define": "æ¯è¯­",
  };
</script>

<div class="lr">
  <header class="lr-head">
    <div>
      <div class="eyebrow">Literature reader</div>
      <h1 class="lr-title">{title || "æç®éè¯»"}</h1>
      {#if finalUrl}
        <a class="lr-url mono" href={finalUrl} target="_blank" rel="noreferrer">{finalUrl}</a>
      {/if}
    </div>
    <div class="lr-tools">
      <div class="seg">
        <button class="seg-btn{inputKind === 'url' ? ' on' : ''}" onclick={() => (inputKind = "url")}>URL</button>
        <button class="seg-btn{inputKind === 'paste' ? ' on' : ''}" onclick={() => (inputKind = "paste")}>粘贴文本</button>
      </div>
      {#if inputKind === "url"}
        <input class="ipt" placeholder="https://arxiv.org/abs/..." bind:value={input} onkeydown={(e) => e.key === "Enter" && void openUrl()} />
        <button class="btn primary" disabled={!input.trim() || loading} onclick={() => void openUrl()}>
          <Icon name="external" size={12} /> 打开
        </button>
      {:else}
        <textarea class="ipt paste" placeholder="把 PDF / OCR / 网页文本贴到这里…" bind:value={rawText}></textarea>
        <button class="btn" disabled={!rawText.trim()} onclick={() => { title = title || "粘贴的文本"; }}>
          <Icon name="check" size={12} /> 加载
        </button>
      {/if}
      <button class="btn" disabled={!rawText || loading} onclick={() => void repairLayout()}>
        <Icon name="refresh" size={12} /> 修复排版
      </button>
      <button class="btn primary" disabled={!canSave} onclick={() => (saveOpen = true)}>
        <Icon name="plus" size={12} /> 保存到项目
      </button>
    </div>
  </header>

  {#if error}
    <div class="err mono">{error}</div>
  {/if}

  <div class="lr-body" onmouseup={onMouseUp} role="presentation">
    <article class="paper">
      {#if loading}
        <div class="paper-empty mono">正在加载…</div>
      {:else if !rawText}
        <div class="paper-empty">
          <div class="paper-empty-big">还没有内容</div>
          <div class="paper-empty-sub mono">在上方输入论文 URL 打开页面，或切换到「粘贴文本」直接读取本地摘录。</div>
        </div>
      {:else}
        {#each rawText.split(/\n{2,}/) as para, i (i)}
          {#if para.trim()}
            <p>{para.trim()}</p>
          {/if}
        {/each}
        <div class="paper-foot mono">
          {wordCount.toLocaleString()} 词 · {charCount.toLocaleString()} 字符{#if repaired} · 已修复排版{/if}
        </div>
      {/if}
    </article>

    {#if selectionAction}
      <aside class="ans">
        <div class="ans-head">
          <div class="ans-title">{modeLabel[selectionAction.mode]}</div>
          <button class="ans-x" onclick={closeSelectionAction} title="关闭"><Icon name="x" size={12} /></button>
        </div>
        <div class="ans-q mono">{selectionAction.prompt}</div>
        <div class="ans-body">
          {#if selectionAction.loading}
            <span class="mono faint">生成中…</span>
          {:else}
            {selectionAction.result}
          {/if}
        </div>
      </aside>
    {/if}
  </div>

  {#if selection}
    <div class="float" style:left={selection.x + "px"} style:top={selection.y + "px"} role="toolbar">
      <button class="fb" onclick={() => void runSelection("translate:zh")}><Icon name="globe" size={11} /> 译</button>
      <button class="fb" onclick={() => void runSelection("translate:en")}><Icon name="globe" size={11} /> EN</button>
      <button class="fb" onclick={() => void runSelection("explain")}><Icon name="bolt" size={11} /> 讲解</button>
      <button class="fb" onclick={() => void runSelection("keypoints")}><Icon name="star" size={11} /> 要点</button>
      <button class="fb" onclick={() => void runSelection("summarize")}><Icon name="doc" size={11} /> 摘要</button>
      <button class="fb" onclick={() => void runSelection("define")}><Icon name="book" size={11} /> 术语</button>
      <button class="fb" onclick={copySelection}><Icon name="check" size={11} /> 复制</button>
    </div>
  {/if}

  {#if saveOpen}
    <div class="modal-bg" onclick={() => (saveOpen = false)} role="presentation"></div>
    <div class="modal" role="dialog" aria-modal="true">
      <div class="modal-h">
        <div class="modal-t">保存到项目</div>
        <button class="ans-x" onclick={() => (saveOpen = false)}><Icon name="x" size={12} /></button>
      </div>
      <div class="modal-b">
        <label class="fld">
          <span class="fld-l">标题</span>
          <input class="ipt" bind:value={title} />
        </label>
        <label class="fld">
          <span class="fld-l">科目</span>
          <select class="ipt sel" bind:value={saveSubjectId}>
            {#each app.subjects as s (s.id)}
              <option value={s.id}>{s.name}</option>
            {/each}
          </select>
        </label>
        <div class="modal-meta mono">
          {wordCount.toLocaleString()} 词 · {charCount.toLocaleString()} 字符
        </div>
        {#if saveError}
          <div class="err mono">{saveError}</div>
        {/if}
        <div class="modal-actions">
          <button class="btn" onclick={() => (saveOpen = false)}>取消</button>
          <button class="btn primary" disabled={!canSave || saving} onclick={() => void saveAsSource()}>
            <Icon name="check" size={12} /> {saving ? "保存中…" : "保存"}
          </button>
        </div>
      </div>
    </div>
  {/if}
</div>

<style>
  .lr {
    flex: 1; min-height: 0; overflow: auto;
    padding: 22px 28px 32px;
    display: flex; flex-direction: column; gap: 14px;
    position: relative;
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

  .lr-head {
    display: flex; align-items: flex-start; justify-content: space-between; gap: 16px;
    border-bottom: 1px solid var(--border); padding-bottom: 14px; flex-wrap: wrap;
  }
  .lr-title {
    margin: 4px 0 0;
    font-family: var(--font-display);
    font-size: 22px; font-weight: 600; letter-spacing: -0.01em;
    color: var(--fg-bright);
    word-break: break-word;
  }
  .lr-url {
    margin-top: 4px; font-size: 11px; color: var(--fg-muted);
    text-decoration: none; word-break: break-all;
  }
  .lr-url:hover { color: var(--accent); text-decoration: underline; }
  .lr-tools { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; max-width: 720px; }

  .ipt {
    font-family: var(--font-mono); font-size: 12px; color: var(--fg);
    background: var(--surface-2); border: 1px solid var(--border);
    border-radius: 7px; padding: 6px 9px; outline: none; min-width: 200px;
  }
  .ipt:focus { border-color: var(--accent-dim); color: var(--fg-bright); }
  .ipt.paste { width: 320px; min-height: 38px; resize: vertical; font-family: var(--font-read); }
  .ipt.sel { min-width: 140px; }

  .seg { display: inline-flex; border: 1px solid var(--border); border-radius: 7px; overflow: hidden; }
  .seg-btn {
    background: var(--surface-2); color: var(--fg-muted); border: none; cursor: pointer;
    font-family: var(--font-mono); font-size: 11px; padding: 6px 10px;
  }
  .seg-btn + .seg-btn { border-left: 1px solid var(--border); }
  .seg-btn.on { background: color-mix(in srgb, var(--accent) 22%, transparent); color: var(--fg-bright); }

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

  .lr-body {
    display: grid; grid-template-columns: minmax(0, 1fr); gap: 14px;
    align-items: start;
  }
  .lr-body:has(.ans) { grid-template-columns: minmax(0, 1fr) 320px; }

  .paper {
    background: var(--surface); border: 1px solid var(--border);
    border-radius: 8px; padding: 28px 32px;
    font-family: var(--font-read); font-size: 14.5px; line-height: 1.78;
    color: var(--fg);
  }
  .paper p { margin: 0 0 1em; }
  .paper-foot {
    margin-top: 24px; padding-top: 14px; border-top: 1px solid var(--border);
    font-size: 11px; color: var(--fg-faint);
  }
  .paper-empty {
    padding: 80px 24px; text-align: center;
  }
  .paper-empty-big {
    font-family: var(--font-display); font-size: 18px; font-weight: 600;
    color: var(--fg-bright);
  }
  .paper-empty-sub { margin: 8px auto 0; max-width: 460px; font-size: 11.5px; color: var(--fg-muted); }

  .ans {
    background: var(--surface); border: 1px solid var(--border);
    border-radius: 8px; padding: 14px 16px;
    position: sticky; top: 0;
    max-height: calc(100vh - 200px); overflow: auto;
  }
  .ans-head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 8px; }
  .ans-title { font-family: var(--font-display); font-size: 14px; font-weight: 600; color: var(--fg-bright); }
  .ans-x { background: none; border: none; color: var(--fg-muted); cursor: pointer; padding: 4px; border-radius: 4px; }
  .ans-x:hover { color: var(--fg-bright); background: var(--surface-2); }
  .ans-q {
    background: var(--surface-2); border-left: 2px solid var(--accent-dim);
    padding: 8px 10px; font-size: 11.5px; color: var(--fg-muted);
    margin-bottom: 10px; line-height: 1.5; max-height: 100px; overflow: auto;
  }
  .ans-body { font-family: var(--font-read); font-size: 13.5px; line-height: 1.7; color: var(--fg); white-space: pre-wrap; }

  .float {
    position: fixed; transform: translate(-50%, -100%);
    display: flex; gap: 2px; padding: 4px;
    background: var(--surface-3); border: 1px solid var(--border-strong);
    border-radius: 7px; box-shadow: 0 4px 16px rgba(0,0,0,0.4);
    z-index: 100;
  }
  .fb {
    background: none; border: none; cursor: pointer;
    color: var(--fg-muted); font-family: var(--font-mono); font-size: 11px;
    padding: 4px 8px; border-radius: 4px;
    display: inline-flex; align-items: center; gap: 4px;
  }
  .fb:hover { background: color-mix(in srgb, var(--accent) 22%, transparent); color: var(--fg-bright); }

  .modal-bg { position: fixed; inset: 0; background: rgba(0,0,0,0.4); z-index: 90; }
  .modal {
    position: fixed; top: 50%; left: 50%; transform: translate(-50%, -50%);
    background: var(--surface); border: 1px solid var(--border);
    border-radius: 8px; padding: 16px 18px; min-width: 360px; z-index: 91;
    box-shadow: 0 8px 24px rgba(0,0,0,0.5);
  }
  .modal-h { display: flex; align-items: center; justify-content: space-between; margin-bottom: 12px; }
  .modal-t { font-family: var(--font-display); font-size: 15px; font-weight: 600; color: var(--fg-bright); }
  .modal-b { display: flex; flex-direction: column; gap: 10px; }
  .fld { display: flex; flex-direction: column; gap: 4px; }
  .fld-l { font-family: var(--font-mono); font-size: 10px; color: var(--fg-faint); text-transform: uppercase; letter-spacing: 0.04em; }
  .modal-meta { font-size: 11px; color: var(--fg-faint); }
  .modal-actions { display: flex; gap: 8px; justify-content: flex-end; }
</style>
