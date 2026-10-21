<script lang="ts">
  // Select a passage → file it under a note. One global component, mounted
  // once in App.svelte, serving two paths:
  //
  //   1. a real selection inside any `[data-clip-scope]` element → a small bar
  //      above it ("摘到笔记" / "先存着");
  //   2. an answer's "+" button → `app.openClipPicker({ …whole reply… })`,
  //      which skips the bar and opens the picker directly.
  //
  // Scopes declare their own attribution (see the data-clip-* attributes), so
  // this component never needs to know whether the text came from a chat reply,
  // a PDF page or a note. Adding a new source is one attribute, not a branch.
  import { app } from "../lib/store.svelte";
  import * as api from "../lib/api";
  import Icon from "./Icon.svelte";

  const MIN_CHARS = 2;
  const MAX_CHARS = 20_000;

  let bar = $state<{ x: number; y: number; below: boolean } | null>(null);
  let sel = $state<api.ClipRequest | null>(null);

  // ── selection bar ────────────────────────────────────────────────────────
  function hideBar() {
    bar = null;
    sel = null;
  }

  function capture() {
    const s = window.getSelection();
    const text = s?.toString().trim() ?? "";
    if (!s || s.rangeCount === 0 || text.length < MIN_CHARS) {
      hideBar();
      return;
    }
    const anchor = s.anchorNode;
    const host =
      (anchor?.nodeType === 1 ? (anchor as HTMLElement) : anchor?.parentElement) ?? null;
    const scope = host?.closest("[data-clip-scope]") as HTMLElement | null;
    // Not every selection is worth keeping: only inside a declared scope.
    if (!scope) {
      hideBar();
      return;
    }
    const rect = s.getRangeAt(0).getBoundingClientRect();
    if (rect.width === 0 && rect.height === 0) {
      hideBar();
      return;
    }
    const below = rect.top < 64; // no room above (scrolled to the top)
    sel = {
      quote: text.slice(0, MAX_CHARS),
      sourceKind: (scope.dataset.clipScope || "manual") as api.ClipKind,
      sourceId: scope.dataset.clipSource ?? null,
      sourceLabel: scope.dataset.clipLabel ?? null,
      page: scope.dataset.clipPage ? Number(scope.dataset.clipPage) : null,
    };
    bar = {
      x: Math.min(Math.max(rect.left + rect.width / 2, 110), Math.max(110, window.innerWidth - 110)),
      y: below ? rect.bottom + 10 : rect.top - 10,
      below,
    };
  }

  function onDocUp(e: MouseEvent) {
    const t = e.target as HTMLElement | null;
    if (t?.closest("[data-clip-ui]")) return; // clicks on our own chrome
    // The browser updates the selection after mouseup in some engines.
    setTimeout(capture, 0);
  }

  function onDocKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      hideBar();
      if (isOpen) close();
    }
    // Shift+Arrow / Cmd+A selections are worth clipping too.
    if (e.shiftKey || e.key === "a") setTimeout(capture, 0);
  }

  // ── picker ───────────────────────────────────────────────────────────────
  let isOpen = $state(false);
  let req = $state<api.ClipRequest | null>(null);
  let notes = $state<api.Note[] | null>(null);
  let q = $state("");
  let newTitle = $state("");
  let busy = $state(false);
  let error = $state<string | null>(null);

  // The "+" button on an answer sets app.clipPick; the bar sets it too.
  $effect(() => {
    const r = app.clipPick;
    if (!r) return;
    req = r;
    isOpen = true;
    error = null;
    q = "";
    newTitle = suggestTitle(r.quote);
    hideBar();
    void loadNotes();
    app.closeClipPicker();
  });

  let filtered = $derived(
    (notes ?? []).filter((n) => n.title.toLowerCase().includes(q.trim().toLowerCase())).slice(0, 12),
  );

  async function loadNotes() {
    try {
      notes = await api.listNotes(null);
    } catch {
      notes = [];
    }
  }

  /** First meaningful line, clipped — a starting title the user can overwrite. */
  function suggestTitle(text: string): string {
    const line =
      text
        .split("\n")
        .map((l) => l.replace(/^[#>\-*\s]+/, "").trim())
        .find((l) => l.length > 0) ?? "摘录";
    return line.length > 28 ? line.slice(0, 28) + "…" : line;
  }

  function close() {
    if (busy) return;
    isOpen = false;
    req = null;
    error = null;
  }

  /** The clip row to file: either one already in the inbox, or a fresh capture. */
  async function ensureClip(r: api.ClipRequest): Promise<string> {
    if (r.clipId) return r.clipId;
    const clip = await api.clipAdd(r);
    return clip.id;
  }

  async function fileToNote(noteId: string) {
    const r = req;
    if (!r || busy) return;
    busy = true;
    error = null;
    try {
      // Land any pending edit in the open note first — the clip is about to be
      // written to the same row, and a late autosave would drop it.
      await app.flushNote?.();
      const clipId = await ensureClip(r);
      const filed = await api.clipToNote(clipId, noteId);
      await app.reloadNote?.(); // if that note is the open one, show the passage
      app.pushToast({
        kind: "success",
        title: `已摘到《${filed.note_title ?? "笔记"}》`,
        body: `写在「## 摘录」下 · ${oneLine(r.quote)}`,
        action: filed.note_id ? { label: "打开", run: () => app.openNote(filed.note_id!) } : undefined,
      });
      isOpen = false;
      req = null;
      await loadNotes();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function fileToNewNote() {
    const r = req;
    if (!r || busy) return;
    const title = newTitle.trim();
    if (!title) {
      error = "新笔记需要一个标题";
      return;
    }
    busy = true;
    error = null;
    try {
      await app.flushNote?.();
      const clipId = await ensureClip(r);
      const filed = await api.clipToNewNote(clipId, title, app.activeSubjectId ?? null);
      app.pushToast({
        kind: "success",
        title: `已建《${title}》并摘入`,
        body: oneLine(r.quote),
        action: filed.note_id ? { label: "打开", run: () => app.openNote(filed.note_id!) } : undefined,
      });
      isOpen = false;
      req = null;
      await loadNotes();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  /** Inbox: keep the passage without deciding where it goes. */
  async function stash() {
    const r = sel;
    if (!r || busy) return;
    busy = true;
    try {
      await api.clipAdd(r);
      hideBar();
      window.getSelection()?.removeAllRanges();
      app.pushToast({
        kind: "info",
        title: "已收进待整理",
        body: "在「笔记」页顶部的摘录收件箱里随时归位",
        action: { label: "去看看", run: () => app.setView("notes") },
      });
    } catch (e) {
      app.pushToast({ kind: "error", title: "没能保存这条摘录", body: String(e) });
    } finally {
      busy = false;
    }
  }

  function oneLine(t: string): string {
    const s = t.replace(/\s+/g, " ").trim();
    return s.length > 48 ? s.slice(0, 48) + "…" : s;
  }

  function kindLabel(k: string): string {
    return (
      { chat: "AI 对话", agent: "智能体", note: "笔记", paper: "论文", source: "资料", web: "网页" }[
        k
      ] ?? "摘录"
    );
  }
</script>

<svelte:window onmouseup={onDocUp} onkeyup={onDocKey} onscroll={() => hideBar()} />

<!-- ── selection bar ─────────────────────────────────────────────────────── -->
{#if bar && sel && !isOpen}
  <div
    class="cb"
    class:cb-below={bar.below}
    style={`left:${bar.x}px; top:${bar.y}px`}
    data-clip-ui="1"
    role="toolbar"
    aria-label="摘录所选文字"
  >
    <button
      type="button"
      class="cb-main"
      onmousedown={(e) => e.preventDefault()}
      onclick={() => {
        if (sel) app.openClipPicker(sel);
      }}
      title="把这段文字摘到某篇笔记的「## 摘录」下"
    >
      <Icon name="plus" size={12} /> 摘到笔记
    </button>
    <button
      type="button"
      class="cb-side"
      onmousedown={(e) => e.preventDefault()}
      onclick={() => void stash()}
      disabled={busy}
      title="先存进收件箱，回头再归位"
    >
      先存着
    </button>
  </div>
{/if}

<!-- ── picker ────────────────────────────────────────────────────────────── -->
{#if isOpen && req}
  <div class="cp-back" data-clip-ui="1" role="presentation" onmousedown={close}>
    <div
      class="cp"
      role="dialog"
      aria-modal="true"
      aria-label="摘录到笔记"
      tabindex="-1"
      onmousedown={(e) => e.stopPropagation()}
    >
      <header class="cp-head">
        <div class="cp-title">
          <Icon name="doc" size={13} /> 摘到笔记
        </div>
        <div class="cp-src mono">
          {req.sourceLabel || kindLabel(req.sourceKind)}{req.page ? ` · p.${req.page}` : ""}
        </div>
      </header>

      <blockquote class="cp-quote">{req.quote}</blockquote>

      {#if app.lastNoteId}
        <button
          type="button"
          class="cp-row cp-current"
          disabled={busy}
          onclick={() => void fileToNote(app.lastNoteId!)}
        >
          <span class="cp-cur-tag mono">当前</span>
          <span class="cp-row-title">{app.lastNoteTitle ?? "正在读的笔记"}</span>
          <span class="cp-row-hint mono">摘到这里</span>
        </button>
      {/if}

      <div class="cp-sect mono">放进已有笔记</div>
      <input class="input cp-search" bind:value={q} placeholder="搜索笔记标题…" spellcheck="false" />
      <div class="cp-list">
        {#if notes === null}
          <div class="cp-empty mono">读取中…</div>
        {:else if filtered.length === 0}
          <div class="cp-empty mono">
            {notes.length === 0 ? "还没有笔记 —— 用下面「新建一篇」" : `没有匹配「${q}」的笔记`}
          </div>
        {:else}
          {#each filtered as n (n.id)}
            <button type="button" class="cp-row" disabled={busy} onclick={() => void fileToNote(n.id)}>
              <span class="cp-row-title">{n.title}</span>
              {#if n.vault_path}<span class="cp-dot" title="已落盘为 .md">·md</span>{/if}
              <span class="cp-row-hint mono">{relTime(n.updated_at)}</span>
            </button>
          {/each}
        {/if}
      </div>

      <div class="cp-sect mono">新建一篇</div>
      <div class="cp-new">
        <input
          class="input"
          bind:value={newTitle}
          placeholder="新笔记标题"
          spellcheck="false"
          onkeydown={(e) => {
            if (e.key === "Enter") void fileToNewNote();
          }}
        />
        <button type="button" class="btn btn--primary btn--sm" disabled={busy} onclick={() => void fileToNewNote()}>
          {busy ? "…" : "新建并摘入"}
        </button>
      </div>

      {#if error}
        <div class="cp-err mono">{error}</div>
      {/if}

      <footer class="cp-foot">
        <span class="mono">落在目标笔记的「## 摘录」标题下，不改动你已有的正文</span>
        <button type="button" class="btn btn--ghost btn--sm" onclick={close} disabled={busy}>取消</button>
      </footer>
    </div>
  </div>
{/if}

<script lang="ts" module>
  function relTime(ms: number): string {
    const d = Date.now() - ms;
    if (d < 60_000) return "刚刚";
    if (d < 3_600_000) return `${Math.floor(d / 60_000)} 分钟前`;
    if (d < 86_400_000) return `${Math.floor(d / 3_600_000)} 小时前`;
    if (d < 2_592_000_000) return `${Math.floor(d / 86_400_000)} 天前`;
    return new Date(ms).toLocaleDateString();
  }
</script>

<style>
  /* selection bar — floats over the text, never takes focus */
  .cb {
    position: fixed;
    z-index: 940;
    transform: translate(-50%, -100%);
    display: flex;
    align-items: stretch;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: 9px;
    box-shadow: 0 10px 28px rgba(0, 0, 0, 0.45);
    overflow: hidden;
    animation: cb-in 0.1s ease;
  }
  .cb-below {
    transform: translate(-50%, 0);
  }
  .cb-main,
  .cb-side {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    border: 0;
    background: transparent;
    color: var(--fg);
    font-family: var(--font-mono);
    font-size: 11.5px;
    padding: 7px 10px;
    cursor: pointer;
    white-space: nowrap;
  }
  .cb-main:hover,
  .cb-side:hover {
    background: var(--surface-2);
    color: var(--fg-bright);
  }
  .cb-main {
    color: var(--accent);
    font-weight: 600;
  }
  .cb-side {
    border-left: 1px solid var(--border);
    color: var(--fg-muted);
  }
  .cb-side:disabled {
    opacity: 0.55;
    cursor: default;
  }
  @keyframes cb-in {
    from { opacity: 0; transform: translate(-50%, calc(-100% + 4px)); }
    to { opacity: 1; }
  }

  /* picker */
  .cp-back {
    position: fixed;
    inset: 0;
    z-index: 950;
    display: flex;
    align-items: center;
    justify-content: center;
    background: color-mix(in oklab, var(--bg) 60%, transparent);
    backdrop-filter: blur(3px);
    animation: cp-fade 0.12s ease;
  }
  .cp {
    width: min(480px, calc(100vw - 40px));
    max-height: min(78vh, 720px);
    display: flex;
    flex-direction: column;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--r-lg, 12px);
    box-shadow: 0 22px 60px rgba(0, 0, 0, 0.55);
    padding: 16px;
    animation: cp-pop 0.13s ease;
  }
  .cp-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 10px;
  }
  .cp-title {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-family: var(--font-mono);
    font-size: var(--t-md, 14px);
    font-weight: 600;
    color: var(--fg-bright);
  }
  .cp-src {
    font-size: 10.5px;
    color: var(--fg-faint);
  }
  .cp-quote {
    margin: 12px 0 0;
    padding: 9px 11px;
    max-height: 132px;
    overflow-y: auto;
    background: var(--surface-2);
    border-left: 3px solid var(--accent);
    border-radius: 0 8px 8px 0;
    font-size: 12.5px;
    line-height: 1.55;
    color: var(--fg-muted);
    white-space: pre-wrap;
    word-break: break-word;
  }
  .cp-sect {
    margin: 14px 0 6px;
    font-size: 10px;
    font-weight: 600;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }
  .cp-search {
    width: 100%;
  }
  .cp-list {
    margin-top: 6px;
    max-height: 200px;
    overflow-y: auto;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface-2);
  }
  .cp-row {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    border: 0;
    border-bottom: 1px solid var(--border);
    background: transparent;
    padding: 8px 10px;
    text-align: left;
    color: var(--fg);
    font-size: 12.5px;
    cursor: pointer;
  }
  .cp-row:last-child {
    border-bottom: 0;
  }
  .cp-row:hover:not(:disabled) {
    background: color-mix(in oklab, var(--accent) 9%, var(--surface-2));
    color: var(--fg-bright);
  }
  .cp-row:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .cp-current {
    margin-top: 12px;
    border: 1px solid color-mix(in oklab, var(--accent) 45%, var(--border)) !important;
    border-radius: 8px;
    background: color-mix(in oklab, var(--accent) 7%, var(--surface));
  }
  .cp-cur-tag {
    font-size: 9.5px;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--accent);
    border: 1px solid color-mix(in oklab, var(--accent) 40%, transparent);
    border-radius: 4px;
    padding: 1px 4px;
    flex: none;
  }
  .cp-row-title {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .cp-dot {
    color: var(--fg-faint);
    font-family: var(--font-mono);
    font-size: 10px;
    flex: none;
  }
  .cp-row-hint {
    flex: none;
    font-size: 10px;
    color: var(--fg-faint);
  }
  .cp-empty {
    padding: 14px 10px;
    text-align: center;
    font-size: 11px;
    color: var(--fg-faint);
  }
  .cp-new {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .cp-new .input {
    flex: 1;
  }
  .cp-err {
    margin-top: 10px;
    font-size: 11px;
    line-height: 1.5;
    color: var(--warn);
  }
  .cp-foot {
    margin-top: 14px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }
  .cp-foot .mono {
    font-size: 10px;
    line-height: 1.4;
    color: var(--fg-faint);
  }
  @keyframes cp-fade {
    from { opacity: 0; }
    to { opacity: 1; }
  }
  @keyframes cp-pop {
    from { opacity: 0; transform: translateY(6px) scale(0.985); }
    to { opacity: 1; transform: none; }
  }
</style>
