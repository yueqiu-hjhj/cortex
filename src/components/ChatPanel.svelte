<script lang="ts">
  import { app } from "../lib/store.svelte";
  import { isMobile } from "../lib/platform";
  import { safeUrl, safeImgSrc } from "../lib/url";
  import Icon from "./Icon.svelte";
  import RichText from "./RichText.svelte";
  import ModelSearch from "./ModelSearch.svelte";
  import type { Source } from "../lib/api";
  import * as api from "../lib/api";
  import { loadOpenRouterModels, type OrModel } from "../lib/openrouter";

  // `popout` = the floating overlay chat (Ask-c / source viewer). The model picker
  // is hidden there to keep the compact header clean; it stays in the full-screen
  // chat (Chats tab + mobile full sheet).
  let { compact = false, popout = false, onClose, onFullscreen }:
    { compact?: boolean; popout?: boolean; onClose?: () => void; onFullscreen?: () => void } = $props();

  // ── scope state ──────────────────────────────────────────────────────────
  type Level = "subject" | "topic" | "source" | "sources" | "none";

  interface ChatMessage {
    role: "system" | "user" | "assistant";
    text: string;
    citations?: api.Citation[];
    images?: api.WebImage[];
    /** Persist time (ms) — also the address citesnap uses to re-fetch this
     *  message's frozen citation snapshots (0078). Absent on optimistic
     *  local bubbles (no citations to verify anyway). */
    created_at?: number;
    /** Terminal generation state (0077) — how this answer ended. */
    answer_state?: api.AnswerState;
    /** Persisted retrieval accounting (0044) — drives the red 未检索 badge. */
    retrieval?: api.RetrievalMeta;
    /** Fabricated citations citecheck stripped (0069) — the badge that says
     *  "the model tried to cite sources that were never retrieved". */
    fabricated_dropped?: number;
  }

  // Web mode: let the AI pull live web snippets + images (diagrams/examples).
  let webOn = $state(false);

  let level = $state<Level>("subject");
  let srcId = $state<string | null>(null);
  let topicId = $state<string | null>(null);
  // Multi-source scope: which specific sources to chat with (level === "sources").
  let picked = $state<Record<string, boolean>>({});
  const pickedIds = $derived(Object.keys(picked).filter((k) => picked[k]));
  // Session retrieval rate: how many assistant answers actually consulted the
  // library (hits > 0). Answers written before 0044 have no retrieval → not
  // counted (no retroactive guessing).
  const retrievalStats = $derived.by(() => {
    const asst = messages.filter((m) => m.role === "assistant" && m.retrieval);
    const grounded = asst.filter((m) => (m.retrieval?.hits ?? 0) > 0).length;
    return { total: asst.length, grounded };
  });
  let messages = $state<ChatMessage[]>([]);
  // Per-message evidence-panel expansion. Default collapsed so the chat stays
  // scannable; clicking the Evidence header opens the snippets.
  let evidenceOpen = $state<Record<number, boolean>>({});
  // Frozen-quote verification per message index (0078): when the evidence
  // panel opens, re-check the answer's citation snapshots against the
  // sources' current text. Lazy — history rows cost nothing until opened.
  let snapChecks = $state<Record<number, (api.QuoteCheck | null)[]>>({});
  function toggleEvidence(i: number) {
    evidenceOpen = { ...evidenceOpen, [i]: !evidenceOpen[i] };
    const m = messages[i];
    if (evidenceOpen[i] && m && m.created_at != null && snapChecks[i] === undefined && (m.citations ?? []).some((c) => c.quote)) {
      api
        .verifyCitationSnapshots(app.activeSubjectId ?? "", m.created_at)
        .then((checks) => { snapChecks = { ...snapChecks, [i]: checks }; })
        .catch(() => {});
    }
  }
  // Repairing a drifted snapshot (citesnap): re-anchor the frozen quote to the
  // source's CURRENT text. The backend keeps the original quote on the
  // citation, and we re-verify from the database afterwards instead of
  // assuming success — a "fixed" badge that isn't actually fixed would be
  // worse than the drift it replaced.
  let refreezing = $state<Record<string, boolean>>({});
  async function refreezeOne(i: number, c: api.Citation) {
    const m = messages[i];
    const subjectId = app.activeSubjectId ?? "";
    const key = `${i}:${c.chunk_id}`;
    if (!m?.created_at || !subjectId || refreezing[key]) return;
    refreezing = { ...refreezing, [key]: true };
    try {
      const out = await api.refreezeCitation(subjectId, m.created_at, c.chunk_id);
      if (!out.citation) {
        const why = out.reanchor.kind === "unanchorable" ? out.reanchor.reason : "";
        app.pushToast({
          kind: "warning",
          title: "这条引用无法重锚",
          body:
            why === "source_gone"
              ? "来源已被删除，没有可对照的现文。"
              : why === "text_gone"
                ? "原引文和它所在的分块都已在现文中消失——照着相似段落重锚就是凭空造证据，所以不做。"
                : "这条引用没有冻结快照（0078 之前的回答），无从重锚。",
        });
        return;
      }
      const fixed = out.citation;
      const oldChunk = c.chunk_id;
      messages = messages.map((msg, mi) =>
        mi === i
          ? { ...msg, citations: (msg.citations ?? []).map((x) => (x.chunk_id === oldChunk ? fixed : x)) }
          : msg,
      );
      const checks = await api
        .verifyCitationSnapshots(subjectId, m.created_at)
        .catch(() => null);
      if (checks) snapChecks = { ...snapChecks, [i]: checks };
      app.pushToast({
        kind: "success",
        title: out.reanchor.kind === "relocated" ? "已按现文重定位这条引用" : "已按现文重新冻结这条引用",
        body: "原引文保留在这条引用的快照里，随时可查。",
      });
    } catch (e) {
      app.pushToast({ kind: "error", title: "重锚失败", body: String(e) });
    } finally {
      refreezing = { ...refreezing, [key]: false };
    }
  }
  let draft = $state("");
  // Streaming + suggestions now live in the store (app.sendChat) so a reply keeps
  // going even if this panel unmounts. We only surface the live text when it's for
  // the subject this panel is showing.
  const streaming = $derived(app.chatGenSubject === app.activeSubjectId ? app.chatStreaming : null);
  const suggestions = $derived(app.chatGenSubject === null ? app.chatSuggestions : []);
  let queued = $state<string[]>([]); // messages sent while a response is streaming

  // In-chat model picker (writes the same model_chat setting Settings uses). Mirrors
  // the Settings picker: a searchable ModelSearch over the live OpenRouter catalog
  // (price/context per row), with a few non-OpenRouter quick picks. The stored value
  // is always a "provider:model" spec.
  const CHAT_FALLBACK = [
    { id: "gemini:gemini-2.5-flash", label: "Gemini 2.5 Flash" },
    { id: "gemini:gemini-2.5-pro", label: "Gemini 2.5 Pro" },
    { id: "openai:gpt-4o-mini", label: "GPT-4o mini" },
    { id: "claude:claude-3-5-sonnet-20241022", label: "Claude 3.5 Sonnet" },
  ];
  let chatModel = $state("");
  let chatModelLoaded = false;
  $effect(() => {
    if (chatModelLoaded) return;
    chatModelLoaded = true;
    api.getSetting("model_chat").then((v) => { if (v) chatModel = v; }).catch(() => {});
  });

  // Live OpenRouter catalog — fetched once, the first time the picker is opened.
  let orModels = $state<OrModel[]>([]);
  let orLoading = $state(false);
  let orTried = false;
  function ensureOrModels() {
    if (orTried) return;
    orTried = true;
    orLoading = true;
    loadOpenRouterModels()
      .then((m) => { orModels = m; })
      .catch(() => {})
      .finally(() => { orLoading = false; });
  }

  const modelOptions = $derived.by(() => {
    const opts: { id: string; label: string; sub?: string; recommended?: boolean }[] = [
      ...orModels.map((m) => ({
        id: "openrouter:" + m.id, label: m.label, sub: m.sub, recommended: m.recommended,
      })),
      ...CHAT_FALLBACK,
    ];
    // Always show the current selection, even if it isn't in either list.
    if (chatModel && !opts.some((o) => o.id === chatModel)) {
      const label = chatModel.includes(":") ? chatModel.split(":").slice(1).join(":") : chatModel;
      opts.unshift({ id: chatModel, label });
    }
    return opts;
  });

  function setChatModel(spec: string) {
    chatModel = spec;
    api.setSetting("model_chat", spec).catch(() => {});
  }

  // ── persisted chat history: load the subject's conversation on open ────────
  let loadedSubject: string | null = null;
  $effect(() => {
    const sid = app.activeSubjectId;
    if (sid === loadedSubject) return;
    loadedSubject = sid;
    messages = [];
    queued = [];
    if (!sid) return;
    api.listChatMessages(sid)
      .then((ms) => {
        if (app.activeSubjectId === sid)
          messages = ms.map((m) => ({
            role: m.role as ChatMessage["role"],
            text: m.text,
            citations: m.citations ?? [],
            retrieval: m.retrieval ?? undefined,
            fabricated_dropped: m.fabricated_dropped ?? undefined,
            created_at: m.created_at,
            answer_state: m.answer_state ?? undefined,
          }));
      })
      .catch(() => {});
  });

  // When the store persists a freshly-generated reply it bumps chatMsgNonce; pull
  // the updated history in so the assistant message lands (this is what lets a
  // reply that finished while the panel was closed appear on reopen). Web-mode
  // images aren't persisted, so re-attach the last reply's images to the tail.
  let seenMsgNonce = app.chatMsgNonce;
  $effect(() => {
    const n = app.chatMsgNonce;
    if (n === seenMsgNonce) return;
    seenMsgNonce = n;
    const sid = app.activeSubjectId;
    if (!sid) return;
    api.listChatMessages(sid)
      .then((ms) => {
        if (app.activeSubjectId !== sid) return;
        const next = ms.map((m) => ({
          role: m.role as ChatMessage["role"],
          text: m.text,
          citations: m.citations ?? [],
          retrieval: m.retrieval ?? undefined,
          fabricated_dropped: m.fabricated_dropped ?? undefined,
          created_at: m.created_at,
          answer_state: m.answer_state ?? undefined,
        })) as ChatMessage[];
        const imgs = app.chatLastImages;
        if (imgs.length && next.length && next[next.length - 1].role === "assistant") {
          next[next.length - 1] = { ...next[next.length - 1], images: imgs };
          app.chatLastImages = [];
        }
        messages = next;
      })
      .catch(() => {});
  });

  // Once the store finishes a reply (chatBusy falls), fire the next queued message.
  let wasBusy = false;
  $effect(() => {
    const busy = app.chatBusy;
    if (wasBusy && !busy) dequeue();
    wasBusy = busy;
  });

  // Jump-to-message (引用回流 / 问题索引 set app.chatJumpTo): reload the
  // current thread, scroll the target bubble into view and flash it. The
  // signal is consumed on read so jumping to the same message twice works.
  $effect(() => {
    const target = app.chatJumpTo;
    if (target === null) return;
    app.chatJumpTo = null;
    const sid = app.activeSubjectId;
    if (!sid) return;
    api.listChatMessages(sid)
      .then((ms) => {
        if (app.activeSubjectId !== sid) return;
        messages = ms.map((m) => ({
          role: m.role as ChatMessage["role"],
          text: m.text,
          citations: m.citations ?? [],
          retrieval: m.retrieval ?? undefined,
          fabricated_dropped: m.fabricated_dropped ?? undefined,
          created_at: m.created_at,
          answer_state: m.answer_state ?? undefined,
        }));
        requestAnimationFrame(() => {
          const el = document.querySelector(`[data-msg-at="${target}"]`);
          if (el) {
            el.scrollIntoView({ block: "center", behavior: "smooth" });
            el.classList.add("bubble-flash");
            setTimeout(() => el.classList.remove("bubble-flash"), 1600);
          }
        });
      })
      .catch(() => {});
  });
  let scrollEl = $state<HTMLElement | null>(null);
  let composeEl = $state<HTMLTextAreaElement | null>(null);

  // ── source-switcher overlay ────────────────────────────────────────────────
  let switcherOpen = $state(false);
  let switcherSel = $state(0); // highlighted row in the flat option list

  // All sources for the active subject, flattened across topics
  const topicSources = $derived<Source[]>(
    app.activeSubject?.topics?.flatMap((t) => t.sources) ?? []
  );

  // Resolve the active source object
  const curSrcObj = $derived<Source | null>(
    topicSources.find((s) => s.id === srcId) ?? topicSources[0] ?? null
  );

  // Resolve the active topic object (explicit topicId, else the source's topic, else first)
  const curTopic = $derived(
    app.activeSubject?.topics?.find((t) => t.id === topicId) ??
      app.activeSubject?.topics?.find((t) => t.id === curSrcObj?.topic_id) ??
      app.activeSubject?.topics?.[0] ??
      null
  );

  // Short display name for the source segment
  function shortName(name: string) {
    return name.replace(/\.[^.]+$/, "").replace(/^lecture-0?/, "lec-");
  }

  function openEvidence(citation: api.Citation) {
    const src = topicSources.find((s) => s.id === citation.source_id);
    if (!src) return;
    // snippet → PDF 模式的页定位线索（chunk loc 对 PDF 来源为空）
    app.openSource(src, citation.loc, citation.snippet);
    onClose?.();
  }

  // Effective level: downgrade gracefully when the chosen scope has no target
  // (e.g. "source" scope but the subject has no sources → fall back to topic/subject).
  const effLevel = $derived<Level>(
    level === "source" && !curSrcObj ? (curTopic ? "topic" : "subject")
      : level === "topic" && !curTopic ? "subject"
      : level
  );
  // Plain display name of the current scope (no "Source:" prefix), used in the
  // breadcrumb selector and the empty state.
  const scopeName = $derived(
    level === "sources" && pickedIds.length
      ? `${pickedIds.length} selected source${pickedIds.length === 1 ? "" : "s"}`
      : effLevel === "source" ? (curSrcObj?.name ?? "")
      : effLevel === "topic" ? (curTopic?.name ?? "")
      : (app.activeSubject?.name ?? "")
  );

  // ── status-bar PWD sync ─────────────────────────────────────────────────────
  // Keep app.chatScope in lock-step with the chat's scope so the status bar
  // reflects subject › topic › source.
  $effect(() => {
    if (!app.activeSubject) {
      app.chatScope = null;
      return;
    }
    if (effLevel === "source" && curSrcObj) {
      app.chatScope = { topicName: curTopic?.name, sourceName: curSrcObj.name };
    } else if (effLevel === "topic") {
      app.chatScope = { topicName: curTopic?.name };
    } else {
      app.chatScope = null; // whole-subject
    }
  });

  // ── flat option list for the switcher (subject / topics / sources) ──────────
  type ScopeOption =
    | { kind: "subject"; label: string }
    | { kind: "tag"; label: string; tag: string }
    | { kind: "topic"; label: string; topicId: string }
    | { kind: "source"; label: string; src: Source }
    | { kind: "none" };

  // Distinct tags across the subject's topics — chat scoped to a tag covers every
  // source under topics carrying it (e.g. tag "A2" → all your A2 exam material).
  const subjectTags = $derived.by(() => {
    const set = new Set<string>();
    for (const t of app.activeSubject?.topics ?? []) for (const tag of t.tags ?? []) set.add(tag);
    return [...set].sort((a, b) => a.localeCompare(b));
  });

  const switcherOptions = $derived.by<ScopeOption[]>(() => {
    const out: ScopeOption[] = [
      { kind: "subject", label: app.activeSubject?.name ?? "Whole subject" },
    ];
    for (const tag of subjectTags) out.push({ kind: "tag", label: `#${tag}`, tag });
    for (const t of app.activeSubject?.topics ?? []) {
      out.push({ kind: "topic", label: t.name, topicId: t.id });
      for (const s of t.sources) {
        out.push({ kind: "source", label: s.name, src: s });
      }
    }
    // Explicit third state (Gloss): answer from the model alone.
    out.push({ kind: "none" });
    return out;
  });

  // Index of the option matching the current scope (for highlight on open)
  const currentOptionIndex = $derived.by(() => {
    const opts = switcherOptions;
    if (level === "subject") return 0;
    if (level === "source" && curSrcObj)
      return opts.findIndex((o) => o.kind === "source" && o.src.id === curSrcObj.id);
    if (level === "topic" && curTopic)
      return opts.findIndex((o) => o.kind === "topic" && o.topicId === curTopic.id);
    if (level === "none") return opts.length - 1;
    return 0;
  });


  // ── source-switcher overlay actions ─────────────────────────────────────────
  function openSwitcher() {
    if (!app.activeSubject) return;
    switcherSel = Math.max(0, currentOptionIndex);
    switcherOpen = true;
  }

  function applyOption(o: ScopeOption) {
    // Silent scope change — no system message in the thread.
    picked = {}; // single-select clears any multi-source pick
    if (o.kind === "none") {
      level = "none";
    } else if (o.kind === "subject") {
      level = "subject";
    } else if (o.kind === "topic") {
      topicId = o.topicId;
      level = "topic";
    } else if (o.kind === "tag") {
      applyTag(o.tag);
      return;
    } else {
      srcId = o.src.id;
      topicId = o.src.topic_id ?? topicId;
      level = "source";
    }
    switcherOpen = false;
    // Return focus to the composer so typing keeps working.
    composeEl?.focus();
  }

  // Toggle a source into/out of the multi-source selection (checkbox in the switcher).
  function togglePick(id: string, e: Event) {
    e.stopPropagation();
    picked = { ...picked, [id]: !picked[id] };
  }
  // Chat scoped to a tag: select every source under topics carrying that tag and
  // switch to the multi-source scope.
  function applyTag(tag: string) {
    const ids: Record<string, boolean> = {};
    for (const t of app.activeSubject?.topics ?? []) {
      if ((t.tags ?? []).includes(tag)) for (const s of t.sources) ids[s.id] = true;
    }
    if (Object.keys(ids).length === 0) {
      app.pushToast({ kind: "warning", title: `No sources tagged #${tag}` });
      return;
    }
    picked = ids;
    level = "sources";
    switcherOpen = false;
    composeEl?.focus();
  }

  // Clicking a switcher row: source rows TOGGLE the multi-pick (so selections
  // persist and accumulate); tag rows select the whole tag; subject/topic rows
  // switch scope immediately.
  function onSwitcherRow(o: ScopeOption) {
    if (o.kind === "source") {
      picked = { ...picked, [o.src.id]: !picked[o.src.id] };
    } else if (o.kind === "tag") {
      applyTag(o.tag);
    } else {
      applyOption(o);
    }
  }
  // Confirm the multi-source selection as the active scope.
  function applyMulti() {
    if (pickedIds.length === 0) return;
    level = "sources";
    switcherOpen = false;
    composeEl?.focus();
  }

  // Focus the overlay so ArrowUp/Down/Enter/Esc are captured immediately.
  function autofocus(node: HTMLElement) {
    node.focus();
  }

  function switcherKey(e: KeyboardEvent) {
    const opts = switcherOptions;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      switcherSel = (switcherSel + 1) % opts.length;
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      switcherSel = (switcherSel - 1 + opts.length) % opts.length;
    } else if (e.key === "Enter") {
      e.preventDefault();
      const o = opts[switcherSel];
      if (o) onSwitcherRow(o);
    } else if (e.key === "Escape") {
      e.preventDefault();
      switcherOpen = false;
      composeEl?.focus();
    }
  }

  // ── streaming send (with queue + stop) ─────────────────────────────────────
  // Generation itself lives in the store (app.sendChat), so closing this panel
  // mid-answer no longer kills the reply. Here we just record the user turn and
  // hand off; the store persists the reply and bumps chatMsgNonce, which our
  // reload effect picks up.
  function send(textArg?: string) {
    const text = (textArg ?? draft).trim();
    if (!text || !app.activeSubject) return;
    // While a response is in-flight, queue the message instead of dropping it.
    if (app.chatBusy) {
      queued = [...queued, text];
      if (textArg === undefined) draft = "";
      return;
    }
    if (textArg === undefined) draft = "";
    const sid = app.activeSubject.id;
    messages = [...messages, { role: "user", text }];
    api.addChatMessage(sid, "user", text).catch(() => {}); // persist

    const multi = level === "sources" && pickedIds.length > 0;
    const sendLevel: "subject" | "topic" | "source" | "none" =
      multi || effLevel === "sources"
        ? "subject"
        : effLevel === "none"
          ? "none"
          : effLevel === "topic"
            ? "topic"
            : effLevel === "source"
              ? "source"
              : "subject";
    const sourceId = !multi && sendLevel === "source" && curSrcObj ? curSrcObj.id : undefined;
    // Fire-and-forget: the store keeps running even if we unmount.
    app.sendChat({ subjectId: sid, level: sendLevel, query: text, sourceId, sourceIds: multi ? pickedIds : undefined, web: webOn });
  }

  // ── chat sessions / history ────────────────────────────────────────────
  let historyOpen = $state(false);
  let threads = $state<api.ThreadInfo[]>([]);

  async function startNewConversation() {
    const sid = app.activeSubject?.id;
    if (!sid) return;
    await api.newChat(sid).catch(() => {});
    messages = [];
    app.chatSuggestions = [];
    queued = [];
    composeEl?.focus();
  }

  async function openHistory() {
    const sid = app.activeSubject?.id;
    if (!sid) return;
    threads = await api.listChatThreads(sid).catch(() => [] as api.ThreadInfo[]);
    historyOpen = true;
  }

  async function pickThread(id: string) {
    const sid = app.activeSubject?.id;
    if (!sid) return;
    await api.openChatThread(sid, id).catch(() => {});
    const ms = await api.listChatMessages(sid).catch(() => []);
    messages = ms.map((m) => ({
      role: m.role as ChatMessage["role"],
      text: m.text,
      citations: m.citations ?? [],
      created_at: m.created_at,
      answer_state: m.answer_state ?? undefined,
    }));
    app.chatSuggestions = [];
    historyOpen = false;
  }

  // 问题索引（KnowNote borrow）：本科目问过的每个问题，连同配对的回答的
  // provenance（终止态 / 检索命中 / 剥离的伪造引用）。点一条跳回那段对话。
  let histTab = $state<"threads" | "questions">("threads");
  let questions = $state<api.QuestionEntry[]>([]);
  let questionsBusy = $state(false);
  async function loadQuestions() {
    const sid = app.activeSubjectId;
    if (!sid) {
      questions = [];
      return;
    }
    questionsBusy = true;
    try {
      questions = await api.questionIndex(sid);
    } catch {
      questions = [];
    } finally {
      questionsBusy = false;
    }
  }
  function showQuestions() {
    histTab = "questions";
    if (questions.length === 0 && !questionsBusy) void loadQuestions();
  }
  async function jumpToQuestion(q: api.QuestionEntry) {
    const sid = app.activeSubjectId;
    if (!sid) return;
    await api.openChatThread(sid, q.thread_id).catch(() => {});
    historyOpen = false;
    app.jumpToChatMessage(q.created_at);
  }

  // Send the next queued message (if any) once the current one finishes.
  function dequeue() {
    if (queued.length === 0 || app.chatBusy) return;
    const [next, ...rest] = queued;
    queued = rest;
    send(next);
  }

  // Stop the current generation, keeping whatever has streamed so far. The store
  // persists the partial reply and bumps chatMsgNonce; our reload effect shows it.
  function stop() {
    app.stopChat();
  }

  // ── keep an answer: the whole reply, or just the part you select ───────────
  // Both end in the same picker (SelectionClip): pick a note, and the passage
  // lands under its 「## 摘录」 heading, attributed. The old behaviour — silently
  // creating a note titled "the first six words" — is gone: it produced notes
  // nobody could find again, and it could not keep a single sentence.
  /** "provider:model" → the model part; that is what a reader recognizes. */
  function modelLabel(): string {
    const m = chatModel.trim();
    if (!m) return "chat";
    return m.includes(":") ? m.split(":").slice(1).join(":") : m;
  }

  function clipReply(text: string) {
    app.openClipPicker({
      quote: text,
      sourceKind: "chat",
      sourceLabel: `AI 回答 · ${modelLabel()}`,
    });
  }

  function handleKey(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
      return;
    }
    // Esc leaves chat *insert* mode (→ normal), staying in the chat. It must NOT
    // bubble to the global key handler, which would navigate back a page.
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      (e.target as HTMLElement | null)?.blur();
      app.setMode("NOR");
      return;
    }
    // When the composer is EMPTY, "c" closes the chat (the same key opens it);
    // with text typed, "c" types normally.
    if (!draft.trim() && !e.metaKey && !e.ctrlKey && !e.altKey && e.key === "c") {
      e.preventDefault();
      (e.target as HTMLElement | null)?.blur();
      app.setMode("NOR");
      if (onClose) onClose();
      else app.chatOpen = false;
    }
  }

  // Root-level keybind for the whole panel: while the chat is focused and not
  // typing in the composer, "s" opens the source switcher.
  function panelKey(e: KeyboardEvent) {
    if (switcherOpen) return; // overlay owns the keys while open
    const typing =
      app.mode === "INS" ||
      (e.target instanceof HTMLElement &&
        (e.target.tagName === "TEXTAREA" || e.target.tagName === "INPUT"));
    if (e.key === "s" && !typing && !e.metaKey && !e.ctrlKey && !e.altKey) {
      e.preventDefault();
      openSwitcher();
    }
  }

  // ── auto-scroll (stick to bottom only when the user is already there, so
  // they can freely scroll up to read history mid-stream) ────────────────────
  let stick = $state(true);
  function onScroll() {
    if (!scrollEl) return;
    stick = scrollEl.scrollHeight - scrollEl.scrollTop - scrollEl.clientHeight < 120;
  }
  $effect(() => {
    const _ = messages.length;
    const __ = streaming;
    if (scrollEl && stick) scrollEl.scrollTop = scrollEl.scrollHeight;
  });

  // ── badge label map ───────────────────────────────────────────────────────
  const kindLabel: Record<string, string> = {
    pdf: "PDF",
    pptx: "PPTX",
    docx: "DOCX",
    web: "WEB",
    yt: "YT",
    audio: "AUD",
    image: "IMG",
  };
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<!-- Window-level so "s" works without the panel div holding focus (a plain div
     never receives keydown unless focused). panelKey ignores it while typing. -->
