<script lang="ts">
  import { app } from "../lib/store.svelte";
  import * as api from "../lib/api";
  import type { ChunkInfo, Note } from "../lib/api";
  import { convertFileSrc } from "@tauri-apps/api/core";
  import Icon from "../components/Icon.svelte";
  import RichText from "../components/RichText.svelte";
  import ChatPanel from "../components/ChatPanel.svelte";
  import { isMobile } from "../lib/platform";
  import { tick } from "svelte";
  // pdf.js's own .textLayer styles (positioned transparent spans over the
  // canvas) — required for the text layer to be selectable AND invisible.
  import "pdfjs-dist/web/pdf_viewer.css";

  // ---- state ----
  let chunks = $state<ChunkInfo[]>([]);
  let loading = $state(true);
  let split = $state(58); // percent for left pane
  let dragging = $state(false);

  // Citation deep-link: when a source is opened from a ⟦… · loc⟧ chip, scroll to the
  // chunk whose loc matches the cited location and flash it. Best-effort — no match just
  // opens the source. Re-runs when chunks finish loading.
  $effect(() => {
    const loc = app.sourceJumpLoc;
    void chunks;
    if (!loc || chunks.length === 0) return;
    const lc = loc.toLowerCase();
    const target = chunks.find((c) => {
      const cl = (c.loc ?? "").toLowerCase();
      return !!cl && (cl === lc || cl.includes(lc) || lc.includes(cl));
    });
    app.sourceJumpLoc = null; // consume
    if (!target) return;
    void tick().then(() => {
      const el = document.getElementById("sv-chunk-" + target.ord);
      if (!el) return;
      el.scrollIntoView({ behavior: "smooth", block: "center" });
      el.classList.add("sv-chunk-flash");
      setTimeout(() => el.classList.remove("sv-chunk-flash"), 1600);
    });
  });

  // Mobile: zoom is locked app-wide (native-app feel), but a source IS the one place
  // you need pinch-zoom — to read a dense PDF/slide. Unlock the viewport while this
  // viewer is mounted and restore the lock on exit. Desktop is unaffected (no touch-zoom).
  $effect(() => {
    if (!isMobile) return;
    const meta = document.querySelector('meta[name="viewport"]');
    if (!meta) return;
    const locked = meta.getAttribute("content") ?? "width=device-width, initial-scale=1.0, maximum-scale=1, user-scalable=no";
    meta.setAttribute("content", "width=device-width, initial-scale=1.0, maximum-scale=5, user-scalable=yes");
    return () => meta.setAttribute("content", locked);
  });

  // ---- per-kind preview routing ----
  // `assetUrl` is the webview-loadable URL for the persisted original (or, for
  // pptx/docx, the backend-rendered PDF). Guarded against a null stored_path.
  const assetUrl = $derived(
    app.activeSource?.stored_path
      ? convertFileSrc(app.activeSource.stored_path)
      : null
  );
  // A document renders as an embedded PDF when its kind is a PDF-previewable
  // document. pptx/docx are rendered to PDF by the backend (their stored_path
  // points at that PDF), so they preview as slides via the same iframe.
  const PDF_KINDS = ["pdf", "pptx", "docx"];
  const isPdfDoc = $derived(
    !!app.activeSource && PDF_KINDS.includes(app.activeSource.kind) && !!assetUrl
  );
  const isImage = $derived(app.activeSource?.kind === "image" && !!assetUrl);
  const isAudio = $derived(app.activeSource?.kind === "audio" && !!assetUrl);
  // Readable text: explicit text-ish kinds, or any source with no stored file
  // but extracted content to show.
  const TEXT_KINDS = ["txt", "md", "web", "url", "epub"];
  const isText = $derived(
    !!app.activeSource &&
      !isPdfDoc &&
      !isImage &&
      !isAudio &&
      !!app.activeSource.content &&
      (TEXT_KINDS.includes(app.activeSource.kind) || !app.activeSource.stored_path)
  );
  // Anything with a dedicated preview skips the chunk-list fallback.
  const hasPreview = $derived(isPdfDoc || isImage || isAudio || isText);

  // ---- PDF rendering (all platforms) ----
  // Desktop used to keep a native <iframe>, but an iframe's PDF plugin hides
  // its text layer from us — no selection, no annotation. PDF.js canvas +
  // TextLayer gives EVERY platform real text selection, which the 摘录 flow
  // (gap ②读) builds on. WKWebView additionally needs this because its iframe
  // only renders page 1.
  let pdfBox = $state<HTMLDivElement | null>(null);
  let pdfRendering = $state(false);
  let pdfError = $state<string | null>(null);
  $effect(() => {
    if (!(isPdfDoc && assetUrl && pdfBox)) return;
    const url = assetUrl;
    const box = pdfBox;
    let cancelled = false;
    pdfRendering = true;
    pdfError = null;
    box.replaceChildren();
    (async () => {
      try {
        const pdfjs = await import("pdfjs-dist");
        const worker = await import("pdfjs-dist/build/pdf.worker.min.mjs?url");
        pdfjs.GlobalWorkerOptions.workerSrc = worker.default;
        // isEvalSupported/enableScripting harden the renderer (no eval, no in-PDF JS);
        // real runtime options in pdfjs v6 but absent from its public types — hence the cast.
        const doc = await pdfjs.getDocument({ url, isEvalSupported: false, enableScripting: false } as any).promise;
        const dpr = Math.min(window.devicePixelRatio || 1, 2);
        for (let n = 1; n <= doc.numPages; n++) {
          if (cancelled) return;
          const page = await doc.getPage(n);
          const base = page.getViewport({ scale: 1 });
          const cssWidth = box.clientWidth || 360;
          const scale = (cssWidth / base.width) * dpr;
          const vp = page.getViewport({ scale });
          // Page wrapper: canvas + invisible text layer stacked on top.
          const wrap = document.createElement("div");
          wrap.className = "sv-pdf-page-wrap";
          wrap.dataset.page = String(n);
          wrap.style.position = "relative";
          const canvas = document.createElement("canvas");
          canvas.className = "sv-pdf-page";
          canvas.width = vp.width;
          canvas.height = vp.height;
          canvas.style.width = `${cssWidth}px`;
          canvas.style.height = "auto";
          const ctx = canvas.getContext("2d");
          if (!ctx) continue;
          // The invisible text layer makes the rendered page SELECTABLE —
          // the anchor of the 摘录 (excerpt) flow. Both CSS vars are set
          // because pdf.js renamed --scale-factor → --total-scale-factor
          // across major versions.
          const textDiv = document.createElement("div");
          textDiv.className = "textLayer sv-text-layer";
          textDiv.style.setProperty("--scale-factor", String(scale));
          textDiv.style.setProperty("--total-scale-factor", String(scale));
          wrap.appendChild(canvas);
          wrap.appendChild(textDiv);
          box.appendChild(wrap);
          await page.render({ canvasContext: ctx, viewport: vp, canvas }).promise;
          const tl = new pdfjs.TextLayer({
            textContentSource: page.streamTextContent(),
            container: textDiv,
          } as any);
          await tl.render();
        }
      } catch (e) {
        if (!cancelled) pdfError = String(e);
      } finally {
        if (!cancelled) pdfRendering = false;
      }
    })();
    return () => { cancelled = true; };
  });

  // ---- selection → excerpt (②读 → ④引) ----------------------------------
  // The TextLayer spans are selectable; on mouseup we find which page the
  // selection lives in and offer 摘录 (with the page number auto-filled —
  // excerpts keep their page so drafts can cite "p.12").
  let pdfSel = $state<{ text: string; page: number; x: number; y: number } | null>(null);
  let excerptFlash = $state<string | null>(null);
  function onPdfMouseUp(ev: MouseEvent) {
    const s = window.getSelection();
    const text = s?.toString().replace(/\s+/g, " ").trim() ?? "";
    if (!s || s.isCollapsed || text.length < 2) { pdfSel = null; return; }
    const anchor = s.anchorNode;
    const el = anchor instanceof Element ? anchor : anchor?.parentElement ?? null;
    const pageWrap = el?.closest("[data-page]") as HTMLElement | null;
    if (!pageWrap || !pdfBox?.contains(pageWrap)) { pdfSel = null; return; }
    const rect = s.getRangeAt(0).getBoundingClientRect();
    pdfSel = { text, page: Number(pageWrap.dataset.page), x: rect.left + rect.width / 2, y: rect.top - 10 };
  }
  function copyPdfSelection() {
    if (pdfSel) void navigator.clipboard.writeText(pdfSel.text);
    pdfSel = null;
  }
  async function excerptPdfSelection() {
    if (!pdfSel || !app.activeSource) return;
    try {
      await api.excerptAdd(pdfSel.text, undefined, app.activeSource.id, pdfSel.page);
      excerptFlash = `已存为摘录（p.${pdfSel.page}）`;
      setTimeout(() => (excerptFlash = null), 2600);
      window.getSelection()?.removeAllRanges();
      pdfSel = null;
    } catch (e) {
      excerptFlash = `摘录失败：${String(e)}`;
      setTimeout(() => (excerptFlash = null), 4000);
    }
  }

  // ---- load chunks on mount / when the active source changes ----
  // Re-runs reactively whenever app.activeSource (or its id) changes. We only
  // need raw chunks when there's no richer preview to fall back to, plus audio
  // uses them as a transcript fallback.
  $effect(() => {
    const src = app.activeSource;
    if (!src) return;
    // Skip the (potentially large) chunk fetch when a real preview is shown and
    // we don't need a transcript fallback.
    if (hasPreview && !isAudio) {
      chunks = [];
      loading = false;
      return;
    }
    loading = true;
    api.listChunks(src.id)
      .then((c) => { chunks = c; })
      .catch(() => { chunks = []; })
      .finally(() => { loading = false; });
  });

  // ---- delete ----
  async function confirmDelete() {
    const src = app.activeSource;
    if (!src) return;
    if (await app.confirm({ title: "Delete this source?", danger: true, okLabel: "Delete" })) {
      await app.deleteSource(src.id); // store action closes the viewer on success
    }
  }

  // ---- edit ----
  function editSource() {
    const src = app.activeSource;
    if (!src) return;
    app.openEdit({
      kind: "source",
      id: src.id,
      name: src.name,
      subjectId: src.subject_id,
      topicId: src.topic_id,
      tags: src.tags ?? [],
      topicOptions: (app.activeSubject?.topics ?? []).map((t) => ({ id: t.id, label: t.name })),
    });
  }

  // transcript text for audio: prefer extracted content, fall back to chunks
  const transcript = $derived(
    app.activeSource?.content ?? (chunks.length ? chunks.map((c) => c.text).join("\n\n") : "")
  );

  // ---- lecture overview note (audio) ----
  // Transcription auto-saves a markdown summary as a note titled "Summary — {name}"
  // in the same subject (spawn_lecture_summary). No FK links it to the source, so
  // it's found by that title convention; a stale-response guard keeps a slow fetch
  // from landing on a different source.
  let summaryNote = $state<Note | null>(null);
  let showTranscript = $state(false);
  $effect(() => {
    const s = app.activeSource;
    summaryNote = null;
    showTranscript = false;
    if (!s || s.kind !== "audio") return;
    api.listNotes(s.subject_id)
      .then((notes) => {
        if (app.activeSource?.id !== s.id) return;
        summaryNote = notes.find((n) => n.title === `Summary — ${s.name}`) ?? null;
      })
      .catch(() => {});
  });

  // ---- splitter drag ----
  $effect(() => {
    function onMove(e: MouseEvent) {
      if (!dragging) return;
      const sidebar = 248; // --sb-w default
      const pct = ((e.clientX - sidebar) / (window.innerWidth - sidebar)) * 100;
      split = Math.min(72, Math.max(38, pct));
    }
    function onUp() {
      dragging = false;
      document.body.style.cursor = "";
    }
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  });

  function startDrag() {
    dragging = true;
    document.body.style.cursor = "col-resize";
  }

  // ---- derived ----
  const src = $derived(app.activeSource);
  const dim = $derived(chunks.length > 0 ? chunks[0].dim : 0);
  const kindBadge = $derived(
    (src?.kind ?? "web") === "audio" ? "audio" : (src?.kind ?? "web")
  );
