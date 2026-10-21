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
          // pdf.js v6's TextLayer REQUIRES the viewport — without it the
          // constructor leaves #viewport undefined and render() throws
          // "Cannot read properties of undefined (reading 'scale')", leaving
          // zero selectable spans (划词 silently dead).
          const tl = new pdfjs.TextLayer({
            textContentSource: page.streamTextContent(),
            container: textDiv,
            viewport: vp,
          } as any);
          // A text-layer failure must not kill the canvas render for the rest
          // of the document — the page stays readable, only 划词 is lost.
          try { await tl.render(); } catch { /* page still visible */ }
        }
        // All pages rendered — now the text layer can be searched for a
        // citation jump target (consumes app.sourceJumpText/Loc).
        if (!cancelled) jumpPdfCitation();
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
  // excerpts keep their page so drafts can cite "p.12"). Selection rects are
  // captured as page-fraction coordinates so highlights re-render at any zoom.
  type PdfRect = { x: number; y: number; w: number; h: number };
  let pdfSel = $state<{ text: string; page: number; x: number; y: number; rects: PdfRect[] } | null>(null);
  let excerptFlash = $state<string | null>(null);
  function onPdfMouseUp(ev: MouseEvent) {
    const s = window.getSelection();
    const text = s?.toString().replace(/\s+/g, " ").trim() ?? "";
    if (!s || s.isCollapsed || text.length < 2) { pdfSel = null; return; }
    const anchor = s.anchorNode;
    const el = anchor instanceof Element ? anchor : anchor?.parentElement ?? null;
    const pageWrap = el?.closest("[data-page]") as HTMLElement | null;
    if (!pageWrap || !pdfBox?.contains(pageWrap)) { pdfSel = null; return; }
    const pageRect = pageWrap.getBoundingClientRect();
    const rects: PdfRect[] = [];
    for (const r of Array.from(s.getRangeAt(0).getClientRects())) {
      const w = r.width / pageRect.width;
      const h = r.height / pageRect.height;
      if (w <= 0.0005 || h <= 0.0005) continue;
      rects.push({
        x: (r.left - pageRect.left) / pageRect.width,
        y: (r.top - pageRect.top) / pageRect.height,
        w, h,
      });
    }
    const rect = s.getRangeAt(0).getBoundingClientRect();
    pdfSel = { text, page: Number(pageWrap.dataset.page), x: rect.left + rect.width / 2, y: rect.top - 10, rects };
  }
  function copyPdfSelection() {
    if (pdfSel) void navigator.clipboard.writeText(pdfSel.text);
    pdfSel = null;
  }
  async function excerptPdfSelection() {
    if (!pdfSel || !app.activeSource) return;
    try {
      await api.excerptAdd(pdfSel.text, undefined, app.activeSource.id, pdfSel.page, undefined, undefined,
        pdfSel.rects.length ? JSON.stringify(pdfSel.rects) : undefined);
      excerptFlash = `已存为摘录（p.${pdfSel.page}）`;
      setTimeout(() => (excerptFlash = null), 2600);
      window.getSelection()?.removeAllRanges();
      pdfSel = null;
      void loadAnnots();
    } catch (e) {
      excerptFlash = `摘录失败：${String(e)}`;
      setTimeout(() => (excerptFlash = null), 4000);
    }
  }

  // ---- highlights overlay + annotation sidebar (③坐标回链) ---------------
  // Saved excerpts with rects render as translucent highlight blocks on their
  // page (page-fraction coords → zoom independent). Sidebar entries scroll the
  // page into view and flash their highlight.
  let pdfAnnots = $state<api.Excerpt[]>([]);
  let annotsOpen = $state(false);
  let flashId = $state<string | null>(null);
  async function loadAnnots() {
    if (!app.activeSource) { pdfAnnots = []; return; }
    try { pdfAnnots = await api.excerptsListSource(app.activeSource.id); }
    catch { pdfAnnots = []; }
  }
  $effect(() => { void app.activeSource?.id; void loadAnnots(); });

  function overlayHighlights(box: HTMLElement, annots: api.Excerpt[]) {
    for (const wrap of Array.from(box.querySelectorAll<HTMLElement>("[data-page]"))) {
      for (const old of Array.from(wrap.querySelectorAll(".sv-hl"))) old.remove();
      const page = Number(wrap.dataset.page);
      for (const a of annots) {
        if (!a.rects || a.page !== page) continue;
        let rects: PdfRect[] = [];
        try { rects = JSON.parse(a.rects); } catch { continue; }
        for (const r of rects) {
          const div = document.createElement("div");
          div.className = "sv-hl" + (flashId === a.id ? " sv-hl--flash" : "");
          div.dataset.ann = a.id;
          div.style.left = `${r.x * 100}%`;
          div.style.top = `${r.y * 100}%`;
          div.style.width = `${r.w * 100}%`;
          div.style.height = `${r.h * 100}%`;
          wrap.appendChild(div);
        }
      }
    }
  }
  // Rendering finishes asynchronously page by page; re-overlay once it settles
  // and whenever the annotation list changes.
  $effect(() => {
    if (pdfRendering) return;
    const box = pdfBox;
    if (!box) return;
    const annots = pdfAnnots;
    overlayHighlights(box, annots);
  });
  function jumpToAnnot(a: api.Excerpt) {
    if (!pdfBox || !a.page) return;
    const wrap = pdfBox.querySelector<HTMLElement>(`[data-page="${a.page}"]`);
    if (!wrap) return;
    wrap.scrollIntoView({ behavior: "smooth", block: "start" });
    flashId = a.id;
    overlayHighlights(pdfBox, pdfAnnots);
    setTimeout(() => {
      flashId = null;
      if (pdfBox) overlayHighlights(pdfBox, pdfAnnots);
    }, 1800);
  }
  async function deleteAnnot(id: string) {
    try {
      await api.excerptDelete(id);
      await loadAnnots();
    } catch (e) {
      excerptFlash = `删除失败：${String(e)}`;
      setTimeout(() => (excerptFlash = null), 3000);
    }
  }

  // ---- 精读：划词翻译/解释 + 中英对照 ----------------------------------
  // Both used to live in 文献阅读, which had its own reading surface for the
  // same paper. Reading happens here now (PDF pages, highlights, citations);
  // leaving the精读 actions behind would have made "one reader" a downgrade.
  let aiSel = $state<{ text: string; mode: string; busy: boolean; out: string; err: string | null } | null>(null);
  const AI_MODE_LABEL: Record<string, string> = {
    "translate:zh": "翻译",
    explain: "讲解",
    summarize: "摘要",
  };
  async function explainSelection(mode: "translate:zh" | "explain" | "summarize") {
    if (!pdfSel) return;
    const text = pdfSel.text;
    aiSel = { text, mode, busy: true, out: "", err: null };
    pdfSel = null; // the toolbar would otherwise sit on top of the answer
    try {
      const out = await api.literatureExplain(text, mode);
      aiSel = { ...aiSel, busy: false, out };
    } catch (e) {
      aiSel = { ...aiSel, busy: false, out: "", err: String(e) };
    }
  }

  // 中英对照: paragraph-aligned translation of a text source. Capped at 24
  // paragraphs — each one is a model call, and an unbounded book is a bill the
  // user did not ask for.
  const BI_CAP = 24;
  let biMode = $state(false);
  let biTrans = $state<Record<number, string>>({});
  let biPending = $state<Set<number>>(new Set());
  let biKey = $state("");
  const biParas = $derived.by(() =>
    (src?.content ?? "")
      .split(/\n{2,}/)
      .map((p) => p.trim())
      .filter((p) => p.length > 0),
  );
  function biDirection(text: string): "zh" | "en" {
    const head = text.slice(0, 400);
    const ascii = [...head].filter((c) => c.charCodeAt(0) < 128).length;
    return ascii > [...head].length * 0.6 ? "zh" : "en";
  }
  async function toggleBilingual() {
    if (biMode) { biMode = false; return; }
    const paras = biParas.slice(0, BI_CAP);
    if (!paras.length) return;
    const key = `${paras.length}:${paras[0]}`;
    if (biKey !== key) { biKey = key; biTrans = {}; }
    const target = biDirection(paras.join(" "));
    biMode = true;
    for (let i = 0; i < paras.length; i++) {
      if (biTrans[i] !== undefined || biPending.has(i)) continue;
      biPending = new Set([...biPending, i]);
      try {
        const t = await api.literatureExplain(paras[i], `translate:${target}`);
        biTrans = { ...biTrans, [i]: t.trim() };
      } catch {
        biTrans = { ...biTrans, [i]: "（翻译失败 — 检查模型配置）" };
      } finally {
        const next = new Set(biPending);
        next.delete(i);
        biPending = next;
      }
    }
  }

  // ---- 2.6 图表自动解释 ------------------------------------------------
  // 选中的文字（通常是图表题注 "Figure 3: …"）作为定位线索，整页 150dpi 渲染
  // 后交给 vision 模型解释。结果展示在右侧浮层，可关闭。
  let figAnswer = $state<string | null>(null);
  let figBusy = $state(false);
  let figError = $state<string | null>(null);

  async function explainFigure() {
    if (!pdfSel || !app.activeSource?.stored_path || figBusy) return;
    figBusy = true;
    figError = null;
    figAnswer = null;
    try {
      figAnswer = await api.figureExplain(
        app.activeSource.stored_path,
        pdfSel.page,
        pdfSel.text,
      );
    } catch (e) {
      figError = String(e);
    } finally {
      figBusy = false;
      pdfSel = null;
    }
  }

  // ---- citation → PDF page jump (④全文问答的回链) --------------------------
  // PDF chunks carry no `loc`, so the chunk-list jump can't apply. Instead the
  // citation's snippet (the chunk's own text) is searched across every page's
  // rendered text layer; a match scrolls the page into view and flashes the
  // matched spans. Best-effort across extraction engines: pdftotext/docling
  // (chunk text) and pdf.js (text layer) disagree on spacing/hyphens, so both
  // sides are normalized (lowercase, hyphens/whitespace stripped) and the
  // needle is retried shorter before giving up silently.
  function findPdfText(box: HTMLElement, needle: string): { wrap: HTMLElement; rects: PdfRect[] } | null {
    const norm = (s: string) =>
      s.toLowerCase().replace(/[\u00ad\u2010-\u2015-]/g, "").replace(/\s+/g, "");
    for (let cut = 60; cut >= 24; cut -= 12) {
      const target = norm(needle).slice(0, cut);
      if (target.length < 10) break;
      for (const wrap of Array.from(box.querySelectorAll<HTMLElement>("[data-page]"))) {
        const parts: { el: HTMLElement; text: string }[] = [];
        let acc = "";
        for (const sp of Array.from(wrap.querySelectorAll<HTMLElement>(".textLayer span"))) {
          const t = norm(sp.textContent ?? "");
          if (!t) continue;
          parts.push({ el: sp, text: t });
          acc += t;
        }
        const idx = acc.indexOf(target);
        if (idx < 0) continue;
        const end = idx + target.length;
        const pageRect = wrap.getBoundingClientRect();
        const rects: PdfRect[] = [];
        let pos = 0;
        for (const p of parts) {
          const s0 = pos;
          pos += p.text.length;
          if (pos <= idx || s0 >= end) continue;
          const r = p.el.getBoundingClientRect();
          const w = r.width / pageRect.width;
          const h = r.height / pageRect.height;
          if (w <= 0.0005 || h <= 0.0005) continue;
          rects.push({
            x: (r.left - pageRect.left) / pageRect.width,
            y: (r.top - pageRect.top) / pageRect.height,
            w, h,
          });
        }
        return { wrap, rects };
      }
    }
    return null;
  }
  function jumpPdfCitation() {
    const box = pdfBox;
    const needle = app.sourceJumpText ?? app.sourceJumpLoc ?? "";
    if (!box || !needle) return;
    app.sourceJumpText = null;
    app.sourceJumpLoc = null;
    const m = findPdfText(box, needle);
    if (!m) return;
    m.wrap.scrollIntoView({ behavior: "smooth", block: "start" });
    for (const r of m.rects) {
      const div = document.createElement("div");
      div.className = "sv-hl sv-hl--flash";
      div.style.left = `${r.x * 100}%`;
      div.style.top = `${r.y * 100}%`;
      div.style.width = `${r.w * 100}%`;
      div.style.height = `${r.h * 100}%`;
      m.wrap.appendChild(div);
      setTimeout(() => div.remove(), 3200);
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
      {#if isPdfDoc}
        <button
          class="btn btn--icon btn--sm btn--ghost"
          class:sv-annots-toggle-on={annotsOpen}
          onclick={() => (annotsOpen = !annotsOpen)}
          title="本篇的高亮与摘录（点击批注跳回原文位置）"
        >
          <Icon name="book" size={13} />{pdfAnnots.length ? ` ${pdfAnnots.length}` : ""}
        </button>
      {/if}
      {#if src}
        {#if isText && src?.content}
          <button
            class="btn btn--sm btn--ghost"
            class:sv-on={biMode}
            onclick={() => void toggleBilingual()}
            title="逐段对照：原文在左，模型译文在右（最多 {BI_CAP} 段）"
          >
            <Icon name="globe" size={13} /> {biMode ? "关闭对照" : "中英对照"}
          </button>
        {/if}
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
            <button class="fb" onclick={() => void explainSelection("translate:zh")}
                    title="把选中段落翻成中文">
              <Icon name="globe" size={11} /> 翻译
            </button>
            <button class="fb" onclick={() => void explainSelection("explain")}
                    title="让模型讲解这段在说什么">
              <Icon name="chat" size={11} /> 讲解
            </button>
            <button class="fb" onclick={explainFigure} disabled={figBusy}
                    title="用视觉模型解释本页图表（以选中文字为题注线索）">
              <Icon name="chart" size={11} /> {figBusy ? "解读中…" : "图解"}
            </button>
          </div>
        {/if}
        {#if figAnswer || figError || figBusy}
          <div class="float" style="position:fixed;right:18px;top:76px;max-width:min(420px,40vw);max-height:70vh;overflow-y:auto;text-align:left" role="status">
            {#if figBusy}
              <span class="fb mono"><Icon name="chart" size={11} /> 正在解读第 {pdfSel?.page ?? ""} 页图表…</span>
            {:else if figError}
              <div class="fb" style="color:var(--err,var(--warn))">图表解读失败：{figError}</div>
            {:else if figAnswer}
              <div class="fb" style="display:block;padding:10px 12px">
                <div style="display:flex;align-items:center;gap:6px;margin-bottom:6px">
                  <Icon name="chart" size={11} />
                  <strong style="font-size:var(--t-xs)">图表解读</strong>
                  <span style="flex:1"></span>
                  <button class="fb" onclick={() => { figAnswer = null; figError = null; }} title="关闭">
                    <Icon name="x" size={11} />
                  </button>
                </div>
                <RichText text={figAnswer} />
              </div>
            {/if}
          </div>
        {/if}
        {#if excerptFlash}
          <div class="float" style="position:fixed;left:50%;top:64px;transform:translateX(-50%)">
            <span class="fb mono">{excerptFlash}</span>
          </div>
        {/if}
        {#if aiSel}
          <div class="float sv-ai" role="status">
            {#if aiSel.busy}
              <span class="fb mono">{AI_MODE_LABEL[aiSel.mode]}中…</span>
            {:else if aiSel.err}
              <div class="fb" style="color:var(--err,var(--warn))">{AI_MODE_LABEL[aiSel.mode]}失败：{aiSel.err}</div>
            {:else}
              <div class="fb sv-ai-body">
                <div class="sv-ai-head">
                  <Icon name={aiSel.mode === "explain" ? "chat" : "globe"} size={11} />
                  <strong>{AI_MODE_LABEL[aiSel.mode]}</strong>
                  <span style="flex:1"></span>
                  <button class="fb" onclick={() => (aiSel = null)} title="关闭"><Icon name="x" size={11} /></button>
                </div>
                <p class="sv-ai-q mono">{aiSel.text.slice(0, 120)}{aiSel.text.length > 120 ? "…" : ""}</p>
                <div class="sv-ai-out"><RichText text={aiSel.out} /></div>
              </div>
            {/if}
            {#if aiSel && !aiSel.busy}
              <button class="fb sv-ai-close" onclick={() => (aiSel = null)} title="关闭">
                <Icon name="x" size={11} />
              </button>
            {/if}
          </div>
        {/if}
        {#if annotsOpen}
          <aside class="sv-annots" role="complementary" aria-label="本篇批注">
            <div class="sv-annots-head">
              <span class="mono" style="font-size:var(--t-xs)">本篇批注 · {pdfAnnots.length}</span>
              <span style="flex:1"></span>
              <button class="btn btn--icon btn--sm btn--ghost" onclick={() => (annotsOpen = false)} title="收起">
                <Icon name="x" size={11} />
              </button>
            </div>
            {#if !pdfAnnots.length}
              <p class="sv-annot-empty mono">选中正文即可存摘录——摘录会以高亮形式留在原页上，点击这里可跳回原文。</p>
            {:else}
              {#each pdfAnnots as a (a.id)}
                <div class="sv-annot" class:sv-annot--flash={flashId === a.id}>
                  <button class="sv-annot-main" onclick={() => jumpToAnnot(a)} title="跳转到原文位置">
                    {#if a.page}<span class="mono sv-annot-p">p.{a.page}</span>{/if}
                    <span class="sv-annot-q">{a.quote}</span>
                  </button>
                  <button class="btn btn--icon btn--sm btn--ghost sv-annot-del" onclick={() => void deleteAnnot(a.id)} title="删除该批注">
                    <Icon name="x" size={11} />
                  </button>
                </div>
              {/each}
            {/if}
          </aside>
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
          {#if biMode}
            {#each biParas.slice(0, BI_CAP) as para, i (i)}
              <div class="bi-row">
                <p class="read bi-orig">{para}</p>
                <p class="read bi-trans">
                  {#if biTrans[i] !== undefined}
                    {biTrans[i]}
                  {:else if biPending.has(i)}
                    <span class="bi-wait mono">翻译中…</span>
                  {:else}
                    <span class="bi-wait mono">—</span>
                  {/if}
                </p>
              </div>
            {/each}
            {#if biParas.length > BI_CAP}
              <p class="pdf-note mono" style="font-size:var(--t-xs);color:var(--fg-muted);margin-top:12px">
                只对照了前 {BI_CAP} 段（共 {biParas.length} 段）—— 每段一次模型调用，长文请分段读。
              </p>
            {/if}
          {:else}
            <p class="read pdf-note sv-text">{src.content}</p>
          {/if}
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
  /* 划词 AI 面板：固定在右上，避开阅读区正文 */
  .sv-ai {
    right: 18px; top: 76px;
    max-width: min(420px, 42vw); max-height: 68vh;
    overflow-y: auto; text-align: left;
    flex-direction: column; align-items: stretch;
  }
  .sv-ai-body { display: block; padding: 10px 12px; }
  .sv-ai-head { display: flex; align-items: center; gap: 6px; margin-bottom: 6px; }
  .sv-ai-q {
    margin: 0 0 8px; font-size: 10.5px; color: var(--fg-faint);
    border-left: 2px solid var(--border-strong); padding-left: 8px;
  }
  .sv-ai-out { font-family: var(--font-read); font-size: 13px; line-height: 1.7; color: var(--fg); }
  .sv-ai-close { align-self: flex-end; }
  .btn.sv-on {
    color: var(--accent);
    border-color: color-mix(in oklab, var(--accent) 45%, transparent);
    background: color-mix(in oklab, var(--accent) 12%, transparent);
  }

  /* 中英对照：每段一行两栏，段级天然对齐（来自原文献阅读面） */
  .bi-row {
    display: grid; grid-template-columns: 1fr 1fr; gap: 18px;
    padding: 10px 0; border-bottom: 1px dashed var(--border); align-items: start;
  }
  .bi-row:last-of-type { border-bottom: none; }
  .bi-orig { margin: 0; }
  .bi-trans {
    margin: 0; color: var(--fg-muted);
    border-left: 2px solid color-mix(in oklab, var(--accent) 40%, transparent);
    padding-left: 12px;
  }
  .bi-wait { font-size: 10px; opacity: 0.6; }
  @media (max-width: 900px) {
    .bi-row { grid-template-columns: 1fr; gap: 6px; }
    .bi-trans { border-left: none; padding-left: 0; border-top: 1px solid var(--border); padding-top: 6px; }
  }

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

  /* PDF highlights (created via DOM, hence :global) + annotation sidebar */
  :global(.sv-hl) {
    position: absolute;
    background: rgba(255, 200, 0, 0.32);
    border-radius: 2px;
    pointer-events: none;
    mix-blend-mode: multiply;
    transition: background 0.25s, box-shadow 0.25s;
  }
  :global(.sv-hl--flash) {
    background: rgba(255, 145, 0, 0.45);
    box-shadow: 0 0 0 3px rgba(255, 145, 0, 0.35);
  }
  .sv-annots {
    position: fixed;
    right: 18px;
    top: 76px;
    width: min(330px, 36vw);
    max-height: 72vh;
    overflow-y: auto;
    z-index: 66;
    background: var(--panel, var(--bg-elevated, #fff));
    border: 1px solid var(--border);
    border-radius: 10px;
    box-shadow: 0 10px 30px rgba(0, 0, 0, 0.14);
  }
  .sv-annots-head {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 8px 6px 12px;
    border-bottom: 1px solid var(--border);
    position: sticky;
    top: 0;
    background: inherit;
  }
  .sv-annot {
    display: flex;
    align-items: stretch;
    border-bottom: 1px solid var(--border);
    transition: background 0.2s;
  }
  .sv-annot:last-child {
    border-bottom: none;
  }
  .sv-annot--flash {
    background: rgba(255, 170, 0, 0.16);
  }
  .sv-annot-main {
    flex: 1;
    min-width: 0;
    display: flex;
    gap: 7px;
    align-items: baseline;
    text-align: left;
    background: none;
    border: none;
    cursor: pointer;
    padding: 8px 10px;
    font-size: var(--t-xs);
    color: inherit;
  }
  .sv-annot-p {
    color: var(--fg-muted);
    flex-shrink: 0;
  }
  .sv-annot-q {
    display: -webkit-box;
    -webkit-line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow: hidden;
    line-height: 1.45;
  }
  .sv-annot-del {
    align-self: center;
  }
  .sv-annot-empty {
    padding: 10px 12px;
    font-size: var(--t-xs);
    color: var(--fg-muted);
    line-height: 1.6;
  }
  .sv-annots-toggle-on {
    color: var(--accent, inherit);
  }
  .sv-delete:hover {
    color: var(--err, var(--warn));
  }
</style>
