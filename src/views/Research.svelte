<script lang="ts">
  // Research — the reading list and the excerpts taken from it.
  //
  // This is the middle of the research pipeline that used to be missing:
  // literature_search could find a paper and LiteratureReader could display one,
  // but nothing remembered "I plan to read this", "I read it", or "this is the
  // sentence I will cite". A paper here can become a schedulable reading task,
  // and every excerpt is kept verbatim so it can go straight into a draft.
  import * as api from "../lib/api";
  import Icon from "../components/Icon.svelte";

  const STATUSES = [
    { id: "all", label: "全部" },
    { id: "to_read", label: "待读" },
    { id: "reading", label: "在读" },
    { id: "read", label: "已读" },
    { id: "cited", label: "已引用" },
  ] as const;

  const STATUS_LABEL: Record<string, string> = {
    to_read: "待读",
    reading: "在读",
    read: "已读",
    cited: "已引用",
    dropped: "搁置",
  };

  let papers = $state<api.Paper[]>([]);
  let filter = $state<string>("all");
  let search = $state("");
  let selected = $state<api.Paper | null>(null);
  let excerpts = $state<api.Excerpt[]>([]);
  let loading = $state(true);
  let busy = $state<string | null>(null);
  let error = $state<string | null>(null);
  let flash = $state<string | null>(null);

  let importOpen = $state(false);
  let importText = $state("");

  let quote = $state("");
  let page = $state<number | null>(null);
  let note = $state("");

  async function load() {
    try {
      papers = await api.paperList(filter, undefined, search.trim() || undefined);
      error = null;
      if (selected && !papers.some((p) => p.id === selected!.id)) {
        selected = papers[0] ?? null;
      }
      if (!selected && papers.length) selected = papers[0];
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  async function loadExcerpts() {
    if (!selected) {
      excerpts = [];
      return;
    }
    try {
      excerpts = await api.excerptList(selected.id);
    } catch {
      excerpts = [];
    }
  }

  $effect(() => {
    void filter;
    void load();
  });

  $effect(() => {
    void selected?.id;
    void loadExcerpts();
  });

  function say(msg: string) {
    flash = msg;
    setTimeout(() => (flash = null), 2600);
  }

  async function pick(p: api.Paper) {
    selected = p;
  }

  async function startReading(p: api.Paper) {
    if (busy) return;
    busy = p.id;
    try {
      const item = await api.paperStartReading(p.id);
      say(`已加入今日任务：${item.title}（去「计划」排期）`);
      await load();
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
    }
  }

  async function setStatus(p: api.Paper, status: string) {
    if (busy) return;
    busy = p.id;
    try {
      const updated = await api.paperUpdate(p.id, { status });
      if (selected?.id === updated.id) selected = updated;
      await load();
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
    }
  }

  async function remove(p: api.Paper) {
    if (busy) return;
    busy = p.id;
    try {
      await api.paperDelete(p.id);
      if (selected?.id === p.id) selected = null;
      await load();
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
    }
  }

  async function addExcerpt() {
    if (!selected || !quote.trim() || busy) return;
    busy = "excerpt";
    try {
      await api.excerptAdd(
        quote.trim(),
        selected.id,
        undefined,
        page ?? undefined,
        note.trim() || undefined,
      );
      quote = "";
      page = null;
      note = "";
      await Promise.all([loadExcerpts(), load()]);
      // Re-read the paper so its status badge ("reading") updates.
      selected = await api.paperGet(selected.id);
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
    }
  }

  async function dropExcerpt(id: string) {
    busy = id;
    try {
      await api.excerptDelete(id);
      await Promise.all([loadExcerpts(), load()]);
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
    }
  }

  async function runImport() {
    if (!importText.trim() || busy) return;
    busy = "import";
    try {
      const out = await api.paperImportBibtex(importText);
      say(`导入 ${out.imported} 篇，跳过 ${out.skipped} 篇（重复或缺少标题）`);
      importText = "";
      importOpen = false;
      await load();
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
    }
  }

  async function copyBibtex() {
    if (busy) return;
    busy = "bib";
    try {
      const bib = await api.paperBibtex();
      await navigator.clipboard.writeText(bib);
      say("BibTeX 已复制到剪贴板");
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
    }
  }

  function metaLine(p: api.Paper): string {
    const bits: string[] = [];
    if (p.authors) bits.push(p.authors);
    if (p.venue) bits.push(p.venue);
    if (p.year) bits.push(String(p.year));
    return bits.join(" · ");
  }
</script>

<div class="research">
  <header class="head">
    <div>
      <h1>科研</h1>
      <p class="sub">阅读清单 · 摘录 · 引用导出（{papers.length} 篇）</p>
    </div>
    <div class="controls">
      <input class="search" bind:value={search} placeholder="搜索标题 / 作者 / 摘要" oninput={() => load()} />
      <button class="btn" onclick={() => (importOpen = !importOpen)}>导入 BibTeX</button>
      <button class="btn" disabled={busy === "bib"} onclick={copyBibtex}>导出 BibTeX</button>
    </div>
  </header>

  {#if flash}<div class="flash">{flash}</div>{/if}
  {#if error}<div class="err">{error}</div>{/if}

  {#if importOpen}
    <div class="import">
      <textarea bind:value={importText} placeholder={"粘贴 BibTeX，例如：\n@article{key,\n  title = {…},\n  author = {Last, First},\n  year = {2024},\n  doi = {10.…}\n}"}></textarea>
      <div class="irow">
        <span class="faint">按 DOI 去重；无 DOI 的条目按标题各自入库。</span>
        <button class="btn primary" disabled={!importText.trim() || busy === "import"} onclick={runImport}>
          {busy === "import" ? "导入中…" : "导入"}
        </button>
      </div>
    </div>
  {/if}

  <div class="tabs">
    {#each STATUSES as s (s.id)}
      <button class="tab" class:on={filter === s.id} onclick={() => (filter = s.id)}>{s.label}</button>
    {/each}
  </div>

  <div class="panes">
    <div class="list">
      {#if loading}
        <div class="empty">正在加载…</div>
      {:else if !papers.length}
        <div class="empty">
          <p>还没有论文。</p>
          <p class="hint">从「文献」搜索后保存，或点右上角导入 BibTeX。</p>
        </div>
      {:else}
        {#each papers as p (p.id)}
          <button class="prow" class:on={selected?.id === p.id} onclick={() => pick(p)}>
            <div class="ptitle">{p.title}</div>
            <div class="pmeta">
              <span class="badge">{STATUS_LABEL[p.status] ?? p.status}</span>
              {#if p.year}<span>{p.year}</span>{/if}
              {#if p.excerpt_count}<span>{p.excerpt_count} 摘录</span>{/if}
              {#if p.open_task_count}<span class="task">{p.open_task_count} 任务</span>{/if}
            </div>
          </button>
        {/each}
      {/if}
    </div>

    <div class="detail">
      {#if !selected}
        <div class="empty">选一篇论文开始。</div>
      {:else}
        <div class="dhead">
          <h2>{selected.title}</h2>
          <p class="dmeta">{metaLine(selected) || "无元数据"}</p>
          {#if selected.doi}
            <p class="doi">DOI {selected.doi}</p>
          {/if}
        </div>

        <div class="actions">
          <button class="btn primary" disabled={busy === selected.id} onclick={() => startReading(selected!)}>
            开始阅读（加入任务）
          </button>
          <select
            class="sel"
            value={selected.status}
            onchange={(e) => setStatus(selected!, (e.target as HTMLSelectElement).value)}
          >
            {#each Object.entries(STATUS_LABEL) as [k, v] (k)}
              <option value={k}>{v}</option>
            {/each}
          </select>
          <button class="btn danger" onclick={() => remove(selected!)}>删除</button>
        </div>

        {#if selected.abstract_text}
          <section class="abstract">
            <div class="shead">摘要</div>
            <p>{selected.abstract_text}</p>
          </section>
        {/if}

        <section class="excerpts">
          <div class="shead">摘录 <span class="n">{excerpts.length}</span></div>

          <div class="addbox">
            <textarea bind:value={quote} placeholder="粘贴原文句子（保持原样，之后直接进稿子）"></textarea>
            <div class="addrow">
              <input class="pg" type="number" bind:value={page} placeholder="页码" />
              <input class="nt" bind:value={note} placeholder="自己的批注（可选）" />
              <button class="btn primary" disabled={!quote.trim() || busy === "excerpt"} onclick={addExcerpt}>
                添加
              </button>
            </div>
          </div>

          {#if excerpts.length}
            {#each excerpts as ex (ex.id)}
              <blockquote class="ex">
                <p class="q">{ex.quote}</p>
                {#if ex.note}<p class="note">{ex.note}</p>{/if}
                <div class="efoot">
                  {#if ex.page}<span class="pg">p.{ex.page}</span>{/if}
                  <span class="faint">{new Date(ex.created_at).toLocaleDateString("zh-CN")}</span>
                  <button class="x" title="删除摘录" onclick={() => dropExcerpt(ex.id)}>
                    <Icon name="x" size={10} />
                  </button>
                </div>
              </blockquote>
            {/each}
          {:else}
            <p class="none">还没有摘录。读到关键句就存一条——写综述时它比摘要有用。</p>
          {/if}
        </section>
      {/if}
    </div>
  </div>
</div>

<style>
  .research {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    padding: 24px 28px 40px;
    overflow: hidden;
  }
  .head {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: 20px;
    flex-wrap: wrap;
    margin-bottom: 14px;
  }
  .head h1 {
    margin: 0;
    font-size: 22px;
    font-weight: 500;
    color: var(--fg-bright);
    letter-spacing: 0.02em;
  }
  .sub {
    margin: 4px 0 0;
    font-size: var(--t-xs);
    color: var(--fg-muted);
  }
  .controls {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .search {
    width: 200px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 7px;
    color: var(--fg);
    font-size: var(--t-xs);
    padding: 6px 9px;
    outline: none;
  }
  .search:focus {
    border-color: var(--accent-dim);
  }
  .btn {
    border: 1px solid var(--border);
    background: transparent;
    color: var(--fg-muted);
    font-size: var(--t-xs);
    padding: 5px 11px;
    border-radius: 7px;
    cursor: pointer;
    white-space: nowrap;
  }
  .btn:hover:not(:disabled) {
    color: var(--fg-bright);
    border-color: var(--border-strong);
  }
  .btn.primary {
    border-color: var(--accent-dim);
    color: var(--accent);
  }
  .btn.danger:hover {
    color: var(--err);
    border-color: var(--err);
  }
  .btn:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .flash {
    margin-bottom: 12px;
    padding: 7px 10px;
    border: 1px solid var(--accent-dim);
    border-radius: 7px;
    color: var(--accent);
    font-size: var(--t-xs);
  }
  .err {
    margin-bottom: 12px;
    padding: 7px 10px;
    border: 1px solid var(--err);
    border-radius: 7px;
    color: var(--err);
    font-size: var(--t-xs);
  }

  .import {
    margin-bottom: 14px;
    border: 1px solid var(--border);
    border-radius: 9px;
    background: var(--surface);
    padding: 10px;
  }
  .import textarea {
    width: 100%;
    min-height: 110px;
    resize: vertical;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 7px;
    color: var(--fg);
    font-family: var(--font-mono, monospace);
    font-size: 11px;
    padding: 8px;
    outline: none;
  }
  .irow {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-top: 8px;
  }
  .faint {
    font-size: 11px;
    color: var(--fg-faint);
  }

  .tabs {
    display: flex;
    gap: 6px;
    margin-bottom: 12px;
  }
  .tab {
    border: 1px solid var(--border);
    background: transparent;
    color: var(--fg-muted);
    font-size: 11px;
    padding: 3px 10px;
    border-radius: 20px;
    cursor: pointer;
  }
  .tab.on {
    border-color: var(--accent-dim);
    color: var(--accent);
  }

  .panes {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: 320px 1fr;
    gap: 16px;
  }
  @media (max-width: 900px) {
    .panes {
      grid-template-columns: 1fr;
    }
  }
  .list {
    overflow-y: auto;
    border-right: 1px solid var(--border);
    padding-right: 8px;
  }
  .prow {
    display: block;
    width: 100%;
    text-align: left;
    background: transparent;
    border: 1px solid transparent;
    border-radius: 8px;
    padding: 8px 9px;
    cursor: pointer;
    margin-bottom: 4px;
  }
  .prow:hover {
    background: var(--surface);
  }
  .prow.on {
    background: var(--surface-2);
    border-color: var(--border);
  }
  .ptitle {
    font-size: var(--t-sm);
    color: var(--fg);
    line-height: 1.4;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .pmeta {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    margin-top: 4px;
    font-size: 11px;
    color: var(--fg-faint);
  }
  .badge {
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0 4px;
  }
  .task {
    color: var(--accent);
  }

  .detail {
    overflow-y: auto;
    padding-right: 4px;
  }
  .dhead h2 {
    margin: 0;
    font-size: var(--t-lg);
    font-weight: 500;
    color: var(--fg-bright);
    line-height: 1.4;
  }
  .dmeta {
    margin: 6px 0 0;
    font-size: var(--t-xs);
    color: var(--fg-muted);
  }
  .doi {
    margin: 4px 0 0;
    font-family: var(--font-mono, monospace);
    font-size: 11px;
    color: var(--fg-faint);
  }
  .actions {
    display: flex;
    gap: 8px;
    align-items: center;
    margin: 14px 0 18px;
    flex-wrap: wrap;
  }
  .sel {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 7px;
    color: var(--fg);
    font-size: var(--t-xs);
    padding: 5px 8px;
    outline: none;
  }

  .shead {
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--fg-faint);
    margin-bottom: 8px;
  }
  .n {
    font-family: var(--font-mono, monospace);
    background: var(--surface-2);
    border-radius: 20px;
    padding: 0 6px;
  }
  .abstract {
    margin-bottom: 20px;
  }
  .abstract p {
    margin: 0;
    font-size: var(--t-sm);
    line-height: 1.65;
    color: var(--fg-muted);
  }

  .addbox {
    border: 1px solid var(--border);
    border-radius: 9px;
    background: var(--surface);
    padding: 8px;
    margin-bottom: 14px;
  }
  .addbox textarea {
    width: 100%;
    min-height: 60px;
    resize: vertical;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 7px;
    color: var(--fg);
    font-size: var(--t-xs);
    line-height: 1.6;
    padding: 8px;
    outline: none;
  }
  .addrow {
    display: flex;
    gap: 6px;
    margin-top: 6px;
  }
  .pg {
    width: 72px;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--fg);
    font-size: 11px;
    padding: 4px 7px;
    outline: none;
  }
  .nt {
    flex: 1;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--fg);
    font-size: 11px;
    padding: 4px 7px;
    outline: none;
  }

  .ex {
    margin: 0 0 10px;
    padding: 9px 12px;
    border-left: 2px solid var(--accent-dim);
    background: var(--surface);
    border-radius: 0 8px 8px 0;
  }
  .q {
    margin: 0;
    font-size: var(--t-sm);
    line-height: 1.65;
    color: var(--fg);
  }
  .note {
    margin: 6px 0 0;
    font-size: var(--t-xs);
    color: var(--fg-muted);
    font-style: italic;
  }
  .efoot {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 6px;
    font-size: 11px;
    color: var(--fg-faint);
  }
  .x {
    margin-left: auto;
    border: 0;
    background: transparent;
    color: var(--fg-faint);
    cursor: pointer;
    padding: 2px;
    display: flex;
  }
  .x:hover {
    color: var(--err);
  }
  .none {
    margin: 0;
    font-size: var(--t-xs);
    color: var(--fg-faint);
  }
  .empty {
    padding: 36px 0;
    text-align: center;
    color: var(--fg-muted);
    font-size: var(--t-sm);
  }
  .empty .hint {
    margin-top: 6px;
    font-size: var(--t-xs);
    color: var(--fg-faint);
  }
</style>
