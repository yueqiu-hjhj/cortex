<script lang="ts">
  import { app } from "../lib/store.svelte";
  import { isMobile } from "../lib/platform";
  import * as api from "../lib/api";
  import { jobs } from "../lib/jobs.svelte";
  import Icon from "../components/Icon.svelte";
  import Picker from "../components/Picker.svelte";

  // Current input method selected
  let method = $state<"upload" | "url" | "text" | "record" | "photo" | null>(null);
  // User-entered URL or text body
  let value = $state("");
  // Title for text pastes
  let textTitle = $state("");

  const allMethods = [
    { id: "upload" as const, ico: "doc",    t: "上传文件",   d: "PDF · PPTX · DOCX · TXT · MD", k: "u" },
    { id: "url"    as const, ico: "search", t: "粘贴链接",       d: "网页 · YouTube",            k: "p" },
    { id: "text"   as const, ico: "doc",    t: "粘贴文本",      d: "Markdown · 纯文本",         k: "t" },
    { id: "record" as const, ico: "record", t: "录制讲座",  d: "实时录音 + 转写",       k: "r" },
    { id: "photo"  as const, ico: "grid",   t: "拍照识别",      d: "白板 / 书页 OCR",       k: "o" },
  ] as const;
  // Recording is available on mobile again: it captures audio and transcribes via the
  // homelab Whisper endpoint (remote-first). MobileShell mounts the Recorder view.
  const methods = allMethods;

  // Subject + topic selectors, seeded ONCE from the per-topic "+" token
  // (app.addSourceTopicId) or the active subject. A single guarded effect
  // avoids the prior two-effect cascade that reset the subject back to the
  // active one when the picked topic lived in a different subject. The subject
  // Picker's onChange resets the topic imperatively for later changes.
  let selectedSubjectId = $state<string>(app.activeSubjectId ?? "");
  let selectedTopic = $state(""); // "" = no topic → null on the wire
  let seeded = false;
  $effect(() => {
    if (seeded) return;
    const pending = app.addSourceTopicId;
    const owning = pending
      ? (app.subjects.find((s) => s.topics.some((t) => t.id === pending)) ?? null)
      : null;
    if (owning) {
      selectedSubjectId = owning.id;
      selectedTopic = pending!;
    } else {
      selectedSubjectId = app.activeSubjectId ?? "";
      selectedTopic = app.subjects.find((s) => s.id === selectedSubjectId)?.topics[0]?.id ?? "";
    }
    seeded = true;
    if (pending) app.addSourceTopicId = null; // consume once
  });
  const subjectOptions = $derived(
    app.subjects.map((s) => ({ id: s.id, label: s.name }))
  );
  const selectedSubject = $derived(
    app.subjects.find((s) => s.id === selectedSubjectId) ?? null
  );
  const topicId = $derived(selectedTopic || null);
  // Themed dropdown options: the selected subject's topics, plus an explicit "no topic" entry.
  const topicOptions = $derived([
    ...(selectedSubject?.topics ?? []).map((t) => ({ id: t.id, label: t.name })),
    { id: "", label: "— no topic —" },
  ]);

  function guardSubject(): boolean {
    if (!selectedSubject) {
      app.pushToast({ kind: "error", title: "Select a subject first", body: "Choose a subject before adding a source." });
      return false;
    }
    return true;
  }

  /** Register a background ingest job (no navigation). */
  function queueIngest(input: Parameters<typeof api.addSource>[0], name: string) {
    jobs.start({
      kind: "source",
      label: name,
      subjectId: input.subject_id,
      topicId: input.topic_id ?? null,
      run: () => api.addSource(input),
      onDone: () => app.refresh(),
    });
  }

  /** Queue one ingest then jump to the subject's Sources tab. */
  function startIngest(input: Parameters<typeof api.addSource>[0], name: string) {
    queueIngest(input, name);
    app.openSubject(input.subject_id);
    app.setTab("sources");
  }

  // The iOS picker returns a percent-encoded file:// URL; derive a clean, decoded
  // display name from it (the actual file is staged+decoded in Rust stage_upload).
  function pickedName(p: string): string {
    const n = p.split(/[\\/?#]/).filter(Boolean).pop() ?? p;
    try { return decodeURIComponent(n); } catch { return n; }
  }

  async function beginUpload() {
    if (!guardSubject()) return;
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const picked = await open({
        multiple: true,
        directory: false,
        filters: [{ name: "Documents", extensions: ["pdf", "epub", "docx", "pptx", "doc", "ppt", "txt", "md"] }],
      });
      const paths = Array.isArray(picked) ? picked : picked ? [picked] : [];
      if (paths.length === 0) return;

      // Queue every picked file (concurrent ingests are safe now that each
      // LibreOffice conversion gets its own profile), then navigate once.
      for (const path of paths) {
        const name = pickedName(path);
        // On mobile the picker returns a temp path the OS deletes before the
        // background ingest runs — copy it into app storage first. Surface a real
        // error (don't silently fall back to the doomed temp path) so a staging
        // failure is visible instead of a later "no such file" deep in ingest.
        let ingestPath = path;
        if (isMobile) {
          try {
            ingestPath = await api.stageUpload(path);
          } catch (e) {
            app.pushToast({ kind: "error", title: "Couldn't read the file", body: String(e) });
            continue;
          }
        }
        queueIngest({ subject_id: selectedSubjectId, topic_id: topicId, path: ingestPath, name, tags: [] }, name);
      }
      if (paths.length > 1) {
        app.pushToast({ kind: "info", title: `Ingesting ${paths.length} files`, body: "Added to the queue." });
      }
      app.openSubject(selectedSubjectId);
      app.setTab("sources");
    } catch (e) {
      app.pushToast({ kind: "error", title: "File pick failed", body: String(e) });
    }
  }

  /** Pick a folder and queue every supported file inside it (recursively). */
  async function beginFolder() {
    if (!guardSubject()) return;
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const dir = await open({ directory: true, multiple: false });
      if (!dir || typeof dir !== "string") return;
      const files = await api.listFolderSources(dir);
      if (files.length === 0) {
        app.pushToast({ kind: "warning", title: "Nothing to import", body: "No PDFs, slides, docs or images in that folder." });
        return;
      }
      const imgExts = ["png", "jpg", "jpeg", "webp"];
      for (const f of files) {
        const ext = (f.name.split(".").pop() ?? "").toLowerCase();
        const input: Parameters<typeof api.addSource>[0] = imgExts.includes(ext)
          ? { subject_id: selectedSubjectId, topic_id: topicId, path: f.path, kind: "image", name: f.name, tags: [] }
          : { subject_id: selectedSubjectId, topic_id: topicId, path: f.path, name: f.name, tags: [] };
        queueIngest(input, f.name);
      }
      app.pushToast({ kind: "info", title: `Ingesting ${files.length} file${files.length === 1 ? "" : "s"}`, body: "Folder contents added to the queue." });
      app.openSubject(selectedSubjectId);
      app.setTab("sources");
    } catch (e) {
      app.pushToast({ kind: "error", title: "Folder import failed", body: String(e) });
    }
  }

  function beginUrl() {
    if (!guardSubject()) return;
    if (!value.trim()) return;
    const url = value.trim();
    startIngest({ subject_id: selectedSubjectId, topic_id: topicId, url, name: url, tags: [] }, url);
  }

  function beginText() {
    if (!guardSubject()) return;
    if (!value.trim()) return;
    const name = textTitle.trim() || "Pasted text";
    startIngest({ subject_id: selectedSubjectId, topic_id: topicId, text: value.trim(), kind: "md", name, tags: [] }, name);
  }

  async function beginPhoto() {
    if (!guardSubject()) return;
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const picked = await open({
        multiple: false,
        directory: false,
        filters: [{ name: "Images", extensions: ["png", "jpg", "jpeg", "webp"] }],
      });
      const path = typeof picked === "string" ? picked : picked?.[0] ?? null;
      if (!path) return;

      const name = pickedName(path);
      let ingestPath = path;
      if (isMobile) {
        try {
          ingestPath = await api.stageUpload(path);
        } catch (e) {
          app.pushToast({ kind: "error", title: "Couldn't read the image", body: String(e) });
          return;
        }
      }
      startIngest({ subject_id: selectedSubjectId, topic_id: topicId, path: ingestPath, kind: "image", name, tags: [] }, name);
    } catch (e) {
      app.pushToast({ kind: "error", title: "Image pick failed", body: String(e) });
    }
  }

  function handleBegin() {
    if (method === "upload") beginUpload();
    else if (method === "url") beginUrl();
    else if (method === "text") beginText();
    else if (method === "record") app.setView("recorder");
    else if (method === "photo") beginPhoto();
  }

  function selectMethod(id: typeof methods[number]["id"]) {
    method = id;
    value = "";
    if (id === "record") app.setView("recorder");
  }

  // Claim the keyboard while this view is open so the method mnemonics
  // (u/p/t/r/o) work and don't collide with the global single-key shortcuts
  // (t = cycle theme, r = recorder). Esc back-nav still works (handled before
  // the view-keys guard in App.svelte). Typing in an input is left alone.
  $effect(() => {
    (window as any).__cortexViewKeys = true;
    function onKey(e: KeyboardEvent) {
      const el = document.activeElement as HTMLElement | null;
      const typing = !!el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable);
      if (typing || e.metaKey || e.ctrlKey || e.altKey) return;
      const m = methods.find((x) => x.k === e.key);
      if (m) { e.preventDefault(); selectMethod(m.id); }
    }
    window.addEventListener("keydown", onKey);
    return () => {
      (window as any).__cortexViewKeys = false;
      window.removeEventListener("keydown", onKey);
    };
  });

  // OS drag-and-drop: Tauri's native webview drag-drop is ON by default, so it
  // swallows HTML5 ondrop — we must listen via the webview API instead. Dropping
  // files (from Finder/Explorer) anywhere on the Add Source view queues them, the
  // same path beginUpload() uses. (Wrapped in try/catch so a browser/dev preview
  // without the Tauri API doesn't break the view.)
  $effect(() => {
    let un: (() => void) | undefined;
    (async () => {
      try {
        const { getCurrentWebview } = await import("@tauri-apps/api/webview");
        un = await getCurrentWebview().onDragDropEvent((event) => {
          if (event.payload.type !== "drop") return;
          if (!guardSubject()) return;
          const docExts = ["pdf", "epub", "docx", "pptx", "doc", "ppt", "txt", "md"];
          const imgExts = ["png", "jpg", "jpeg", "webp"];
          const dropped = event.payload.paths.filter((p) => {
            const e = (p.split(".").pop() ?? "").toLowerCase();
            return docExts.includes(e) || imgExts.includes(e);
          });
          if (dropped.length === 0) return;
          for (const path of dropped) {
            const name = path.split(/[\\/]/).pop() ?? path;
            const ext = (path.split(".").pop() ?? "").toLowerCase();
            const input: Parameters<typeof api.addSource>[0] = imgExts.includes(ext)
              ? { subject_id: selectedSubjectId, topic_id: topicId, path, kind: "image", name, tags: [] }
              : { subject_id: selectedSubjectId, topic_id: topicId, path, name, tags: [] };
            queueIngest(input, name);
          }
          app.pushToast({ kind: "info", title: `Ingesting ${dropped.length} file${dropped.length === 1 ? "" : "s"}`, body: "Dropped files added to the queue." });
          app.openSubject(selectedSubjectId);
          app.setTab("sources");
        });
      } catch { /* not running under Tauri (dev/web preview) */ }
    })();
    return () => un?.();
  });
