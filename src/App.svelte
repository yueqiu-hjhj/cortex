<script lang="ts">
  import { app } from "./lib/store.svelte";
  import Sidebar from "./components/Sidebar.svelte";
  import Titlebar from "./components/Titlebar.svelte";
  import StatusBar from "./components/StatusBar.svelte";
  import CommandPalette from "./components/CommandPalette.svelte";
  import GlobalSearch from "./components/GlobalSearch.svelte";
  import ToastStack from "./components/ToastStack.svelte";
  import OcrModal from "./components/OcrModal.svelte";
  import DiffModal from "./components/DiffModal.svelte";
  import MusicPanel from "./components/MusicPanel.svelte";
  import SourceMetaModal from "./components/SourceMetaModal.svelte";
  import LeaderPane from "./components/LeaderPane.svelte";
  import ChatPanel from "./components/ChatPanel.svelte";
  import ResizeHandle from "./components/ResizeHandle.svelte";
  import Icon from "./components/Icon.svelte";
  import FindBar from "./components/FindBar.svelte";
  import HelpOverlay from "./components/HelpOverlay.svelte";
  import ContextMenu from "./components/ContextMenu.svelte";
  import Dialog from "./components/Dialog.svelte";
  import EditModal from "./components/EditModal.svelte";
  import SubjectPanel from "./components/SubjectPanel.svelte";
  import NotificationCenter from "./components/NotificationCenter.svelte";
  import NotificationDetail from "./components/NotificationDetail.svelte";
  import ExamBanner from "./components/ExamBanner.svelte";
  import PomodoroPanel from "./components/PomodoroPanel.svelte";
  import LiveActivity from "./components/LiveActivity.svelte";
  import RecordingActivity from "./components/RecordingActivity.svelte";
  import { keybinds } from "./lib/keybinds.svelte";
  import { isMobile } from "./lib/platform";
  import * as api from "./lib/api";
  import { jobs } from "./lib/jobs.svelte";
  import MindMapView from "./components/MindMapView.svelte";
  import MobileShell from "./shell/MobileShell.svelte";

  import Dashboard from "./views/Dashboard.svelte";
  import Today from "./views/Today.svelte";
  import Plan from "./views/Plan.svelte";
  import Research from "./views/Research.svelte";
  import Agents from "./views/Agents.svelte";
  import SubjectView from "./views/SubjectView.svelte";
  import SourceViewer from "./views/SourceViewer.svelte";
  import AddSource from "./views/AddSource.svelte";
  import AddSubject from "./views/AddSubject.svelte";
  import GenerateMaterial from "./views/GenerateMaterial.svelte";
  import NotesView from "./views/NotesView.svelte";
  import Onboarding from "./views/Onboarding.svelte";

  // The three heaviest views (Settings 70KB, Recorder 32KB, Calendar 31KB) are
  // code-split out of the startup bundle and loaded on first visit. They're
  // prefetched right after init settles, so by the time a human can navigate
  // there the chunk is already resolved — no flash, just a faster cold start.
  let SettingsView = $state<any>(null);
  let RecorderView = $state<any>(null);
  let CalendarViewC = $state<any>(null);
  let AnalyticsViewC = $state<any>(null);
  let ExamViewC = $state<any>(null);
  let NeuralGraphC = $state<any>(null);
  let PomodoroTimelineC = $state<any>(null);
  let GlossaryViewC = $state<any>(null);
  let ReviewTodayC = $state<any>(null);
  let SprintPlannerC = $state<any>(null);
  let LiteratureReaderC = $state<any>(null);
  let VocabularyWorkbenchC = $state<any>(null);
  const loadSettings = () =>
    SettingsView ?? import("./views/Settings.svelte").then((m) => (SettingsView = m.default));
  const loadRecorder = () =>
    RecorderView ?? import("./views/Recorder.svelte").then((m) => (RecorderView = m.default));
  const loadCalendar = () =>
    CalendarViewC ?? import("./views/CalendarView.svelte").then((m) => (CalendarViewC = m.default));
  const loadAnalytics = () =>
    AnalyticsViewC ?? import("./views/AnalyticsView.svelte").then((m) => (AnalyticsViewC = m.default));
  const loadExam = () =>
    ExamViewC ?? import("./views/ExamView.svelte").then((m) => (ExamViewC = m.default));
  const loadNeuralGraph = () =>
    NeuralGraphC ?? import("./views/NeuralGraph.svelte").then((m) => (NeuralGraphC = m.default));
  const loadPomodoro = () =>
    PomodoroTimelineC ?? import("./views/PomodoroTimeline.svelte").then((m) => (PomodoroTimelineC = m.default));
  const loadGlossary = () =>
    GlossaryViewC ?? import("./views/Glossary.svelte").then((m) => (GlossaryViewC = m.default));
  const loadReviewToday = () =>
    ReviewTodayC ?? import("./views/ReviewToday.svelte").then((m) => (ReviewTodayC = m.default));
  const loadSprintPlanner = () =>
    SprintPlannerC ?? import("./views/SprintPlanner.svelte").then((m) => (SprintPlannerC = m.default));
  const loadLiteratureReader = () =>
    LiteratureReaderC ?? import("./views/LiteratureReader.svelte").then((m) => (LiteratureReaderC = m.default));
  const loadVocabularyWorkbench = () =>
    VocabularyWorkbenchC ?? import("./views/VocabularyWorkbench.svelte").then((m) => (VocabularyWorkbenchC = m.default));
  $effect(() => {
    if (app.view === "settings") void loadSettings();
    else if (app.view === "recorder") void loadRecorder();
    else if (app.view === "calendar") void loadCalendar();
    else if (app.view === "analytics") void loadAnalytics();
    else if (app.view === "exam") void loadExam();

    else if (app.view === "neural-graph") void loadNeuralGraph();
    else if (app.view === "pomodoro") void loadPomodoro();
    else if (app.view === "review") void loadReviewToday();
    else if (app.view === "sprint") void loadSprintPlanner();
    else if (app.view === "literature") void loadLiteratureReader();
    else if (app.view === "vocabulary") void loadVocabularyWorkbench();
    else if (app.view === "glossary") void loadGlossary();
  });

  // Custom title bar is only for macOS and Windows — on Linux (especially
  // tiling WMs like Hyprland/Omarchy) the window manager frames windows itself,
  // so a custom bar is intrusive. Linux keeps the frameless look it had before.
  const showTitlebar = /Mac|Win/i.test(navigator.userAgent);

  // Initialize app state on mount (loads subjects, seeds demo if empty, restores theme)
  $effect(() => {
    app.init();
    const prefetch = setTimeout(() => {
      void loadSettings();
      void loadRecorder();
      void loadCalendar();
      void loadAnalytics();
      void loadExam();
    }, 1500);
    return () => clearTimeout(prefetch);
  });

  // Global OS drag-drop: drop documents on ANY view to queue them for ingest.
  // The Add Source view registers its own handler (with its subject/topic
  // pickers) — skip there to avoid double-queuing. Tauri's native webview
  // drag-drop swallows HTML5 ondrop, so the webview API is the only channel.
  $effect(() => {
    let un: (() => void) | undefined;
    (async () => {
      try {
        const { getCurrentWebview } = await import("@tauri-apps/api/webview");
        un = await getCurrentWebview().onDragDropEvent((event) => {
          if (event.payload.type !== "drop") return;
          if (app.view === "add-source") return; // covered by AddSource's handler
          const docExts = ["pdf", "epub", "docx", "pptx", "doc", "ppt", "txt", "md"];
          const imgExts = ["png", "jpg", "jpeg", "webp"];
          const dropped = event.payload.paths.filter((p) => {
            const e = (p.split(".").pop() ?? "").toLowerCase();
            return docExts.includes(e) || imgExts.includes(e);
          });
          if (dropped.length === 0) return;
          const subjectId = app.activeSubjectId;
          if (!subjectId) {
            app.pushToast({ kind: "error", title: "还没有科目", body: "先新建一个科目，之后在任何页面拖入文件即可入库。" });
            return;
          }
          for (const path of dropped) {
            const name = path.split(/[\\/]/).pop() ?? path;
            const ext = (path.split(".").pop() ?? "").toLowerCase();
            const input: Parameters<typeof api.addSource>[0] = imgExts.includes(ext)
              ? { subject_id: subjectId, topic_id: null, path, kind: "image", name, tags: [] }
              : { subject_id: subjectId, topic_id: null, path, name, tags: [] };
            jobs.start({ kind: "source", label: name, subjectId, topicId: null, run: () => api.addSource(input), onDone: () => app.refresh() });
          }
          app.pushToast({ kind: "info", title: `已开始导入 ${dropped.length} 个文件`, body: "进度见当前科目的任务卡片。" });
        });
      } catch { /* not running under Tauri */ }
    })();
    return () => un?.();
  });

  // Startup self-check. Runs once, shortly after init, and only surfaces a
  // dismissible banner when a check fails — the app stays fully usable either
  // way, so this never blocks boot or interrupts a working session.
  let selfcheck = $state<api.SelfCheckReport | null>(null);
  let selfcheckDismissed = $state(false);
  let selfcheckOpen = $state(false);
  let selfcheckStarted = false;
  $effect(() => {
    if (selfcheckStarted) return;
    selfcheckStarted = true;
    const t = setTimeout(() => {
      api
        .startupSelfCheck()
        .then((r) => (selfcheck = r))
        .catch(() => {});
    }, 1200);
    return () => clearTimeout(t);
  });
  const selfcheckFailures = $derived(selfcheck?.checks.filter((c) => !c.ok) ?? []);

  // Chat dock / FAB visibility. Read EVERY signal into a local first so &&/||
  // short-circuiting can't drop one from Svelte's reactive dependency set (that
  // bug left the dock stuck visible on the notes view regardless of chatOpen).
  const chatViewOk = $derived.by(() => {
    const v = app.view;
    const tab = app.subjectTab;
    return v === "notes" || (v === "subject" && tab !== "chats");
  });
  const showChatDock = $derived.by(() => {
    const open = app.chatOpen;
    return open && chatViewOk;
  });
  const showChatFab = $derived.by(() => {
    const open = app.chatOpen;
    const v = app.view;
    return !open && (chatViewOk || v === "source");
  });

  // ---- responsive shell ----
  // The drawer/compact layout is fully styled in app.css but was never switched
  // on. Drive it from the viewport width so the app collapses gracefully toward
  // mobile: < 1080px → chat dock becomes an overlay (compact); < 760px → the
  // sidebar becomes a slide-in drawer behind a hamburger (tight).
  let vw = $state(typeof window !== "undefined" ? window.innerWidth : 1280);
  // Global mind map data: subjects → topics → sources. Built client-side;
  // no backend call needed (subjects/topics are already in app state).
  const globalMap = $derived.by(() => {
    if (app.view !== "mindmap") return null;
    const branches: { label: string; children?: { label: string }[] }[] = [];
    for (const s of app.subjects) {
      const sub: typeof branches[number] = { label: (s.glyph + " " + s.name).trim() };
      const kids: { label: string }[] = [];
      for (const t of s.topics ?? []) {
        kids.push({ label: ((t.glyph ?? "•") + " " + t.name).trim() });
      }
      kids.push({ label: s.sourceCount + " 个来源" });
      sub.children = kids;
      branches.push(sub);
    }
    return { central: "Cortex · 知识脑图", branches };
  });

    // Mobile/desktop branch is reactive: a WebView resized below 640px
  // switches to MobileShell even on Windows (where isMobile() stays false).
  const mobileNow = $derived(isMobile || vw < 640);
  $effect(() => {
    const onResize = () => (vw = window.innerWidth);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  });
  // Below 1080 the sidebar becomes a slide-in drawer (overlay, never pushes the
  // workspace) so the content always has the full width to itself — that kills
  // the 760-1080 "squash" where a fixed 248px rail + workspace both fought for
  // ~900px. `tight` (<760) keeps its phone tweaks; `drawer` is the shared
  // hamburger+overlay sidebar mode for the whole sub-1080 band.
  const tight = $derived(vw < 760);
  const drawer = $derived(vw < 1080);
  const compact = $derived(vw < 1080);
  let navOpen = $state(false);
  // Close the drawer when leaving drawer mode or when navigating anywhere.
  $effect(() => { if (!drawer) navOpen = false; });
  $effect(() => { void app.view; void app.activeSubjectId; void app.subjectTab; navOpen = false; });

  // ---- view back-stack: Esc goes back to the previous page ----
  let viewHistory: string[] = [];
  let prevView = app.view;
  let navigatingBack = false;
  $effect(() => {
    const v = app.view;
    if (v !== prevView) {
      if (!navigatingBack) {
        viewHistory.push(prevView);
        if (viewHistory.length > 50) viewHistory.shift();
      }
      navigatingBack = false;
      prevView = v;
    }
  });
  function goBack() {
    const last = viewHistory.pop();
    if (last !== undefined) {
      navigatingBack = true;
      app.setView(last as typeof app.view);
    }
  }

  // Find-in-page (Ctrl/Cmd+F) — claimed in the CAPTURE phase so it beats the
  // WebKitGTK native find-in-page (which otherwise popped up and swallowed
  // Ctrl+C/Ctrl+V). Only this exact combo is intercepted; plain Ctrl and other
  // Ctrl chords (copy/paste/etc.) pass straight through.
  $effect(() => {
    function onFind(e: KeyboardEvent) {
      // NB: keep the two halves as separate booleans. Writing this as one
      // `(mod) && (key)` expression triggers a Svelte 5 compiler bug that strips
      // the parentheses (→ `ctrl || meta && key... || key...`), which made bare
      // Ctrl and Shift+F open find. Separate statements compile correctly.
      const mod = e.ctrlKey || e.metaKey;
      const isF = e.key === "f" || e.key === "F";
      if (mod && isF) {
        e.preventDefault();
        e.stopPropagation();
        e.stopImmediatePropagation(); // also beat any native find-in-page handler
        app.findOpen = true;
      }
    }
    window.addEventListener("keydown", onFind, true);
    return () => window.removeEventListener("keydown", onFind, true);
  });

  // Kill webview zoom — Ctrl/Cmd+scroll and Ctrl/Cmd +/-/0 otherwise scale the
  // whole UI, which makes Cortex feel like a zoomable web page / PWA instead of
  // a native app. Captured early so WebKitGTK never gets to zoom. Ctrl+F / Ctrl+P
  // (letters) are untouched — only the zoom combos are swallowed.
  $effect(() => {
    function onWheel(e: WheelEvent) {
      if (e.ctrlKey || e.metaKey) e.preventDefault();
    }
    function onZoomKey(e: KeyboardEvent) {
      if (!(e.ctrlKey || e.metaKey)) return;
      // Repurpose the native zoom combos to drive Cortex's OWN UI scale (CSS zoom),
      // so Cmd/Ctrl +/- actually resize the app on every OS (incl. macOS) without the
      // zoomable-web-page feel, and Cmd/Ctrl 0 resets to 100%.
      if (["=", "+"].includes(e.key)) { e.preventDefault(); e.stopPropagation(); app.setUiScale(app.uiScale + 10); }
      else if (["-", "_"].includes(e.key)) { e.preventDefault(); e.stopPropagation(); app.setUiScale(app.uiScale - 10); }
      else if (e.key === "0") { e.preventDefault(); e.stopPropagation(); app.setUiScale(100); }
    }
    // passive:false is required for preventDefault on wheel.
    window.addEventListener("wheel", onWheel, { passive: false, capture: true });
    window.addEventListener("keydown", onZoomKey, true);
    return () => {
      window.removeEventListener("wheel", onWheel, true);
      window.removeEventListener("keydown", onZoomKey, true);
    };
  });

  // Global keyboard engine (Helix-style). Modals/sessions set window.__cortexModalOpen
  // to claim the keyboard; we stay out of their way then.
  let gPrefix = false;
  $effect(() => {
    if (isMobile) return; // phones have no hardware-keyboard engine — disable it
    function onKey(e: KeyboardEvent) {
      const el = document.activeElement as HTMLElement | null;
      const typing =
        !!el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable);

      // Alt+1..9 — jump to the Nth subject (like clicking it in the navbar).
      if (e.altKey && !e.ctrlKey && !e.metaKey && /^[1-9]$/.test(e.key)) {
        e.preventDefault();
        const s = app.subjects[parseInt(e.key, 10) - 1];
        if (s) app.openSubject(s.id);
        return;
      }

      if (e.key === "Escape") {
        // 1. Close any transient overlay first.
        if (app.findOpen) { app.findOpen = false; return; }
        if (app.detail) { app.closeDetail(); return; }
        if (app.notifOpen) { app.notifOpen = false; return; }
        if (app.cmdkOpen || app.leaderOpen || app.musicOpen || app.searchOpen) {
          app.cmdkOpen = false; app.leaderOpen = false; app.musicOpen = false; app.searchOpen = false; return;
        }
        if ((window as any).__cortexModalOpen) return; // modals handle their own Esc
        // 2. In a text field (e.g. the chat compose box) Esc just leaves edit mode.
        if (typing) { el?.blur(); app.setMode("NOR"); return; }
        // 3. Otherwise Esc ALWAYS navigates back to the previous page (chat is
        //    closed with `c`/its × button, so Esc is reserved for back-nav).
        app.setMode("NOR");
        goBack();
        return;
      }
      // Never act on a standalone modifier press (so Ctrl/Cmd for copy etc. work,
      // and a stray "Control" keybind can't open the palette).
      if (["Control", "Shift", "Alt", "Meta", "AltGraph", "CapsLock", "ContextMenu"].includes(e.key)) return;
      if (typing) return;
      if ((window as any).__cortexModalOpen) return; // diff/flashcards/quiz own the keyboard
      // Full-screen views (e.g. Settings) claim the keyboard so single-key
      // shortcuts (t/c/m/n…) don't fire while you're configuring things. Esc is
      // handled above this guard, so back-nav still works.
      if ((window as any).__cortexViewKeys) return;
      if (app.cmdkOpen) return;

      // Separate booleans on purpose — see the onFind note above (Svelte 5
      // paren-stripping compiler bug would otherwise make any Ctrl chord open this).
      const cmdMod = e.ctrlKey || e.metaKey;
      const isP = e.key === "p" || e.key === "P";
      if (cmdMod && isP) { e.preventDefault(); app.cmdkOpen = true; return; }
      const isK = e.key === "k" || e.key === "K";
      if (cmdMod && isK) { e.preventDefault(); app.searchOpen = true; return; }
      if (e.metaKey || e.ctrlKey || e.altKey) return;

      if (app.leaderOpen) return; // LeaderPane consumes its own keys
      const k = keybinds.map;
      // The command-palette key must be a symbol (":"). If a corrupt bind ever
      // made it an alphanumeric (e.g. "c"), that would hijack typing AND steal
      // the chat toggle — so sanitize at the use site, never trusting the value.
      const cmdkKey = /^[a-z0-9]$/i.test(k.cmdk) ? ":" : k.cmdk;
      if (e.key === cmdkKey) { e.preventDefault(); app.cmdkOpen = true; return; }
      if (e.key === k.leader) { e.preventDefault(); app.leaderOpen = true; return; }
      if (e.key === k.help) { e.preventDefault(); app.helpOpen = true; return; }
      if (e.key === k.dismissToast && app.toasts.length) { e.preventDefault(); app.dismissToast(app.toasts[app.toasts.length - 1].id); return; }

      if (gPrefix) {
        gPrefix = false;
        if (e.key === k.dashboard) { app.setView("dashboard"); return; }
        if (e.key === "s") { app.openSettings(); return; } // g s → Settings
      }
      if (e.key === "g") { gPrefix = true; setTimeout(() => (gPrefix = false), 600); return; }

      // Chat toggle. Use the configured bind, falling back to "c" ONLY when it's
      // unset/corrupt — so rebinding the chat key actually replaces "c" instead of
      // leaving both keys active (the old behaviour, which made rebinds "not work").
      if (e.key === (k.toggleChat || "c")) { app.toggleChat(); return; }
      if (e.key === k.toggleSidebar) { app.toggleSidebar(); return; }
      if (e.key === k.newSubject) { e.preventDefault(); app.setView("add-subject"); return; }
      if (e.key === k.recorder) { app.setView("recorder"); return; }
      if (e.key === k.cycleTheme) { app.cycleTheme(); return; }
      if (e.key === k.music) { app.musicOpen = true; return; }
      if (e.key === k.notifications) { e.preventDefault(); app.toggleNotifications(); return; }
      if (e.key === k.insert) {
        const ta = document.querySelector<HTMLTextAreaElement>(".compose-box textarea");
        if (ta) { e.preventDefault(); ta.focus(); app.setMode("INS"); }
        return;
      }

      if (app.view === "dashboard") {
        const n = app.subjects.length;
        if (e.key === "j" || e.key === "ArrowDown" || e.key === "l" || e.key === "ArrowRight") {
          e.preventDefault(); app.dashFocus = Math.min(n - 1, app.dashFocus + 1);
        } else if (e.key === "k" || e.key === "ArrowUp" || e.key === "h" || e.key === "ArrowLeft") {
          e.preventDefault(); app.dashFocus = Math.max(0, app.dashFocus - 1);
        } else if (e.key === "Enter" && app.subjects[app.dashFocus]) {
          e.preventDefault(); app.openSubject(app.subjects[app.dashFocus].id);
        }
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  // Global OS drag-and-drop (desktop): Tauri's native webview drag-drop is ON
  // by default and swallows HTML5 ondrop, so the ONLY way to receive Explorer
  // drops is the webview API — registered ONCE here for the whole app. Any
  // drop of an ingestible file (PDF/md/txt/docx/…/images) is routed to the
  // pendingDrops queue and lands on the「添加资料」view, which drains the
  // queue. Before this, dropping a PDF on any other view silently did
  // nothing — the #1 "PDF 拖不进去" complaint.
  $effect(() => {
    if (isMobile) return; // phones have no OS drag source
    let un: (() => void) | undefined;
    (async () => {
      try {
        const { getCurrentWebview } = await import("@tauri-apps/api/webview");
        un = await getCurrentWebview().onDragDropEvent((event) => {
          if (event.payload.type !== "drop") return;
          const docExts = ["pdf", "epub", "docx", "pptx", "doc", "ppt", "txt", "md"];
          const imgExts = ["png", "jpg", "jpeg", "webp"];
          const dropped = event.payload.paths.filter((p) => {
            const e = (p.split(".").pop() ?? "").toLowerCase();
            return docExts.includes(e) || imgExts.includes(e);
          });
          if (dropped.length === 0) return;
          app.pendingDrops.push(...dropped);
          app.setView("add-source");
          app.pushToast({
            kind: "info",
            title: `收到拖入的 ${dropped.length} 个文件`,
            body: "已切换到「添加资料」，选好科目后自动开始导入。",
          });
        });
      } catch { /* not running under Tauri (dev/web preview) */ }
    })();
    return () => un?.();
  });
</script>

{#if app.loading}
  <div class="boot">Cortex — loading…</div>
{:else if app.onboarding}
  <Onboarding onFinish={() => (app.onboarding = false)} />
{:else if mobileNow}
  <!-- Phone/tablet: touch shell (bottom tabs + sheets). Renders only when isMobile,
       so the desktop shell below is completely unaffected. See docs/MOBILE_PORT.md. -->
  <MobileShell />
{:else}
  <div
    class="app-shell"
    class:tight
    class:drawer
    class:compact
    class:nav-open={navOpen}
    style:--sb-w={app.sidebarCollapsed ? "52px" : app.sidebarWidth + "px"}
    style:--chat-w={app.chatWidth + "px"}
    style:--tb-h={showTitlebar ? "30px" : "0px"}
  >
    {#if showTitlebar}
      <div class="app-titlebar"><Titlebar /></div>
    {/if}
    {#if drawer}
      <!-- Sub-1080: hamburger toggles the slide-in sidebar drawer (overlay). -->
      <button class="nav-toggle" onclick={() => (navOpen = !navOpen)} aria-label="Toggle menu">
        <Icon name="grid" size={16} />
      </button>
      <Sidebar />
      <button class="nav-backdrop" aria-label="Close menu" onclick={() => (navOpen = false)}></button>
    {:else}
      <Sidebar />
      {#if app.sidebarCollapsed}
        <button class="sb-expand" onclick={() => app.toggleSidebar()} title="Show subjects tree (b)">
          <Icon name="chevron" size={14} />
        </button>
      {/if}
    {/if}

    <div class="app-main">
      {#if selfcheckFailures.length > 0 && !selfcheckDismissed}
        <div class="selfcheck" role="alert">
          <button
            class="sc-head"
            onclick={() => (selfcheckOpen = !selfcheckOpen)}
            aria-expanded={selfcheckOpen}
          >
            <Icon name="bolt" size={14} />
            <span class="sc-title">启动自检发现 {selfcheckFailures.length} 项问题</span>
            <span class="sc-spacer"></span>
            <span class="sc-toggle">{selfcheckOpen ? "收起" : "详情"}</span>
          </button>
          {#if selfcheckOpen}
            <ul class="sc-list">
              {#each selfcheckFailures as c (c.name)}
                <li class="sc-item">
                  <span class="sc-name">{c.name}</span>
                  <span class="sc-detail">{c.detail}</span>
                  {#if c.hint}<span class="sc-hint">{c.hint}</span>{/if}
                </li>
              {/each}
            </ul>
          {/if}
          <button class="sc-close" onclick={() => (selfcheckDismissed = true)} title="关闭">
            <Icon name="x" size={13} />
          </button>
        </div>
      {/if}
      <!-- Exam countdown banner (2.11): ≤7 days orange / ≤3 days red + daily push -->
      <ExamBanner />
      <div class="workspace">
        {#if app.view === "today"}
          <Today />
        {:else if app.view === "plan"}
          <Plan />
        {:else if app.view === "research"}
          <Research />
        {:else if app.view === "agents"}
          <Agents />
        {:else if app.view === "dashboard"}
          <Dashboard />
        {:else if app.view === "subject"}
          <SubjectView />
        {:else if app.view === "source"}
          <SourceViewer />
        {:else if app.view === "add-source"}
          <AddSource />
        {:else if app.view === "add-subject"}
          <AddSubject />
        {:else if app.view === "recorder"}
          {#if RecorderView}{@const Recorder = RecorderView}<Recorder />{/if}
        {:else if app.view === "gen-material"}
          <GenerateMaterial />
        {:else if app.view === "notes"}
          <NotesView />
        {:else if app.view === "calendar"}
          {#if CalendarViewC}{@const CalendarView = CalendarViewC}<CalendarView />{/if}
        {:else if app.view === "analytics"}
          {#if AnalyticsViewC}{@const AnalyticsView = AnalyticsViewC}<AnalyticsView />{/if}
        {:else if app.view === "exam"}
          {#if ExamViewC}{@const ExamView = ExamViewC}<ExamView />{/if}
        {:else if app.view === "neural-graph"}
          {#if NeuralGraphC}{@const NeuralGraph = NeuralGraphC}<NeuralGraph />{:else}<div class="mono faint" style="flex:1;min-height:180px;display:flex;align-items:center;justify-content:center;font-size:12px">正在加载图引擎…</div>{/if}
        {:else if app.view === "pomodoro"}
          {#if PomodoroTimelineC}{@const PomodoroTimeline = PomodoroTimelineC}<PomodoroTimeline />{/if}
        {:else if app.view === "review"}
          {#if ReviewTodayC}{@const ReviewToday = ReviewTodayC}<ReviewToday />{/if}
        {:else if app.view === "sprint"}
          {#if SprintPlannerC}{@const SprintPlanner = SprintPlannerC}<SprintPlanner />{/if}
        {:else if app.view === "literature"}
          {#if LiteratureReaderC}{@const LiteratureReader = LiteratureReaderC}<LiteratureReader />{/if}
        {:else if app.view === "vocabulary"}
          {#if VocabularyWorkbenchC}{@const VocabularyWorkbench = VocabularyWorkbenchC}<VocabularyWorkbench />{/if}
        {:else if app.view === "glossary"}
          {#if GlossaryViewC}{@const GlossaryView = GlossaryViewC}<GlossaryView />{/if}
        {:else if app.view === "mindmap"}
          <div class="workspace-scroll" style="padding: 24px;">
            <header style="display:flex;align-items:center;gap:12px;margin-bottom:12px;">
              <h1 class="read" style="font-size:var(--t-lg);font-weight:600;color:var(--fg-bright);margin:0;">知识脑图 · 全局</h1>
              <span class="mono faint" style="font-size:var(--t-xs);">{app.subjects.length} 个科目</span>
            </header>
            {#if globalMap}
              <MindMapView data={globalMap} />
            {/if}
          </div>
        {:else if app.view === "settings"}
          {#if SettingsView}{@const Settings = SettingsView}<Settings />{/if}
        {/if}
      </div>

      {#if showChatDock}
        <button class="chatdock-backdrop" aria-label="Close chat" onclick={() => (app.chatOpen = false)}></button>
        <div class="chatdock chatdock--float">
          {#if !compact}
            <ResizeHandle
              side="left"
              sign={-1}
              min={300}
              max={720}
              get={() => app.chatWidth}
              set={(v) => (app.chatWidth = v)}
              commit={() => app.saveChatWidth()}
            />
          {/if}
          <ChatPanel
            popout
            onClose={() => (app.chatOpen = false)}
            onFullscreen={() => { app.setView("subject"); app.subjectTab = "chats"; }}
          />
        </div>
      {/if}

      {#if showChatFab}
        <button class="chat-fab" onclick={() => (app.chatOpen = true)} title="Open chat (c)">
          Ask <span class="kbd">c</span>
        </button>
      {/if}
    </div>

    <StatusBar />

    <!-- overlays -->
    <FindBar />
    <CommandPalette />
    <GlobalSearch />
    <LeaderPane />
    <HelpOverlay />
    <DiffModal />
    <MusicPanel />
    <SourceMetaModal />
    <Dialog />
    <EditModal />
    <SubjectPanel />
    <NotificationCenter />
    <NotificationDetail />
    <PomodoroPanel />
    <LiveActivity />
    <RecordingActivity />
    <ContextMenu />
    <ToastStack />
    <OcrModal />
  </div>
{/if}

<style>
  .boot {
    display: flex; align-items: center; justify-content: center; height: 100vh;
    color: var(--fg-bright); font-family: var(--font-mono); font-size: var(--t-md);
    background: var(--bg);
  }

  /* Floating button to reopen the sidebar when it's minimized. */
  .sb-expand {
    position: fixed; top: calc(var(--tb-h, 30px) + 10px); left: 10px; z-index: 40;
    width: 30px; height: 30px; display: inline-flex; align-items: center; justify-content: center;
    border-radius: 8px; border: 1px solid var(--border); background: var(--surface);
    color: var(--fg-muted); cursor: pointer; transition: color var(--dur), border-color var(--dur);
  }
  .sb-expand:hover { color: var(--fg-bright); border-color: var(--border-strong); }

  /* Startup self-check banner. Anchored to the top of the main column so it
     never overlaps the sidebar, and dismissible in one click. */
  .selfcheck {
    position: absolute; top: 10px; left: 12px; z-index: 35;
    width: min(560px, calc(100% - 24px));
    background: var(--surface); color: var(--fg);
    border: 1px solid color-mix(in oklab, var(--warn) 45%, transparent);
    border-radius: 8px; overflow: hidden;
    box-shadow: 0 8px 28px color-mix(in oklab, var(--bg-sunken) 55%, transparent);
    font-size: var(--t-sm);
  }
  .sc-head {
    display: flex; align-items: center; gap: 8px; width: 100%;
    padding: 9px 34px 9px 12px; border: 0; background: transparent;
    color: var(--fg-bright); cursor: pointer; text-align: left;
  }
  .sc-head:hover { background: color-mix(in oklab, var(--warn) 10%, transparent); }
  .sc-head > :global(svg) { color: var(--warn); flex: none; }
  .sc-title { font-weight: 600; }
  .sc-spacer { flex: 1; }
  .sc-toggle { color: var(--fg-muted); font-size: var(--t-2xs); }
  .sc-close {
    position: absolute; top: 6px; right: 6px;
    width: 24px; height: 24px; display: inline-flex; align-items: center; justify-content: center;
    border: 0; border-radius: 6px; background: transparent; color: var(--fg-muted); cursor: pointer;
  }
  .sc-close:hover { color: var(--fg-bright); background: color-mix(in oklab, var(--fg) 12%, transparent); }
  .sc-list { list-style: none; margin: 0; padding: 4px 12px 10px; display: flex; flex-direction: column; gap: 8px; }
  .sc-item { display: flex; flex-direction: column; gap: 2px; padding-left: 22px; position: relative; }
  .sc-item::before {
    content: ""; position: absolute; left: 6px; top: 6px;
    width: 6px; height: 6px; border-radius: 50%; background: var(--warn);
  }
  .sc-name { font-family: var(--font-mono); font-size: var(--t-2xs); color: var(--fg-muted); }
  .sc-detail { color: var(--fg-bright); }
  .sc-hint { color: var(--fg-muted); font-size: var(--t-2xs); }
</style>
