<script lang="ts">
  // Literature Reader â focused paper reading surface.
  //
  // Three entry modes:
  //   1. URL: paste a paper URL, the backend fetches it (via fetch_page) and
  //      we render the readable text. Best for arXiv abstracts, blog posts,
  //      and HTML papers.
  //   2. Paste: drop a chunk of text in directly (PDF excerpts, OCR dumps).
  //
  // Selection toolbar: when the user selects text inside the article body, a
  // small floating bar appears with Translate / Explain / Summarize / Copy.
  // Each action calls literature_explain; the result appears in a right-side
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
  import RichText from "../components/RichText.svelte";

  type Mode = "translate:zh" | "translate:en" | "explain" | "summarize" | "keypoints" | "define";

  let searchQuery = $state("");
  let searchBusy = $state(false);
  let searchResult = $state<api.LiteratureResult | null>(null);
  let paperElement: HTMLElement;
  let actionSerial = 0;
  async function searchPapers() {
    if (!searchQuery.trim() || searchBusy) return;
    searchBusy = true; error = null;
    try { searchResult = await api.literatureSearch(searchQuery.trim(), app.activeSubjectId, 30); }
    catch (e) { error = String(e); }
    finally { searchBusy = false; }
  }
  // Source diagnostics: sources that were tried but contributed nothing (they
  // fail silently by design — timeout / rate limit / network) so "结果怎么这么
  // 少" always has a visible answer on screen.
  const failedSources = $derived.by(() => {
    if (!searchResult) return [];
    const ok = new Set(searchResult.sources_succeeded);
    return searchResult.sources_attempted.filter((s) => !ok.has(s));
  });

  // 收藏：把一条检索命中写进 papers 表（Research 页的阅读清单）。
  // 按 DOI 去重由后端保证，所以重复点也不会产生两条记录。
  let savedKeys = $state<Record<string, true>>({});
  let saveBusy = $state<string | null>(null);
  // hitKey → paper id (from paperImport), so the selection toolbar can file an
  // excerpt against the right paper. currentPaperId mirrors it for what's open.
  let savedPapers = $state<Record<string, string>>({});
  let currentHit = $state<api.LiteratureHit | null>(null);
  let currentPaperId = $state<string | null>(null);
  // Selection → excerpt (D-科研: ②读 → ④引 的直通车)
  let excerptBusy = $state(false);
  let excerptFlash = $state<string | null>(null);
  const hitKey = (h: api.LiteratureHit) => h.doi || h.title;
  async function saveHit(h: api.LiteratureHit) {
    const key = hitKey(h);
    if (savedKeys[key] || saveBusy) return;
    saveBusy = key;
    try {
      const paper = await api.paperImport({
        title: h.title,
        authors: h.authors.length ? h.authors.join(", ") : null,
        year: h.year ?? null,
        venue: h.venue ?? null,
        doi: h.doi ?? null,
        url: h.url ?? null,
        abstract_text: h.abstract_text ?? null,
        subject_id: app.activeSubjectId ?? null,
      });
      savedKeys = { ...savedKeys, [key]: true };
      savedPapers = { ...savedPapers, [key]: paper.id };
      if (currentHit && hitKey(currentHit) === key) currentPaperId = paper.id;
    } catch (e) {
      error = String(e);
    } finally {
      saveBusy = null;
    }
  }
  async function saveSelectionExcerpt() {
    if (!selection || excerptBusy) return;
    const quote = selection.text.trim();
    if (!quote) return;
    excerptBusy = true;
    try {
      // Not collected yet → collect first, then file the excerpt against it.
      if (!currentPaperId) {
        if (!currentHit) {
          excerptFlash = "粘贴文本模式没有论文 —— 先收藏论文，再存摘录。";
          setTimeout(() => (excerptFlash = null), 3000);
          return;
        }
        await saveHit(currentHit);
        currentPaperId = savedPapers[hitKey(currentHit)] ?? null;
        if (!currentPaperId) {
          excerptFlash = "收藏失败，摘录未保存。";
          setTimeout(() => (excerptFlash = null), 3000);
          return;
        }
      }
      await api.excerptAdd(quote, currentPaperId);
      excerptFlash = `已存为摘录（${quote.length} 字）`;
      setTimeout(() => (excerptFlash = null), 3000);
      selection = null;
    } catch (e) {
      excerptFlash = `摘录失败：${String(e)}`;
      setTimeout(() => (excerptFlash = null), 4000);
    } finally {
      excerptBusy = false;
    }
  }
  function readHit(hit: api.LiteratureHit) {
    title = hit.title; finalUrl = hit.url ?? null; rawText = hit.abstract_text ?? "";
    input = hit.url ?? ""; repaired = false; selection = null;
    currentHit = hit; currentPaperId = savedPapers[hitKey(hit)] ?? null;
  }
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

  // ── 中英对照（知源的核心精读 UX）：左原文、右译文，逐段对齐 ──────────────
  // 翻译复用 literature_explain 的 translate: 模式；译文逐段懒取并缓存，
  // 段落在同一网格行里天然对齐，不需要滚动同步。
  let bilingual = $state(false);
  let biTarget = $state<"zh" | "en">("zh");
  let translations = $state<Record<number, string>>({});
  let biPending = $state<Set<number>>(new Set());
  // 只为当前正文缓存 —— 换一篇就从零开始。
  let biKey = $state("");

  function biDetectDirection(text: string): "zh" | "en" {
    const head = text.slice(0, 400);
    const ascii = [...head].filter((c) => c.charCodeAt(0) < 128).length;
    return ascii > [...head].length * 0.6 ? "zh" : "en";
  }

  async function startBilingual() {
    if (!rawText) return;
    const key = `${rawText.length}:${rawText.slice(0, 64)}`;
    if (biKey !== key) {
      biKey = key;
      translations = {};
    }
    biTarget = biDetectDirection(rawText);
    bilingual = true;
    const paras = rawText
      .split(/\n{2,}/)
      .map((p) => p.trim())
      .filter((p) => p.length > 0);
    for (let i = 0; i < paras.length; i++) {
      if (translations[i] !== undefined || biPending.has(i)) continue;
      biPending = new Set([...biPending, i]);
      try {
        const t = await api.literatureExplain(paras[i], `translate:${biTarget}`);
        translations = { ...translations, [i]: t.trim() };
      } catch {
        translations = { ...translations, [i]: "（翻译失败 — 检查模型配置）" };
      } finally {
        const next = new Set(biPending);
        next.delete(i);
        biPending = next;
      }
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

  // 2.6 公式修复：\[..\] → $$..$$、散行公式并成展示块，随后段落经 RichText
  // (KaTeX) 渲染，公式从死字符串变成真公式。
  let mathFixed = $state(false);
  async function fixMath() {
    if (!rawText) return;
    loading = true;
    try {
      rawText = await api.literatureFixMath(rawText);
      mathFixed = true;
    } catch (e) { error = String(e); }
    finally { loading = false; }
  }

  function onMouseUp() {
    const sel = window.getSelection();
    if (!sel || sel.isCollapsed) { selection = null; return; }
    if (!sel.anchorNode || !sel.focusNode || !paperElement?.contains(sel.anchorNode) || !paperElement.contains(sel.focusNode)) { selection = null; return; }
    const text = sel.toString().trim();
    if (text.length < 2) { selection = null; return; }
    const range = sel.getRangeAt(0);
    const rect = range.getBoundingClientRect();
    selection = { text, x: rect.left + rect.width / 2, y: rect.top - 8 };
  }

  async function runSelection(m: Mode) {
    if (!selection) return;
    const request = ++actionSerial;
    selectionAction = { mode: m, prompt: selection.text, result: "", loading: true };
    try {
      const r = await api.literatureExplain(selection.text, m);
      if (request === actionSerial && selectionAction) selectionAction.result = r;
    } catch (e) {
      if (request === actionSerial && selectionAction) selectionAction.result = `(${String(e)})`;
    } finally {
      if (request === actionSerial && selectionAction) selectionAction.loading = false;
    }
  }

  function closeSelectionAction() {
    actionSerial++; selectionAction = null;
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
    "translate:zh": "翻译成中文", "translate:en": "Translate to English",
    explain: "AI 讲解", summarize: "摘要", keypoints: "要点", define: "术语",
  };
</script>

<div class="lr">
  <header class="lr-head">
    <div>
      <div class="eyebrow">文献阅读</div>
      <h1 class="lr-title">{title || "文献阅读"}</h1>
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
      <button class="btn" class:btn--primary={!bilingual} disabled={!rawText || loading}
              onclick={() => (bilingual ? (bilingual = false) : void startBilingual())}
              title="逐段对照：原文在左，模型译文在右">
        {bilingual ? "关闭对照" : "中英对照"}
      </button>
      <button class="btn" disabled={!rawText || loading} onclick={() => void repairLayout()}>
        <Icon name="refresh" size={12} /> 修复排版
      </button>
      <button class="btn" disabled={!rawText || loading} onclick={() => void fixMath()}
              title="把 PDF 提取出的碎公式整理成可渲染的 LaTeX（\\[..\\] → $$..$$，散行公式并为展示块）">
        <Icon name="pencil" size={12} /> 修公式
      </button>
      <button class="btn primary" disabled={!canSave} onclick={() => (saveOpen = true)}>
        <Icon name="plus" size={12} /> 保存到项目
      </button>
    </div>
  </header>

  <section class="literature-search">
    <input class="ipt" aria-label="文献检索" placeholder="检索论文关键词…" bind:value={searchQuery} onkeydown={(e) => e.key === "Enter" && void searchPapers()} />
    <button class="btn primary" disabled={searchBusy || !searchQuery.trim()} onclick={() => void searchPapers()}>{searchBusy ? "检索中…" : "多源检索"}</button>
    {#if searchResult}
      <p class="mono">{searchResult.hits.length} 篇 · {searchResult.took_ms} ms · 返回结果的来源：{searchResult.sources_succeeded.join(", ") || "无"}</p>
      {#if failedSources.length}
        <p class="mono" style:color="var(--warn, #c8a24a)">⚠ 未返回结果的来源（超时/限流/网络）：{failedSources.join(", ")} —— 重试或换关键词通常能补齐。</p>
      {/if}
      {#each searchResult.hits as hit}
        <div class="search-hit">
          <button class="btn" onclick={() => readHit(hit)}>{hit.title}</button>
          <p class="mono">{hit.year ?? ""} · {hit.authors.slice(0, 3).join(", ")} · {hit.sources.join(", ")}</p>
          <p>{hit.abstract_text?.slice(0, 450) ?? "暂无摘要；可点击下方链接阅读原文。"}</p>
          <div class="hit-actions">
            {#if hit.url}<a href={hit.url} target="_blank" rel="noreferrer">原文</a>{/if}
            <button
              class="btn btn--sm"
              class:btn--primary={!savedKeys[hitKey(hit)]}
              disabled={!!savedKeys[hitKey(hit)] || saveBusy === hitKey(hit)}
              onclick={() => void saveHit(hit)}
              title="加入「研究」阅读清单"
            >
              {savedKeys[hitKey(hit)] ? "已收藏" : saveBusy === hitKey(hit) ? "收藏中…" : "收藏"}
            </button>
          </div>
          <details><summary>排序依据</summary>{hit.reasons.join(" · ")}</details>
        </div>
      {/each}
      {#if !searchResult.hits.length}<p>没有检索结果；请调整关键词后重试。</p>{/if}
    {/if}
  </section>
  {#if error}
    <div class="err mono">{error}</div>
  {/if}

  <div class="lr-body" onmouseup={onMouseUp} role="presentation">
    <article class="paper" bind:this={paperElement}>
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
            {#if bilingual}
              <div class="bi-row">
                <p class="bi-orig">{para.trim()}</p>
                <p class="bi-trans">
                  {#if translations[i] !== undefined}
                    {translations[i]}
                  {:else if biPending.has(i)}
                    <span class="bi-wait mono">翻译中…</span>
                  {:else}
                    <span class="bi-wait mono">…</span>
                  {/if}
                </p>
              </div>
            {:else}
              <RichText text={para.trim()} />
            {/if}
          {/if}
        {/each}
        <div class="paper-foot mono">
          {wordCount.toLocaleString()} 词 · {charCount.toLocaleString()} 字符{#if repaired} · 已修复排版{/if}{#if mathFixed} · 已修公式{/if}
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
      <button class="fb" onclick={() => void saveSelectionExcerpt()} disabled={excerptBusy}
              title={currentPaperId ? "存为这篇论文的摘录（可进成稿）" : "收藏后存为摘录"}>
        <Icon name="book" size={11} /> {excerptBusy ? "…" : "摘录"}
      </button>
      <button class="fb" onclick={copySelection}><Icon name="check" size={11} /> 复制</button>
    </div>
  {/if}

  {#if excerptFlash}
    <div class="float" style:left="50%" style:top="72px" style:transform="translateX(-50%)">
      <span class="fb mono">{excerptFlash}</span>
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
  .hit-actions {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 6px 0;
  }
  .literature-search { padding: 12px 0; }
  .search-hit { padding: 12px; border-bottom: 1px solid var(--border); }
  .search-hit p { margin: 6px 0; }
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
  /* 中英对照：每段一行两栏，段级天然对齐 */
  .bi-row {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 18px;
    padding: 10px 0;
    border-bottom: 1px dashed var(--border);
    align-items: start;
  }
  .bi-row:last-of-type { border-bottom: none; }
  .bi-orig { margin: 0; }
  .bi-trans {
    margin: 0;
    color: var(--fg-muted);
    border-left: 2px solid color-mix(in oklab, var(--accent) 40%, transparent);
    padding-left: 12px;
  }
  .bi-wait { font-size: 10px; opacity: 0.6; }
  @media (max-width: 900px) {
    .bi-row { grid-template-columns: 1fr; gap: 6px; }
    .bi-trans { border-left: none; padding-left: 0; border-top: 1px solid var(--border); padding-top: 6px; }
  }

</style>
