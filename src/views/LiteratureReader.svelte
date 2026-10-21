<script lang="ts">
  // 文献发现 — find a paper, then read it in the one reader.
  //
  // This page used to be a second reading surface: its own URL fetcher, its own
  // paste box, its own paragraph renderer, its own selection toolbar. The same
  // paper opened from 源列表 got a different view with different features (and
  // neither view knew about the other's highlights). Two readers meant two
  // places to fix every reading bug, and a user who could not tell which one
  // was "the real one".
  //
  // Reading now happens in 阅读器 (PDF pages, page-anchored highlights,
  // annotations, citations, 中英对照, 划词翻译/讲解). This page does what a
  // discovery surface should: search four sources, show why each hit ranked,
  // let you keep it, and open it — importing on the way in, because the reader
  // reads sources, not search results.
  import { app } from "../lib/store.svelte";
  import * as api from "../lib/api";
  import Icon from "../components/Icon.svelte";

  let searchQuery = $state("");
  let searchBusy = $state(false);
  let searchResult = $state<api.LiteratureResult | null>(null);
  let searchLimit = $state(30);
  let searchSort = $state<api.LiteratureSort>("relevance");
  let aiBusy = $state(false);
  let aiResult = $state<api.AiSearchResult | null>(null);
  let error = $state<string | null>(null);

  async function searchPapers() {
    if (!searchQuery.trim() || searchBusy) return;
    searchBusy = true;
    error = null;
    aiResult = null;
    try {
      searchResult = await api.literatureSearch(searchQuery.trim(), app.activeSubjectId, searchLimit, searchSort);
    } catch (e) {
      error = String(e);
    } finally {
      searchBusy = false;
    }
  }

  // AI 深度检索：LLM 把自然语言问题拆成多组检索式并行搜全源，再按相关度重排。
  async function aiSearchPapers() {
    if (!searchQuery.trim() || aiBusy) return;
    aiBusy = true;
    error = null;
    searchResult = null;
    try {
      aiResult = await api.literatureAiSearch(searchQuery.trim(), app.activeSubjectId, searchLimit);
    } catch (e) {
      error = String(e);
    } finally {
      aiBusy = false;
    }
  }

  // Source diagnostics: sources that were tried but contributed nothing (they
  // fail silently by design — timeout / rate limit / network) so "结果怎么这么
  // 少" always has a visible answer on screen.
  const failedSources = $derived.by(() => {
    if (!searchResult) return [];
    const ok = new Set(searchResult.sources_succeeded);
    return searchResult.sources_attempted.filter((s) => !ok.has(s));
  });

  // 收藏：把一条检索命中写进 papers 表（研究页的阅读清单）。DOI 去重由后端保证。
  let savedKeys = $state<Record<string, true>>({});
  let saveBusy = $state<string | null>(null);
  const hitKey = (h: api.LiteratureHit) => h.doi || h.title;
  async function saveHit(h: api.LiteratureHit) {
    const key = hitKey(h);
    if (savedKeys[key] || saveBusy) return;
    saveBusy = key;
    try {
      await api.paperImport({
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
    } catch (e) {
      error = String(e);
    } finally {
      saveBusy = null;
    }
  }

  // 读 = 入库 + 打开阅读器。The reader reads sources; a search hit is only
  // metadata, so the two-step is not ceremony — it is what gives the passage a
  // chunk id, a citation target and a highlight coordinate system.
  let readingKey = $state<string | null>(null);
  async function readHit(h: api.LiteratureHit) {
    const key = hitKey(h);
    if (readingKey) return;
    const subjectId = app.activeSubjectId ?? app.subjects[0]?.id ?? null;
    if (!subjectId) {
      error = "先选一个学科 —— 「读」会把这篇文章入库，全文在阅读器里看";
      return;
    }
    readingKey = key;
    error = null;
    try {
      // Re-opening must not create a second copy: the source name is the paper
      // title, so a hit already in this subject opens as-is.
      const existing = (await api.listSources(subjectId)).find((s) => s.name === h.title);
      if (existing) {
        app.openSource(existing);
        return;
      }
      let text = (h.abstract_text ?? "").trim();
      // 有链接就先抓正文：摘要读不出方法细节，而「读」这个按钮承诺的是读这篇。
      if (h.url) {
        try {
          const page = await api.fetchPage(h.url);
          if (page.text && page.text.length > text.length) text = page.text;
        } catch {
          // 抓不到就退回摘要 —— 比「什么都没打开」好，且阅读器里能看到只有摘要。
        }
      }
      if (!text) throw new Error("这篇既没有摘要，也抓不到正文 —— 用「原文」看链接");
      const sid = await api.literatureSaveSource(subjectId, h.title, text, h.url ?? null, null);
      const full = await api.getSource(sid);
      if (full) app.openSource(full);
      else error = "入库成功但读不到这份来源，去资料列表里找一下";
    } catch (e) {
      error = `打开失败：${String(e)}`;
    } finally {
      readingKey = null;
    }
  }
</script>

<div class="lr">
  <header class="lr-head">
    <div>
      <div class="eyebrow">文献发现</div>
      <h1 class="lr-title">文献</h1>
      <p class="lr-sub">
        四个来源一起搜（arXiv · Crossref · OpenAlex · Semantic Scholar），按相关度融合。点「读」会把它入库并在阅读器里打开
        —— 阅读只有一个地方：有 PDF 页面、有高亮、有批注、能引用到页码。
      </p>
    </div>
  </header>

  <section class="literature-search">
    <input
      class="ipt"
      aria-label="文献检索"
      placeholder="检索论文关键词，或输入一句自然语言研究问题…"
      bind:value={searchQuery}
      onkeydown={(e) => e.key === "Enter" && void searchPapers()}
    />
    <select class="ipt sel" aria-label="结果数" bind:value={searchLimit} title="最终融合排序后保留的条数（每个来源先各取 25 条参与融合）">
      <option value={10}>10 篇结果</option>
      <option value={30}>30 篇结果</option>
      <option value={50}>50 篇结果</option>
      <option value={100}>100 篇结果</option>
    </select>
    <select class="ipt sel" aria-label="排序" bind:value={searchSort} title="结果排序方式">
      <option value="relevance">按相关度</option>
      <option value="newest">按最新</option>
      <option value="cited">按被引最多</option>
    </select>
    <button class="btn primary" disabled={searchBusy || aiBusy || !searchQuery.trim()} onclick={() => void searchPapers()}>
      {searchBusy ? "检索中…" : "多源检索"}
    </button>
    <button
      class="btn"
      disabled={searchBusy || aiBusy || !searchQuery.trim()}
      onclick={() => void aiSearchPapers()}
      title="LLM 拆解问题为多组检索式并行搜索，再按相关度重排——适合自然语言长问题"
    >
      {aiBusy ? "AI 检索中…" : "AI 深度检索"}
    </button>
  </section>

  {#if error}
    <div class="err mono">{error}</div>
  {/if}

  {#if aiResult}
    <section class="results">
      <p class="mono faint">
        AI 展开的检索式：{aiResult.queries.map((q) => `「${q.query}」(${q.hits})`).join(" · ")} · 共 {aiResult.hits.length} 篇 · {aiResult.took_ms} ms
      </p>
      {#each aiResult.hits as hit (hitKey(hit))}
        <div class="search-hit">
          <p class="hit-t">{hit.title}</p>
          <p class="mono faint">{hit.year ?? ""} · {hit.authors.slice(0, 3).join(", ")} · {hit.sources.join(", ")}</p>
          <p class="hit-a">{hit.abstract_text?.slice(0, 450) ?? "暂无摘要；点「原文」看链接。"}</p>
          <div class="hit-actions">
            <button
              class="btn primary"
              disabled={readingKey !== null}
              onclick={() => void readHit(hit)}
              title="入库并在阅读器里打开（有 PDF 才会有页内高亮；只有摘要时就只读摘要）"
            >
              <Icon name="reader" size={12} /> {readingKey === hitKey(hit) ? "打开中…" : "读"}
            </button>
            {#if hit.url}<a href={hit.url} target="_blank" rel="noreferrer">原文</a>{/if}
            <button
              class="btn btn--sm"
              disabled={!!savedKeys[hitKey(hit)] || saveBusy === hitKey(hit)}
              onclick={() => void saveHit(hit)}
              title="加入「研究」的阅读清单（不产生来源）"
            >
              {savedKeys[hitKey(hit)] ? "已收藏" : saveBusy === hitKey(hit) ? "收藏中…" : "收藏"}
            </button>
          </div>
        </div>
      {/each}
    </section>
  {/if}

  {#if searchResult}
    <section class="results">
      <p class="mono faint">{searchResult.hits.length} 篇 · {searchResult.took_ms} ms · 返回结果的来源：{searchResult.sources_succeeded.join(", ") || "无"}</p>
      {#if failedSources.length}
        <p class="mono warn">⚠ 未返回结果的来源（超时/限流/网络）：{failedSources.join(", ")} —— 重试或换关键词通常能补齐。</p>
      {/if}
      {#each searchResult.hits as hit (hitKey(hit))}
        <div class="search-hit">
          <p class="hit-t">{hit.title}</p>
          <p class="mono faint">{hit.year ?? ""} · {hit.authors.slice(0, 3).join(", ")} · {hit.sources.join(", ")}</p>
          <p class="hit-a">{hit.abstract_text?.slice(0, 450) ?? "暂无摘要；点「原文」看链接。"}</p>
          <div class="hit-actions">
            <button
              class="btn primary"
              disabled={readingKey !== null}
              onclick={() => void readHit(hit)}
              title="入库并在阅读器里打开（有 PDF 才会有页内高亮；只有摘要时就只读摘要）"
            >
              <Icon name="reader" size={12} /> {readingKey === hitKey(hit) ? "打开中…" : "读"}
            </button>
            {#if hit.url}<a href={hit.url} target="_blank" rel="noreferrer">原文</a>{/if}
            <button
              class="btn btn--sm"
              disabled={!!savedKeys[hitKey(hit)] || saveBusy === hitKey(hit)}
              onclick={() => void saveHit(hit)}
              title="加入「研究」的阅读清单（不产生来源）"
            >
              {savedKeys[hitKey(hit)] ? "已收藏" : saveBusy === hitKey(hit) ? "收藏中…" : "收藏"}
            </button>
          </div>
          <details><summary>排序依据</summary>{hit.reasons.join(" · ")}</details>
        </div>
      {/each}
      {#if !searchResult.hits.length}<p class="faint">没有检索结果；请调整关键词后重试。</p>{/if}
    </section>
  {/if}
</div>

<style>
  .lr {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 22px 28px 32px;
    display: flex;
    flex-direction: column;
    gap: 14px;
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
  .warn { color: var(--warn, #c8a24a); }

  .lr-head {
    border-bottom: 1px solid var(--border);
    padding-bottom: 14px;
  }
  .lr-title {
    margin: 4px 0 0;
    font-family: var(--font-display);
    font-size: 22px;
    font-weight: 600;
    letter-spacing: -0.01em;
    color: var(--fg-bright);
  }
  .lr-sub {
    margin: 8px 0 0;
    max-width: 78ch;
    font-size: var(--t-xs);
    line-height: 1.65;
    color: var(--fg-muted);
  }

  .literature-search { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; padding: 4px 0 8px; }
  .results { display: flex; flex-direction: column; }
  .search-hit { padding: 12px; border-bottom: 1px solid var(--border); }
  .search-hit p { margin: 6px 0; }
  .hit-t { color: var(--fg-bright); font-weight: 500; font-size: var(--t-sm); }
  .hit-a { font-size: var(--t-xs); line-height: 1.6; color: var(--fg-muted); }
  .hit-actions { display: flex; align-items: center; gap: 10px; margin: 6px 0; }
  .hit-actions a { font-size: var(--t-2xs); color: var(--accent); }

  .ipt {
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--fg);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 7px;
    padding: 6px 9px;
    outline: none;
    min-width: 200px;
  }
  .ipt:focus { border-color: var(--accent-dim); color: var(--fg-bright); }
  .ipt.sel { min-width: 132px; }

  .btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-family: var(--font-mono);
    font-size: 11.5px;
    padding: 7px 12px;
    border-radius: 7px;
    cursor: pointer;
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--fg);
  }
  .btn:hover:not(:disabled) { color: var(--fg-bright); border-color: var(--border-strong); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn.primary {
    background: color-mix(in srgb, var(--accent) 22%, transparent);
    border-color: color-mix(in srgb, var(--accent) 45%, transparent);
    color: var(--fg-bright);
  }

  .err {
    padding: 10px 12px;
    border-radius: 7px;
    background: color-mix(in srgb, var(--err) 10%, var(--surface));
    border: 1px solid color-mix(in srgb, var(--err) 35%, transparent);
    color: var(--err);
    font-size: 11.5px;
  }
</style>