<svelte:window onkeydown={panelKey} />

<div class="chatdock-inner" class:is-full={!onClose}>
  <!-- ── header ─────────────────────────────────────────────────────────── -->
  <div class="chat-head" class:is-popout={popout}>
    {#if app.activeSubject}
      <!-- One clean clickable scope selector (opens the switcher). Shows the
           current scope path; no redundant chevrons or banner. -->
      <button class="scope-pick" type="button" title="Change scope (s)" onclick={openSwitcher}>
        <span class="sp-ico">{app.activeSubject.glyph || "◆"}</span>
        <span class="sp-name">{app.activeSubject.name}</span>
        {#if effLevel !== "subject" && curTopic}
          <span class="sp-sep">›</span>
          <span class="sp-name sp-dim">{curTopic.name}</span>
        {/if}
        {#if effLevel === "source" && curSrcObj}
          <span class="sp-sep">›</span>
          <span class="sp-name">{shortName(curSrcObj.name)}</span>
        {/if}
        <Icon name="chevron" size={11} style="transform:rotate(90deg);opacity:.55;margin-left:2px" />
      </button>
    {:else}
      <span class="faint" style="font-size:12px">未打开科目</span>
    {/if}

    <div class="grow"></div>
    {#if retrievalStats.total > 0}
      {@const pct = Math.round((retrievalStats.grounded / retrievalStats.total) * 100)}
      <span class="retr-stat" title="本会话回答中实际检索了资料库的比例">
        <svg viewBox="0 0 20 20" width="16" height="16" aria-hidden="true">
          <circle cx="10" cy="10" r="7.5" fill="none" stroke="var(--border)" stroke-width="2.5" />
          <circle cx="10" cy="10" r="7.5" fill="none"
                  stroke={pct >= 50 ? "var(--accent)" : "var(--err, var(--warn))"}
                  stroke-width="2.5"
                  stroke-dasharray={`${(pct / 100) * 47.1} 47.1`}
                  stroke-linecap="round"
                  transform="rotate(-90 10 10)" />
        </svg>
        <span class="mono">{retrievalStats.grounded}/{retrievalStats.total}</span>
      </span>
    {/if}
    {#if !popout}
      <div class="chat-model-pick" title="Chat model">
        <ModelSearch
          value={chatModel}
          onChange={setChatModel}
          options={modelOptions}
          icon="bolt"
          placeholder="模型"
          loading={orLoading && orModels.length === 0}
          onOpen={ensureOrModels}
        />
      </div>
    {/if}
    <button class="btn btn--icon btn--sm btn--ghost" title="Chat history" onclick={openHistory}>
      <Icon name="book" size={13} />
    </button>
    <button class="btn btn--icon btn--sm btn--ghost" title="New conversation" onclick={startNewConversation}>
      <Icon name="plus" size={13} />
    </button>
    {#if onFullscreen}
      <button class="btn btn--icon btn--sm btn--ghost" onclick={onFullscreen} title="Fullscreen chat">
        <Icon name="external" size={12} />
      </button>
    {/if}
    {#if onClose}
      <button class="btn btn--icon btn--sm btn--ghost" onclick={onClose} title="Close chat">
        <Icon name="x" size={12} />
      </button>
    {/if}
  </div>

  {#if app.chatRollover}
    <div class="rollover-note" role="status">
      <span class="rn-ico"><Icon name="chat" size={12} /></span>
      <span class="rn-body">
        {#if app.chatRollover.reason === "prompt"}
          这一轮的上下文（历史 + 检索资料）已达上下文预算，已自动总结并<strong>开启新对话</strong>续上——上文摘要已继承，可以直接接着说。
        {:else}
          对话已达上下文预算，已自动总结并<strong>开启新对话</strong>续上——上文摘要已继承，可以直接接着说。
        {/if}
        <details>
          <summary>查看摘要</summary>
          <p>{app.chatRollover.summary}</p>
        </details>
      </span>
      <button class="btn btn--icon btn--sm btn--ghost" title="知道了" onclick={() => (app.chatRollover = null)}>
        <Icon name="x" size={12} />
      </button>
    </div>
  {/if}

  <!-- ── message list ───────────────────────────────────────────────────── -->
  <div class="chat-scroll" bind:this={scrollEl} onscroll={onScroll}>
    {#if !app.activeSubject}
      <div class="chat-empty-state">
        <div class="ces-ico">
          <Icon name="diamond" size={22} color="var(--fg3)" />
        </div>
        <div class="ces-title">打开一个科目开始对话</div>
        <div class="ces-sub">基于你的资料提问。</div>
      </div>
    {:else}
      {#if messages.length === 0 && streaming === null}
        <div class="chat-empty-state" style="min-height:48vh;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:8px;text-align:center;">
          <div class="ces-ico"><Icon name="chat" size={22} color="var(--fg3)" /></div>
          <div class="ces-title">Ask anything about {scopeName}</div>
          <div class="ces-sub">按 <span class="kbd">i</span> 开始 · <span class="kbd">s</span> 切换范围</div>
        </div>
      {/if}

      {#each messages as m, i (i)}
        {#if m.role === "system"}
          <div class="bubble system">— {m.text} —</div>
        {:else if m.role === "user"}
          <div class="bubble user" data-msg-at={m.created_at}>{m.text}</div>
        {:else}
          {@const cites = m.citations ?? []}
          {@const citedCount = cites.filter((c) => c.cited).length}
          {@const isUncited = cites.length > 0 && citedCount === 0}
          {@const noRetrieval = !!m.retrieval && m.retrieval.hits === 0}
          {@const fabricated = m.fabricated_dropped ?? 0}
          {@const state = m.answer_state}
          <div
            class="bubble assistant"
            class:is-uncited={isUncited}
            class:is-unretrieved={noRetrieval}
            data-msg-at={m.created_at}
            data-clip-scope="chat"
            data-clip-label={`AI 回答 · ${modelLabel()}`}
          >
            {#if noRetrieval}
              <span class="retr-badge">未检索 · 零命中</span>
            {/if}
            {#if state === "truncated"}
              <span
                class="retr-badge state-truncated"
                title="生成达到了输出上限，回答在中间被截断——保留的部分仍然可用。可以直接追问「继续」让模型接着写。"
                >已截断 · 可追问「继续」</span
              >
            {:else if state === "blocked"}
              <span
                class="retr-badge state-blocked"
                title="模型的安全/内容策略在这轮生成中途叫停（如 SAFETY、content_filter）。换一种问法通常可以继续。"
                >被安全策略拦截</span
              >
            {:else if state === "failed"}
              <span class="retr-badge state-failed" title="模型没有返回任何可用内容（finishReason 见会话记录）。重试或换一个模型通常能解决。"
                >模型无输出</span
              >
            {:else if state === "aborted"}
              <span class="retr-badge state-aborted" title="你按下了停止，这里保留的是中断前已经生成的部分。">已中止</span>
            {/if}
            {#if (m.retrieval?.history_turns ?? 0) > 0}
              {@const historyTurns = m.retrieval?.history_turns ?? 0}
              <span
                class="retr-badge state-history"
                title="本轮的 prompt 带入了此前 {historyTurns} 个问题及回答作为上下文——「它 / 刚才 / 那个」因此能贯穿整个对话，而不只限于单轮。"
                >上文 · {historyTurns} 轮</span
              >
            {/if}
            {#if m.retrieval?.intent === "synthesis"}
              <span
                class="retr-badge state-intent"
                title="这个问题听起来要「整体脉络」（总结、对比、框架），所以上下文预算多分给了主题/材料摘要，原始片段相应减少——回答的视角因此偏概览。"
                >概览视角</span
              >
            {:else if m.retrieval?.intent === "pinpoint"}
              <span
                class="retr-badge state-intent"
                title="这个问题要的是具体位置（定义、公式、为什么），所以上下文预算多分给了原文片段，摘要层相应压缩——回答的视角因此偏精读。"
                >精读视角</span
              >
            {/if}
            <RichText text={m.text} />
            {#if cites.length}
              <div class="evidence">
                <button
                  type="button"
                  class="evidence-head"
                  class:is-uncited={isUncited}
                  onclick={() => toggleEvidence(i)}
                  aria-expanded={!!evidenceOpen[i]}
                >
                  <Icon name={evidenceOpen[i] ? "x" : "book"} size={11} />
                  <span class="mono">证据</span>
                  <span class="evidence-count mono">{citedCount}/{cites.length} cited</span>
                  {#if isUncited}
                    <span class="evidence-warning">没有行内引用</span>
                  {/if}
                  {#if fabricated > 0}
                    <span
                      class="evidence-fabricated"
                      title="citecheck 机械校验：回答里引用了检索结果中不存在的来源，已就地剥掉。数字越大越说明这轮回答在凭空编引用。"
                      >剥离伪造引用 {fabricated} 条</span
                    >
                  {/if}
                  <span class="evidence-caret">{evidenceOpen[i] ? "▾" : "▸"}</span>
                </button>
                {#if evidenceOpen[i]}
                  <div class="evidence-body">
                    <div class="evidence-list">
                      {#each cites as c, ci (c.chunk_id)}
                        {@const snap = snapChecks[i]?.[ci] ?? null}
                        <div class="ec-cell">
                          <button
                            type="button"
                            class="evidence-chip"
                            class:is-cited={c.cited}
                            class:is-uncited-chip={!c.cited}
                            title={`${c.source_name}${c.loc ? " · " + c.loc : ""}${
                        c.char_start != null && c.char_end != null
                          ? ` · chars ${c.char_start}-${c.char_end}`
                          : ""
                      }\n${c.snippet}`}
                            onclick={() => openEvidence(c)}
                          >
                            <Icon name={c.cited ? "book" : "warn"} size={11} />
                            <span class="ec-source">{c.source_name}</span>
                            {#if c.loc}<span class="ec-loc">{c.loc}</span>{/if}
                            {#if c.reanchored_at != null}
                              <span
                                class="ec-snap ec-snap-reanchored"
                                title={`这条引文已按来源现文重新冻结（${new Date(c.reanchored_at).toLocaleString()}）。原引文保留：${(c.superseded_quote ?? "").slice(0, 200)}`}
                                >✎ 已重锚</span
                              >
                            {:else if snap && snap.kind === "moved"}
                              <span
                                class="ec-snap ec-snap-moved"
                                title="快照明文仍在源文里，但位置随重索引移动了——引用依然成立"
                                >⟲ 已移位</span
                              >
                            {:else if snap && snap.kind === "drifted"}
                              <span
                                class="ec-snap ec-snap-drift"
                                title="这条引用冻结时的原文，已经不在来源的当前文本里了（来源被改过或重新提取过）——证据待人工核对"
                                >⚠ 与现文不符</span
                              >
                            {:else if snap && snap.kind === "source_gone"}
                              <span class="ec-snap ec-snap-drift" title="来源已被删除，无从核对">⚠ 来源已删</span>
                            {/if}
                            {#if c.score != null}<span class="ec-score mono">{Math.round(c.score * 100)}%</span>{/if}
                          </button>
                          {#if snap && snap.kind === "drifted"}
                            <button
                              type="button"
                              class="ec-fix"
                              disabled={!!refreezing[`${i}:${c.chunk_id}`]}
                              title="用来源现在的文本重新冻结这条引文（来源被改写或重新提取过时用）。原引文会保留，重锚留下痕迹。"
                              onclick={() => refreezeOne(i, c)}
                              >{refreezing[`${i}:${c.chunk_id}`] ? "…" : "重锚"}</button
                            >
                          {/if}
                        </div>
                      {/each}
                    </div>
                    <div class="evidence-trace mono">
                      retrieved {cites.length} chunk{cites.length === 1 ? "" : "s"}
                      from {new Set(cites.map((c) => c.source_id)).size} source{new Set(cites.map((c) => c.source_id)).size === 1 ? "" : "s"}
                      · {citedCount} cited inline
                      {#if isUncited}· <span class="trace-warn">回答未关联任何来源</span>{/if}
                      {#if fabricated > 0}· <span class="trace-warn">剥离伪造引用 {fabricated} 条</span>{/if}
                    </div>
                  </div>
                {/if}
              </div>
            {/if}
            {#if m.images && m.images.length}
              <div class="chat-images">
                {#each m.images as img (img.img)}
                  <a class="chat-img" href={safeUrl(img.source || img.img)} target="_blank" rel="noreferrer" title={img.title}>
                    <img src={safeImgSrc(img.thumb)} alt={img.title} loading="lazy" />
                  </a>
                {/each}
              </div>
            {/if}
            <div class="bubble-actions">
              <button
                type="button"
                class="msg-action"
                title="摘到笔记 —— 也可以先选中回答里的一段"
                aria-label="摘到笔记"
                onclick={() => clipReply(m.text)}
              >
                <Icon name="plus" size={12} />
              </button>
            </div>
          </div>
        {/if}
      {/each}

      {#if streaming !== null}
        <div class="bubble assistant" data-clip-scope="chat" data-clip-label={`AI 回答 · ${modelLabel()}`}>
          <RichText text={streaming} /><span class="cursor-blink">▋</span>
        </div>
      {/if}
    {/if}
  </div>

  <!-- ── compose ───────────────────────────────────────────────────────── -->
  <div class="chat-compose">
    {#if suggestions.length && streaming === null}
      <div class="chat-suggest">
        <span class="cs-label mono">接下来</span>
        {#each suggestions as s}
          <button type="button" class="suggest-chip" title={s} onclick={() => send(s)}>{s}</button>
        {/each}
      </div>
    {/if}
    {#if queued.length}
      <div class="chat-queued mono faint">{queued.length} message{queued.length === 1 ? "" : "s"} queued…</div>
    {/if}
    <div class="compose-box{app.mode === 'INS' ? ' is-insert' : ''}">
      <textarea
        bind:this={composeEl}
        rows={1}
        placeholder={!app.activeSubject
          ? "Open a subject first…"
          : app.mode === "INS"
          ? "Ask about " + scopeName + "…"
          : isMobile ? "Ask…" : "Press i to ask…"}
        bind:value={draft}
        disabled={!app.activeSubject}
        onfocus={() => app.setMode("INS")}
        onblur={() => app.setMode("NOR")}
        onkeydown={handleKey}
      ></textarea>
      <button
        type="button"
        class="btn btn--icon btn--sm chat-web{webOn ? ' is-on' : ''}"
        onclick={() => (webOn = !webOn)}
        title={webOn ? "Web mode on — pulls live results + images (needs SearXNG)" : "Web mode off — answers from your sources only"}
        aria-pressed={webOn}
      >
        <Icon name="globe" size={13} />
      </button>
      {#if streaming !== null}
        <button class="btn btn--icon btn--sm chat-stop" onclick={stop} title="Stop generating">
          <span class="stop-sq"></span>
        </button>
      {:else}
        <button
          class="btn btn--icon btn--sm btn--primary"
          onclick={() => send()}
          disabled={!draft.trim() || !app.activeSubject}
          title="Send"
        >
          <Icon name="arrowR" size={13} />
        </button>
      {/if}
    </div>
    <div class="compose-hint">
      <span><span class="kbd">i</span> 输入</span>
      <span><span class="kbd">⏎</span> 发送</span>
      <span><span class="kbd">⎋</span> 普通模式</span>
      <span><span class="kbd">s</span> 范围</span>
    </div>
  </div>

  <!-- ── source / scope switcher overlay ──────────────────────────────────── -->
  {#if switcherOpen}
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="scopesw-overlay"
      onmousedown={() => { switcherOpen = false; composeEl?.focus(); }}
    >
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="scopesw"
        onmousedown={(e) => e.stopPropagation()}
        onkeydown={switcherKey}
        tabindex="-1"
        use:autofocus
      >
        <div class="scopesw-head">
          <span class="scopesw-title mono">切换对话范围</span>
          <span class="kbd">esc</span>
        </div>
        <div class="scopesw-list" role="listbox" aria-label="chat scope options">
          {#each switcherOptions as o, i (i)}
            {@const sel = i === switcherSel}
            {@const isCur = i === currentOptionIndex}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <div
              class="scopesw-item scopesw-item--{o.kind}{sel ? ' sel' : ''}"
              role="option"
              tabindex="-1"
              aria-selected={sel}
              onmouseenter={() => (switcherSel = i)}
              onclick={() => onSwitcherRow(o)}
            >
              {#if o.kind === "none"}
                <Icon name="x" size={11} color="var(--fg-faint)" />
                <span class="scopesw-label">不检索 — 只用模型通用知识回答</span>
                <span class="scopesw-kindtag mono">显式三态</span>
              {:else if o.kind === "subject"}
                <Icon name="diamond" size={12} color="var(--accent)" />
                <span class="scopesw-label">Whole subject — {o.label}</span>
              {:else if o.kind === "tag"}
                <span class="scopesw-hash mono">#</span>
                <span class="scopesw-label">{o.tag}</span>
                <span class="scopesw-kindtag mono">标签</span>
              {:else if o.kind === "topic"}
                <Icon name="chevron" size={11} color="var(--fg-faint)" />
                <span class="scopesw-label">{o.label}</span>
                <span class="scopesw-kindtag mono">主题</span>
              {:else}
                <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
                <span
                  class="scopesw-check{picked[o.src.id] ? ' on' : ''}"
                  title="Add to multi-source selection"
                  onclick={(e) => togglePick(o.src.id, e)}
                >
                  {#if picked[o.src.id]}<Icon name="check" size={10} />{/if}
                </span>
                <span class="badge badge--{o.src.kind === 'audio' ? 'audio' : o.src.kind}" style="height:15px;padding:0 5px">{kindLabel[o.src.kind] ?? o.src.kind.toUpperCase()}</span>
                <span class="scopesw-label mono">{o.label}</span>
              {/if}
              {#if isCur && o.kind !== "source"}
                <Icon name="check" size={13} color="var(--accent)" />
              {/if}
            </div>
          {/each}
        </div>
        {#if pickedIds.length > 0}
          <button class="btn btn--sm btn--primary" style="margin:8px 10px 0" onclick={applyMulti}>
            Chat with {pickedIds.length} selected source{pickedIds.length === 1 ? "" : "s"}
          </button>
        {/if}
        <div class="scopesw-foot mono">
          <span><span class="kbd">↑</span><span class="kbd">↓</span> 移动</span>
          <span><span class="kbd">⏎</span> 选择 · 勾选=多选</span>
          <span><span class="kbd">esc</span> 关闭</span>
        </div>
      </div>
    </div>
  {/if}

  <!-- ── chat history (past conversation sessions) ─────────────────────────── -->
  {#if historyOpen}
    <div class="hist-overlay" role="presentation" onmousedown={() => (historyOpen = false)}>
      <div class="hist-panel" role="dialog" aria-modal="true" tabindex="-1" onmousedown={(e) => e.stopPropagation()}>
        <div class="hist-head">
          <span class="hist-title">对话历史</span>
          <div class="hist-tabs">
            <button
              type="button"
              class="hist-tab"
              class:hist-tab-on={histTab === "threads"}
              onclick={() => (histTab = "threads")}
            >
              对话
            </button>
            <button type="button" class="hist-tab" class:hist-tab-on={histTab === "questions"} onclick={showQuestions}>
              问题索引
            </button>
          </div>
          <div class="grow"></div>
          <button class="btn btn--sm btn--ghost" onclick={startNewConversation} title="New conversation">
            <Icon name="plus" size={12} /> New
          </button>
          <button class="btn btn--icon btn--sm btn--ghost" onclick={() => (historyOpen = false)} title="Close">
            <Icon name="x" size={12} />
          </button>
        </div>
        {#if histTab === "threads"}
          {#if threads.length === 0}
            <div class="hist-empty mono faint">还没有历史对话。</div>
          {:else}
            <div class="hist-list">
              {#each threads as th}
                <button type="button" class="hist-item" onclick={() => pickThread(th.id)}>
                  <Icon name="chat" size={13} color="var(--fg-faint)" />
                  <span class="hist-item-title">{th.title || "New conversation"}</span>
                  <span class="hist-item-meta mono faint">{th.count} msg</span>
                </button>
              {/each}
            </div>
          {/if}
        {:else if questionsBusy}
          <div class="hist-empty mono faint">正在建立问题索引…</div>
        {:else if questions.length === 0}
          <div class="hist-empty mono faint">这个科目还没有问过问题。</div>
        {:else}
          <div class="hist-list">
            {#each questions as q (q.created_at + q.thread_id)}
              <button type="button" class="q-item" onclick={() => void jumpToQuestion(q)}>
                <span class="q-text">{q.question || "（空问题）"}</span>
                <span class="q-badges">
                  {#if q.answer_state === null}
                    <span class="q-badge q-badge-warn" title="这个问题还没有得到回答（生成被中断或尚未发送）">未回答</span>
                  {:else}
                    {#if q.answer_state === "truncated"}
                      <span class="q-badge q-badge-warn" title="回答被输出上限截断，保留了部分内容">截断</span>
                    {:else if q.answer_state === "blocked"}
                      <span class="q-badge q-badge-err" title="回答被模型安全策略拦截">拦截</span>
                    {:else if q.answer_state === "failed"}
                      <span class="q-badge q-badge-err" title="模型没有返回可用内容">无输出</span>
                    {:else if q.answer_state === "aborted"}
                      <span class="q-badge" title="你按下了停止">已中止</span>
                    {/if}
                    {#if q.hits === 0}
                      <span class="q-badge q-badge-err" title="检索零命中：这一轮回答没有用到你的资料">零命中</span>
                    {:else if q.hits}
                      <span class="q-badge" title="检索命中 {q.hits} 个分块">{q.hits} 命中</span>
                    {/if}
                    {#if q.fabricated_dropped}
                      <span class="q-badge q-badge-err" title="citecheck 剥掉了 {q.fabricated_dropped} 条捏造的引用">
                        剥离 {q.fabricated_dropped}
                      </span>
                    {/if}
                  {/if}
                </span>
              </button>
            {/each}
          </div>
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  /* web image gallery under an assistant answer */
  .chat-images { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 10px; }
  /* Uncited answer: red border around the whole bubble so it cannot be missed. */
  .bubble.assistant.is-uncited { border-color: var(--warn); box-shadow: 0 0 0 1px color-mix(in oklab, var(--warn) 35%, transparent); }

  /* Retrieval accounting (0044): session ring + per-message zero-hit badge.
     The red left border is the "this answer never touched your library" signal
     — distinct from .is-uncited (retrieved but nothing cited inline). */
  .bubble.assistant.is-unretrieved {
    border-left: 3px solid var(--err, #e5484d);
    padding-left: 11px;
  }
  .retr-badge {
    display: inline-block;
    margin-bottom: 6px;
    padding: 1px 7px;
    border-radius: 999px;
    font-size: 10px;
    letter-spacing: 0.04em;
    color: var(--err, #e5484d);
    background: color-mix(in oklab, var(--err, #e5484d) 12%, transparent);
    border: 1px solid color-mix(in oklab, var(--err, #e5484d) 35%, transparent);
  }
  /* Answer-state badges (0077): same pill shape, per-state hue. Truncation
     is a warning (partial answer kept), block/failure are errors, abort is
     neutral grey. */
  .retr-badge.state-truncated {
    color: var(--warn, #f5a623);
    background: color-mix(in oklab, var(--warn, #f5a623) 12%, transparent);
    border-color: color-mix(in oklab, var(--warn, #f5a623) 35%, transparent);
  }
  .retr-badge.state-blocked,
  .retr-badge.state-failed {
    color: var(--err, #e5484d);
  }
  .retr-badge.state-aborted {
    color: var(--fg-muted);
    background: color-mix(in oklab, var(--fg-muted) 12%, transparent);
    border-color: color-mix(in oklab, var(--fg-muted) 35%, transparent);
  }
  .retr-badge.state-history {
    color: var(--fg-muted);
    background: color-mix(in oklab, var(--fg-muted) 8%, transparent);
    border-color: color-mix(in oklab, var(--fg-muted) 25%, transparent);
  }
  /* Context-intent badge: informational, not a warning — the answer is not
     degraded, the budget simply leaned toward the layer the question wants. */
  .retr-badge.state-intent {
    color: var(--accent);
    background: color-mix(in oklab, var(--accent) 8%, transparent);
    border-color: color-mix(in oklab, var(--accent) 28%, transparent);
  }
  .retr-stat {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    margin-right: 6px;
    color: var(--fg-muted);
    font-size: 10px;
    cursor: help;
  }

  /* Auto-rollover notice (0082): a summary was inherited from the archived
     thread. Deliberately loud — a rollover the user doesn't notice is a
     conversation that silently lost its thread. */
  .rollover-note {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    margin: 8px 12px 0;
    padding: 8px 10px;
    border: 1px solid color-mix(in oklab, var(--accent) 40%, var(--border));
    border-radius: 8px;
    background: color-mix(in oklab, var(--accent) 8%, var(--surface-2));
    font-size: 12px;
    color: var(--fg-muted);
  }
  .rollover-note .rn-ico { color: var(--accent); flex: none; margin-top: 2px; }
  .rollover-note .rn-body { flex: 1; min-width: 0; line-height: 1.5; }
  .rollover-note strong { color: var(--fg-bright); }
  .rollover-note details { margin-top: 4px; }
  .rollover-note summary {
    cursor: pointer; font-size: 11px; color: var(--fg-faint);
    list-style: none;
  }
  .rollover-note summary::-webkit-details-marker { display: none; }
  .rollover-note summary::before { content: "▸ "; }
  .rollover-note details[open] summary::before { content: "▾ "; }
  .rollover-note details p {
    margin: 4px 0 0;
    font-size: 11.5px;
    color: var(--fg-muted);
    white-space: pre-wrap;
  }

  .evidence { margin-top: 10px; padding-top: 9px; border-top: 1px solid var(--border); }
  .evidence-head {
    display: flex; align-items: center; gap: 8px;
    font-size: 10px; letter-spacing: 0.1em; text-transform: uppercase; color: var(--fg-faint);
    background: transparent; border: 0; padding: 0; margin: 0 0 6px;
    cursor: pointer; font: inherit;
    width: 100%; text-align: left;
  }
  .evidence-head:hover { color: var(--fg-muted); }
  .evidence-head.is-uncited { color: var(--warn); }
  .evidence-count { margin-left: 4px; color: var(--fg-faint); text-transform: none; letter-spacing: 0; }
  .evidence-caret { margin-left: auto; color: var(--fg-faint); }
  .evidence-warning { color: var(--warn); letter-spacing: 0; text-transform: none; margin-left: 4px; }
  .evidence-fabricated {
    color: var(--danger, #e24b4a);
    letter-spacing: 0; text-transform: none; margin-left: 4px;
    border: 1px solid color-mix(in oklab, var(--danger, #e24b4a) 45%, transparent);
    border-radius: 999px;
    padding: 0 7px;
    font-size: 10px;
    cursor: help;
  }
  .evidence-body { margin-top: 6px; display: flex; flex-direction: column; gap: 6px; }
  .evidence-list { display: flex; flex-wrap: wrap; gap: 5px; }
  .evidence-trace {
    font-size: 10.5px; color: var(--fg-faint);
    padding: 4px 6px; border-top: 1px dashed var(--border);
  }
  .trace-warn { color: var(--warn); }
  .ec-score { color: var(--fg-faint); margin-left: 2px; }
  .evidence-chip.is-uncited-chip { opacity: 0.65; border-style: dashed; }
  .evidence-chip {
    display: inline-flex; align-items: center; gap: 5px; max-width: 100%;
    padding: 4px 7px; border: 1px solid var(--border); border-radius: 6px;
    background: var(--surface-2); color: var(--fg-muted); font: inherit; font-size: 11px;
    cursor: pointer;
  }
  .evidence-chip:hover { border-color: var(--border-strong); color: var(--fg-bright); }
  .evidence-chip.is-cited { border-color: color-mix(in oklab, var(--accent) 45%, var(--border)); }
  .ec-source { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ec-loc { flex: none; color: var(--fg-faint); font-family: var(--font-mono); font-size: 10px; }
  /* Snapshot verification badges (0078): the frozen quote was re-checked
     against the source's current text. Moved = content intact, re-indexed;
     drift = the quote is no longer in the source (edited/re-extracted). */
  .ec-snap {
    flex: none;
    font-size: 10px;
    padding: 0 5px;
    border-radius: 999px;
    border: 1px solid transparent;
  }
  .ec-snap-moved {
    color: var(--accent, #7c8cf8);
    border-color: color-mix(in oklab, var(--accent, #7c8cf8) 40%, transparent);
    background: color-mix(in oklab, var(--accent, #7c8cf8) 10%, transparent);
  }
  .ec-snap-drift {
    color: var(--warn, #f5a623);
    border-color: color-mix(in oklab, var(--warn, #f5a623) 45%, transparent);
    background: color-mix(in oklab, var(--warn, #f5a623) 12%, transparent);
  }
  /* Re-anchored (citesnap repair): the snapshot now matches the current
     source, and the citation carries the original quote it replaced. */
  .ec-snap-reanchored {
    color: var(--accent, #7c8cf8);
    border-color: color-mix(in oklab, var(--accent, #7c8cf8) 40%, transparent);
    background: color-mix(in oklab, var(--accent, #7c8cf8) 10%, transparent);
  }
  .ec-cell { display: inline-flex; align-items: stretch; gap: 3px; max-width: 100%; }
  .ec-fix {
    flex: none; padding: 0 7px; border-radius: 6px;
    border: 1px dashed color-mix(in oklab, var(--warn, #f5a623) 55%, var(--border));
    background: transparent; color: var(--warn, #f5a623);
    font: inherit; font-size: 10.5px; cursor: pointer;
  }
  .ec-fix:hover:not(:disabled) {
    background: color-mix(in oklab, var(--warn, #f5a623) 14%, transparent);
  }
  .ec-fix:disabled { opacity: 0.55; cursor: progress; }
  .chat-img {
    display: block; width: 116px; height: 86px; border-radius: var(--rad-2);
    overflow: hidden; border: 1px solid var(--border); background: var(--surface-2);
  }
  .chat-img img { width: 100%; height: 100%; object-fit: cover; display: block; }
  .chat-img:hover { border-color: var(--accent); }
  /* web-mode toggle in the composer */
  .chat-web.is-on { color: var(--accent); border-color: color-mix(in oklab, var(--accent) 55%, var(--border)); background: color-mix(in oklab, var(--accent) 12%, transparent); }

  /* ── chat history panel ────────────────────────────────────────────────── */
  .hist-overlay {
    position: absolute; inset: 0; z-index: 80; display: flex;
    align-items: flex-start; justify-content: center; padding-top: 56px;
    background: color-mix(in oklab, var(--bg) 55%, transparent); backdrop-filter: blur(2px);
  }
  .hist-panel {
    width: min(440px, calc(100% - 32px)); max-height: 70%;
    display: flex; flex-direction: column;
    background: var(--surface); border: 1px solid var(--border-strong);
    border-radius: 12px; box-shadow: 0 18px 50px rgba(0,0,0,0.45); padding: 12px;
  }
  .hist-head { display: flex; align-items: center; gap: 8px; margin-bottom: 8px; }
  .hist-title { font-family: var(--font-mono); font-weight: 600; color: var(--fg-bright); font-size: 13px; }
  .hist-empty { padding: 24px; text-align: center; font-size: 12px; }
  .hist-list { overflow-y: auto; display: flex; flex-direction: column; gap: 2px; }
  .hist-item {
    display: flex; align-items: center; gap: 9px; width: 100%; text-align: left;
    padding: 9px 10px; border-radius: 8px; border: 1px solid transparent;
    background: none; color: var(--fg); font: inherit; cursor: pointer;
  }
  .hist-item:hover { background: var(--surface-2); border-color: var(--border); }
  .hist-item-title { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--fg-bright); font-size: 12.5px; }
  .hist-item-meta { flex: none; font-size: 10.5px; }

  /* History drawer tabs: 对话 / 问题索引 (KnowNote borrow). */
  .hist-tabs { display: inline-flex; gap: 2px; padding: 2px; border-radius: 999px; background: var(--surface-2); }
  .hist-tab {
    border: none; background: transparent; cursor: pointer;
    padding: 2px 10px; border-radius: 999px;
    font-size: 11px; color: var(--fg-muted);
  }
  .hist-tab:hover { color: var(--fg-bright); }
  .hist-tab-on { color: var(--accent); background: color-mix(in oklab, var(--accent) 14%, transparent); }

  /* Question index rows: the question, then its answer's provenance badges. */
  .q-item {
    display: flex; flex-direction: column; gap: 4px; align-items: stretch;
    width: 100%; text-align: left; cursor: pointer;
    padding: 8px 10px; border-radius: var(--rad-2, 8px);
    border: 1px solid transparent; background: transparent;
  }
  .q-item:hover { background: var(--surface-2); border-color: var(--border); }
  .q-text {
    color: var(--fg-bright); font-size: 12.5px; line-height: 1.45;
    display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden;
  }
  .q-badges { display: flex; flex-wrap: wrap; gap: 4px; }
  .q-badge {
    font-size: 10px; padding: 0 6px; border-radius: 999px;
    color: var(--fg-muted); background: var(--surface-2);
    border: 1px solid var(--border);
  }
  .q-badge-warn {
    color: var(--warn, #f5a623);
    border-color: color-mix(in oklab, var(--warn, #f5a623) 40%, transparent);
    background: color-mix(in oklab, var(--warn, #f5a623) 10%, transparent);
  }
  .q-badge-err {
    color: var(--err, #e5484d);
    border-color: color-mix(in oklab, var(--err, #e5484d) 40%, transparent);
    background: color-mix(in oklab, var(--err, #e5484d) 10%, transparent);
  }

  /* Jump target flash (引用回流 / 问题索引 scroll-here). */
  @keyframes bubble-flash {
    0%, 100% { box-shadow: none; }
    30% { box-shadow: 0 0 0 2px var(--accent); }
  }
  .bubble-flash { animation: bubble-flash 1.6s ease-in-out 1; }

  /* ── next-step suggestion chips + queue + stop + model picker ──────────── */
  .chat-suggest {
    display: flex; flex-wrap: wrap; align-items: center; gap: 6px; margin-bottom: 8px;
  }
  .chat-suggest .cs-label {
    font-size: var(--t-2xs, 10.5px); letter-spacing: 0.12em; text-transform: uppercase;
    color: var(--fg-faint); margin-right: 2px;
  }
  .suggest-chip {
    font: inherit; font-size: 12px; cursor: pointer;
    padding: 5px 10px; border-radius: 999px;
    border: 1px solid var(--border-strong); background: var(--surface-2); color: var(--fg);
    transition: background 0.12s ease, border-color 0.12s ease, color 0.12s ease;
    max-width: 220px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  }
  .suggest-chip:hover { background: var(--surface-3); border-color: var(--accent-dim, var(--accent)); color: var(--fg-bright); }
  .chat-queued { font-size: 11px; margin-bottom: 6px; }

  /* per-message save-to-note action (appears on hover of an assistant bubble) */
  .bubble-actions {
    display: flex;
    justify-content: flex-end;
    gap: 4px;
    margin-top: 6px;
    opacity: 0;
    transition: opacity 0.12s ease;
  }
  .bubble.assistant:hover .bubble-actions {
    opacity: 1;
  }
  .msg-action {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    border-radius: var(--r-lg, 8px);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--fg-muted);
    cursor: pointer;
    transition: background 0.12s ease, color 0.12s ease, border-color 0.12s ease;
  }
  .msg-action:hover {
    background: var(--surface-3);
    color: var(--fg-bright);
    border-color: var(--border-strong);
  }
  .chat-stop .stop-sq { width: 10px; height: 10px; border-radius: 2px; background: var(--err); display: block; }
  .chat-stop { border-color: var(--border-strong); }
  /* Shrinkable so a narrow (resized) chat dock keeps the action buttons visible
     instead of pushing them off the right edge — the picker truncates instead. */
  .chat-model-pick { width: 190px; min-width: 160px; flex: 0 0 auto; font-size: 11px; }

  /* ── scope selector (the reworked top bar: one clean clickable control) ─── */
  .scope-pick {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    max-width: 100%;
    min-width: 0;
    flex: 0 1 auto; /* shrink (truncating the label) before the action buttons give up space */
    overflow: hidden;
    padding: 5px 10px;
    border: 1px solid var(--border-strong);
    border-radius: var(--r-md, 8px);
    background: var(--surface-2);
    color: var(--fg);
    font: inherit;
    font-size: var(--t-sm, 12.5px);
    cursor: pointer;
    transition: background 0.12s ease, border-color 0.12s ease;
  }
  .scope-pick:hover {
    background: var(--surface-3);
    border-color: var(--accent-dim, var(--accent));
  }
  .scope-pick .sp-ico { flex: none; font-size: 13px; line-height: 1; }
  .scope-pick .sp-name {
    color: var(--fg-bright);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }
  .scope-pick .sp-name.sp-dim { color: var(--fg-muted); }
  .scope-pick .sp-sep { flex: none; color: var(--fg-faint); }

  /* The floating pop-out chat must never wrap its header — at the panel's minimum
     width the scope label truncates so the action buttons stay fixed on one row,
     instead of the buttons stacking onto a second line. (The full-screen / mobile
     chat keeps the @container rule below, which drops the model picker to its own
     row where there's a real keyboard-friendly reason to.) */
  .chatdock-inner :global(.chat-head.is-popout) { flex-wrap: nowrap; }

  @container (max-width: 430px) {
    .chatdock-inner :global(.chat-head) {
      height: auto;
      min-height: 44px;
      flex-wrap: wrap;
      padding-top: 6px;
      padding-bottom: 6px;
    }
    .chat-model-pick {
      order: 10;
      width: 100%;
      min-width: 0;
      flex-basis: 100%;
    }
  }

  /* ── fit-to-page: the panel always fills its container as a flex column.
     header (fixed) · messages (flex:1, scroll) · composer (fixed). This makes
     the messages region grow to fill the page in the full "Chats" tab while
     the docked variant keeps working (both render .chatdock-inner). ───────── */
  .chatdock-inner {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    position: relative; /* anchor the scope-switcher overlay */
    container-type: inline-size;
  }
  .chatdock-inner :global(.chat-head) {
    flex: none;
  }
  .chatdock-inner :global(.chat-scroll) {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
  }
  .chatdock-inner :global(.chat-compose) {
    flex: none;
  }

  /* ── scope switcher overlay (themed like CommandPalette / Picker) ───────── */
  .scopesw-overlay {
    position: absolute;
    inset: 0;
    z-index: 70;
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding-top: 52px;
    background: color-mix(in oklab, var(--bg) 52%, transparent);
    backdrop-filter: blur(2px);
    outline: none;
  }
  .scopesw {
    width: min(340px, calc(100% - 28px));
    max-height: calc(100% - 80px);
    display: flex;
    flex-direction: column;
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--rad-3);
    box-shadow: var(--shadow-pop);
    overflow: hidden;
    outline: none;
    animation: popIn var(--dur-fast) var(--ease);
  }
  .scopesw-head {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 9px 11px;
    border-bottom: 1px solid var(--border);
  }
  .scopesw-title {
    flex: 1;
    font-size: var(--t-xs);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }
  .scopesw-list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 6px;
  }
  .scopesw-item {
    display: flex;
    align-items: center;
    gap: 9px;
    width: 100%;
    padding: 7px 9px;
    border-radius: var(--rad-2);
    cursor: pointer;
    color: var(--fg-muted);
    user-select: none;
  }
  .scopesw-item--source {
    padding-left: 22px; /* indent sources under their topic */
  }
  .scopesw-check {
    flex: none;
    width: 15px;
    height: 15px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border-radius: 4px;
    border: 1px solid var(--border-strong);
    background: var(--bg);
    color: var(--bg);
    cursor: pointer;
  }
  .scopesw-check:hover { border-color: var(--accent); }
  .scopesw-check.on { background: var(--accent); border-color: var(--accent); }
  .scopesw-item.sel {
    background: var(--surface-3);
    color: var(--fg-bright);
  }
  .scopesw-label {
    flex: 1;
    font-size: var(--t-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .scopesw-kindtag {
    font-size: var(--t-2xs);
    letter-spacing: 0.08em;
    color: var(--fg-faint);
  }
  .scopesw-hash {
    width: 13px;
    text-align: center;
    color: var(--accent);
    font-weight: 700;
    flex: none;
  }
  .scopesw-foot {
    display: flex;
    gap: 12px;
    align-items: center;
    padding: 8px 11px;
    border-top: 1px solid var(--border);
    font-size: var(--t-2xs);
    color: var(--fg-faint);
  }

  /* Centered "no subject" empty state — mirrors GenerateMaterial's .genmat--working */
  .chat-empty-state {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    text-align: center;
    gap: 10px;
  }
  .chat-empty-state .ces-ico {
    width: 48px;
    height: 48px;
    border-radius: 50%;
    border: 1px solid var(--border-strong);
    background: var(--surface);
    display: grid;
    place-items: center;
    margin-bottom: 2px;
  }
  .chat-empty-state .ces-title {
    font-size: var(--r-md);
    color: var(--fg-bright);
    font-weight: 500;
  }
  .chat-empty-state .ces-sub {
    max-width: 260px;
    font-size: var(--t-sm);
    color: var(--fg-muted);
    line-height: 1.5;
  }
</style>