</script>

<div class="source-viewer">
  <!-- LEFT: embedding proof pane -->
  <div class="sv-pane sv-source" style:width="{app.chatOpen ? split : 100}%">
    <div class="sv-head">
      <button
        class="btn btn--icon btn--sm btn--ghost"
        onclick={() => app.closeSource()}
        title="返回"
      >
        <span style="display:block;transform:rotate(180deg)">
          <Icon name="chevron" size={13} color="currentColor" />
        </span>
      </button>

      {#if src}
        <span class="badge badge--{kindBadge}">
          <span class="dot"></span>
          {(src.kind ?? "web").toUpperCase()}
        </span>
        <span class="sv-name mono">{src.name}</span>
        <div class="grow"></div>
        {#if src.meta}
          <div class="sv-tools mono faint">{src.meta}</div>
        {/if}
      {/if}

      <button class="btn btn--icon btn--sm btn--ghost" title="搜索">
        <Icon name="search" size={13} />
      </button>
      {#if src}
        <button
          class="btn btn--icon btn--sm btn--ghost"
          onclick={editSource}
          title="编辑资料"
        >
          <Icon name="pencil" size={13} color="currentColor" />
        </button>
        <button
          class="btn btn--icon btn--sm btn--ghost sv-delete"
          onclick={confirmDelete}
          title="删除资料"
        >
          <Icon name="x" size={13} color="currentColor" />
        </button>
      {/if}
    </div>

    <!-- Preview / embedding proof scrollable area -->
    <div class="sv-doc" class:sv-doc--flush={isPdfDoc || isImage}>
      {#if src?.error}
        <!-- Failed ingest: surface the error prominently instead of an empty view -->
        <div class="pdf-page sv-error" style="width:100%;max-width:560px">
          <h3 class="pdf-h" style="color:var(--err, var(--warn))">入库失败</h3>
          <p class="read pdf-note">{src.error}</p>
          <p class="pdf-note mono" style="font-size:var(--t-xs);color:var(--fg-muted)">
            This source couldn't be parsed. Try deleting and re-adding it.
          </p>
        </div>
      {:else if isPdfDoc && assetUrl}
        <!-- PDF / rendered slide preview (also covers pptx & docx via rendered PDF).
             PDF.js canvas + TextLayer on ALL platforms: real text selection → 摘录. -->
        <div class="sv-pdf-pages" bind:this={pdfBox} onmouseup={onPdfMouseUp} role="presentation"></div>
        {#if pdfRendering}
          <p class="pdf-note mono" style="font-size:var(--t-xs);color:var(--fg-muted);text-align:center;padding:12px">正在渲染页面…</p>
        {/if}
        {#if pdfError}
          <p class="pdf-note mono" style="font-size:var(--t-xs);color:var(--err,var(--warn));text-align:center;padding:12px">Couldn't render PDF: {pdfError}</p>
        {/if}
        {#if pdfSel}
          <div class="float" style="position:fixed" style:left="{pdfSel.x}px" style:top="{pdfSel.y}px" role="toolbar">
            <button class="fb" onclick={excerptPdfSelection} title="存为该来源的摘录（页码自动带上）">
              <Icon name="book" size={11} /> 摘录
            </button>
            <button class="fb" onclick={copyPdfSelection}><Icon name="check" size={11} /> 复制</button>
          </div>
        {/if}
        {#if excerptFlash}
          <div class="float" style="position:fixed;left:50%;top:64px;transform:translateX(-50%)">
            <span class="fb mono">{excerptFlash}</span>
          </div>
        {/if}
      {:else if isImage && assetUrl}
        <!-- Image preview, centered & fit -->
        <div class="sv-img-wrap">
          <img class="sv-img" src={assetUrl} alt={src?.name ?? "image"} />
        </div>
      {:else if isAudio && assetUrl}
        <!-- Audio player + rendered overview note + transcript behind a toggle -->
        <div class="pdf-page" style="width:100%;max-width:560px">
          <audio class="sv-audio" controls src={assetUrl}></audio>
        </div>
        <div class="pdf-page" style="width:100%;max-width:560px">
          <h3 class="pdf-h">概览</h3>
          {#if summaryNote}
            <RichText text={summaryNote.body} />
          {:else}
            <p class="pdf-note mono" style="font-size:var(--t-xs);color:var(--fg-muted)">
              No overview yet — a markdown summary is generated automatically after
              transcription and saved to Notes as “Summary — {src?.name}”.
            </p>
          {/if}
        </div>
        <div class="pdf-page" style="width:100%;max-width:560px">
          <div class="sv-sec-head">
            <h3 class="pdf-h">转写稿</h3>
            <button
              class="btn btn--sm btn--ghost"
              onclick={() => (showTranscript = !showTranscript)}
            >
              {showTranscript ? "Hide transcript" : "View transcript"}
            </button>
          </div>
          {#if showTranscript}
            {#if transcript}
              <p class="read pdf-note sv-text">{transcript}</p>
            {:else}
              <p class="pdf-note mono" style="font-size:var(--t-xs);color:var(--fg-muted)">
                No transcript available yet.
              </p>
            {/if}
          {/if}
        </div>
      {:else if isText && src?.content}
        <!-- Readable extracted text (txt / md / web / url, or content-only sources) -->
        <div class="pdf-page" style="width:100%;max-width:680px">
          <p class="read pdf-note sv-text">{src.content}</p>
        </div>
      {:else if loading}
        <div class="pdf-page" style="width:100%;max-width:560px">
          <div class="pdf-line sk" style="width:60%;height:14px;margin-bottom:18px"></div>
          <div class="pdf-line sk" style="width:90%"></div>
          <div class="pdf-line sk" style="width:80%"></div>
          <div class="pdf-line sk" style="width:70%"></div>
        </div>
      {:else}
        <!-- Fallback: raw chunk list (nothing richer to show) -->
        <div class="pdf-page" style="width:100%;max-width:560px">
          {#if chunks.length > 0}
            <h3 class="pdf-h" style="color:var(--ok)">
              ✓ {chunks.length} chunk{chunks.length === 1 ? "" : "s"} embedded · {dim}-dim vectors
            </h3>
            <p class="pdf-note mono" style="font-size:var(--t-xs);color:var(--fg-muted)">
              Source fully parsed and embedded. Each chunk below is stored with its vector.
            </p>
          {:else}
            <h3 class="pdf-h" style="color:var(--warn)">未向量化 / 正在入库</h3>
            <p class="pdf-note mono" style="font-size:var(--t-xs);color:var(--fg-muted)">
              No chunks found — the source may still be ingesting or failed to parse.
            </p>
          {/if}
        </div>

        <!-- Chunk list -->
        {#each chunks as chunk (chunk.ord)}
          <div id={"sv-chunk-" + chunk.ord} class="pdf-page" style="width:100%;max-width:560px">
            <div class="pdf-pageno mono">#{chunk.ord}</div>
            {#if chunk.loc}
              <div class="pdf-formula mono" style="margin-bottom:10px">{chunk.loc}</div>
            {/if}
            <p class="read pdf-note">{chunk.text}</p>
          </div>
        {/each}
      {/if}
    </div>
  </div>

  <!-- Splitter + scoped chat — hidden when the chat is toggled off (c). The
       left pane expands to full width and App's "Ask c" FAB reopens it. -->
  {#if app.chatOpen}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      class="sv-splitter"
      role="separator"
      aria-orientation="vertical"
      tabindex="-1"
      onmousedown={startDrag}
    >
      <span></span>
    </div>

    <!-- RIGHT: scoped chat -->
    <div class="sv-pane sv-chat" style:width="{100 - split}%">
      <ChatPanel onClose={() => (app.chatOpen = false)} />
    </div>
  {/if}
</div>

<style>
  /* Selection toolbar + flash — the LiteratureReader float look, scoped here. */
  .float {
    position: fixed;
    z-index: 60;
    display: flex;
    gap: 4px;
    background: var(--surface, #171d1a);
    border: 1px solid var(--border, #2a3530);
    border-radius: 8px;
    padding: 4px;
    box-shadow: var(--shadow-2, 0 8px 24px rgba(0, 0, 0, 0.35));
  }
  .fb {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    border: none;
    background: transparent;
    color: var(--fg, #d7e0db);
    font-size: 11px;
    padding: 4px 8px;
    border-radius: 6px;
    cursor: pointer;
  }
  .fb:hover { background: var(--surface-2, #1e2622); color: var(--accent, #2dd5b7); }

  /* Page column: each canvas wrapped with its (invisible) text layer. */
  :global(.sv-pdf-page-wrap) {
    position: relative;
    margin: 0 auto 14px;
    width: fit-content;
    max-width: 100%;
  }
  :global(.sv-text-layer) {
    position: absolute;
    inset: 0;
    overflow: hidden;
    opacity: 1;
    line-height: 1;
  }
  :global(.sv-text-layer span) {
    color: transparent;
    position: absolute;
    white-space: pre;
    cursor: text;
    transform-origin: 0 0;
  }

  /* PDF / image previews fill the pane edge-to-edge instead of the padded,
     centered "document" column used by the chunk list. */
  .sv-doc--flush {
    padding: 0;
    gap: 0;
    align-items: stretch;
  }
  .sv-frame {
    flex: 1;
    width: 100%;
    border: none;
    background: var(--surface);
  }
  .sv-img-wrap {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: var(--sp-6);
    min-height: 0;
  }
  .sv-img {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
    border-radius: var(--rad-2);
    border: 1px solid var(--border);
  }
  .sv-audio {
    width: 100%;
    display: block;
  }
  /* Section header row with an inline action (Transcript · View/Hide button). */
  .sv-sec-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3, 8px);
  }
  .sv-sec-head .pdf-h {
    margin: 0;
  }
  /* Preserve paragraphs/newlines for readable plaintext and transcripts. */
  .sv-text {
    white-space: pre-wrap;
    word-break: break-word;
  }
  .sv-error {
    border-color: color-mix(in oklab, var(--warn) 40%, var(--border));
  }
  .sv-delete:hover {
    color: var(--err, var(--warn));
  }
</style>