</script>

<div class="addsrc">
  <!-- Page header -->
  <div class="addsrc-head">
    {#if !isMobile}
      <button class="btn btn--icon btn--sm btn--ghost" onclick={() => app.setView("subject")} title="Back">
        <span style:transform="rotate(180deg)" style:display="flex"><Icon name="chevron" size={14} /></span>
      </button>
    {/if}
    <div>
      <div class="eyebrow">添加资料</div>
      <h1 class="addsrc-title">新资料</h1>
    </div>
    {#if selectedSubject}
      <div class="addsrc-crumb mono faint">
        into {selectedSubject.name}{selectedTopic ? " › " + (selectedSubject.topics.find((t) => t.id === selectedTopic)?.name ?? "") : ""}
      </div>
    {/if}
  </div>

  <div class="addsrc-grid">
    <!-- LEFT: method picker + target -->
    <div class="addsrc-left">
      {#if isMobile}
        <!-- Touch: a themed dropdown instead of clipping tiles. -->
        <div class="field">
          <span class="onb-label mono">资料来源</span>
          <Picker
            value={method ?? ""}
            onChange={(id) => selectMethod(id as typeof methods[number]["id"])}
            options={methods.map((m) => ({ id: m.id, label: m.t }))}
            icon={methods.find((m) => m.id === method)?.ico ?? "doc"}
            placeholder="选择一种来源…"
          />
        </div>
      {:else}
      <div class="add-methods">
        {#each methods as m (m.id)}
          <button
            class="add-method{method === m.id ? ' on' : ''}"
            onclick={() => selectMethod(m.id)}
          >
            <span class="am-ico"><Icon name={m.ico} size={18} /></span>
            <div class="am-text">
              <div class="am-t">{m.t}</div>
              <div class="am-d mono">{m.d}</div>
            </div>
            <span class="kbd">{m.k}</span>
          </button>
        {/each}
      </div>
      {/if}

      <div class="addsrc-target">
        <div class="field">
          <span class="onb-label mono">科目</span>
          <Picker
            value={selectedSubjectId}
            onChange={(id) => { selectedSubjectId = id; selectedTopic = ""; }}
            options={subjectOptions}
            placeholder="— 选择科目 —"
          />
        </div>
        {#if selectedSubject}
          <div class="field">
            <span class="onb-label mono">主题 <span class="faint">归到哪里</span></span>
            <Picker
              value={selectedTopic}
              onChange={(id) => (selectedTopic = id)}
              options={topicOptions}
              placeholder="— 不归入主题 —"
            />
          </div>
        {/if}
      </div>
    </div>

    <!-- RIGHT: input panel for the chosen method -->
    <div class="addsrc-right">
      <!-- On mobile, don't render the big empty panel before a type is picked. -->
      {#if !(isMobile && method === null)}
      <div class="addsrc-panel">
        {#if method === null}
          <div class="addsrc-empty">
            <Icon name="plus" size={26} color="var(--fg-faint)" />
            <p class="mono faint">在左侧选择来源类型，<br />或按对应按键（u · p · t · r · o）。</p>
          </div>
        {:else if method === "url"}
          <span class="onb-label mono">链接</span>
          <!-- svelte-ignore a11y_autofocus -->
          <input class="input" autofocus placeholder="https://… 或 YouTube 链接" bind:value onkeydown={(e) => e.key === "Enter" && beginUrl()} />
          <p class="mono faint addsrc-hint">网页或 YouTube 链接，会自动抓取正文并入库。</p>
        {:else if method === "text"}
          <span class="onb-label mono">粘贴文本</span>
          <!-- svelte-ignore a11y_autofocus -->
          <input class="input" autofocus placeholder="标题（可选）" bind:value={textTitle} />
          <textarea class="input addsrc-textarea" placeholder="在这里粘贴文本或 Markdown…" bind:value></textarea>
        {:else if method === "upload"}
          <span class="onb-label mono">上传文件</span>
          <!-- svelte-ignore a11y_click_events_have_key_events -->
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div class="add-drop addsrc-drop" onclick={beginUpload}>
            <Icon name="doc" size={22} color="var(--fg-faint)" />
            <span class="mono">拖入文件，或点击浏览 — 支持多选</span>
            <span class="mono faint">PDF · PPTX · DOCX · TXT · MD · 图片</span>
          </div>
          <button class="btn btn--ghost btn--sm" style="margin-top:8px" onclick={beginFolder}>
            <Icon name="grid" size={12} /> Add a folder — imports every supported file inside
          </button>
        {:else if method === "photo"}
          <span class="onb-label mono">拍照识别</span>
          <!-- svelte-ignore a11y_click_events_have_key_events -->
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div class="add-drop addsrc-drop" onclick={beginPhoto}>
            <Icon name="grid" size={22} color="var(--fg-faint)" />
            <span class="mono">点击选择一张图片</span>
            <span class="mono faint">PNG · JPG · WebP — 自动 OCR 成文字</span>
          </div>
        {:else if method === "record"}
          <div class="addsrc-empty">
            <Icon name="record" size={26} color="var(--accent)" />
            <p class="mono faint">正在打开讲座录音…</p>
          </div>
        {/if}
      </div>

      <!-- Footer actions -->
      <div class="add-foot addsrc-foot">
        <button class="btn btn--ghost" onclick={() => app.setView("subject")}>取消</button>
        {#if method === "url" || method === "text"}
          <button class="btn btn--primary" disabled={!value.trim() || !selectedSubject} onclick={handleBegin}>
            Ingest source <Icon name="arrowR" size={13} />
          </button>
        {:else if method === "upload"}
          <button class="btn btn--primary" disabled={!selectedSubject} onclick={beginUpload}>
            Pick file(s) <Icon name="arrowR" size={13} />
          </button>
        {:else if method === "photo"}
          <button class="btn btn--primary" disabled={!selectedSubject} onclick={beginPhoto}>
            Pick image <Icon name="arrowR" size={13} />
          </button>
        {:else if method === "record"}
          <button class="btn btn--primary" disabled={!selectedSubject} onclick={() => app.setView("recorder")}>
            Open recorder <Icon name="arrowR" size={13} />
          </button>
        {:else}
          <button class="btn btn--primary" disabled onclick={handleBegin}>
            Ingest source <Icon name="arrowR" size={13} />
          </button>
        {/if}
      </div>
      {/if}
    </div>
  </div>
</div>

<style>
  .addsrc {
    height: 100%;
    display: flex;
    flex-direction: column;
    padding: clamp(16px, 3vh, 32px) clamp(20px, 4vw, 56px);
    gap: 18px;
    overflow: hidden;
  }
  .addsrc-head { display: flex; align-items: center; gap: 12px; flex: 0 0 auto; }
  /* .addsrc-title typography comes from the shared .page-title rule (app.css). */
  .addsrc-title { margin: 0; }
  .addsrc-crumb { margin-left: auto; font-size: var(--t-xs); }
  .addsrc-grid {
    flex: 1 1 auto;
    min-height: 0;
    display: grid;
    grid-template-columns: minmax(280px, 360px) 1fr;
    gap: clamp(18px, 3vw, 40px);
    align-items: stretch;
  }
  .addsrc-left { display: flex; flex-direction: column; gap: 16px; min-height: 0; }
  .add-methods { display: flex; flex-direction: column; gap: 8px; }
  .addsrc-target { display: flex; flex-direction: column; gap: 12px; margin-top: auto; }
  .addsrc-right { display: flex; flex-direction: column; min-height: 0; }
  .addsrc-panel {
    flex: 1 1 auto;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
    border: 1px solid var(--border);
    border-radius: var(--rad-3, 12px);
    background: var(--surface);
    padding: 18px;
    overflow: auto;
  }
  .addsrc-empty {
    flex: 1 1 auto;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    text-align: center;
  }
  .addsrc-textarea { flex: 1 1 auto; min-height: 160px; resize: none; }
  .addsrc-drop { flex: 1 1 auto; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 8px; }
  .addsrc-hint { font-size: var(--t-xs); margin: 2px 0 0; }
  .addsrc-foot { margin-top: 14px; display: flex; justify-content: flex-end; gap: 10px; flex: 0 0 auto; }
  @media (max-width: 760px) {
    .addsrc { overflow: auto; }
    .addsrc-grid { grid-template-columns: 1fr; }
  }
</style>
