<script lang="ts">
  import { app, THEMES, THEME_LABELS } from "../lib/store.svelte";
  import type { Theme } from "../lib/store.svelte";
  import { isMobile } from "../lib/platform";
  import * as api from "../lib/api";
  import { getVersion } from "@tauri-apps/api/app";
  import type { Memory } from "../lib/api";
  import Icon from "../components/Icon.svelte";
  import Picker from "../components/Picker.svelte";
  import ModelSearch from "../components/ModelSearch.svelte";
  import { loadOpenRouterModels, type OrModel } from "../lib/openrouter";
  import { stations } from "../lib/mock";
  import { keybinds, ACTION_LABELS, ACTION_ORDER, LEADER_ACTIONS } from "../lib/keybinds.svelte";
  import type { Action } from "../lib/keybinds.svelte";
  import { onMount } from "svelte";
  import type { Snippet } from "svelte";

  // Shape for the reusable homelab endpoint-service snippet (Integrations tab).
  type EndpointOpts = {
    title: string;
    desc: string;
    value: string;
    oninput: (v: string) => void;
    onsave: () => void;
    onTest: () => void;
    state: null | "testing" | "ok" | "fail";
    placeholder: string;
    failHint?: string;
    hint?: string;
    extra?: Snippet;
  };

  // Fixed system bindings (not rebindable) shown for reference in the Keybinds tab
  // so the page reflects every shortcut, not just the customizable single-key set.
  const SYSTEM_BINDS = [
    { keys: "Ctrl F", label: "Find on page" },
    { keys: "Ctrl P", label: "Command palette" },
    { keys: "Esc",    label: "Close overlay / go back" },
    { keys: "g d",    label: "Go to dashboard" },
    { keys: "Alt 1–9", label: "Jump to subject N" },
  ];

  // ---- tab navigation ----
  const TABS = [
    { id: "profile",    label: "个人资料",       icon: "diamond" },
    { id: "models",     label: "模型",        icon: "bolt" },
    { id: "keys",       label: "API 密钥",      icon: "lock" },
    { id: "appearance", label: "外观",    icon: "grid" },
    { id: "keybinds",   label: "快捷键",      icon: "cmd" },
    { id: "homelab",    label: "集成",  icon: "globe" },
    { id: "calendar",   label: "Google 日历", icon: "globe" },
    { id: "cron",       label: "自动化",      icon: "refresh" },
    { id: "experimental", label: "实验功能", icon: "bolt" },
    { id: "audio",      label: "音频",         icon: "music" },
    { id: "data",       label: "数据与隐私",icon: "doc" },
    { id: "about",      label: "关于",         icon: "diamond" },
  ] as const;

  // Mobile is a homelab-first portable view: drop the desktop-only Keybinds tab
  // and lead with Integrations (homelab). Desktop order/contents are unchanged.
  const MOBILE_TABS = ["homelab", "keys", "models", "appearance", "calendar", "experimental", "audio", "data", "profile", "about"];
  const navTabs = isMobile
    ? MOBILE_TABS.map((id) => TABS.find((t) => t.id === id)).filter((t): t is (typeof TABS)[number] => !!t)
    : TABS;

  let tab = $state<string>("profile");

  // ── 检索评测 ──
  let evalBusy = $state(false);
  let evalError = $state<string | null>(null);
  let evalReports = $state<api.EvalReport[] | null>(null);
  async function runRetrievalEval() {
    if (evalBusy) return;
    evalBusy = true;
    evalError = null;
    try {
      evalReports = await api.retrievalEval(5);
    } catch (e) {
      evalError = String(e);
    } finally {
      evalBusy = false;
    }
  }
  const evalBest = $derived(
    evalReports?.reduce((a, b) => (b.recall_at_k > a.recall_at_k ? b : a), evalReports[0]) ?? null,
  );

  // ── 真实语料评测（用户自己的库） ──
  let realEvalBusy = $state(false);
  let realEvalError = $state<string | null>(null);
  let realEvalReports = $state<api.EvalReport[] | null>(null);
  let evalSubjectIds = $state<string[]>([]);
  let evalSubjects = $state<api.Subject[]>([]);
  async function runRealEval() {
    if (realEvalBusy) return;
    realEvalBusy = true;
    realEvalError = null;
    try {
      realEvalReports = await api.retrievalEvalReal(evalSubjectIds.length ? evalSubjectIds : null, 5);
    } catch (e) {
      realEvalError = String(e);
    } finally {
      realEvalBusy = false;
    }
  }
  function toggleEvalSubject(id: string) {
    evalSubjectIds = evalSubjectIds.includes(id)
      ? evalSubjectIds.filter((x) => x !== id)
      : [...evalSubjectIds, id];
  }
  const realEvalBest = $derived(
    realEvalReports?.reduce((a, b) => (b.recall_at_k > a.recall_at_k ? b : a), realEvalReports[0]) ?? null,
  );

  // Honour a deep-link from elsewhere (e.g. the subject Overview → Moodle setup).
  $effect(() => {
    if (app.settingsTab) {
      tab = app.settingsTab;
      app.settingsTab = null;
    }
  });

  // This view owns the global keys while open
  $effect(() => {
    (window as any).__cortexViewKeys = true;
    return () => { (window as any).__cortexViewKeys = false; };
  });

  // ---- providers ----
  // Model catalog. Each provider's models are ordered by MY cost-to-quality read —
  // best value (cheap + capable) first, then premium/frontier, then reasoning — and
  // labelled with a human name + a one-word tier hint so the dropdown is scannable.
  // `custom` (OpenRouter / OpenAI-compatible) lets you type any slug not listed.
  // The OpenAI-compatible registry (DeepSeek, Qwen, GLM, Kimi, …) is MERGED IN at
  // load from the Rust `list_providers` command, so a new provider in Rust needs
  // no frontend edit. ($state, not const, because that merge replaces the array.)
  type Model = { id: string; label: string };
  type ProvList = { id: string; label: string; models: Model[] }[];
  let PROVIDERS = $state<ProvList>([
    { id: "gemini", label: "Gemini", models: [
      { id: "gemini-2.5-flash",      label: "Gemini 2.5 Flash — ⚡ best value" },
      { id: "gemini-2.5-flash-lite", label: "Gemini 2.5 Flash-Lite — cheapest" },
      { id: "gemini-2.0-flash-001",  label: "Gemini 2.0 Flash — legacy, cheap" },
      { id: "gemini-2.5-pro",        label: "Gemini 2.5 Pro — ★ premium" },
    ] },
    { id: "openrouter", label: "OpenRouter", models: [
      // Curated shortlist + ordering (the live catalog with per-row pricing replaces
      // these labels once it loads). Order ≈ best value → all-rounders → premium →
      // reasoning; the editorial "cheap/fast" tags are dropped since the searchable
      // list shows price-per-token + context per row.
      { id: "deepseek/deepseek-v4-flash",         label: "DeepSeek V4 Flash" },
      { id: "google/gemini-2.5-flash",            label: "Gemini 2.5 Flash" },
      { id: "google/gemini-2.5-flash-lite",       label: "Gemini 2.5 Flash-Lite" },
      { id: "openai/gpt-5-mini",                  label: "GPT-5 mini" },
      { id: "openai/gpt-4o-mini",                 label: "GPT-4o mini" },
      { id: "anthropic/claude-3.5-haiku",         label: "Claude 3.5 Haiku" },
      { id: "stepfun/step-3.7-flash",             label: "Step 3.7 Flash" },
      { id: "deepseek/deepseek-chat",             label: "DeepSeek V3" },
      { id: "meta-llama/llama-3.3-70b-instruct",  label: "Llama 3.3 70B" },
      { id: "qwen/qwen-2.5-72b-instruct",         label: "Qwen 2.5 72B" },
      { id: "mistralai/mistral-large",            label: "Mistral Large" },
      { id: "x-ai/grok-2-1212",                   label: "Grok 2" },
      { id: "google/gemini-2.0-flash-001",        label: "Gemini 2.0 Flash" },
      { id: "openai/gpt-4o",                      label: "GPT-4o" },
      { id: "anthropic/claude-3.5-sonnet",        label: "Claude 3.5 Sonnet" },
      { id: "anthropic/claude-3.7-sonnet",        label: "Claude 3.7 Sonnet" },
      { id: "google/gemini-2.5-pro",              label: "Gemini 2.5 Pro" },
      { id: "anthropic/claude-sonnet-4.5",        label: "Claude Sonnet 4.5" },
      { id: "openai/gpt-5",                       label: "GPT-5" },
      { id: "anthropic/claude-opus-4.1",          label: "Claude Opus 4.1" },
      { id: "x-ai/grok-3",                        label: "Grok 3" },
      { id: "openai/o3-mini",                     label: "o3-mini" },
      { id: "openai/o3",                          label: "o3" },
      { id: "deepseek/deepseek-r1",               label: "DeepSeek R1" },
    ] },
    { id: "openai", label: "OpenAI", models: [
      { id: "gpt-4o-mini", label: "GPT-4o mini — cheap" },
      { id: "gpt-5-mini",  label: "GPT-5 mini — cheap + smart" },
      { id: "gpt-4o",      label: "GPT-4o — balanced" },
      { id: "gpt-5",       label: "GPT-5 — ★ frontier" },
      { id: "o3-mini",     label: "o3-mini — cheap reasoning" },
      { id: "o3",          label: "o3 — deep reasoning" },
    ] },
    { id: "claude", label: "Claude", models: [
      { id: "claude-haiku-4-5-20251001",  label: "Claude Haiku 4.5 — fast + cheap" },
      { id: "claude-3-5-haiku-20241022",  label: "Claude 3.5 Haiku — cheap" },
      { id: "claude-3-5-sonnet-20241022", label: "Claude 3.5 Sonnet — strong" },
      { id: "claude-3-7-sonnet-20250219", label: "Claude 3.7 Sonnet — strong" },
      { id: "claude-sonnet-4-6",          label: "Claude Sonnet 4.6 — ★ premium" },
      { id: "claude-opus-4-8",            label: "Claude Opus 4.8 — top, pricey" },
    ] },
    { id: "ollama", label: "Ollama (local)", models: [
      { id: "mistral-small", label: "Mistral Small — light, local" },
      { id: "qwen2.5:32b",   label: "Qwen 2.5 32B — local" },
      { id: "llama3.3:70b",  label: "Llama 3.3 70B — local, heavy" },
    ] },
    { id: "custom", label: "Custom endpoint", models: [] },
  ]);
  let EMBED_PROVIDERS = $state<ProvList>([
    { id: "onnx",   label: "Local ONNX (zero-config)", models: [{ id: "multilingual-e5-small", label: "multilingual-e5-small — 384d, on-device" }] },
    { id: "gemini",  label: "Gemini",        models: [{ id: "text-embedding-004", label: "text-embedding-004" }] },
    { id: "openai",  label: "OpenAI",        models: [{ id: "text-embedding-3-small", label: "text-embedding-3-small — cheap" }, { id: "text-embedding-3-large", label: "text-embedding-3-large — best" }] },
    { id: "ollama",  label: "Ollama (local)", models: [{ id: "nomic-embed-text", label: "nomic-embed-text — local" }, { id: "mxbai-embed-large", label: "mxbai-embed-large — local" }] },
  ]);
  const MODEL_TASKS = [
    { id: "chat",       label: "Chat",                  desc: "Scoped Q&A across sources" },
    { id: "cheatsheet", label: "Cheatsheet synthesis",  desc: "Completeness-checked merges" },
    { id: "audio",      label: "Audio overview script", desc: "Two-host podcast dialogue" },
    { id: "quiz",       label: "Quiz generation",       desc: "MCQ · short answer · cloze" },
    { id: "flashcard",  label: "Flashcard generation",  desc: "Q/A pairs + SRS scheduling" },
    { id: "embedding",  label: "Embedding",             desc: "Vector index for retrieval" },
  ] as const;

  type TaskId = typeof MODEL_TASKS[number]["id"];

  // ---- profile state ----
  let name       = $state("Sam Okonkwo");
  let pronouns   = $state("they/them");
  let level      = $state("postgrad");
  let field      = $state("Computer Science — MSc");
  let about      = $state("Final-year MSc student. I think in code and analogies, already comfortable with Big-O. I revise late at night and learn fastest from worked examples, then a terse summary.");
  let style      = $state("balanced");
  // coach_tone: gentle/friendly/neutral/strict/savage — injected into every
  // system prompt (chat + generation). Default neutral changes nothing.
  let coachTone  = $state("neutral");
  let explain    = $state<string[]>(["worked-examples","analogies"]);

  // ---- long-term memory state ----
  let memories     = $state<Memory[]>([]);
  let newMemory    = $state("");
  let memoryLayer = $state("declarative");
  let memoryEvidence = $state("");
  let memoryConfidence = $state(0.7);
  let memoryBusy   = $state(false);

  async function loadMemory() {
    memories = await api.listMemory().catch(() => [] as Memory[]);
  }
  async function addMemoryFact() {
    const text = newMemory.trim();
    if (!text || memoryBusy) return;
    memoryBusy = true;
    try {
      await api.addMemory(text, memoryLayer, memoryLayer === "declarative" ? 1 : memoryConfidence, memoryEvidence.trim() || null);
      newMemory = "";
      await loadMemory();
    } catch {
      app.pushToast({ kind: "error", title: "Could not save memory" });
    } finally {
      memoryBusy = false;
    }
  }
  async function removeMemory(id: string) {
    if (!await app.confirm({ title: "删除这条记忆？", danger: true })) return;
    try {
      await api.deleteMemory(id);
      await loadMemory();
    } catch {
      app.pushToast({ kind: "error", title: "Could not delete memory" });
    }
  }

  // ---- models state ----
  type TaskAssign = { provider: string; model: string; budget: string };
  // Defaults: DeepSeek V4 Flash (via OpenRouter) for ALL text generation —
  // extremely cheap ($0.09/$0.18 per Mtok), 1M context, fast and non-reasoning.
  // Falls back to any configured key if OpenRouter isn't set (see
  // llm::from_spec_or_any). Embeddings stay on Gemini (DeepSeek doesn't embed).
  let assign = $state<Record<TaskId, TaskAssign>>({
    chat:       { provider: "openrouter", model: "deepseek/deepseek-v4-flash", budget: "8000" },
    cheatsheet: { provider: "openrouter", model: "deepseek/deepseek-v4-flash", budget: "32000" },
    audio:      { provider: "openrouter", model: "deepseek/deepseek-v4-flash", budget: "16000" },
    quiz:       { provider: "openrouter", model: "deepseek/deepseek-v4-flash", budget: "8000" },
    flashcard:  { provider: "openrouter", model: "deepseek/deepseek-v4-flash", budget: "6000" },
    embedding:  { provider: "gemini",     model: "text-embedding-004",        budget: "—" },
  });

  function setTask(id: TaskId, patch: Partial<TaskAssign>) {
    assign = { ...assign, [id]: { ...assign[id], ...patch } };
  }

  // ---- TTS + embedding model-name overrides (the slots a task row can't hold) ----
  // Speech: cloud TTS model (tts-1/tts-1-hd/…) + optional OpenAI-compatible
  // /audio/speech endpoint & key override (aggregators, homelab gateways).
  // Embedding: model-name override for the gemini/ollama arms (defaults are
  // hardcoded); changing to different dimensions requires re-ingest.
  let speechModel  = $state("tts-1");
  let ttsEndpoint  = $state("");
  let ttsKey       = $state("");
  let embedOv      = $state("");
  let pdfEngine    = $state("auto");
  let doclingBin   = $state("");
  let economyOn    = $state(false);
  let economySpec  = $state("");
  function saveEconomyMode(e: Event) {
    economyOn = (e.target as HTMLInputElement).checked;
    api.setSetting("economy_mode", economyOn ? "1" : "0").catch(() => {});
  }
  function saveEconomySpec(e: Event) {
    const v = (e.target as HTMLInputElement).value.trim();
    economySpec = v;
    api.setSetting("model_cheap", v).catch(() => {});
  }
  function saveSpeech(e: Event) {
    const v = (e.target as HTMLInputElement).value.trim();
    speechModel = v;
    api.setSetting("model_speech", v || "tts-1").catch(() => {});
  }
  function saveTtsEndpoint(e: Event) {
    const v = (e.target as HTMLInputElement).value.trim();
    ttsEndpoint = v;
    api.setSetting("tts_endpoint", v).catch(() => {});
  }
  function saveTtsKey(e: Event) {
    const v = (e.target as HTMLInputElement).value.trim();
    ttsKey = v;
    api.setSetting("tts_api_key", v).catch(() => {});
  }
  function saveEmbedOv(e: Event) {
    const v = (e.target as HTMLInputElement).value.trim();
    embedOv = v;
    api.setSetting("embed_model", v).catch(() => {});
  }
  function savePdfEngine(e: Event) {
    const v = (e.target as HTMLSelectElement).value;
    pdfEngine = v;
    api.setSetting("pdf_engine", v).catch(() => {});
  }
  function saveDoclingBin(e: Event) {
    const v = (e.target as HTMLInputElement).value.trim();
    doclingBin = v;
    api.setSetting("docling_bin", v).catch(() => {});
  }

  // ---- cron (scheduled automation) ----
  let cronJobs = $state<api.CronJob[] | null>(null);
  let cronRuns = $state<Record<string, api.CronRun[]>>({});
  let cronRunsOpen = $state<string | null>(null);
  let cronBusy = $state(false);
  let cronForm = $state({
    name: "",
    prompt: "",
    subjectId: "",
    kind: "daily",
    intervalMinutes: 60,
    dailyTime: "08:00",
    action: "prompt", // "prompt" | "sync"
    connectorId: "",
    autoFlashcards: false,
  });
  let cronConnectors = $state<api.ConnectorInfo[] | null>(null);

  async function loadCron() {
    try { cronJobs = await api.cronList(); } catch { cronJobs = []; }
    try { cronConnectors = await api.connectorList(); } catch { cronConnectors = []; }
  }
  async function cronCreate() {
    const isSync = cronForm.action === "sync";
    if (!cronForm.name.trim() || cronBusy) return;
    if (!isSync && !cronForm.prompt.trim()) return;
    if (isSync && (!cronForm.connectorId || !cronForm.subjectId)) return;
    cronBusy = true;
    try {
      await api.cronAdd({
        name: cronForm.name.trim(),
        prompt: cronForm.prompt.trim(),
        subject_id: cronForm.subjectId || null,
        kind: cronForm.kind,
        interval_minutes:
          cronForm.kind === "interval" ? Math.max(1, Math.round(cronForm.intervalMinutes || 60)) : null,
        daily_time: cronForm.kind === "daily" ? cronForm.dailyTime : null,
        // JS getTimezoneOffset is minutes WEST of UTC; backend wants east.
        tz_offset_minutes: -new Date().getTimezoneOffset(),
        action: isSync ? "sync" : "prompt",
        action_ref: isSync ? cronForm.connectorId : null,
        auto_flashcards: isSync ? cronForm.autoFlashcards : false,
      });
      cronForm = { ...cronForm, name: "", prompt: "", connectorId: "" };
      cronJobs = await api.cronList();
      app.pushToast({ kind: "success", title: "定时任务已创建" });
    } catch (e) {
      app.pushToast({ kind: "error", title: "创建失败", body: String(e) });
    }
    cronBusy = false;
  }
  async function cronToggle(j: api.CronJob) {
    try {
      await api.cronSetEnabled(j.id, !j.enabled);
      cronJobs = await api.cronList();
    } catch { /* keep last state */ }
  }
  async function cronRemove(j: api.CronJob) {
    if (!confirm(`删除定时任务「${j.name}」及其运行记录？`)) return;
    try {
      await api.cronDelete(j.id);
      cronJobs = await api.cronList();
    } catch { /* keep last state */ }
  }
  async function cronTrigger(j: api.CronJob) {
    if (cronBusy) return;
    cronBusy = true;
    try {
      await api.cronRunNow(j.id);
      cronJobs = await api.cronList();
      if (cronRunsOpen === j.id) await loadRuns(j.id);
    } catch (e) {
      app.pushToast({ kind: "error", title: "运行失败", body: String(e) });
    }
    cronBusy = false;
  }
  async function loadRuns(jobId: string) {
    try { cronRuns = { ...cronRuns, [jobId]: await api.cronRuns(jobId) }; } catch { /* lazy */ }
  }
  function cronToggleRuns(jobId: string) {
    cronRunsOpen = cronRunsOpen === jobId ? null : jobId;
    if (cronRunsOpen) loadRuns(jobId);
  }
  function cronScheduleText(j: api.CronJob): string {
    if (j.kind === "daily") return `每天 ${j.daily_time ?? "08:00"}`;
    const m = j.interval_minutes ?? 60;
    return m % 60 === 0 ? `每 ${m / 60} 小时` : `每 ${m} 分钟`;
  }
  const cronFmt = (ms: number | null) => (ms ? new Date(ms).toLocaleString() : "—");

  // ---- mcp client (4.4) ----
  let mcpServersJson = $state("");
  let mcpBusy = $state(false);
  const MCP_PLACEHOLDER = '[{"name":"homelab","command":"npx","args":["-y","mcp-server-xxx"]}]';
  async function saveMcpServers() {
    const raw = mcpServersJson.trim() || "[]";
    try {
      const parsed = JSON.parse(raw);
      if (!Array.isArray(parsed)) throw new Error("必须是数组");
      for (const s of parsed) {
        if (!s.name || !s.command) throw new Error("每项需要 name 和 command");
        if (!/^[a-z0-9_-]+$/.test(s.name)) throw new Error(`name「${s.name}」只能用小写字母/数字/-/_`);
      }
    } catch (e) {
      app.pushToast({ kind: "error", title: "MCP 配置 JSON 无效", body: String(e) });
      return;
    }
    api.setSetting("mcp_servers", raw).catch(() => {});
    app.pushToast({ kind: "success", title: "MCP 配置已保存" });
  }
  async function mcpSync() {
    if (mcpBusy) return;
    mcpBusy = true;
    try {
      const reports = await api.mcpSyncTools();
      for (const r of reports) {
        if (r.ok) {
          app.pushToast({ kind: "success", title: `MCP ${r.server}：同步 ${r.tools} 个工具` });
        } else {
          app.pushToast({ kind: "error", title: `MCP ${r.server} 连接失败`, body: r.error ?? "" });
        }
      }
      if (reports.length === 0) {
        app.pushToast({ kind: "warning", title: "没有启用的 MCP server" });
      }
    } catch (e) {
      app.pushToast({ kind: "error", title: "同步失败", body: String(e) });
    }
    mcpBusy = false;
  }

  // ---- keys state ----
  let keys = $state({
    openrouter: "",
    gemini: "",
    claude: "",
    openai: "",
    custom_endpoint: "",
    custom_api_key: "",
  });
  // $state, not const: registry key rows (DeepSeek, Qwen, GLM, …) are merged in
  // at load, ahead of the two custom-endpoint rows.
  let keyMeta = $state<{ id: string; label: string; note: string; placeholder: string }[]>([
    { id: "openrouter", label: "OpenRouter",              note: "openrouter.ai/keys",    placeholder: "sk-or-…" },
    { id: "gemini",     label: "Gemini",                  note: "Google AI Studio",       placeholder: "AIza…" },
    { id: "claude",     label: "Claude",                  note: "console.anthropic.com",  placeholder: "sk-ant-…" },
    { id: "openai",     label: "OpenAI",                  note: "platform.openai.com",    placeholder: "sk-…" },
    { id: "custom_endpoint", label: "Custom endpoint URL", note: "OpenAI-compatible base URL", placeholder: "https://…/v1" },
    { id: "custom_api_key", label: "Custom endpoint API key", note: "Bearer token for the custom endpoint", placeholder: "sk-…" },
  ]);
  // show/hide per key
  let showKey = $state<Record<string, boolean>>({
    openrouter: false,
    gemini: false,
    claude: false,
    openai: false,
    custom_endpoint: false,
    custom_api_key: false,
  });

  // ---- plugins (plugins/*.json manifests) ----
  let plugins = $state<api.Plugin[]>([]);
  let pluginErrors = $state<string[]>([]);

  // ---- appearance state ----
  const THEME_OPTS: { id: Theme; n: string; c: string; b: string }[] = [
    { id: "osaka-jade",  n: "Osaka Jade",       c: "#2dd5b7", b: "#111c18" },
    { id: "tokyo-night", n: "Tokyo Night",      c: "#7aa2f7", b: "#1a1b26" },
    { id: "catppuccin",  n: "Catppuccin Mocha", c: "#94e2d5", b: "#1e1e2e" },
    { id: "gruvbox",     n: "Gruvbox",          c: "#fabd2f", b: "#282828" },
    { id: "nord",        n: "Nord",             c: "#88c0d0", b: "#2e3440" },
    { id: "dracula",     n: "Dracula",          c: "#bd93f9", b: "#282a36" },
    { id: "rose-pine",   n: "Rosé Pine",        c: "#ebbcba", b: "#191724" },
    { id: "everforest",  n: "Everforest",       c: "#a7c080", b: "#2d353b" },
    { id: "solarized",   n: "Solarized",        c: "#268bd2", b: "#002b36" },
    { id: "kanagawa",    n: "Kanagawa",         c: "#7e9cd8", b: "#1f1f28" },
  ];
  let readFont      = $state("mono");
  let density       = $state("regular");

  // Settings is the ONLY view that mutates the <html> root. Writing an attribute on
  // documentElement invalidates styles for the ENTIRE document, forcing WebKit
  // (WebKitGTK / iOS WKWebView) to re-match every element against the large global
  // stylesheet (~1740 rules) — ~2× slower than Blink, which is why only Settings
  // freezes/lags on WebKit. The store already applies these on boot, so these were
  // redundant no-op writes that still triggered a full-document recalc every open.
  // Only write when the value actually changes (a getAttribute read is cheap and
  // does NOT force layout); live appearance changes still apply instantly.
  $effect(() => {
    const el = document.documentElement;
    if (el.getAttribute("data-read") !== readFont) el.setAttribute("data-read", readFont);
  });
  $effect(() => {
    const el = document.documentElement;
    const v = density === "compact" ? "compact" : "regular";
    if (el.getAttribute("data-density") !== v) el.setAttribute("data-density", v);
  });

  // persist appearance on change
  $effect(() => {
    // track both values
    const rf = readFont;
    const d  = density;
    if (!loaded) return;
    api.setSettings({ reading_font: rf, density: d }).catch(() => {});
  });

  // ---- custom appearance (2.10) ----
  // The accent picker applies instantly on every input event (color swatches
  // fire rapidly — setCustomAppearance is cheap, one <style> textContent write).
  // Custom CSS is debounced: keystrokes shouldn't thrash a full-document
  // restyle mid-sentence, 500ms quiet is the sweet spot between "live" and
  // "I pressed apply".
  let customCssDraft = $state(app.customCss);
  let cssTimer: ReturnType<typeof setTimeout> | null = null;
  function onCustomCssInput() {
    if (cssTimer) clearTimeout(cssTimer);
    cssTimer = setTimeout(() => app.setCustomCss(customCssDraft), 500);
  }

  // ---- keybinds state ----
  // Binds live in the shared keybinds module (persisted + applied live).
  let listening = $state<Action | null>(null);

  function displayKey(k: string): string {
    return k === " " ? "Space" : k;
  }

  $effect(() => {
    if (!listening) return;
    (window as any).__cortexModalOpen = true;
    function onKey(e: KeyboardEvent) {
      // Ignore standalone modifier presses so the user can type a shifted key
      // (e.g. press Shift then ":") — keep listening until a real key arrives.
      if (["Shift", "Control", "Alt", "Meta", "AltGraph", "CapsLock"].includes(e.key)) {
        return;
      }
      // Swallow the event entirely so the global keyboard engine doesn't also
      // act on it (otherwise ":" would open the command palette).
      e.preventDefault();
      e.stopPropagation();
      e.stopImmediatePropagation();
      const k = e.key === "Escape" ? null : e.key;
      if (k && listening) {
        const action = listening;
        const ok = keybinds.set(action, k); // persists + applies live
        if (ok) {
          app.pushToast({
            kind: "success",
            title: "Keybind updated",
            body: `${ACTION_LABELS[action]} → “${k === " " ? "Space" : k}”. Takes effect outside this screen.`,
          });
        } else {
          app.pushToast({
            kind: "warning",
            title: "Key already in use",
            body: `“${k === " " ? "Space" : k}” is bound to another action — pick a different key.`,
          });
        }
      }
      listening = null;
      (window as any).__cortexModalOpen = false;
    }
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      (window as any).__cortexModalOpen = false;
    };
  });

  // ---- local models (ollama) + web search + remote whisper ----
  let endpoint   = $state("http://localhost:11434");
  let searxng    = $state("");
  let whisperUrl = $state("");
  let whisperModel = $state("");
  let webImages  = $state(false);
  let testState  = $state<null | "testing" | "ok" | "fail">(null);
  let searxState = $state<null | "testing" | "ok" | "fail">(null);
  let whisperState = $state<null | "testing" | "ok" | "fail">(null);

  function saveWhisper() {
    api.setSetting("whisper_url", whisperUrl.trim()).catch(() => {});
  }
  function saveWhisperModel() {
    api.setSetting("whisper_model", whisperModel.trim()).catch(() => {});
  }
  async function testWhisper() {
    if (!whisperUrl.trim()) return;
    whisperState = "testing";
    whisperState = await testEndpoint(whisperUrl);
  }
  // End-to-end whisper validation: server reachable AND the configured model
  // installed — downloading it server-side on the spot when missing, so the
  // first real lecture never pays the multi-minute cold pull.
  let whisperCheckState = $state<null | "checking" | "ok" | "fail">(null);
  let whisperCheckNote = $state("");
  async function checkWhisper() {
    whisperCheckState = "checking";
    whisperCheckNote = "Checking the server (a first-time model download can take a few minutes)…";
    try {
      whisperCheckNote = await api.checkWhisperModel();
      whisperCheckState = "ok";
    } catch (e) {
      whisperCheckNote = String(e);
      whisperCheckState = "fail";
    }
  }

  // ---- transcription mode (Settings → Integrations → Transcription) ----
  // "local" = on this machine (zero setup) · "cloud" = OpenAI-compatible API
  // with a key (Groq/OpenAI/custom) · "homelab" = the /whisper service behind
  // the Homelab URL. Unset falls back to the legacy auto behavior (homelab if
  // configured, else local) — shown as whichever of those applies.
  let transcriptionMode = $state<"local" | "cloud" | "homelab">("local");
  let whisperCloudProvider = $state<"groq" | "openai" | "custom">("groq");
  let whisperCloudUrl = $state("");
  let whisperCloudModel = $state("");
  let whisperApiKey = $state("");
  const CLOUD_WHISPER_PRESETS = {
    groq: { url: "https://api.groq.com/openai/v1", model: "whisper-large-v3-turbo" },
    openai: { url: "https://api.openai.com/v1", model: "whisper-1" },
  } as const;
  function setTranscriptionMode(m: "local" | "cloud" | "homelab") {
    transcriptionMode = m;
    whisperCheckState = null;
    whisperCheckNote = "";
    api.setSetting("transcription_mode", m).catch(() => {});
    // Entering cloud mode with everything blank: land on the Groq preset so the
    // only thing left to do is paste a key.
    if (m === "cloud" && !whisperCloudUrl.trim() && !whisperCloudModel.trim()) {
      applyCloudPreset("groq");
    }
  }
  function applyCloudPreset(p: "groq" | "openai" | "custom") {
    whisperCloudProvider = p;
    api.setSetting("whisper_cloud_provider", p).catch(() => {});
    if (p !== "custom") {
      whisperCloudUrl = CLOUD_WHISPER_PRESETS[p].url;
      whisperCloudModel = CLOUD_WHISPER_PRESETS[p].model;
      saveCloudWhisper();
    }
  }
  function saveCloudWhisper() {
    api.setSettings({
      whisper_cloud_url: whisperCloudUrl.trim(),
      whisper_cloud_model: whisperCloudModel.trim(),
      whisper_api_key: whisperApiKey.trim(),
    }).catch(() => {});
  }

  // ---- homelab access token (auth for every service except /sync) ----
  let hlToken = $state("");
  function saveHlToken() {
    api.setSetting("homelab_token", hlToken.trim()).catch(() => {});
  }

  // ---- unified homelab access ----
  // One base URL fronts every service (search / whisper / ollama / sync) behind
  // a reverse proxy; Cortex appends each service's path. Tailscale/public bases
  // are the auto local→Tailscale→public fallbacks.
  let hlBase      = $state("");
  let hlTailscale = $state("");
  let hlPublic    = $state("");
  let hlState     = $state<"idle" | "testing" | "ok" | "fail">("idle");
  let hlReach     = $state<Record<string, "ok" | "fail">>({});
  let hlSaveTimer: ReturnType<typeof setTimeout> | undefined;

  // Real app version for the footer (the Tauri build version), loaded once.
  let appVersion = $state("");
  $effect(() => { void getVersion().then((v) => (appVersion = v)).catch(() => {}); });
  function saveHomelabBases() {
    clearTimeout(hlSaveTimer);
    api.setSettings({
      homelab_base: hlBase.trim(),
      homelab_tailscale_base: hlTailscale.trim(),
      homelab_public_base: hlPublic.trim(),
    }).catch(() => {});
  }
  // iOS WKWebView fires change/blur unreliably (and "Sync now" lives in another
  // section), so the base could stay unpersisted → sync reports "target not set".
  // Persist shortly after each keystroke so the URL is always in the DB by sync time.
  function saveHomelabBasesSoon() {
    clearTimeout(hlSaveTimer);
    hlSaveTimer = setTimeout(saveHomelabBases, 400);
  }
  // Per-service health grid (SearXNG / WhisperX / WebDAV / syncd / ingest / Ollama),
  // probed through the SAME resolved URLs the app actually uses.
  let svcStatus = $state<api.HomelabServiceStatus[] | null>(null);
  let svcTesting = $state(false);
  async function testHomelab() {
    const tiers: [string, string][] = [["local", hlBase], ["tailscale", hlTailscale], ["public", hlPublic]];
    if (!tiers.some(([, u]) => u.trim())) return;
    hlState = "testing";
    svcTesting = true;
    const reach: Record<string, "ok" | "fail"> = {};
    for (const [tier, url] of tiers) {
      if (!url.trim()) continue;
      reach[tier] = await testEndpoint(url.trim());
    }
    hlReach = reach;
    hlState = Object.values(reach).some((r) => r === "ok") ? "ok" : "fail";
    // Then check each service individually so a broken one is named, not guessed.
    try { svcStatus = await api.homelabStatus(); } catch { svcStatus = null; }
    svcTesting = false;
  }
  // Whether the homelab's live-sync service answered on the last test (null = unknown).
  const syncdOk = $derived(svcStatus?.find((s) => s.id === "syncd")?.ok ?? null);

  // ---- live homelab sync (smart per-record merge + binary file sync) ----
  // Three endpoint tiers tried in order (auto): local LAN → Tailscale → public.
  let syncUrl   = $state(""); // local / LAN
  let syncUrlTs = $state(""); // tailscale
  let syncUrlPub = $state(""); // public
  let syncMode  = $state<"auto" | "local" | "tailscale" | "public">("auto");
  let syncUser  = $state("");
  let syncPass  = $state("");
  let syncOn    = $state(false);
  let syncTestState = $state<null | "testing" | "ok" | "fail">(null);
  // Per-endpoint reachability for the Test-all button.
  let syncReach = $state<Record<string, boolean>>({});

  const anySyncUrl = $derived(!!(syncUrl.trim() || syncUrlTs.trim() || syncUrlPub.trim()));
  // On mobile the per-tier sync URL fields are hidden — live sync rides the single
  // Homelab base URL (→ its /sync WebDAV). So the base alone must be enough to enable
  // the toggle; gating only on anySyncUrl left mobile stuck on "live sync off".
  const canSync = $derived(anySyncUrl || !!hlBase.trim() || !!hlTailscale.trim() || !!hlPublic.trim());

  let syncSaveTimer: ReturnType<typeof setTimeout> | undefined;
  function saveSync() {
    clearTimeout(syncSaveTimer);
    api.setSettings({
      sync_url: syncUrl.trim(),
      sync_url_tailscale: syncUrlTs.trim(),
      sync_url_public: syncUrlPub.trim(),
      sync_mode: syncMode,
      sync_user: syncUser.trim(),
      sync_pass: syncPass,
      sync_enabled: syncOn ? "true" : "false",
    }).then(() => app.loadSyncStatus()).catch(() => {});
  }
  // Same iOS blur/change unreliability as the homelab bases — persist creds as typed.
  function saveSyncSoon() {
    clearTimeout(syncSaveTimer);
    syncSaveTimer = setTimeout(saveSync, 400);
  }
  function toggleSync() {
    syncOn = !syncOn;
    if (syncOn && !canSync) { syncOn = false; return; }
    saveSync();
  }
  function setSyncMode(m: "auto" | "local" | "tailscale" | "public") {
    syncMode = m;
    saveSync();
  }
  async function testSync() {
    if (!anySyncUrl) return;
    syncTestState = "testing";
    const reach: Record<string, boolean> = {};
    const tiers: [string, string][] = [["local", syncUrl], ["tailscale", syncUrlTs], ["public", syncUrlPub]];
    for (const [tier, url] of tiers) {
      if (!url.trim()) continue;
      try { reach[tier] = await api.syncTest(url.trim(), syncUser.trim(), syncPass); }
      catch { reach[tier] = false; }
    }
    syncReach = reach;
    try {
      syncTestState = Object.values(reach).some(Boolean) ? "ok" : "fail";
    } catch {
      syncTestState = "fail";
    }
  }
  function fmtSyncTime(ms: number): string {
    return ms ? new Date(ms).toLocaleString() : "never";
  }
  function syncPill() {
    // Live (WebSocket) trumps the periodic states — changes are propagating
    // across devices within about a second.
    if (app.syncLive && app.syncState !== "off" && app.syncState !== "error") {
      return { cls: "ready", label: "Live" };
    }
    switch (app.syncState) {
      case "syncing": return { cls: "draft", label: "Syncing…" };
      case "synced":  return { cls: "ready", label: "Synced" };
      case "error":   return { cls: "error", label: "Sync error" };
      case "idle":    return { cls: "ready", label: "On" };
      default:        return { cls: "pending", label: "Off" };
    }
  }

  // ---- Browser clip extension ----
  // The loopback server only exists while the desktop app is running, so the
  // row reads its liveness from the backend instead of assuming it is up.
  let clipStatus = $state<api.ClipServerStatus>({ running: false, addr: null });

  // ---- Experimental features ----
  let expMoodle = $state(false);
  function toggleExpMoodle() {
    expMoodle = !expMoodle;
    api.setSetting("exp_moodle", expMoodle ? "true" : "false").catch(() => {});
  }

  // ---- Moodle integration ----
  let mdUrl   = $state("");
  let mdUser  = $state("");
  let mdPass  = $state("");
  let mdToken = $state("");
  let mdAuthMode = $state<"password" | "token">("token"); // SU uses Microsoft SSO → token paste
  let mdBusy  = $state(false);
  let mdStatus = $state<api.MoodleStatus>({ configured: false, user_id: 0, last_sync: 0 });
  let mdData = $state<api.MoodleData>({ courses: [], grades: [], deadlines: [], announcements: [] });
  let mdSummary = $state<api.MoodleSummary | null>(null);
  // 即将到来的截止日 (future, soonest first) for the synced-data preview.
  const mdUpcoming = $derived(
    mdData.deadlines
      .filter((d) => d.due_at * 1000 >= Date.now() - 12 * 3600 * 1000)
      .sort((a, b) => a.due_at - b.due_at)
  );

  async function loadMoodle() {
    try {
      mdStatus = await api.moodleStatus();
      if (mdStatus.configured) mdData = await api.moodleData();
    } catch { /* not configured yet */ }
  }
  async function mdConnect() {
    mdBusy = true;
    try {
      const name = mdAuthMode === "password"
        ? await api.moodleConnect(mdUrl, mdUser.trim(), mdPass)
        : await api.moodleSetToken(mdUrl, mdToken);
      mdPass = ""; // never keep the password around
      app.pushToast({ kind: "success", title: "Moodle connected", body: name || undefined });
      await loadMoodle();
    } catch (e) {
      app.pushToast({ kind: "error", title: "Moodle connect failed", body: String(e) });
    } finally { mdBusy = false; }
  }
  async function mdSyncNow() {
    mdBusy = true;
    try {
      mdSummary = await api.moodleSync();
      app.pushToast({ kind: "success", title: "Moodle synced",
        body: `${mdSummary.courses} courses · ${mdSummary.grades} grades · ${mdSummary.deadlines} deadlines · ${mdSummary.announcements} announcements` });
      await loadMoodle();
    } catch (e) {
      app.pushToast({ kind: "error", title: "Moodle sync failed", body: String(e) });
    } finally { mdBusy = false; }
  }
  async function mdAutolink() {
    try {
      const n = await api.moodleAutolink();
      app.pushToast({ kind: n ? "success" : "info", title: `Auto-linked ${n} subject${n === 1 ? "" : "s"}` });
      await app.refresh();
    } catch (e) {
      app.pushToast({ kind: "error", title: "Auto-link failed", body: String(e) });
    }
  }
  async function mdDisconnect() {
    try {
      await api.moodleDisconnect();
      mdToken = "";
      await loadMoodle();
      app.pushToast({ kind: "info", title: "Moodle disconnected" });
    } catch { /* ignore */ }
  }
  function mdCourseName(courseId: string): string {
    return mdData.courses.find((c) => c.id === courseId)?.fullname || courseId;
  }
  async function mdLoginSso() {
    try {
      await api.moodleLoginSso(mdUrl);
    } catch (e) {
      app.pushToast({ kind: "error", title: "Could not open SSO login", body: String(e) });
    }
  }
  // SSO login happens in a separate window; react to its result here.
  $effect(() => {
    let un1: (() => void) | undefined;
    let un2: (() => void) | undefined;
    api.onMoodleSsoDone(async (name) => {
      app.pushToast({ kind: "success", title: "Moodle connected", body: name || undefined });
      await loadMoodle();
    }).then((u) => (un1 = u));
    api.onMoodleSsoError((msg) =>
      app.pushToast({ kind: "error", title: "SSO login failed", body: msg })
    ).then((u) => (un2 = u));
    return () => { un1?.(); un2?.(); };
  });

  // Diagrams/images from the homelab SearXNG. Mirrors app.webImagesEnabled and
  // persists an explicit choice so it survives the "default-on-when-connected".
  function setWebImages(on: boolean) {
    webImages = on;
    app.webImagesEnabled = on;
    api.setSetting("web_images_enabled", on ? "true" : "false").catch(() => {});
  }

  // ---- encrypted backups (age + rclone) ----
  let ageRecipient = $state("");
  let rcloneRemote = $state("");
  let backupInfo   = $state<api.BackupStatus | null>(null);
  let backingUp    = $state(false);
  async function refreshBackupStatus() {
    try { backupInfo = await api.backupStatus(); } catch { /* non-fatal */ }
  }
  function saveBackupConfig() {
    api.setSettings({
      backup_age_recipient: ageRecipient.trim(),
      backup_rclone_remote: rcloneRemote.trim(),
    }).then(refreshBackupStatus).catch(() => {});
  }
  async function runBackup() {
    if (backingUp) return;
    backingUp = true;
    try {
      const dest = await api.backupNow();
      app.pushToast({ kind: "success", title: "Backup uploaded", body: dest });
      await refreshBackupStatus();
    } catch (e) {
      app.pushToast({ kind: "error", title: "Backup failed", body: String(e) });
    } finally {
      backingUp = false;
    }
  }
  // ---- portable archive (.cortex-pkg.tar.gz, P0 #2) ----
  let pkgBusy = $state<null | "export" | "import" | "verify">(null);
  let pkgInfo = $state<api.PkgExportResult | null>(null);
  // Recovery directories (.deleted-<id>) — Gloss-style delete safety net.
  let deleted = $state<api.DeletedSubject[]>([]);
  let recoveryBusy = $state<string | null>(null);
  async function loadDeleted() {
    deleted = await api.recoveryList().catch(() => []);
  }
  let offsetBusy = $state(false);
  async function runBackfillOffsets() {
    if (offsetBusy) return;
    offsetBusy = true;
    try {
      const n = await api.backfillChunkOffsets();
      app.pushToast({
        kind: "success",
        title: n > 0 ? `已为 ${n} 个旧分块补上字符偏移` : "无需补偿",
        body: n > 0 ? "引用校验现在可切片复核（MCP 与证据面板均已显示）" : "所有分块都已有准确偏移",
      });
    } catch (e) {
      app.pushToast({ kind: "error", title: "补偿失败", body: String(e) });
    } finally {
      offsetBusy = false;
    }
  }
  async function restoreDeleted(d: api.DeletedSubject) {
    if (recoveryBusy) return;
    recoveryBusy = d.dir_name;
    try {
      const [name, src, note] = await api.recoveryRestore(d.dir_name);
      app.pushToast({ kind: "success", title: `已恢复「${name}」`, body: `${src} 个来源 · ${note} 条笔记` });
      await loadDeleted();
    } catch (e) {
      app.pushToast({ kind: "error", title: "恢复失败", body: String(e) });
    } finally {
      recoveryBusy = null;
    }
  }
  async function purgeDeleted(d: api.DeletedSubject) {
    if (recoveryBusy) return;
    const ok = await app.confirm?.({
      title: "彻底删除？",
      body: `将永久删除「${d.subject_name}」的恢复目录（含 ${d.sources} 个来源、${d.notes} 条笔记），不可恢复。`,
    });
    if (ok !== true) return;   // destructive: only an explicit yes proceeds
    recoveryBusy = d.dir_name;
    try {
      await api.recoveryPurge(d.dir_name);
      await loadDeleted();
    } catch (e) {
      app.pushToast({ kind: "error", title: "删除失败", body: String(e) });
    } finally {
      recoveryBusy = null;
    }
  }

  async function exportPkg() {
    if (pkgBusy) return;
    pkgBusy = "export";
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const dest = await save({
        defaultPath: "cortex-library.cortex-pkg.tar.gz",
        filters: [{ name: "Cortex 归档", extensions: ["gz", "tar.gz"] }],
      });
      if (!dest) return;
      const r = await api.pkgExport(dest, true);
      pkgInfo = r;
      app.pushToast({
        kind: "success",
        title: "归档已导出",
        body: `${r.files} 个附件 · ${(r.bytes / 1048576).toFixed(1)} MB · sha256 ${r.sha256.slice(0, 12)}…`,
      });
    } catch (e) {
      app.pushToast({ kind: "error", title: "导出失败", body: String(e) });
    } finally {
      pkgBusy = null;
    }
  }
  async function verifyPkg() {
    if (pkgBusy) return;
    pkgBusy = "verify";
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const path = await open({ multiple: false, filters: [{ name: "Cortex 归档", extensions: ["gz", "tar.gz"] }] });
      if (!path || typeof path !== "string") return;
      const r = await api.pkgVerify(path);
      app.pushToast({
        kind: r.ok ? "success" : "error",
        title: r.ok ? "归档完整" : "归档有问题",
        body: r.ok
          ? `${r.files} 个文件校验通过 · ${r.counts?.sources ?? 0} 个来源`
          : r.problems.slice(0, 3).join("；"),
      });
    } catch (e) {
      app.pushToast({ kind: "error", title: "校验失败", body: String(e) });
    } finally {
      pkgBusy = null;
    }
  }
  async function importPkg() {
    if (pkgBusy) return;
    pkgBusy = "import";
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const path = await open({ multiple: false, filters: [{ name: "Cortex 归档", extensions: ["gz", "tar.gz"] }] });
      if (!path || typeof path !== "string") return;
      const target = await open({ directory: true, multiple: false, title: "选择导入到哪个空目录" });
      if (!target || typeof target !== "string") return;
      const r = await api.pkgImport(path, target, false);
      app.pushToast({
        kind: "success",
        title: "归档已导入",
        body: `${r.files} 个附件 · ${r.counts.sources} 个来源 · ${r.counts.notes} 条笔记 → ${target}`,
      });
    } catch (e) {
      app.pushToast({ kind: "error", title: "导入失败（未通过校验则不写入）", body: String(e) });
    } finally {
      pkgBusy = null;
    }
  }

  function fmtBackupTime(ms: number | null): string {
    if (!ms) return "never";
    return new Date(ms).toLocaleString();
  }

  // Last transport error from a failed reachability test, so the homelab card can
  // explain WHY (e.g. iOS blocking cleartext LAN, DNS, timeout) instead of "fail".
  let lastTestError = $state("");
  async function testEndpoint(url: string): Promise<"ok" | "fail"> {
    try {
      const ok = await api.pingUrl(url);
      lastTestError = ok ? "" : "Reached the server, but it didn't return a success status.";
      return ok ? "ok" : "fail";
    } catch (e) {
      lastTestError = String(e);
      return "fail";
    }
  }

  async function testConnection() {
    testState = "testing";
    testState = await testEndpoint(endpoint);
  }

  function saveSearxng() {
    api.setSetting("searxng_url", searxng).catch(() => {});
  }

  // ---- Google Calendar ----
  let gClientId = $state("");
  let gClientSecret = $state("");
  let gStatus = $state<api.GoogleStatus | null>(null);
  let gBusy = $state(false);
  $effect(() => {
    if (tab === "calendar" && gStatus === null) loadGoogle();
    if (tab === "homelab" && depReport === null) loadDeps();
    if (tab === "data") loadArchived();
    if (tab === "cron" && cronJobs === null) loadCron();
  });

  // ---- external dependency status ----
  let depReport = $state<api.DependencyReport | null>(null);
  let depLoading = $state(false);
  async function loadDeps() {
    depLoading = true;
    try { depReport = await api.dependencyStatus(); }
    catch { depReport = null; }
    finally { depLoading = false; }
  }
  async function copyDepCmd() {
    if (!depReport?.install_command) return;
    try {
      await navigator.clipboard.writeText(depReport.install_command);
      app.pushToast({ kind: "success", title: "Copied", body: "Install command copied to clipboard." });
    } catch {
      app.pushToast({ kind: "warning", title: "Couldn't copy", body: depReport.install_command });
    }
  }
  // One-click install (macOS/Homebrew only — no sudo needed there).
  let depInstalling = $state(false);
  async function installDeps() {
    depInstalling = true;
    try {
      const msg = await api.installDependencies();
      app.pushToast({ kind: "success", title: "Dependencies", body: msg });
      await loadDeps();
    } catch (e) {
      app.pushToast({ kind: "error", title: "Install failed", body: String(e) });
    } finally {
      depInstalling = false;
    }
  }
  let gCalendars = $state<api.GoogleCalendar[]>([]);
  let gCalBusy = $state(false);
  async function loadGoogle() {
    try {
      gClientId = (await api.getSetting("google_client_id")) ?? "";
      gClientSecret = (await api.getSetting("google_client_secret")) ?? "";
      gStatus = await api.googleStatus();
      if (gStatus.connected) void loadGoogleCalendars();
    } catch {
      gStatus = { connected: false, email: null, configured: false };
    }
  }
  async function loadGoogleCalendars() {
    if (!gStatus?.connected) return;
    gCalBusy = true;
    try { gCalendars = await api.googleListCalendars(); }
    catch (e) { app.pushToast({ kind: "error", title: "Couldn't list calendars", body: String(e) }); }
    finally { gCalBusy = false; }
  }
  function toggleGoogleCal(id: string) {
    gCalendars = gCalendars.map((c) => (c.id === id ? { ...c, selected: !c.selected } : c));
    const csv = gCalendars.filter((c) => c.selected).map((c) => c.id).join(",");
    api.setSettings({ google_pull_calendars: csv }).catch(() => {});
  }
  function saveGoogleCreds() {
    api.setSettings({
      google_client_id: gClientId.trim(),
      google_client_secret: gClientSecret.trim(),
    }).catch(() => {});
  }
  async function connectGoogle() {
    gBusy = true;
    try {
      saveGoogleCreds();
      gStatus = await api.googleConnect();
      app.pushToast({ kind: "success", title: "Google Calendar connected", body: gStatus.email ?? undefined });
      void loadGoogleCalendars();
    } catch (e) {
      app.pushToast({ kind: "error", title: "Connect failed", body: String(e) });
    } finally {
      gBusy = false;
    }
  }
  async function syncGoogle() {
    gBusy = true;
    try {
      const r = await api.googleSync();
      await app.refresh();            // pulled events filed to subjects; colours updated
      app.notifyEventsChanged();      // refresh the calendar view
      app.pushToast({ kind: "success", title: "Calendar synced", body: `${r.pulled} pulled · ${r.pushed} pushed` });
    } catch (e) {
      app.pushToast({ kind: "error", title: "Sync failed", body: String(e) });
    } finally {
      gBusy = false;
    }
  }
  async function disconnectGoogle() {
    try {
      gStatus = await api.googleDisconnect();
      app.pushToast({ kind: "info", title: "Google disconnected" });
    } catch (e) {
      app.pushToast({ kind: "error", title: "Disconnect failed", body: String(e) });
    }
  }

  async function testSearxng() {
    if (!searxng.trim()) return;
    searxState = "testing";
    searxState = await testEndpoint(searxng);
  }

  // Map a connection test state + whether the endpoint is set to a design-system
  // status pill (class modifier + label), so every integration shows state the
  // same way the rest of the app shows source/cheatsheet state.
  function connPill(state: null | "testing" | "ok" | "fail", configured: boolean) {
    if (state === "testing") return { cls: "draft", label: "Checking…" };
    if (state === "ok") return { cls: "ready", label: "Connected" };
    if (state === "fail") return { cls: "error", label: "Unreachable" };
    return configured
      ? { cls: "pending", label: "Untested" }
      : { cls: "pending", label: "Not set" };
  }

  // persist the Ollama endpoint on change, and re-probe its installed models
  $effect(() => {
    const ep = endpoint;
    if (!loaded) return;
    api.setSettings({ ollama_url: ep }).catch(() => {});
    invalidateOllamaModels();
  });

  // ---- focus timer (pomodoro) durations — bound to the app-wide timer ----
  const pomoFields = [
    { key: "workMin",            label: "Focus length",  unit: " min", step: 5, min: 5, max: 90 },
    { key: "breakMin",           label: "Short break",   unit: " min", step: 1, min: 1, max: 30 },
    { key: "longBreakMin",       label: "Long break",    unit: " min", step: 5, min: 5, max: 60 },
    { key: "sessionsBeforeLong", label: "Sessions / set", unit: "",    step: 1, min: 2, max: 8  },
  ] as const;
  function pomoVal(key: string): number {
    return (app.pomo as unknown as Record<string, number>)[key];
  }
  function setPomo(key: string, delta: number) {
    const f = pomoFields.find((x) => x.key === key)!;
    const next = Math.max(f.min, Math.min(f.max, pomoVal(key) + delta));
    (app.pomo as unknown as Record<string, number>)[key] = next;
    api.setSettings({ ["pomo_" + key]: String(next) }).catch(() => {});
  }

  // ---- audio state ----
  let autoplay = $state(false);
  let station  = $state("lofi");
  let voiceA   = $state("maya");
  let voiceB   = $state("theo");

  // Set true only once the mount hydration below has loaded persisted values.
  // The persist $effects fire immediately on mount with their *default* values;
  // without this guard, opening Settings would clobber saved settings (e.g. it
  // silently reset default_station to "lofi" on every visit).
  let loaded = $state(false);

  // Built-in + user-added stations, so the Default-station picker shows them all.
  const allStations = $derived([
    ...stations.map((s) => ({ id: s.id, label: s.name })),
    ...app.customStations.map((s) => ({ id: s.id, label: s.name })),
  ]);

  // Stream-tool detection for the YouTube-audio engine (mpv sidecar).
  let mediaTools = $state<api.MediaTools | null>(null);
  $effect(() => {
    if (tab === "audio" && mediaTools === null) {
      api.mediaToolsStatus().then((t) => (mediaTools = t)).catch(() => {});
    }
  });

  // persist audio on change. NOTE: never read app.music here — reading + writing
  // it in one effect creates a self-triggering reactive loop (crashes the view).
  // The live-player sync happens in the Picker's onChange handler instead.
  $effect(() => {
    const st = station;
    const ap = autoplay;
    if (!loaded) return;
    api.setSettings({ default_station: st, autoplay: String(ap) }).catch(() => {});
  });

  // persist host voices on change
  $effect(() => {
    const a = voiceA;
    const b = voiceB;
    if (!loaded) return;
    api.setSettings({ voice_a: a, voice_b: b }).catch(() => {});
  });

  // ---- window behaviour ----
  // Default ON: closing the window hides to the tray so ingest/generation/music
  // keep running; the backend treats anything but "false" as enabled.
  let closeToTray = $state(true);
  function toggleCloseToTray() {
    closeToTray = !closeToTray;
    api.setSetting("close_to_tray", closeToTray ? "true" : "false").catch(() => {});
  }

  // ---- data & privacy state ----
  let offlineMode = $state(false);
  let stats       = $state<api.DbStats | null>(null);
  let archivedSubjects = $state<api.Subject[]>([]);

  async function loadArchived() {
    archivedSubjects = await app.listArchivedSubjects();
  }
  async function restoreSubject(id: string) {
    await app.setSubjectArchived(id, false);
    await Promise.all([loadArchived(), loadStats()]);
  }

  // ---- undo / recycle bin ----
  // Subjects/topics/sources/notes/materials that were deleted recently are
  // journaled by the Rust backend (delete_*_with_undo); each entry holds a JSON
  // snapshot so `restore_undo` can put the rows (and their chunks/tags) back.
  // Entries auto-expire after UNDO_TTL_MS (7 days, set on the Rust side).
  let undoEntries   = $state<api.UndoEntry[]>([]);
  let undoLoading   = $state(false);
  let undoBusy      = $state<string | null>(null);
  let undoError     = $state<string | null>(null);

  async function loadUndo() {
    undoLoading = true;
    undoError   = null;
    try {
      undoEntries = await api.listUndoEntries(50);
    } catch (e: any) {
      undoError = String(e?.message ?? e);
      undoEntries = [];
    } finally {
      undoLoading = false;
    }
  }
  async function runRestore(id: string) {
    undoBusy = id;
    undoError = null;
    try {
      const label = await api.restoreUndo(id);
      app.pushToast({
        kind: "success",
        title: "Restored",
        body: `Restored "${label}".`,
      });
      await Promise.all([loadUndo(), loadArchived(), loadStats()]);
    } catch (e: any) {
      undoError = String(e?.message ?? e);
      app.pushToast({
        kind: "warning",
        title: "Couldn't restore",
        body: undoError ?? "Unknown error",
      });
    } finally {
      undoBusy = null;
    }
  }
  function undoAge(ms: number): string {
    const ageMs = Date.now() - ms;
    if (ageMs < 60_000) return "just now";
    const m = Math.floor(ageMs / 60_000);
    if (m < 60) return m + "m ago";
    const h = Math.floor(m / 60);
    if (h < 24) return h + "h ago";
    const d = Math.floor(h / 24);
    return d + "d ago";
  }
  function undoExpiry(ms: number): string {
    const left = ms - Date.now();
    if (left <= 0) return "expired";
    const d = Math.floor(left / 86_400_000);
    if (d >= 1) return d + "d left";
    const h = Math.floor(left / 3_600_000);
    return h + "h left";
  }

  function fmtBytes(n: number): string {
    if (n >= 1024 * 1024 * 1024) return (n / (1024 * 1024 * 1024)).toFixed(1) + " GB";
    if (n >= 1024 * 1024)        return (n / (1024 * 1024)).toFixed(1) + " MB";
    if (n >= 1024)               return (n / 1024).toFixed(0) + " KB";
    return n + " B";
  }

  async function loadStats() {
    stats = await api.dbStats().catch(() => null);
  }

  function toggleOffline() {
    offlineMode = !offlineMode;
    api.setSetting("offline_mode", offlineMode ? "true" : "false").catch(() => {});
  }

  async function clearCaches() {
    testState = null;
    searxState = null;
    try {
      await api.optimizeDb();
      await loadStats();
      app.pushToast({ kind: "success", title: "Storage optimized", body: "Reclaimed unused space (VACUUM)." });
    } catch (e) {
      app.pushToast({ kind: "error", title: "Optimize failed", body: String(e) });
    }
  }

  async function exportObsidian() {
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const directory = await open({ directory: true, multiple: false, title: "选择 Obsidian 仓库或导出目录" });
      if (typeof directory !== "string") return;
      const dest = await api.exportObsidian(directory);
      app.pushToast({ kind: "success", title: "Obsidian 导出完成", body: dest });
    } catch (e) { app.pushToast({ kind: "error", title: "导出失败", body: String(e) }); }
  }

  async function exportData() {
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const dest = await save({
        defaultPath: "cortex-export.db",
        filters: [{ name: "SQLite database", extensions: ["db"] }],
      });
      if (!dest) return;
      await api.exportDatabase(dest);
      app.pushToast({ kind: "success", title: "Exported", body: dest });
    } catch (e) {
      app.pushToast({ kind: "error", title: "Export failed", body: String(e) });
    }
  }

  async function deleteEverything() {
    const ok = window.confirm(
      "Delete ALL data?\n\nThis wipes the local database — every subject, source, cheatsheet, and embedding. This cannot be undone. Your settings and API keys are kept.",
    );
    if (!ok) return;
    try {
      await api.deleteAllData();
      await loadStats();
      app.pushToast({ kind: "success", title: "All data deleted", body: "Reload the app to start fresh." });
    } catch {
      app.pushToast({ kind: "error", title: "Delete failed" });
    }
  }

  // ---- mount: hydrate from backend ----
  $effect(() => {
    // All three are independent; issue them concurrently.
    (async () => {
      const [s] = await Promise.all([
        api.getAllSettings().catch(() => ({}) as Record<string, string>),
        loadMemory(),
        loadStats(),
      ]);
      // API keys
      if (s.openrouter_api_key) keys = { ...keys, openrouter: s.openrouter_api_key };
      if (s.gemini_api_key)     keys = { ...keys, gemini: s.gemini_api_key };
      if (s.claude_api_key)     keys = { ...keys, claude: s.claude_api_key };
      if (s.openai_api_key)     keys = { ...keys, openai: s.openai_api_key };
      if (s.custom_endpoint)    keys = { ...keys, custom_endpoint: s.custom_endpoint };
      if (s.custom_api_key)     keys = { ...keys, custom_api_key: s.custom_api_key };

      // OpenAI-compatible registry (DeepSeek, Qwen, GLM, Kimi, …) — merged into
      // the provider dropdowns and the key list. Deduped against the curated
      // entries; chat-only providers are excluded from the embed list (offering
      // them for retrieval would just 404 at index time).
      try {
        const reg = await api.listProviders();
        const chatNew = reg
          .filter((p) => !PROVIDERS.some((x) => x.id === p.id))
          .map((p) => ({ id: p.id, label: p.label, models: p.chat_models.map(([id, label]) => ({ id, label })) }));
        const chatAt = PROVIDERS.findIndex((x) => x.id === "ollama");
        PROVIDERS = [...PROVIDERS.slice(0, chatAt), ...chatNew, ...PROVIDERS.slice(chatAt)];
        const embedNew = reg
          .filter((p) => p.embed_model && !EMBED_PROVIDERS.some((x) => x.id === p.id))
          .map((p) => ({ id: p.id, label: p.label, models: [{ id: p.embed_model as string, label: p.embed_model as string }] }));
        const embedAt = EMBED_PROVIDERS.findIndex((x) => x.id === "ollama");
        EMBED_PROVIDERS = [...EMBED_PROVIDERS.slice(0, embedAt), ...embedNew, ...EMBED_PROVIDERS.slice(embedAt)];
        // Key rows: registry providers ahead of the two custom-endpoint rows.
        const kmNew = reg
          .filter((p) => !keyMeta.some((k) => k.id === p.id))
          .map((p) => ({ id: p.id, label: p.label, note: p.key_note, placeholder: "sk-…" }));
        const kmAt = keyMeta.findIndex((k) => k.id === "custom_endpoint");
        keyMeta = [...keyMeta.slice(0, kmAt), ...kmNew, ...keyMeta.slice(kmAt)];
        // Hydrate any stored registry keys in the same pass.
        const regKeys: Record<string, string> = {};
        for (const p of reg) {
          const v = s[`${p.id}_api_key`];
          if (v) regKeys[p.id] = v;
        }
        keys = { ...keys, ...regKeys };
      } catch { /* registry is additive; the curated lists above still work */ }

      // plugins/*.json manifests — shown in the keys tab's 插件 card.
      try {
        const pl = await api.pluginsList();
        plugins = pl.plugins;
        pluginErrors = pl.errors;
      } catch { /* plugin listing is informational only */ }

      // Subjects for the real-corpus retrieval eval picker (experimental tab).
      try { evalSubjects = await api.listSubjects(); } catch { evalSubjects = []; }

      // Models
      for (const taskId of ["chat","cheatsheet","audio","quiz","flashcard","embedding"] as TaskId[]) {
        const raw = s[`model_${taskId}`];
        if (raw) {
          const sep = raw.indexOf(":");
          if (sep !== -1) {
            const prov  = raw.slice(0, sep);
            const model = raw.slice(sep + 1);
            assign = { ...assign, [taskId]: { ...assign[taskId], provider: prov, model } };
          }
        }
        const budget = s[`budget_${taskId}`];
        if (budget) {
          assign = { ...assign, [taskId]: { ...assign[taskId], budget } };
        }
      }
      // Backend defaults to the on-device ONNX embedder when unset — mirror that.
      assign = { ...assign, embedding: { ...assign.embedding, provider: s.embed_provider || "onnx" } };
      if (s.model_speech) speechModel = s.model_speech;
      if (s.tts_endpoint) ttsEndpoint = s.tts_endpoint;
      if (s.tts_api_key)  ttsKey = s.tts_api_key;
      if (s.embed_model)  embedOv = s.embed_model;
      if (s.pdf_engine === "auto" || s.pdf_engine === "docling" || s.pdf_engine === "poppler") pdfEngine = s.pdf_engine;
      if (s.docling_bin)  doclingBin = s.docling_bin;
      if (s.economy_mode === "1" || s.economy_mode === "true" || s.economy_mode === "on") economyOn = true;
      if (s.model_cheap)  economySpec = s.model_cheap;
      if (s.mcp_servers)  mcpServersJson = s.mcp_servers;

      // Local models + web search + remote whisper
      if (s.ollama_url)                    endpoint = s.ollama_url;
      if (s.searxng_url)                   searxng  = s.searxng_url;
      if (s.whisper_url)                   whisperUrl = s.whisper_url;
      if (s.whisper_model)                 whisperModel = s.whisper_model;
      // Transcription mode + cloud provider. Unset mode = legacy auto: homelab
      // when any whisper/homelab URL is configured, otherwise local.
      if (s.transcription_mode === "local" || s.transcription_mode === "cloud" || s.transcription_mode === "homelab") {
        transcriptionMode = s.transcription_mode;
      } else {
        transcriptionMode =
          (s.whisper_url || s.homelab_base || s.homelab_tailscale_base || s.homelab_public_base)
            ? "homelab" : "local";
      }
      if (s.whisper_cloud_provider === "groq" || s.whisper_cloud_provider === "openai" || s.whisper_cloud_provider === "custom") whisperCloudProvider = s.whisper_cloud_provider;
      if (s.whisper_cloud_url)   whisperCloudUrl = s.whisper_cloud_url;
      if (s.whisper_cloud_model) whisperCloudModel = s.whisper_cloud_model;
      if (s.whisper_api_key)     whisperApiKey = s.whisper_api_key;
      if (s.homelab_token)       hlToken = s.homelab_token;
      // Live sync
      if (s.homelab_base)           hlBase = s.homelab_base;
      if (s.homelab_tailscale_base) hlTailscale = s.homelab_tailscale_base;
      if (s.homelab_public_base)    hlPublic = s.homelab_public_base;
      if (s.sync_url)   syncUrl  = s.sync_url;
      if (s.sync_url_tailscale) syncUrlTs = s.sync_url_tailscale;
      if (s.sync_url_public)    syncUrlPub = s.sync_url_public;
      if (s.sync_mode === "auto" || s.sync_mode === "local" || s.sync_mode === "tailscale" || s.sync_mode === "public") syncMode = s.sync_mode;
      if (s.sync_user)  syncUser = s.sync_user;
      if (s.sync_pass)  syncPass = s.sync_pass;
      syncOn = s.sync_enabled === "true";
      // Encrypted backups
      if (s.backup_age_recipient) ageRecipient = s.backup_age_recipient;
      if (s.backup_rclone_remote) rcloneRemote = s.backup_rclone_remote;
      refreshBackupStatus();

      // Appearance
      if (s.reading_font)   readFont      = s.reading_font;
      if (s.density)        density       = s.density;
      // Window behaviour (default ON: closing hides to the tray)
      if (s.close_to_tray !== undefined) closeToTray = s.close_to_tray !== "false";

      // Audio: default station, autoplay, host voices
      if (s.default_station) station = s.default_station;
      if (s.autoplay !== undefined) autoplay = s.autoplay === "true";
      if (s.voice_a) voiceA = s.voice_a;
      if (s.voice_b) voiceB = s.voice_b;

      // Homelab: web images (diagrams) toggle mirrors the store's resolved value.
      webImages = app.webImagesEnabled;

      // Data & privacy
      if (s.offline_mode !== undefined) offlineMode = s.offline_mode === "true";

      // Profile
      if (s.profile_name)      name     = s.profile_name;
      if (s.profile_pronouns)  pronouns = s.profile_pronouns;
      if (s.profile_level)     level    = s.profile_level;
      if (s.profile_field)     field    = s.profile_field;
      if (s.profile_about)     about    = s.profile_about;
      if (s.profile_style)     style    = s.profile_style;
      if (s.coach_tone)        coachTone = s.coach_tone;
      if (s.profile_explain)   explain  = s.profile_explain.split(",").filter(Boolean);

      expMoodle = s.exp_moodle === "true";
      void api.clipServerStatus().then((v) => (clipStatus = v)).catch(() => {});
      if (s.moodle_url) mdUrl = s.moodle_url;
      void loadMoodle();

      // Hydration complete — persist effects may now write without clobbering.
      loaded = true;

      // Auto-verify each stored key's connection, and pre-load Ollama's installed
      // models if any task uses Ollama (so the picker reflects reality immediately).
      verifyAllKeys();
      if (Object.values(assign).some((x) => x.provider === "ollama")) void ensureOllamaModels();
      void refreshOnnxStatus();
    })();
    // Live progress while the ONNX model downloads.
    onMount(() => {
      let unlisten: (() => void) | null = null;
      api.onOnnxProgress((p) => {
        onnxStat = { kind: "downloading", downloaded: p.downloaded, total: p.total };
      }).then((u) => { unlisten = u; });
      return () => { unlisten?.(); };
    });
  });

  // ---- helpers ----
  function saveProfile() {
    api.setSettings({
      profile_name: name,
      profile_pronouns: pronouns,
      profile_level: level,
      profile_field: field,
      profile_about: about,
      profile_style: style,
      profile_explain: explain.join(","),
    }).then(() => app.pushToast({ kind: "success", title: "Profile saved", body: "The AI will use your updated context." }))
      .catch(() => app.pushToast({ kind: "error", title: "Save failed" }));
  }

  function saveKeys() {
    // Generic mapping: every row persists to `<id>_api_key` — which is exactly
    // what the curated providers already used (openrouter_api_key, …) and what
    // the Rust registry's `key_setting()` expects — except the endpoint URL row,
    // which is a URL, not a key.
    const payload: Record<string, string> = {};
    for (const k of keyMeta) {
      const v = keys[k.id as keyof typeof keys] ?? "";
      // custom_endpoint is a URL and custom_api_key is already the final setting
      // name — the backend (commands.rs) reads both verbatim, so the generic
      // `<id>_api_key` suffix must not be applied to either.
      if (k.id === "custom_endpoint") payload.custom_endpoint = v;
      else if (k.id === "custom_api_key") payload.custom_api_key = v;
      else payload[`${k.id}_api_key`] = v;
    }
    api.setSettings(payload).then(() => app.pushToast({ kind: "success", title: "Keys saved", body: "Stored in the system keychain." }))
      .catch(() => app.pushToast({ kind: "error", title: "Save failed" }));
  }

  // Live OpenRouter catalog for the searchable model picker — fetched once, on the
  // first time an OpenRouter model dropdown is opened.
  let orModels = $state<OrModel[]>([]);
  let orLoading = $state(false);
  let orLoaded = false;
  async function ensureOrModels() {
    if (orLoaded || orLoading) return;
    orLoading = true;
    try {
      orModels = await loadOpenRouterModels();
      orLoaded = true;
    } catch {
      app.pushToast({ kind: "error", title: "OpenRouter models", body: "Couldn’t load the model list — check your connection." });
    } finally {
      orLoading = false;
    }
  }

  async function onModelProviderChange(taskId: TaskId, p: string) {
    if (p === "ollama") await ensureOllamaModels();
    const provList = providersFor(taskId);
    const np = provList.find((x) => x.id === p) ?? provList[0];
    // Ollama → default to the first INSTALLED model (empty if none pulled, which leaves
    // the picker blank as intended). Others → the provider's first curated model.
    const firstModel = p === "ollama" || p === "custom" ? "" : (np.models[0]?.id ?? "");
    setTask(taskId, { provider: p, model: firstModel });
    const kv: Record<string, string> = { [`model_${taskId}`]: p + ":" + firstModel };
    if (taskId === "embedding") kv.embed_provider = p;
    api.setSettings(kv).catch(() => {});
  }

  function onModelChange(taskId: TaskId, m: string) {
    setTask(taskId, { model: m });
    api.setSettings({ [`model_${taskId}`]: assign[taskId].provider + ":" + m }).catch(() => {});
  }

  // ── Ollama: live installed-model list (GET /api/tags) ──────────────────────
  // When a task's provider is Ollama, the model picker shows ONLY the models actually
  // installed on the configured server (local on desktop, or the homelab). An empty
  // list ⇒ an empty picker (nothing to choose) — never models that aren't pulled.
  let ollamaInstalled = $state<string[]>([]);
  let ollamaLoaded = false;
  async function ensureOllamaModels() {
    if (ollamaLoaded) return;
    ollamaLoaded = true;
    try { ollamaInstalled = await api.ollamaModels(); } catch { ollamaInstalled = []; }
  }
  // Changing the Ollama/homelab URL just INVALIDATES the cached list — it's re-probed
  // lazily next time the picker opens, so we don't fire a network call per keystroke.
  function invalidateOllamaModels() { ollamaLoaded = false; ollamaInstalled = []; }

    // ---- local ONNX embeddings (zero-config) ----
    // The desktop app can run embeddings entirely on-device after a one-time
    // download of the int8 multilingual-e5-small ONNX model (~118 MB). Once
    // Ready, chunks are embedded without any external service. iOS never
    // shows the "Ready" state (the ONNX crate is gated out for iOS).
    let onnxStat = $state<api.OnnxStatus | null>(null);
    let onnxDownloading = $state(false);
    async function refreshOnnxStatus() {
      try { onnxStat = await api.onnxStatus(); } catch { onnxStat = null; }
    }
    async function startOnnxDownload() {
      if (onnxDownloading) return;
      onnxDownloading = true;
      onnxStat = { kind: "downloading", downloaded: 0, total: 118_000_000 };
      try { onnxStat = await api.onnxDownload(); }
      catch (e) { onnxStat = { kind: "failed", reason: String(e) }; }
      finally { onnxDownloading = false; }
    }

  // Is local-model (Ollama) usage available here? On desktop, always (localhost). On
  // mobile there is no localhost Ollama, so it needs a homelab base (or an explicit
  // non-localhost Ollama URL) to be reachable.
  const hasHomelabOllama = $derived(
    !!(hlBase.trim() || hlTailscale.trim() || hlPublic.trim()) ||
    (!!endpoint.trim() && !/localhost|127\.0\.0\.1/.test(endpoint))
  );
  const ollamaAvailable = $derived(!isMobile || hasHomelabOllama);

  // Provider list for a task, with Ollama dropped on mobile when no homelab is set
  // (so it isn't offered when it can't possibly work).
  function providersFor(taskId: TaskId) {
    const base = taskId === "embedding" ? EMBED_PROVIDERS : PROVIDERS;
    return ollamaAvailable ? base : base.filter((p) => p.id !== "ollama");
  }

  // Options for a task's model picker: OpenRouter → live catalog; Ollama → installed
  // models (may be empty); else the provider's curated list.
  function modelOptionsFor(prov: { id: string; models: Model[] }) {
    if (prov.id === "openrouter" && orModels.length) return orModels;
    if (prov.id === "ollama") return ollamaInstalled.map((id) => ({ id, label: id }));
    return prov.models.map((m) => ({ id: m.id, label: m.label }));
  }

  // Ollama URL to display in API keys. Desktop: the editable endpoint. Mobile: the
  // homelab-derived URL (base + /ollama), shown read-only — Ollama is homelab-only there.
  const ollamaDisplayUrl = $derived(
    endpoint.trim() ? endpoint.trim()
    : (hlBase.trim() ? hlBase.trim().replace(/\/+$/, "") + "/ollama" : "")
  );

  // ── API-key verification (a real authed connection check, not just "key present") ──
  type VerifyState = api.VerifyResult | "checking" | null;
  let verify = $state<Record<string, VerifyState>>({});
  // Status-badge class + label, shared by the provider rows and the Ollama row.
  const statusClass = (v: VerifyState) =>
    "key-status " + (v === "checking" ? "checking" : v ? (v.ok ? "ok" : "bad") : "off");
  const statusLabel = (v: VerifyState, isSet = false) =>
    v === "checking" ? "checking…" : v ? (v.ok ? "connected" : v.detail) : (isSet ? "not checked" : "not set");
  const verifyIdForKey = (id: string) =>
    id === "custom_endpoint" || id === "custom_api_key" ? "custom" : id;
  async function verifyKey(id: string) {
    const vid = verifyIdForKey(id);
    verify = { ...verify, [vid]: "checking" };
    try { verify = { ...verify, [vid]: await api.verifyProvider(vid) }; }
    catch (e) { verify = { ...verify, [vid]: { ok: false, detail: String(e) } }; }
  }
  // Verify every provider that has a stored key (run on load + after Save keys).
  function verifyAllKeys() {
    for (const k of keyMeta) {
      if (keys[k.id as keyof typeof keys]?.trim()) void verifyKey(k.id);
    }
    if (ollamaAvailable) void verifyKey("ollama");
  }

  const levelLabels: Record<string, string> = {
    undergrad: "Undergraduate",
    postgrad:  "Postgraduate",
    phd:       "PhD / research",
    self:      "Self-study",
  };

  function toggleExplain(id: string) {
    explain = explain.includes(id)
      ? explain.filter((x) => x !== id)
      : [...explain, id];
  }
</script>

<div class="settings">
  <!-- NAV SIDEBAR -->
  <aside class="set-nav">
    <div class="set-nav-h">
      <button
        class="btn btn--icon btn--sm btn--ghost"
        onclick={() => app.setView("subject")}
        title="Back"
      >
        <Icon name="chevron" size={14} style="transform:rotate(180deg)" />
      </button>
      <span class="mono" style="color:var(--fg-bright);font-weight:600">Settings</span>
    </div>

    {#each navTabs as t}
      <button
        class={"set-nav-item" + (tab === t.id ? " on" : "")}
        onclick={() => (tab = t.id)}
      >
        <Icon name={t.icon} size={13} /> {t.label}
      </button>
    {/each}

    <div class="set-nav-foot mono faint">Cortex {appVersion ? "v" + appVersion : ""} · BYOK · local-first</div>
  </aside>

  <!-- MAIN BODY -->
  <div class="set-body">

    {#if isMobile}
      <div class="set-mnav">
        <Picker
          value={tab}
          onChange={(id) => (tab = id)}
          options={navTabs.map((t) => ({ id: t.id, label: t.label }))}
          icon={navTabs.find((t) => t.id === tab)?.icon}
        />
      </div>
    {/if}

    <!-- ===== PROFILE ===== -->
    {#if tab === "profile"}
      <div class="set-pane">
        <header class="set-head">
          <div class="eyebrow">Profile</div>
          <h1 class="set-title">AI 眼中的你</h1>
          <p class="set-sub">与所有对话和生成共享，让回答贴合你的水平和风格。只留在这台机器上。</p>
        </header>

        <section class="set-group">
          <div class="set-group-h"><h3 class="set-group-t">身份</h3></div>
          <div class="set-card">
            <div class="set-row">
              <div class="set-row-l"><div class="set-row-t">显示名称</div></div>
              <div class="set-row-r"><input class="input" bind:value={name} /></div>
            </div>
            <div class="set-row">
              <div class="set-row-l"><div class="set-row-t">称呼代词</div></div>
              <div class="set-row-r"><input class="input" bind:value={pronouns} /></div>
            </div>
            <div class="set-row">
              <div class="set-row-l"><div class="set-row-t">学习阶段</div></div>
              <div class="set-row-r">
                <Picker
                  value={level}
                  onChange={(v) => (level = v)}
                  options={[
                    { id: "undergrad", label: "Undergraduate" },
                    { id: "postgrad",  label: "Postgraduate" },
                    { id: "phd",       label: "PhD / research" },
                    { id: "self",      label: "Self-study" },
                  ]}
                />
              </div>
            </div>
            <div class="set-row">
              <div class="set-row-l"><div class="set-row-t">专业方向</div></div>
              <div class="set-row-r"><input class="input" bind:value={field} /></div>
            </div>
          </div>
        </section>

        <section class="set-group">
          <div class="set-group-h">
            <h3 class="set-group-t">关于你</h3>
            <p class="set-group-d">AI 用来个性化解释的背景信息。</p>
          </div>
          <div class="set-card">
            <div class="set-row stacked">
              <div class="set-row-t">用你的话描述</div>
              <textarea class="input set-textarea set-bio" bind:value={about} rows={6}></textarea>
            </div>
            <div class="set-row">
              <div class="set-row-l">
                <div class="set-row-t">回答风格</div>
                <div class="set-row-d">默认输出多少细节。</div>
              </div>
              <div class="set-row-r">
                <div class="seg">
                  {#each [{ id: "concise", label: "Concise" }, { id: "balanced", label: "Balanced" }, { id: "detailed", label: "Detailed" }] as opt}
                    <button type="button" class={"seg-opt" + (style === opt.id ? " on" : "")} onclick={() => (style = opt.id)}>{opt.label}</button>
                  {/each}
                </div>
              </div>
            </div>
            <div class="set-row">
              <div class="set-row-l">
                <div class="set-row-t">督促语气</div>
                <div class="set-row-d">学习搭子说话的方式；默认「中性」不改变现有输出。</div>
              </div>
              <div class="set-row-r">
                <div class="seg">
                  {#each [{ id: "gentle", label: "温和" }, { id: "friendly", label: "友好" }, { id: "neutral", label: "中性" }, { id: "strict", label: "严格" }, { id: "savage", label: "毒舌" }] as opt}
                    <button type="button" class={"seg-opt" + (coachTone === opt.id ? " on" : "")} onclick={() => (coachTone = opt.id)}>{opt.label}</button>
                  {/each}
                </div>
              </div>
            </div>
            <div class="set-row stacked">
              <div class="set-row-l">
                <div class="set-row-t">讲解方式</div>
                <div class="set-row-d">选对你学习帮助最大的方式。</div>
              </div>
              <div class="set-row-r">
                <div class="tag-suggest" style="margin-top:0">
                  {#each [{ id: "worked-examples", label: "worked examples" }, { id: "analogies", label: "analogies" }, { id: "formal-proofs", label: "formal proofs" }, { id: "diagrams", label: "diagrams" }, { id: "code", label: "code snippets" }] as opt}
                    <button
                      type="button"
                      class={"tag-chip-add" + (explain.includes(opt.id) ? " on" : "")}
                      onclick={() => toggleExplain(opt.id)}
                    >
                      {#if explain.includes(opt.id)}<Icon name="check" size={9} />{/if}
                      {opt.label}
                    </button>
                  {/each}
                </div>
              </div>
            </div>
          </div>
        </section>

        <section class="set-group">
          <div class="set-group-h">
            <h3 class="set-group-t">Memory · 分层记忆</h3>
            <p class="set-group-d">声明式记忆直接用于回答；推断记忆保存证据与置信度，需确认后才用于回答。</p>
          </div>
          <div class="set-card">
            <div class="set-row stacked">
              <div class="set-row-r">
                <div class="row-inline">
                  <input
                    class="input"
                    bind:value={newMemory}
                    placeholder="e.g. My final exam is on June 20th"
                    onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); addMemoryFact(); } }}
                  />
                  <select class="input" aria-label="记忆层级" bind:value={memoryLayer}><option value="declarative">用户声明</option><option value="inferred">待确认推断</option></select>
                  {#if memoryLayer === "inferred"}
                    <input class="input" aria-label="推断证据" placeholder="依据、来源或对话摘录（必填）" bind:value={memoryEvidence} />
                    <input class="input" aria-label="置信度" type="number" min="0" max="1" step="0.05" bind:value={memoryConfidence} />
                  {/if}
                  <button class="btn btn--primary" onclick={addMemoryFact} disabled={!newMemory.trim() || memoryBusy}>
                    <Icon name="check" size={13} /> Remember
                  </button>
                </div>
              </div>
            </div>
            {#if memories.length === 0}
              <div class="set-row">
                <div class="set-row-l"><div class="set-row-d">还没有记忆。在上面添加一条事实，AI 就会记住。</div></div>
              </div>
            {:else}
              {#each memories as m (m.id)}
                <div class="set-row">
                  <div class="set-row-l"><div class="set-row-t">{m.content}</div><div class="set-row-d">{m.layer === "declarative" ? "用户声明" : `推断 · ${Math.round(m.confidence * 100)}% · ${m.confirmed ? "已确认" : "待确认，不用于回答"}`}{#if m.source} · {m.source}{/if}</div></div>
                  <div class="set-row-r">
                    {#if !m.confirmed}<button class="btn" onclick={async () => { try { await api.confirmMemory(m.id); await loadMemory(); } catch (e) { app.pushToast({ kind: "error", title: String(e) }); } }}>确认推断</button>{/if}
                    <button class="btn btn--icon btn--sm btn--ghost" onclick={() => removeMemory(m.id)} title="Forget this">
                      <Icon name="x" size={13} />
                    </button>
                  </div>
                </div>
              {/each}
            {/if}
          </div>
        </section>

        <div class="set-preview">
          <div class="label" style="margin-bottom:8px">AI 会收到什么</div>
          <pre class="set-sysprompt mono">User: {name} ({pronouns}) · {levelLabels[level] ?? level}
Studying: {field}
Style: {style}, prefers {explain.join(", ") || "no special format"}
Notes: {about}</pre>
        </div>

        <div class="set-foot-actions">
          <button class="btn btn--primary" onclick={saveProfile}>
            <Icon name="check" size={13} /> Save profile
          </button>
        </div>
      </div>

    <!-- ===== MODELS ===== -->
    {:else if tab === "models"}
      <div class="set-pane set-pane--models">
        <header class="set-head">
          <div class="eyebrow">Models</div>
          <h1 class="set-title">每类任务各配一个模型</h1>
          <p class="set-sub">把每类任务派给最合适的服务商。未设置的槽位继承 Cheatsheet 的主力模型（vision 除外——它必须保持多模态）。Token 预算为每次调用封顶。</p>
        </header>

        <div class="set-card set-table">
          <div class="mt-head mono">
            <span>Task</span><span>Provider</span><span>Model</span><span>Token 预算</span>
          </div>
          {#each MODEL_TASKS as t}
            {@const a = assign[t.id]}
            {@const provList = providersFor(t.id)}
            {@const allProv = t.id === "embedding" ? EMBED_PROVIDERS : PROVIDERS}
            {@const prov = allProv.find((p) => p.id === a.provider) ?? provList[0]}
            {@const isOr = a.provider === "openrouter"}
            {@const isOllama = a.provider === "ollama"}
            {@const isCustom = a.provider === "custom"}
            <div class="mt-row">
              <div class="mt-task">
                <div class="mt-task-t">{t.label}</div>
                <div class="mt-task-d mono">{t.desc}</div>
              </div>
              <Picker
                value={a.provider}
                onChange={(p) => onModelProviderChange(t.id, p)}
                options={provList.map((p) => ({ id: p.id, label: p.label }))}
              />
              <!-- OpenRouter → live searchable catalog (curated list as offline/pre-fetch
                   fallback); Ollama → only models actually installed (empty ⇒ nothing to
                   pick); every other provider → its own curated list, still searchable. -->
              <ModelSearch
                value={a.model}
                onChange={(m) => onModelChange(t.id, m)}
                options={modelOptionsFor(prov)}
                loading={isOr && orLoading}
                onOpen={isOr ? ensureOrModels : (isOllama ? ensureOllamaModels : undefined)}
                allowCustom={isCustom}
                placeholder={isCustom ? "Type model id, e.g. qwen-plus" : (isOr ? "Search OpenRouter…" : (isOllama ? (ollamaInstalled.length ? "Pick an installed model" : "No models installed") : undefined))}
              />
              {#if t.id === "embedding"}
                <span class="mono faint mt-budget-na">n/a</span>
              {:else}
                <input
                  class="input mono mt-budget"
                  value={a.budget}
                  oninput={(e) => { const v = (e.target as HTMLInputElement).value; setTask(t.id, { budget: v }); api.setSettings({ ["budget_" + t.id]: v }).catch(() => {}); }}
                />
              {/if}
            </div>
          {/each}
        </div>

        <section class="set-group" style="margin-top:14px">
          <div class="set-group-h"><h3 class="set-group-t">经济模式</h3></div>
          <div class="set-card">
            <div class="xtra-row">
              <div class="xtra-label">
                <div class="mt-task-t">经济模式（Economy tier）</div>
                <div class="mt-task-d mono">Quiz / Flashcard 生成自动派给下面的便宜模型（未单独配置槽位时）；Cheatsheet / Chat / Audio 保持主力模型。显式配置过的槽位永远优先</div>
              </div>
              <input type="checkbox" checked={economyOn} onchange={saveEconomyMode} />
            </div>
            <div class="xtra-row">
              <div class="xtra-label">
                <div class="mt-task-t">经济模型（model_cheap）</div>
                <div class="mt-task-d mono">完整 spec：provider:model，如 openrouter:google/gemini-2.0-flash 或 ollama:qwen2.5:7b（本地免费）</div>
              </div>
              <input class="input mono" value={economySpec} onchange={saveEconomySpec} placeholder="openrouter:google/gemini-2.0-flash" />
            </div>
          </div>
        </section>

        <section class="set-group" style="margin-top:14px">
          <div class="set-group-h"><h3 class="set-group-t">语音与检索</h3></div>
          <div class="set-card">
            <div class="xtra-row">
              <div class="xtra-label">
                <div class="mt-task-t">Speech · 云 TTS 模型</div>
                <div class="mt-task-d mono">Audio overview / Video overview 的配音。默认 tts-1</div>
              </div>
              <input class="input mono" value={speechModel} onchange={saveSpeech} placeholder="tts-1" />
            </div>
            <div class="xtra-row">
              <div class="xtra-label">
                <div class="mt-task-t">TTS 端点 + 密钥（可选）</div>
                <div class="mt-task-d mono">OpenAI 兼容 /audio/speech 端点；留空走 api.openai.com + OpenAI key。密钥只存本机数据库</div>
              </div>
              <div class="xtra-stack">
                <input class="input mono" value={ttsEndpoint} onchange={saveTtsEndpoint} placeholder="https://api.openai.com/v1/audio/speech" />
                <input class="input mono" type="password" value={ttsKey} onchange={saveTtsKey} placeholder="TTS API key（可选）" />
              </div>
            </div>
            <div class="xtra-row">
              <div class="xtra-label">
                <div class="mt-task-t">Embedding 模型名覆盖（可选）</div>
                <div class="mt-task-d mono">对 Gemini / Ollama 生效（默认 text-embedding-004 / nomic-embed-text）。维度变了需重新入库</div>
              </div>
              <input class="input mono" value={embedOv} onchange={saveEmbedOv} placeholder="留空用默认" />
            </div>
          </div>
        </section>

        <section class="set-group" style="margin-top:14px">
          <div class="set-group-h"><h3 class="set-group-t">文档解析</h3></div>
          <div class="set-card">
            <div class="xtra-row">
              <div class="xtra-label">
                <div class="mt-task-t">PDF 解析引擎</div>
                <div class="mt-task-d mono">自动：装有 docling（IBM，版面感知，双栏/表格/公式更准）优先，失败回退 poppler。首次解析会下载模型，较慢</div>
              </div>
              <select class="input" value={pdfEngine} onchange={savePdfEngine}>
                <option value="auto">自动（推荐）</option>
                <option value="docling">Docling</option>
                <option value="poppler">Poppler（最快）</option>
              </select>
            </div>
            <div class="xtra-row">
              <div class="xtra-label">
                <div class="mt-task-t">Docling 路径覆盖（可选）</div>
                <div class="mt-task-d mono">docling 不在 PATH 时填可执行文件完整路径</div>
              </div>
              <input class="input mono" value={doclingBin} onchange={saveDoclingBin} placeholder="留空用 PATH 中的 docling" />
            </div>
          </div>
        </section>

        <div class="set-note mono">
          <Icon name="diamond" size={11} color="var(--accent)" />
          Ollama tasks run fully offline on this machine or your homelab — no key required.
        </div>
      </div>

    <!-- ===== API KEYS ===== -->
    {:else if tab === "keys"}
      <div class="set-pane">
        <header class="set-head">
          <div class="eyebrow">API keys</div>
          <h1 class="set-title">带上你自己的密钥</h1>
          <p class="set-sub">存在系统钥匙串里，永不同步。任何内容都不会经过 Cortex 的服务器。</p>
        </header>

        <section class="set-group">
          <div class="set-group-h"><h3 class="set-group-t">服务商</h3></div>
          <div class="set-card">
            {#each keyMeta as k}
              {@const isSet = !!keys[k.id as keyof typeof keys]}
              {@const v = verify[verifyIdForKey(k.id)]}
              <div class="set-row stacked">
                <div class="set-row-l">
                  <div class="set-row-t">
                    <span class="row-keytitle">
                      {k.label}
                      <span class={statusClass(v)}>{statusLabel(v, isSet)}</span>
                    </span>
                  </div>
                  <div class="set-row-d">{k.note}</div>
                </div>
                <div class="set-row-r">
                  <div class="masked">
                    <input
                      class="input mono"
                      type={showKey[k.id] ? "text" : "password"}
                      value={keys[k.id as keyof typeof keys]}
                      oninput={(e) => { keys = { ...keys, [k.id]: (e.target as HTMLInputElement).value }; verify = { ...verify, [k.id]: null }; }}
                      placeholder={k.placeholder}
                      spellcheck={false}
                    />
                    <button
                      type="button"
                      class="masked-eye"
                      onclick={() => { showKey = { ...showKey, [k.id]: !showKey[k.id] }; }}
                      title={showKey[k.id] ? "Hide" : "Show"}
                    >
                      <Icon name={showKey[k.id] ? "x" : "search"} size={13} />
                    </button>
                  </div>
                  {#if isSet}
                    <div style="display:flex;gap:6px;margin-top:6px">
                      <button
                        type="button"
                        class="btn btn--ghost btn--sm"
                        onclick={() => verifyKey(k.id)}
                      >Verify</button>
                      <button
                        type="button"
                        class="btn btn--ghost btn--sm"
                        onclick={() => { keys = { ...keys, [k.id]: "" }; verify = { ...verify, [k.id]: null }; }}
                      >Clear</button>
                    </div>
                  {/if}
                </div>
              </div>
            {/each}
          </div>
        </section>

        <!-- plugins/ directory registry — the contributor entry point. Read-only
             here: drop a JSON manifest into <data>/plugins/ and restart. -->
        <section class="set-group">
          <div class="set-group-h"><h3 class="set-group-t">插件</h3></div>
          <div class="set-card">
            {#if !plugins.length && !pluginErrors.length}
              <div class="set-row-d">
                还没有插件。把 JSON 清单放进数据目录的 <code>plugins/</code> 文件夹后重启即可
                —— 可以贡献新的模型服务商（OpenAI 兼容）、学习材料预设、Space 菜单项和数据源连接器
                （Zotero / Notion / GitHub 等任何 JSON API）。
              </div>
            {:else}
              {#each plugins as pl}
                <div class="set-row stacked">
                  <div class="set-row-l">
                    <div class="set-row-t"><span class="row-keytitle">{pl.name}</span></div>
                    <div class="set-row-d mono">{pl.file}</div>
                    <div class="set-row-d">
                      {pl.providers.length} 个服务商 · {pl.material_types.length} 个材料预设 · {pl.leader_keys.length} 个菜单项 · {pl.connectors.length} 个连接器
                    </div>
                  </div>
                </div>
              {/each}
              {#each pluginErrors as err}
                <div class="set-row stacked">
                  <div class="set-row-l">
                    <div class="set-row-t" style="color:var(--err)">{err}</div>
                  </div>
                </div>
              {/each}
            {/if}
          </div>
        </section>

        <!-- Local models (Ollama) — keyless; a URL, not a key. On mobile Ollama is
             reached only through the Homelab (no localhost on a phone). -->
        {#if ollamaAvailable}
        {@const ov = verify.ollama}
        <section class="set-group">
          <div class="set-group-h"><h3 class="set-group-t">本地模型（Ollama）</h3></div>
          <div class="set-card">
            <div class="set-row stacked">
              <div class="set-row-l">
                <div class="set-row-t">
                  <span class="row-keytitle">
                    Ollama URL
                    <span class={statusClass(ov)}>{statusLabel(ov)}</span>
                  </span>
                </div>
                <div class="set-row-d">
                  {#if isMobile}
                    Runs through your Homelab — set the Homelab URL in Integrations. Keyless.
                  {:else}
                    Keyless, local. Defaults to <span class="mono">http://localhost:11434</span>; leave blank to use your Homelab.
                  {/if}
                </div>
              </div>
              <div class="set-row-r">
                {#if isMobile}
                  <input class="input mono" value={ollamaDisplayUrl} placeholder="set Homelab URL in Integrations" readonly />
                {:else}
                  <input
                    class="input mono"
                    value={endpoint}
                    oninput={(e) => { endpoint = (e.target as HTMLInputElement).value; verify = { ...verify, ollama: null }; }}
                    placeholder="http://localhost:11434"
                    spellcheck={false}
                  />
                {/if}
                <button type="button" class="btn btn--ghost btn--sm" style="margin-top:6px" onclick={() => verifyKey("ollama")}>Verify</button>
              </div>
            </div>
          </div>
        </section>
        {/if}

        <!-- Local ONNX embeddings — keyless, on-device multilingual model. ~118 MB
             one-time download from HuggingFace; once Ready the app embeds without
             any external service. -->
        <section class="set-group">
          <div class="set-group-h"><h3 class="set-group-t">本地 ONNX 向量</h3></div>
          <div class="set-card">
            <div class="set-row stacked">
              <div class="set-row-l">
                <div class="set-row-t">
                  <span class="row-keytitle">
                    Status
                    {#if onnxStat?.kind === "ready"}
                      <span class="key-status ok">ready</span>
                    {:else if onnxStat?.kind === "downloading"}
                      <span class="key-status checking">downloading</span>
                    {:else if onnxStat?.kind === "failed"}
                      <span class="key-status bad">failed</span>
                    {:else}
                      <span class="key-status off">未安装</span>
                    {/if}
                  </span>
                </div>
                <div class="set-row-d">
                  {#if onnxStat?.kind === "ready"}
                    On-device <span class="mono">multilingual-e5-small</span> ready — chunks embed locally with no network. ({Math.round(((onnxStat as any).bytes_on_disk) / 1_048_576)} MB on disk)
                  {:else if onnxStat?.kind === "downloading"}
                    Downloading the int8 ONNX model from HuggingFace…
                    {Math.round(((onnxStat as any).downloaded) / 1_048_576)} / {Math.round((((onnxStat as any).total) || 118_000_000) / 1_048_576)} MB
                  {:else if onnxStat?.kind === "failed"}
                    Download failed: {(onnxStat as any).reason}. Check your network and retry.
                  {:else}
                    Zero-config local embeddings. One-time ~118 MB download from HuggingFace
                    (<span class="mono">Xenova/multilingual-e5-small</span>, int8 quantized, 384 dims).
                    After this the app runs embeddings offline with no API key.
                  {/if}
                </div>
              </div>
              <div class="set-row-r">
                {#if onnxStat?.kind === "downloading"}
                  <div class="onnx-bar"><div class="onnx-bar-f" style="width: {Math.min(100, Math.round((((onnxStat as any).downloaded) / Math.max(1, (onnxStat as any).total)) * 100))}%"></div></div>
                {:else}
                  <button class="btn btn--sm" type="button" disabled={onnxDownloading} onclick={startOnnxDownload}>
                    <Icon name="download" size={12} />
                    {onnxStat?.kind === "failed" ? "Retry download" : (onnxStat?.kind === "ready" ? "Re-download" : "Download (~118 MB)")}
                  </button>
                {/if}
              </div>
            </div>
          </div>
        </section>

        <div class="set-foot-actions">
          <button class="btn btn--primary" onclick={() => { saveKeys(); verifyAllKeys(); }}>
            <Icon name="check" size={13} /> Save keys
          </button>
        </div>
      </div>

    <!-- ===== APPEARANCE ===== -->
    {:else if tab === "appearance"}
      <div class="set-pane">
        <header class="set-head">
          <div class="eyebrow">Appearance</div>
          <h1 class="set-title">按你的习惯调整</h1>
          <p class="set-sub">Cortex 会跟随 Omarchy 主题实时换肤，也可以手动选一个。</p>
        </header>

        <!-- Follow-Omarchy mirrors the desktop's Omarchy palette — meaningless on a phone. -->
        {#if !isMobile}
        <section class="set-group">
          <div class="set-group-h"><h3 class="set-group-t">主题</h3></div>
          <div class="set-card">
            <div class="set-row">
              <div class="set-row-l">
                <div class="set-row-t">跟随 Omarchy 主题</div>
                <div class="set-row-d">每次启动都镜像桌面的 Omarchy 配色。在下面选主题会关掉这个开关。</div>
              </div>
              <div class="set-row-r">
                <button
                  type="button"
                  class={"st-toggle" + (app.followOmarchy ? " on" : "")}
                  role="switch"
                  aria-checked={app.followOmarchy}
                  aria-label="follow omarchy theme"
                  onclick={async () => {
                    const matched = await app.setFollowOmarchy(!app.followOmarchy);
                    if (app.followOmarchy && !matched) {
                      app.pushToast({ kind: "warning", title: "Omarchy theme not found", body: "Couldn't read your Omarchy theme, or it has no Cortex match." });
                    } else if (matched) {
                      app.pushToast({ kind: "success", title: "Following Omarchy", body: `Matched → ${THEME_LABELS[matched]}.` });
                    }
                  }}
                ><span class="st-knob"></span></button>
              </div>
            </div>
          </div>
        </section>
        {/if}

        <section class="set-group">
          <div class="set-group-h"><h3 class="set-group-t">手动主题</h3></div>
          <div class="set-card">
            <div class="set-themes">
              {#each THEME_OPTS as t}
                <button
                  class={"set-theme" + (app.theme === t.id ? " on" : "")}
                  onclick={() => { if (app.followOmarchy) app.setFollowOmarchy(false); app.setTheme(t.id); }}
                  style="background:{t.b}"
                >
                  <div class="set-theme-sws">
                    <span style="background:{t.c}"></span>
                    <span style="background:{t.b};border:1px solid {t.c}"></span>
                  </div>
                  <span class="set-theme-n" style="color:{app.theme === t.id ? '#fff' : '#cbd5d0'}">{t.n}</span>
                  {#if app.theme === t.id}
                    <span class="set-theme-check" style="color:{t.c}">
                      <Icon name="check" size={13} />
                    </span>
                  {/if}
                </button>
              {/each}
            </div>
          </div>
        </section>

        <section class="set-group">
          <div class="set-group-h"><h3 class="set-group-t">自定义主题色</h3></div>
          <div class="set-card">
            <div class="set-row">
              <div class="set-row-l">
                <div class="set-row-t">强调色覆盖</div>
                <div class="set-row-d">替换当前主题的强调色（--accent / --accent-dim，文字前景自动按亮度推导）。对 Omarchy 跟随模式同样生效；点「恢复」回到主题原色。</div>
              </div>
              <div class="set-row-r" style="display:flex; align-items:center; gap:8px;">
                <input type="color" class="set-accent-pick" aria-label="自定义强调色"
                       value={app.customAccent || "#2dd5b7"}
                       oninput={(e) => app.setCustomAccent(e.currentTarget.value)} />
                {#if app.customAccent}
                  <button type="button" class="btn" onclick={() => app.setCustomAccent("")}>恢复</button>
                {/if}
              </div>
            </div>
          </div>
        </section>

        <section class="set-group">
          <div class="set-group-h"><h3 class="set-group-t">自定义 CSS</h3></div>
          <div class="set-card set-css-card">
            <textarea class="set-css mono" rows="7" bind:value={customCssDraft} oninput={onCustomCssInput}
                      spellcheck="false" aria-label="自定义 CSS"
                      placeholder={"/* 覆盖任意变量或选择器，例如：\n:root[data-theme] { --rad-3: 12px; }\n.chat-msg { line-height: 1.7; } */"}></textarea>
            <div class="set-css-hint mono">即时生效，停顿 0.5s 自动保存。只存本机，改坏了清空即可。</div>
          </div>
        </section>

        <section class="set-group">
          <div class="set-group-h"><h3 class="set-group-t">阅读</h3></div>
          <div class="set-card">
            <div class="set-row">
              <div class="set-row-l">
                <div class="set-row-t">速查表字体</div>
                <div class="set-row-d">朗读学习内容用的音色。</div>
              </div>
              <div class="set-row-r">
                <div class="seg">
                  {#each [{ id: "mono", label: "Mono" }, { id: "serif", label: "Serif" }, { id: "sans", label: "Sans" }] as opt}
                    <button type="button" class={"seg-opt" + (readFont === opt.id ? " on" : "")} onclick={() => (readFont = opt.id)}>{opt.label}</button>
                  {/each}
                </div>
              </div>
            </div>
            <!-- Density is a desktop spacing affordance; touch uses one comfortable scale. -->
            {#if !isMobile}
            <div class="set-row">
              <div class="set-row-l">
                <div class="set-row-t">界面密度</div>
                <div class="set-row-d">整个应用的间距。</div>
              </div>
              <div class="set-row-r">
                <div class="seg">
                  {#each [{ id: "regular", label: "Regular" }, { id: "compact", label: "Compact" }] as opt}
                    <button type="button" class={"seg-opt" + (density === opt.id ? " on" : "")} onclick={() => (density = opt.id)}>{opt.label}</button>
                  {/each}
                </div>
              </div>
            </div>
            {/if}
          </div>
        </section>

        <!-- Window / tray is a desktop-only concept — hidden on mobile. -->
        {#if !isMobile}
        <section class="set-group">
          <div class="set-group-h"><h3 class="set-group-t">窗口</h3></div>
          <div class="set-card">
            <div class="set-row">
              <div class="set-row-l">
                <div class="set-row-t">关闭到托盘</div>
                <div class="set-row-d">关闭窗口后 Cortex 继续在托盘运行 — 入库、生成、音乐都不中断。要退出请用托盘菜单。</div>
              </div>
              <div class="set-row-r">
                <button type="button" class={"st-toggle" + (closeToTray ? " on" : "")} onclick={toggleCloseToTray} role="switch" aria-checked={closeToTray} aria-label="close to tray"><span class="st-knob"></span></button>
              </div>
            </div>
          </div>
        </section>

        <section class="set-group">
          <div class="set-group-h"><h3 class="set-group-t">显示</h3></div>
          <div class="set-card">
            <div class="set-row">
              <div class="set-row-l">
                <div class="set-row-t">界面缩放</div>
                <div class="set-row-d">整体放大或缩小 — 高分屏上很有用。</div>
              </div>
              <div class="set-row-r" style="min-width:120px">
                <!-- Dropdown of presets (was a slider — the UI rescaling live under the
                     thumb made the slider itself jump around while dragging). -->
                <Picker
                  value={String(app.uiScale)}
                  onChange={(id) => app.setUiScale(+id)}
                  options={[
                    ...([80, 90, 100, 110, 125, 150].includes(app.uiScale) ? [] : [{ id: String(app.uiScale), label: `${app.uiScale}% (custom)` }]),
                    { id: "80", label: "80%" },
                    { id: "90", label: "90%" },
                    { id: "100", label: "100% — default" },
                    { id: "110", label: "110%" },
                    { id: "125", label: "125%" },
                    { id: "150", label: "150%" },
                  ]}
                  placeholder="100%"
                />
              </div>
            </div>
          </div>
        </section>
        {/if}
      </div>

    <!-- ===== KEYBINDS ===== -->
    {:else if tab === "keybinds"}
      <div class="set-pane">
        <header class="set-head">
          <div class="eyebrow">Keybinds</div>
          <h1 class="set-title">Helix 风格，随你调整</h1>
          <p class="set-sub">点击任意绑定即可重设。监听中按 Esc 取消。</p>
        </header>

        <div class="set-card">
          <div class="set-row">
            <div class="set-row-l">
              <div class="set-row-t">预设</div>
              <div class="set-row-d">键位方案的起点。</div>
            </div>
            <div class="set-row-r">
              <div class="seg">
                {#each [{ id: "helix", label: "Helix" }, { id: "vim", label: "Vim" }, { id: "custom", label: "Custom" }] as opt}
                  <button
                    type="button"
                    class={"seg-opt" + (keybinds.preset === opt.id ? " on" : "")}
                    disabled={opt.id === "custom"}
                    onclick={() => { if (opt.id === "helix" || opt.id === "vim") { keybinds.applyPreset(opt.id); app.pushToast({ kind: "success", title: `${opt.label} keybinds applied`, body: "Shortcuts take effect outside this screen." }); } }}
                  >{opt.label}</button>
                {/each}
              </div>
            </div>
          </div>
        </div>

        <div class="set-card set-binds">
          {#each ACTION_ORDER as action}
            <div class="bind-row">
              <span class="bind-label">{ACTION_LABELS[action]}</span>
              <button
                class={"bind-keys" + (listening === action ? " listening" : "")}
                onclick={() => (listening = action)}
              >
                {#if listening === action}
                  <span class="mono faint">press a key…</span>
                {:else}
                  <span class="kbd">{displayKey(keybinds.map[action])}</span>
                {/if}
              </button>
            </div>
          {/each}
        </div>

        <div class="set-foot-actions">
          <button
            class="btn btn--ghost"
            onclick={() => {
              const p = keybinds.preset === "vim" ? "vim" : "helix";
              keybinds.applyPreset(p);
              app.pushToast({ kind: "info", title: "Reset", body: `Bindings restored to ${p === "vim" ? "Vim" : "Helix"} preset.` });
            }}
          >
            Reset to preset
          </button>
        </div>

        <section class="set-group">
          <div class="set-group-h">
            <h3 class="set-group-t">Leader 菜单（按 Space 再按…）</h3>
            <p class="set-group-d">先按 <span class="kbd">Space</span> 打开 leader 菜单，再按一个键。固定且助记 — 只在菜单打开时生效。</p>
          </div>
          <div class="set-card set-binds">
            {#each LEADER_ACTIONS as a}
              <div class="bind-row">
                <span class="bind-label">{a.label} <span class="faint">· {a.detail}</span></span>
                <span class="bind-keys"><span class="kbd">Space</span> <span class="kbd">{a.key}</span></span>
              </div>
            {/each}
          </div>
        </section>

        <section class="set-group">
          <div class="set-group-h">
            <h3 class="set-group-t">系统</h3>
            <p class="set-group-d">内置快捷键，遵循系统惯例，不可重绑定。</p>
          </div>
          <div class="set-card set-binds">
            {#each SYSTEM_BINDS as b}
              <div class="bind-row">
                <span class="bind-label">{b.label}</span>
                <span class="bind-keys">
                  {#each b.keys.split(" ") as part}<span class="kbd">{part}</span> {/each}
                </span>
              </div>
            {/each}
          </div>
        </section>
      </div>

    <!-- ===== INTEGRATIONS ===== -->
    {:else if tab === "homelab"}
      {#snippet endpointService(o: EndpointOpts)}
        {@const p = connPill(o.state, !!o.value.trim())}
        <section class="set-group">
          <div class="set-group-h svc-h">
            <div>
              <h3 class="set-group-t">{o.title}</h3>
              <p class="set-group-d">{@html o.desc}</p>
            </div>
            <span class="status-pill status-pill--{p.cls}"><span class="dot"></span>{p.label}</span>
          </div>
          <div class="set-card">
            <div class="set-row stacked">
              <div class="row-inline">
                <input class="input mono" value={o.value} oninput={(e) => o.oninput(e.currentTarget.value)} onchange={o.onsave} onblur={o.onsave} placeholder={o.placeholder} />
                <button class="btn" onclick={o.onTest} disabled={o.state === "testing" || !o.value.trim()}>
                  <Icon name="refresh" size={12} /> Test
                </button>
              </div>
              {#if o.state === "fail" && o.failHint}
                <div class="set-row-d" style="color:var(--err,#e5484d)">{o.failHint}</div>
              {/if}
              {#if o.hint}<div class="set-row-d">{@html o.hint}</div>{/if}
            </div>
            {#if o.extra}{@render o.extra()}{/if}
          </div>
        </section>
      {/snippet}

      {#snippet diagramsToggle()}
        <div class="set-row">
          <div class="set-row-l">
            <div class="set-row-t">用图表说明</div>
            <div class="set-row-d">每个速查表章节配一张相关示意图，对话里也会配图。连上 SearXNG 后默认开启。</div>
          </div>
          <div class="set-row-r">
            <button type="button" class={"st-toggle" + (webImages ? " on" : "")} onclick={() => setWebImages(!webImages)} disabled={!(searxng.trim() || hlBase.trim() || hlTailscale.trim() || hlPublic.trim())} role="switch" aria-checked={webImages} aria-label="diagrams"><span class="st-knob"></span></button>
          </div>
        </div>
      {/snippet}

      <div class="set-pane">
        <header class="set-head">
          <div class="eyebrow">Integrations</div>
          <h1 class="set-title">转写、搜索与同步 — 都听你的</h1>
          <p class="set-sub">在这台电脑上开箱即用。需要更大算力就加一个云端 API 密钥，或者把 Cortex 指向你自己的 homelab（<span class="mono">homelab/</span> docker compose）— 这里不填也不影响任何功能。</p>
        </header>

        <!-- ═══ TRANSCRIPTION — outcome-first: pick WHERE audio becomes text ═══ -->
        <section class="set-group">
          <div class="set-group-h">
            <h3 class="set-group-t">转写</h3>
            <p class="set-group-d">讲座录音在这里变成文字。</p>
          </div>
          <div class="set-card">
            <div class="set-row stacked">
              <div class="seg">
                <button type="button" class={"seg-opt" + (transcriptionMode === "local" ? " on" : "")} onclick={() => setTranscriptionMode("local")}>本机</button>
                <button type="button" class={"seg-opt" + (transcriptionMode === "cloud" ? " on" : "")} onclick={() => setTranscriptionMode("cloud")}>Cloud API</button>
                <button type="button" class={"seg-opt" + (transcriptionMode === "homelab" ? " on" : "")} onclick={() => setTranscriptionMode("homelab")}>My homelab</button>
              </div>
              {#if transcriptionMode === "local"}
                <div class="set-row-d">零配置 — Whisper 跑在本机，首次转写会自动下载模型。最私密，但长讲座比另外两种慢。</div>
              {:else if transcriptionMode === "cloud"}
                <div class="set-row-d">无需服务器、无需托管 — 只要一个 API 密钥。Groq 免费额度就能在几秒内把一整堂课转成文字 with <span class="mono">large-v3-turbo</span>.</div>
              {:else}
                <div class="set-row-d">跑在 Homelab 地址后面的 WhisperX 服务上（在下方配置）— 为小时级录音设计，服务器配置 HF token 后还能区分说话人。音频不离开你的机器。</div>
              {/if}
            </div>


            {#if transcriptionMode === "cloud"}
              <div class="set-row stacked">
                <div class="set-row-t">服务商</div>
                <div class="seg">
                  <button type="button" class={"seg-opt" + (whisperCloudProvider === "groq" ? " on" : "")} onclick={() => applyCloudPreset("groq")}>Groq <span class="faint">free</span></button>
                  <button type="button" class={"seg-opt" + (whisperCloudProvider === "openai" ? " on" : "")} onclick={() => applyCloudPreset("openai")}>OpenAI</button>
                  <button type="button" class={"seg-opt" + (whisperCloudProvider === "custom" ? " on" : "")} onclick={() => applyCloudPreset("custom")}>Custom</button>
                </div>
              </div>
              {#if whisperCloudProvider === "custom"}
                <div class="set-row stacked">
                  <div class="set-row-t">服务地址</div>
                  <input class="input mono" bind:value={whisperCloudUrl} onchange={saveCloudWhisper} onblur={saveCloudWhisper} placeholder="https://api.example.com/v1" />
                  <div class="set-row-d">任何 OpenAI 兼容的 <span class="mono">/v1/audio/transcriptions</span> 端点。</div>
                </div>
              {/if}
              <div class="set-row stacked">
                <div class="set-row-t">API 密钥</div>
                <input class="input mono" type="password" bind:value={whisperApiKey} onchange={saveCloudWhisper} onblur={saveCloudWhisper} placeholder={whisperCloudProvider === "openai" ? "sk-…" : "gsk_…"} />
                {#if whisperCloudProvider === "groq"}
                  <div class="set-row-d">免费获取 — 不需要绑卡：
                    <button class="btn btn--sm btn--ghost" onclick={() => api.openExternal("https://console.groq.com/keys")}><Icon name="external" size={11} /> console.groq.com/keys</button>
                  </div>
                {/if}
              </div>
              <div class="set-row stacked">
                <div class="set-row-t">模型</div>
                <input class="input mono" bind:value={whisperCloudModel} onchange={saveCloudWhisper} onblur={saveCloudWhisper} placeholder="whisper-large-v3-turbo" />
              </div>
            {/if}

            {#if transcriptionMode === "homelab"}
              <div class="set-row stacked">
                <div class="set-row-t">模型 <span class="faint">仅兼容旧服务</span></div>
                <input class="input mono" bind:value={whisperModel} onchange={saveWhisperModel} onblur={saveWhisperModel} placeholder="deepdml/faster-whisper-large-v3-turbo-ct2" />
                <div class="set-row-d">仅 OpenAI 兼容服务（speaches）使用。WhisperX 讲座服务器在以下位置选择模型 <span class="mono">docker-compose</span> instead (<span class="mono">WHISPER_MODEL</span>, default <span class="mono">distil-large-v3</span> 中配置）— 这里留空即可。</div>
              </div>
            {/if}

            {#if transcriptionMode !== "local"}
              <div class="set-row stacked">
                <div class="row-inline">
                  <button class="btn" onclick={checkWhisper} disabled={whisperCheckState === "checking"}>
                    <Icon name="refresh" size={12} /> {whisperCheckState === "checking" ? "Verifying…" : "Verify setup"}
                  </button>
                </div>
                {#if whisperCheckNote}
                  <div class="set-row-d" style="color:{whisperCheckState === 'fail' ? 'var(--err,#e5484d)' : whisperCheckState === 'ok' ? 'var(--ok)' : 'var(--fg-muted)'}">{whisperCheckNote}</div>
                {/if}
              </div>
            {/if}
          </div>
        </section>

        <section class="set-group">
          <div class="set-group-h svc-h">
            <div>
              <h3 class="set-group-t">Homelab 地址</h3>
              <p class="set-group-d">一个地址管所有。跑起 <span class="mono">homelab/</span> 的 docker compose 然后把 Cortex 指到这里 — 搜索、讲座转写、即时同步、移动端入库和本地模型都走这同一个地址（Cortex 会自动补上  <span class="mono">/searxng</span>, <span class="mono">/whisper</span>, <span class="mono">/sync</span>, <span class="mono">/syncd</span>, <span class="mono">/ingest</span>, <span class="mono">/ollama</span> 。填上 Tailscale 和/或公网地址后，Cortex 会自动选第一个能连上的：<strong>local → Tailscale → public</strong>.</p>
            </div>
            {#if hlBase.trim()}
              {@const p = connPill(hlState === "idle" ? null : hlState, true)}
              <span class="status-pill status-pill--{p.cls}"><span class="dot"></span>{p.label}</span>
            {/if}
          </div>
          <div class="set-card">
            <div class="set-row stacked">
              <div class="set-row-t">本地地址</div>
              <input class="input mono" bind:value={hlBase} oninput={saveHomelabBasesSoon} onchange={saveHomelabBases} onblur={saveHomelabBases} placeholder="http://192.168.1.10:8080" />
              <div class="set-row-d">homelab 的局域网地址（Caddy 代理端口，默认 <span class="mono">8080</span>).</div>
            </div>
            <div class="set-row stacked">
              <div class="set-row-t">Tailscale 地址 <span class="faint">optional</span></div>
              <input class="input mono" bind:value={hlTailscale} oninput={saveHomelabBasesSoon} onchange={saveHomelabBases} onblur={saveHomelabBases} placeholder="https://homelab.tailnet-xxxx.ts.net" />
              <div class="set-row-d">本地地址不可达时使用。只替换主机（和端口），服务路径保持不变。</div>
            </div>
            <div class="set-row stacked">
              <div class="set-row-t">公网地址 <span class="faint">optional</span></div>
              <input class="input mono" bind:value={hlPublic} oninput={saveHomelabBasesSoon} onchange={saveHomelabBases} onblur={saveHomelabBases} placeholder="https://lab.example.com" />
            </div>
            <div class="set-row stacked">
              <div class="set-row-t">访问令牌 <span class="faint">可选 — 仅公网地址需要</span></div>
              <input class="input mono" type="password" bind:value={hlToken} onchange={saveHlToken} onblur={saveHlToken} placeholder="the CORTEX_TOKEN your proxy was started with" />
              <div class="set-row-d">用同一个共享密钥锁住所有 homelab 服务。启动代理时带上 <span class="mono">CORTEX_TOKEN=… docker compose up -d</span> 然后把同样的值贴在这里 — 之后每次请求 Cortex 都会自动带上（同步用的 WebDAV 凭据是另一套，单独保存）。</div>
              {#if hlPublic.trim() && !hlToken.trim()}
                <div class="set-row-d" style="color:var(--warn)">你的 homelab 有公网地址但没有访问令牌 — 互联网上任何人都能使用你的 Whisper/Ollama/SearXNG. Set <span class="mono">CORTEX_TOKEN</span> 填在代理上，然后把值贴到这里。</div>
              {/if}
            </div>
            <div class="set-row stacked">
              <div class="row-inline" style="flex-wrap:wrap; gap:8px 12px; align-items:center">
                <button class="btn" onclick={testHomelab} disabled={hlState === 'testing' || !(hlBase.trim() || hlTailscale.trim() || hlPublic.trim())}>
                  <Icon name="refresh" size={12} /> {hlState === 'testing' ? "Testing…" : "Test homelab"}
                </button>
                {#if hlReach.local}<span class="hl-reach" style="color:{hlReach.local === 'ok' ? 'var(--ok)' : 'var(--err,#e5484d)'}">LAN {hlReach.local === 'ok' ? '✓' : '✗'}</span>{/if}
                {#if hlReach.tailscale}<span class="hl-reach" style="color:{hlReach.tailscale === 'ok' ? 'var(--ok)' : 'var(--err,#e5484d)'}">Tailscale {hlReach.tailscale === 'ok' ? '✓' : '✗'}</span>{/if}
                {#if hlReach.public}<span class="hl-reach" style="color:{hlReach.public === 'ok' ? 'var(--ok)' : 'var(--err,#e5484d)'}">Public {hlReach.public === 'ok' ? '✓' : '✗'}</span>{/if}
              </div>
            </div>
            <!-- Per-service health: each feature probed on the exact URL the app
                 uses, so a red row names the ONE thing to fix (and how). -->
            {#if svcTesting && !svcStatus}
              <div class="set-row-d faint" style="margin-top:6px">正在逐项检查服务…</div>
            {:else if svcStatus}
              <div style="margin-top:10px">
                {#each svcStatus as s (s.id)}
                  <div class="dep-row" class:dep-missing={s.configured && !s.ok}>
                    <span class="dep-dot" class:on={s.ok}></span>
                    <div class="dep-main">
                      <span class="dep-name">{s.label}</span>
                      <span class="dep-detail mono">{s.detail}</span>
                    </div>
                    <span class="dep-status {s.ok ? 'ok' : s.configured ? 'miss' : ''}">{s.ok ? "working" : s.configured ? "problem" : "not set"}</span>
                  </div>
                {/each}
              </div>
            {/if}
          </div>
        </section>

        <!-- Local CLI dependencies (pdftotext/libreoffice/age/…) never exist on a phone. -->
        {#if !isMobile}
        <section class="set-group">
          <div class="set-group-h svc-h">
            <div>
              <h3 class="set-group-t">依赖组件</h3>
              <p class="set-group-d">Cortex 处理入库、OCR、转写和媒体时调用的外部工具。用下面那一条命令装齐缺的。</p>
            </div>
            <button class="btn btn--sm btn--ghost" onclick={loadDeps} disabled={depLoading}><Icon name="refresh" size={12} /> {depLoading ? "Checking…" : "Re-check"}</button>
          </div>
          <div class="set-card">
            {#if depReport}
              {#each depReport.deps as d (d.name)}
                <div class="dep-row" class:dep-missing={!d.present}>
                  <span class="dep-dot" class:on={d.present}></span>
                  <div class="dep-main">
                    <span class="dep-name">{d.name}</span>
                    <span class="dep-detail mono">{d.detail}</span>
                  </div>
                  <span class="dep-status {d.present ? 'ok' : 'miss'}">{d.present ? "installed" : "missing"}</span>
                </div>
              {/each}
              {#if depReport.install_command}
                <div class="set-row stacked" style="margin-top:10px">
                  <div class="set-row-t">安装缺失的 <span class="faint">· {depReport.manager}</span></div>
                  <div class="row-inline">
                    <input class="input mono" readonly value={depReport.install_command} />
                    <button class="btn" onclick={copyDepCmd}><Icon name="doc" size={12} /> Copy</button>
                    {#if depReport.manager === "brew"}
                      <button class="btn btn--primary" onclick={installDeps} disabled={depInstalling}><Icon name="plus" size={12} /> {depInstalling ? "Installing…" : "Install"}</button>
                    {/if}
                  </div>
                  <div class="set-row-d">{depReport.manager === "brew" ? "One-click install via Homebrew (no sudo). LibreOffice is large — this can take a few minutes." : depReport.note}</div>
                </div>
              {:else}
                <div class="set-row-d" style="color:var(--ok); margin-top:8px">依赖已全部就位 🎉</div>
              {/if}
            {:else}
              <div class="set-row-d faint">{depLoading ? "Checking installed tools…" : "Couldn't check dependencies."}</div>
            {/if}
          </div>
        </section>
        {/if}

        <!-- One section of toggleable homelab features — same on desktop + mobile. No
             per-service URL overrides; everything rides the single Homelab URL above. -->
        <section class="set-group">
          <div class="set-group-h">
            <h3 class="set-group-t">Homelab 功能</h3>
            <p class="set-group-d">由 Homelab 地址后面的服务提供的可选功能 — 一填上地址就自动启用。</p>
          </div>
          <div class="set-card">
            {@render diagramsToggle()}
          </div>
        </section>
        <section class="set-group">
          <div class="set-group-h svc-h">
            <div>
              <h3 class="set-group-t">实时同步</h3>
              <p class="set-group-d">跨设备即时同步。每次改动都以「按记录的微小增量」通过 WebSocket 流到你的其他设备（homelab 的 <span class="mono">/syncd</span> 服务），大约一秒内出现在其他设备上 — 最新的编辑胜出，而且除非你真的删除，否则任何东西都不会被删掉。来源文件和整体快照走 <span class="mono">/sync</span> WebDAV 仓库，它同时作为自动兜底 — 当 <span class="mono">/syncd</span> 连不上时。</p>
            </div>
            <span class="status-pill status-pill--{syncPill().cls}"><span class="dot"></span>{syncPill().label}</span>
          </div>
          <div class="set-card">
            <div class="set-row">
              <div class="set-row-l">
                <div class="set-row-t">启用实时同步</div>
                <div class="set-row-d">启动时在后台合并远端仓库（不阻塞启动），然后把你的改动推上去（带防抖）。</div>
              </div>
              <div class="set-row-r">
                <button type="button" class={"st-toggle" + (syncOn ? " on" : "")} onclick={toggleSync} disabled={!canSync} role="switch" aria-checked={syncOn} aria-label="live sync"><span class="st-knob"></span></button>
              </div>
            </div>

            <div class="set-row stacked">
              <div class="set-row-d">复用上面的 <strong>Homelab 地址</strong>（它的 <span class="mono">/syncd</span> + <span class="mono">/sync</span> 服务），可达路径同样是 本地 → Tailscale → 公网。下面一套用户名/密码同时覆盖两者 — 就是 <span class="mono">docker-compose.yml</span> 里为 <span class="mono">sync</span> 和 <span class="mono">syncd</span> 设置的那一对。</div>
              {#if syncOn}
                {#if app.syncLive}
                  <div class="set-row-d" style="color:var(--ok)">实时 — 已连接 <span class="mono">/syncd</span>；改动会在一秒内同步到所有设备。</div>
                {:else if syncdOk === false}
                  <div class="set-row-d" style="color:var(--warn)">当前是快照模式：homelab 的实时同步服务没有响应（见上面的服务状态检查）。更新 homelab 后重试 — <span class="mono">git pull && docker compose up -d --build</span> — 才能用实时同步；在那之前改动仍会通过 WebDAV 快照同步。</div>
                {:else if app.syncState === "error"}
                  <div class="set-row-d" style="color:var(--warn)">同步失败了 — 点上面的 <strong>测试 homelab</strong>，服务检查会指出具体原因（地址、凭据，或某个服务没在跑）。</div>
                {:else}
                  <div class="set-row-d faint">实时同步当前不可用 — 快照同步仍在后台跑。上面的 <strong>测试 homelab</strong> 会检查你的 homelab 是否提供 <span class="mono">/syncd</span> 实时同步服务。</div>
                {/if}
              {/if}
            </div>
            <div class="set-row stacked">
              <div class="set-row-t">用户名 <span class="faint">optional</span></div>
              <input class="input mono" bind:value={syncUser} oninput={saveSyncSoon} onchange={saveSync} onblur={saveSync} placeholder="cortex" />
            </div>
            <div class="set-row stacked">
              <div class="set-row-t">密码 <span class="faint">optional</span></div>
              <div class="row-inline">
                <input class="input mono" type="password" bind:value={syncPass} oninput={saveSyncSoon} onchange={saveSync} onblur={saveSync} placeholder="••••••••" />
                <button class="btn btn--primary" disabled={app.syncState === "syncing" || !canSync} onclick={() => app.syncManual()}>
                  <Icon name="upload" size={12} /> {app.syncState === "syncing" ? "Syncing…" : "Sync now"}
                </button>
              </div>
              <div class="set-row-d">上次快照同步：<span class="mono">{fmtSyncTime(app.syncLastAt)}</span>（实时增量不会更新这个时间 — 它代表周期性全量合并）。文件存放在目标机的 <span class="mono">files/</span> 目录下；数据库按记录合并。</div>
            </div>
          </div>
        </section>

        <!-- Encrypted backups use the age + rclone sidecars — desktop-only. -->
        {#if !isMobile}
        <section class="set-group">
          <div class="set-group-h svc-h">
            <div>
              <h3 class="set-group-t">加密备份</h3>
              <p class="set-group-d">把数据库打快照，用 <span class="mono">age</span> 加密，再用 <span class="mono">rclone</span> 上传。未加密的内容不会离开本机。</p>
            </div>
            {#if backupInfo}
              <div class="svc-tools">
                <span class="badge {backupInfo.age_found ? 'badge--ok' : 'badge--err'}"><span class="dot"></span>age</span>
                <span class="badge {backupInfo.rclone_found ? 'badge--ok' : 'badge--err'}"><span class="dot"></span>rclone</span>
              </div>
            {/if}
          </div>
          <div class="set-card">
            <div class="set-row stacked">
              <div class="set-row-t">age 收件人（公钥）</div>
              <input class="input mono" bind:value={ageRecipient} onchange={saveBackupConfig} onblur={saveBackupConfig} placeholder="age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p" />
              <div class="set-row-d">来自 <span class="mono">age-keygen</span>。只有这把密钥的持有者能解开备份。</div>
            </div>
            <div class="set-row stacked">
              <div class="set-row-t">rclone 远程</div>
              <div class="row-inline">
                <input class="input mono" bind:value={rcloneRemote} onchange={saveBackupConfig} onblur={saveBackupConfig} placeholder="homelab:cortex-backups" />
                <button class="btn btn--primary" disabled={backingUp} onclick={runBackup}>
                  <Icon name={backingUp ? "refresh" : "upload"} size={12} /> {backingUp ? "Backing up…" : "Back up now"}
                </button>
              </div>
              <div class="set-row-d">
                An rclone remote + path, e.g. <span class="mono">homelab:cortex-backups</span> （在 <span class="mono">rclone 配置</span>).
                Last backup: <span class="mono">{fmtBackupTime(backupInfo?.last_at ?? null)}</span>{#if backupInfo?.last_dest} → <span class="mono">{backupInfo.last_dest}</span>{/if}
              </div>
            </div>
          </div>
        </section>
        {/if}

        <section class="set-group">
          <div class="set-group-h svc-h">
            <div>
              <h3 class="set-group-t">可移植归档</h3>
              <p class="set-group-d">把整个库打成一个 <span class="mono">.cortex-pkg.tar.gz</span>：数据库快照 + 附件 + 每文件 SHA-256。可 grep、可 diff、可换机迁移；与上面的加密云端备份互补。</p>
            </div>
          </div>
          <div class="set-card">
            <div class="set-row">
              <div>
                <div class="set-row-t">导出 / 校验 / 导入</div>
                <div class="set-row-d">
                  导入会先逐文件校验哈希，被篡改的归档直接拒绝；附件缺失默认拒绝导出（可选跳过并记录）。
                  {#if pkgInfo}
                    <br />最近导出：<span class="mono">{pkgInfo.files}</span> 个附件 ·
                    <span class="mono">{(pkgInfo.bytes / 1048576).toFixed(1)} MB</span> ·
                    sha256 <span class="mono">{pkgInfo.sha256.slice(0, 16)}…</span>
                  {/if}
                </div>
              </div>
              <div class="row-inline">
                <button class="btn btn--primary" disabled={pkgBusy !== null} onclick={exportPkg}>
                  <Icon name={pkgBusy === "export" ? "refresh" : "doc"} size={12} />
                  {pkgBusy === "export" ? "导出中…" : "导出归档"}
                </button>
                <button class="btn" disabled={pkgBusy !== null} onclick={verifyPkg}>
                  <Icon name="check" size={12} /> {pkgBusy === "verify" ? "校验中…" : "校验"}
                </button>
                <button class="btn" disabled={pkgBusy !== null} onclick={importPkg}>
                  <Icon name="upload" size={12} /> {pkgBusy === "import" ? "导入中…" : "导入"}
                </button>
              </div>
            </div>
          </div>
        </section>

        <section class="set-group">
          <div class="set-group-h svc-h">
            <div>
              <h3 class="set-group-t">恢复目录</h3>
              <p class="set-group-d">删除科目时完整快照（行 + 附件）会移到 <span class="mono">.deleted-&lt;id&gt;-&lt;ts&gt;</span>，不自动清除。恢复后索引需重新生成；确认不再需要再彻底删除。</p>
            </div>
            <div class="svc-tools">
              <button class="btn btn--sm" disabled={offsetBusy} onclick={runBackfillOffsets}
                      title="为 0046 之前入库的分块补上 char_start/char_end（幂等）">
                <Icon name="link" size={12} /> {offsetBusy ? "补偿中…" : "补偿引文偏移"}
              </button>
              <button class="btn btn--sm" onclick={loadDeleted}><Icon name="refresh" size={12} /> 刷新</button>
            </div>
          </div>
          <div class="set-card">
            {#if !deleted.length}
              <p class="set-row-d" style="padding:6px 0">没有已删除的科目。</p>
            {:else}
              {#each deleted as d (d.dir_name)}
                <div class="set-row">
                  <div class="set-row-l">
                    <div class="set-row-t">「{d.subject_name}」· {new Date(d.deleted_at).toLocaleString("zh-CN")}</div>
                    <div class="set-row-d mono">{d.sources} 个来源 · {d.notes} 条笔记 · {(d.bytes / 1048576).toFixed(1)} MB</div>
                  </div>
                  <div class="set-row-r">
                    <button class="btn btn--sm" disabled={recoveryBusy !== null} onclick={() => restoreDeleted(d)}>
                      {recoveryBusy === d.dir_name ? "处理中…" : "恢复"}
                    </button>
                    <button class="btn btn--sm btn--danger" disabled={recoveryBusy !== null} onclick={() => purgeDeleted(d)}>清除</button>
                  </div>
                </div>
              {/each}
            {/if}
          </div>
        </section>

      </div>

    <!-- ===== AUDIO ===== -->
    {:else if tab === "audio"}
      <div class="set-pane">
        <header class="set-head">
          <div class="eyebrow">Audio</div>
          <h1 class="set-title">学习音乐与音色</h1>
          <p class="set-sub">音乐播放器和音频概览的默认设置。</p>
        </header>

        <!-- Music is cut on mobile (no mpv/yt-dlp sidecars) — hide its settings. -->
        {#if !isMobile}
        <section class="set-group">
          <div class="set-group-h"><h3 class="set-group-t">学习音乐</h3></div>
          <div class="set-card">
            <div class="set-row">
              <div class="set-row-l"><div class="set-row-t">默认电台</div></div>
              <div class="set-row-r">
                <Picker
                  value={station}
                  onChange={(v) => { station = v; app.music = { ...app.music, current: v }; }}
                  options={allStations}
                />
              </div>
            </div>
            <div class="set-row">
              <div class="set-row-l"><div class="set-row-t">启动时自动播放</div></div>
              <div class="set-row-r">
                <button type="button" class={"st-toggle" + (autoplay ? " on" : "")} onclick={() => (autoplay = !autoplay)} role="switch" aria-checked={autoplay} aria-label="autoplay"><span class="st-knob"></span></button>
              </div>
            </div>
          </div>
        </section>
        {/if}

        <!-- YouTube streaming needs the mpv + yt-dlp sidecars, which don't ship on
             mobile — hide the whole section there. -->
        {#if !isMobile}
        <section class="set-group">
          <div class="set-group-h">
            <h3 class="set-group-t">YouTube 播放</h3>
            <p class="set-group-d">在音乐面板里粘贴 YouTube 视频或直播地址，就能无广告播放。用无头 <span class="mono">mpv</span> + <span class="mono">yt-dlp</span> （首次使用自动下载）。应用不附带任何东西 — 只保存这个地址。</p>
          </div>
          <div class="set-card">
            <div class="set-row">
              <div class="set-row-l"><div class="set-row-t">工具</div></div>
              <div class="set-row-r">
                {#if mediaTools}
                  <span class="mono" style="color:{mediaTools.mpv ? 'var(--ok)' : 'var(--danger,#e06c75)'}">
                    <Icon name={mediaTools.mpv ? "check" : "x"} size={12} /> mpv
                  </span>
                  <span class="mono" style="margin-left:14px;color:{mediaTools.ffmpeg ? 'var(--ok)' : 'var(--danger,#e06c75)'}">
                    <Icon name={mediaTools.ffmpeg ? "check" : "x"} size={12} /> ffmpeg
                  </span>
                  <span class="mono" style="margin-left:14px;color:{mediaTools.ytdlp ? 'var(--ok)' : 'var(--warn)'}">
                    <Icon name={mediaTools.ytdlp ? "check" : "refresh"} size={12} /> yt-dlp
                  </span>
                {:else}
                  <span class="mono faint">…</span>
                {/if}
              </div>
            </div>
            {#if mediaTools && !mediaTools.mpv}
              <div class="set-row">
                <div class="set-row-l"><div class="set-row-d">安装 mpv 才能播放 YouTube 流：<span class="mono">sudo pacman -S mpv</span></div></div>
              </div>
            {:else if mediaTools && !mediaTools.ytdlp}
              <div class="set-row">
                <div class="set-row-l"><div class="set-row-d">第一次播放 YouTube 电台时会自动下载 yt-dlp。</div></div>
              </div>
            {/if}
          </div>
        </section>
        {/if}

        <section class="set-group">
          <div class="set-group-h">
            <h3 class="set-group-t">专注计时</h3>
            <p class="set-group-d">番茄钟各段时长（全局生效）。</p>
          </div>
          <div class="set-card">
            {#each pomoFields as f (f.key)}
              <div class="set-row">
                <div class="set-row-l"><div class="set-row-t">{f.label}</div></div>
                <div class="set-row-r">
                  <div style="display:flex;align-items:center;gap:8px">
                    <button class="btn btn--icon btn--sm" onclick={() => setPomo(f.key, -f.step)} aria-label="decrease {f.label}">−</button>
                    <span class="mono" style="min-width:62px;text-align:center;color:var(--fg-bright)">{pomoVal(f.key)}{f.unit}</span>
                    <button class="btn btn--icon btn--sm" onclick={() => setPomo(f.key, f.step)} aria-label="increase {f.label}">+</button>
                  </div>
                </div>
              </div>
            {/each}
          </div>
        </section>

        <section class="set-group">
          <div class="set-group-h">
            <h3 class="set-group-t">音频概览音色</h3>
            <p class="set-group-d">生成播客的两位主持人。</p>
          </div>
          <div class="set-card">
            <div class="set-row">
              <div class="set-row-l"><div class="set-row-t">主机 A</div></div>
              <div class="set-row-r">
                <Picker
                  value={voiceA}
                  onChange={(v) => (voiceA = v)}
                  options={[{ id: "maya", label: "Maya · warm" }, { id: "nova", label: "Nova · bright" }, { id: "io", label: "Io · neutral" }]}
                />
              </div>
            </div>
            <div class="set-row">
              <div class="set-row-l"><div class="set-row-t">主机 B</div></div>
              <div class="set-row-r">
                <Picker
                  value={voiceB}
                  onChange={(v) => (voiceB = v)}
                  options={[{ id: "theo", label: "Theo · calm" }, { id: "rex", label: "Rex · energetic" }, { id: "sol", label: "Sol · deep" }]}
                />
              </div>
            </div>
          </div>
        </section>
      </div>

    <!-- ===== GOOGLE CALENDAR ===== -->
    {:else if tab === "calendar"}
      <div class="set-pane">
        <header class="set-head">
          <div class="eyebrow">Google Calendar</div>
          <h1 class="set-title">同步你的日历</h1>
          <p class="set-sub">与 Google 日历双向同步。不连也能完整使用自带日历 — 连接只是让日程两边镜像。</p>
        </header>

        <section class="set-group">
          <div class="set-group-h">
            <h3 class="set-group-t">状态</h3>
          </div>
          <div class="set-card">
            <div class="set-row">
              <div class="set-row-l">
                <div class="set-row-t">连接</div>
                <div class="set-row-d mono faint">
                  {#if gStatus?.connected}
                    Connected{gStatus.email ? " · " + gStatus.email : ""}
                  {:else if gStatus?.configured}
                    Credentials saved — not connected yet
                  {:else}
                    Not configured
                  {/if}
                </div>
              </div>
              <div class="set-row-r">
                {#if gStatus?.connected}
                  <div class="row-inline">
                    <button class="btn" onclick={syncGoogle} disabled={gBusy}>
                      <Icon name="refresh" size={12} /> Sync now
                    </button>
                    <button class="btn btn--danger" onclick={disconnectGoogle} disabled={gBusy}>Disconnect</button>
                  </div>
                {:else}
                  <button class="btn btn--primary" onclick={connectGoogle} disabled={gBusy}>
                    <Icon name="globe" size={12} /> {gBusy ? "Connecting…" : "Connect Google"}
                  </button>
                {/if}
              </div>
            </div>
          </div>
        </section>

        {#if gStatus?.connected}
          <section class="set-group">
            <div class="set-group-h">
              <h3 class="set-group-t">要同步的日历</h3>
              <p class="set-group-d">选择要从哪些 Google 日历拉取日程 — 在这里勾选你的 <strong>学校/课程表</strong> 日历，它的课程、截止日和考试就会进入 Cortex 日历。</p>
            </div>
            <div class="set-card">
              {#if gCalBusy && gCalendars.length === 0}
                <div class="set-row"><div class="set-row-d faint">正在加载日历…</div></div>
              {:else if gCalendars.length === 0}
                <div class="set-row">
                  <div class="set-row-l"><div class="set-row-d faint">没有找到日历。</div></div>
                  <div class="set-row-r"><button class="btn btn--sm" onclick={loadGoogleCalendars}>Reload</button></div>
                </div>
              {:else}
                {#each gCalendars as cal (cal.id)}
                  <label class="gcal-row" class:on={cal.selected}>
                    <input type="checkbox" checked={cal.selected} onchange={() => toggleGoogleCal(cal.id)} />
                    <span class="gcal-swatch" style:background={cal.color || "var(--border-strong)"}></span>
                    <span class="gcal-name">{cal.summary}</span>
                    {#if cal.primary}<span class="gcal-tag mono">primary</span>{/if}
                    {#if cal.selected}<span class="gcal-on mono">syncing</span>{/if}
                  </label>
                {/each}
              {/if}
            </div>
          </section>
        {/if}

        <section class="set-group">
          <div class="set-group-h">
            <h3 class="set-group-t">凭据</h3>
            <p class="set-group-d">在 Google Cloud → APIs &amp; Services → 凭据 里创建一个「桌面应用」类型的 OAuth 客户端，启用 Calendar API，然后把 ID 和密钥贴到这里。</p>
          </div>
          <div class="set-card">
            <div class="set-row stacked">
              <div class="set-row-l"><div class="set-row-t">客户端 ID</div></div>
              <div class="set-row-r">
                <input class="input mono" bind:value={gClientId} onblur={saveGoogleCreds} placeholder="…apps.googleusercontent.com" />
              </div>
            </div>
            <div class="set-row stacked">
              <div class="set-row-l"><div class="set-row-t">客户端密钥</div></div>
              <div class="set-row-r">
                <input class="input mono" type="password" bind:value={gClientSecret} onblur={saveGoogleCreds} placeholder="GOCSPX-…" />
              </div>
            </div>
          </div>
        </section>
      </div>

    <!-- ===== EXPERIMENTAL ===== -->
    {:else if tab === "experimental"}
      <div class="set-pane">
        <header class="set-head">
          <div class="eyebrow">Settings</div>
          <h2 class="set-title">Experimental</h2>
          <p class="set-sub">早期功能，可能粗糙或变动。开一个试试看。</p>
        </header>

        <section class="set-group">
          <div class="set-group-h">
            <div>
              <h3 class="set-group-t">提示词实验室</h3>
              <p class="set-group-d">
                模型用的提示词现在可以改：并排跑两版、用固定用例判分，改坏了还能一键还原。在侧栏「实验室」里。
              </p>
            </div>
            <button class="btn btn--sm" onclick={() => app.setView("lab")}>打开实验室</button>
          </div>
        </section>

        <section class="set-group">
          <div class="set-group-h">
            <div>
              <h3 class="set-group-t">检索评测</h3>
              <p class="set-group-d">
                用一份固定的合成语料跑四种检索配置，看各自能召回多少。语料临时写入一个名为「(eval) 检索评测」的科目，不会和你自己的资料混在一起。
              </p>
            </div>
            <button class="btn btn--primary btn--sm" disabled={evalBusy} onclick={runRetrievalEval}>
              {evalBusy ? "评测中…" : "运行评测"}
            </button>
          </div>

          {#if evalError}
            <div class="set-row-d" style="color:var(--err)">{evalError}</div>
          {/if}

          {#if evalReports}
            <div class="set-card">
              <table class="eval-table">
                <thead>
                  <tr><th>配置</th><th>Recall@5</th><th>MRR</th><th>命中</th></tr>
                </thead>
                <tbody>
                  {#each evalReports as r (r.mode)}
                    <tr class:best={evalBest?.mode === r.mode && evalBest.recall_at_k > 0}>
                      <td class="mono">{r.mode}</td>
                      <td class="mono">{(r.recall_at_k * 100).toFixed(0)}%</td>
                      <td class="mono">{r.mrr.toFixed(3)}</td>
                      <td class="mono">{r.rows.filter((x) => x.rank).length}/{r.evaluated}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>

              {#each evalReports as r (r.mode)}
                {@const misses = r.rows.filter((x) => !x.rank)}
                {#if misses.length}
                  <div class="eval-misses">
                    <div class="set-row-t mono">{r.mode} · 未命中 {misses.length}</div>
                    {#each misses as m (m.query)}
                      <div class="eval-miss">
                        <span class="mono eval-q">{m.query}</span>
                        <span class="faint">期望 {m.expect}，实际前五：{m.got.slice(0, 5).join(" / ") || "无"}</span>
                      </div>
                    {/each}
                  </div>
                {/if}
              {/each}
            </div>
          {/if}
        </section>

        <section class="set-group">
          <div class="set-group-h">
            <div>
              <h3 class="set-group-t">真实语料评测</h3>
              <p class="set-group-d">
                合成语料只能证明流水线没坏；这一步在你的真实笔记上证明检索确实好用。查询词取自你已索引的分块（无 LLM、确定性采样），期望答案是分块所在的来源。
              </p>
            </div>
            <button class="btn btn--primary btn--sm" disabled={realEvalBusy} onclick={runRealEval}>
              {realEvalBusy ? "评测中…" : "运行真实评测"}
            </button>
          </div>

          <div class="set-card">
            <div class="set-row-d">
              评测范围：
              {#if !evalSubjects.length}
                还没有科目 —— 先去添加并索引来源。
              {:else}
                <span class="mono">{evalSubjectIds.length ? `已选 ${evalSubjectIds.length} 个科目` : "全部科目"}</span>
              {/if}
            </div>
            <div class="eval-subjects">
              {#each evalSubjects as s (s.id)}
                <button
                  type="button"
                  class={"eval-subject" + (evalSubjectIds.includes(s.id) ? " on" : "")}
                  onclick={() => toggleEvalSubject(s.id)}
                >{s.name}</button>
              {/each}
            </div>
          </div>

          {#if realEvalError}
            <div class="set-row-d" style="color:var(--err)">{realEvalError}</div>
          {/if}

          {#if realEvalReports}
            <div class="set-card">
              <table class="eval-table">
                <thead>
                  <tr><th>配置</th><th>Recall@5</th><th>MRR</th><th>命中</th></tr>
                </thead>
                <tbody>
                  {#each realEvalReports as r (r.mode)}
                    <tr class:best={realEvalBest?.mode === r.mode && realEvalBest.recall_at_k > 0}>
                      <td class="mono">{r.mode}</td>
                      <td class="mono">{(r.recall_at_k * 100).toFixed(0)}%</td>
                      <td class="mono">{r.mrr.toFixed(3)}</td>
                      <td class="mono">{r.rows.filter((x) => x.rank).length}/{r.evaluated}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>

              {#each realEvalReports as r (r.mode)}
                {@const misses = r.rows.filter((x) => !x.rank)}
                {#if misses.length}
                  <div class="eval-misses">
                    <div class="set-row-t mono">{r.mode} · 未命中 {misses.length}</div>
                    {#each misses as m (m.query)}
                      <div class="eval-miss">
                        <span class="mono eval-q">{m.query}</span>
                        <span class="faint">期望 {m.expect}，实际前五：{m.got.slice(0, 5).join(" / ") || "无"}</span>
                      </div>
                    {/each}
                  </div>
                {/if}
              {/each}
            </div>
          {/if}
        </section>

        <section class="set-group">
          <div class="set-group-h svc-h">
            <div>
              <h3 class="set-group-t">学校门户（Moodle）</h3>
              <p class="set-group-d">把成绩、作业、截止日和公告从 Moodle 门户拉进 Cortex。</p>
            </div>
            <button class={"st-toggle" + (expMoodle ? " on" : "")} type="button" onclick={toggleExpMoodle} role="switch" aria-checked={expMoodle} aria-label="enable moodle"><span class="st-knob"></span></button>
          </div>

          {#if expMoodle}
            <div class="set-card">
              <div class="set-row">
                <div class="set-row-l">
                  <div class="set-row-t">连接</div>
                  <div class="set-row-d">{mdStatus.configured ? `Connected · last sync ${fmtSyncTime(mdStatus.last_sync)}` : "Not connected"}</div>
                </div>
                <span class="status-pill status-pill--{mdStatus.configured ? 'ready' : 'pending'}"><span class="dot"></span>{mdStatus.configured ? "Connected" : "Off"}</span>
              </div>

              <div class="set-row stacked">
                <div class="set-row-t">Moodle 站点地址</div>
                <input class="input mono" bind:value={mdUrl} placeholder="https://moodle.your-school.edu" />
              </div>

              <div class="set-row">
                <div class="set-row-l">
                  <div class="set-row-t">登录方式</div>
                  <div class="set-row-d">很多学校用 SSO（Microsoft/SAML），用户名密码往往不可用 — 请改填 web services 令牌。</div>
                </div>
                <div class="set-row-r" style="gap:6px">
                  <button class={"btn btn--sm" + (mdAuthMode === 'token' ? ' btn--primary' : ' btn--ghost')} type="button" onclick={() => (mdAuthMode = 'token')}>Token</button>
                  <button class={"btn btn--sm" + (mdAuthMode === 'password' ? ' btn--primary' : ' btn--ghost')} type="button" onclick={() => (mdAuthMode = 'password')}>Password</button>
                </div>
              </div>

              {#if mdAuthMode === 'password'}
                <div class="set-row stacked">
                  <div class="set-row-t">用户名</div>
                  <input class="input mono" bind:value={mdUser} placeholder="student number" />
                </div>
                <div class="set-row stacked">
                  <div class="set-row-t">密码</div>
                  <input class="input mono" type="password" bind:value={mdPass} placeholder="••••••••" />
                </div>
              {:else}
                <div class="set-row stacked">
                  <div class="set-row-t">Web services 令牌</div>
                  <input class="input mono" bind:value={mdToken} placeholder="paste your Moodle token" />
                  <div class="set-row-d">可从官方 Moodle App 或浏览器登录获取。仅存本地，密码不会发送到任何地方。</div>
                </div>
              {/if}

              <div class="set-row">
                <div class="set-row-l"><div class="set-row-d">先连接，再同步，把你的数据拉下来。</div></div>
                <div class="set-row-r" style="gap:8px">
                  {#if mdStatus.configured}
                    <button class="btn btn--ghost btn--sm" type="button" onclick={mdDisconnect} disabled={mdBusy}>Disconnect</button>
                    <button class="btn btn--primary btn--sm" type="button" onclick={mdSyncNow} disabled={mdBusy}><Icon name="refresh" size={12} /> {mdBusy ? "Syncing…" : "Sync now"}</button>
                  {:else}
                    <button class="btn btn--sm" type="button" onclick={mdLoginSso}>通过浏览器登录（SSO）</button>
                    <button class="btn btn--primary btn--sm" type="button" onclick={mdConnect} disabled={mdBusy || (mdAuthMode==='token' ? !mdToken.trim() : !mdUser.trim())}>{mdBusy ? "Connecting…" : "Connect"}</button>
                  {/if}
                </div>
              </div>

              {#if mdStatus.configured}
                <div class="set-row">
                  <div class="set-row-l">
                    <div class="set-row-t">把科目关联到课程</div>
                    <div class="set-row-d">按课程代码/名称，自动把科目匹配到 Moodle 课程。</div>
                  </div>
                  <div class="set-row-r"><button class="btn btn--sm" type="button" onclick={mdAutolink}>Auto-link</button></div>
                </div>
              {/if}

              {#if mdData.courses.length}
                <div class="set-row stacked">
                  <div class="set-row-t">同步内容</div>
                  <div class="md-stats">
                    <span class="md-stat"><b>{mdData.courses.length}</b> courses</span>
                    <span class="md-stat"><b>{mdData.grades.length}</b> grades</span>
                    <span class="md-stat"><b>{mdData.deadlines.length}</b> deadlines</span>
                    <span class="md-stat"><b>{mdData.announcements.length}</b> announcements</span>
                  </div>
                  {#if mdUpcoming.length}
                    <div class="md-up-h mono">即将到来的截止日</div>
                    <ul class="md-up">
                      {#each mdUpcoming.slice(0, 6) as d (d.id)}
                        <li>
                          <span class="md-up-date mono">{new Date(d.due_at * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short" })}</span>
                          {#if d.url}
                            <button class="md-up-name md-up-link" onclick={() => d.url && api.openExternal(d.url)} title={d.name}>{d.name}</button>
                          {:else}
                            <span class="md-up-name" title={d.name}>{d.name}</span>
                          {/if}
                          {#if d.course_id}<span class="md-up-course" title={mdCourseName(d.course_id)}>{mdCourseName(d.course_id)}</span>{/if}
                        </li>
                      {/each}
                    </ul>
                  {/if}
                </div>
              {/if}
            </div>
          {/if}
        </section>

        <section class="set-group">
          <div class="set-group-h">
            <div>
              <h3 class="set-group-t">浏览器剪藏扩展</h3>
              <p class="set-group-d">用浏览器扩展把正在读的网页——或只把划选的一段——一键存进 Cortex，不用再手动贴 URL。</p>
            </div>
            <span class="status-pill status-pill--{clipStatus.running ? 'ready' : 'pending'}">
              <span class="dot"></span>{clipStatus.running ? "运行中" : "未启动"}
            </span>
          </div>
          <div class="set-card">
            <div class="set-row">
              <div class="set-row-l">
                <div class="set-row-t">本地接口</div>
                <div class="set-row-d">只在 Cortex 运行时监听本机回环地址，不暴露到局域网</div>
              </div>
              <div class="set-row-r">
                <span class="mono faint">{clipStatus.addr ? `http://${clipStatus.addr}` : "—"}</span>
              </div>
            </div>
            <div class="set-row">
              <div class="set-row-l">
                <div class="set-row-t">安装扩展</div>
                <div class="set-row-d">Chrome / Edge → 扩展程序 → 打开「开发者模式」→「加载已解压的扩展程序」，选择仓库里的 <span class="mono">browser-extension/</span>。</div>
              </div>
            </div>
            <div class="set-row">
              <div class="set-row-l">
                <div class="set-row-t">怎么用</div>
                <div class="set-row-d">点工具栏图标选学科后保存整页或划选内容；也可以直接在页面上右键「Clip to Cortex」入库。存下的条目自带 <span class="mono">clip</span> 标签。</div>
              </div>
            </div>
          </div>
        </section>
      </div>

    <!-- ===== DATA & PRIVACY ===== -->
    {:else if tab === "data"}
      <div class="set-pane">
        <header class="set-head">
          <div class="eyebrow">Data & privacy</div>
          <h1 class="set-title">默认本地优先</h1>
          <p class="set-sub">所有数据都存在本机的 SQLite 数据库里。它归你。</p>
        </header>

        <section class="set-group">
          <div class="set-group-h"><h3 class="set-group-t">存储</h3></div>
          <div class="set-card">
            <div class="set-row">
              <div class="set-row-l">
                <div class="set-row-t">数据库</div>
                <div class="set-row-d">~/.cortex/cortex.db</div>
              </div>
              <div class="set-row-r">
                <span class="mono faint">
                  {#if stats}
                    {fmtBytes(stats.db_bytes)} · {stats.subjects} subject{stats.subjects === 1 ? "" : "s"} · {stats.sources} source{stats.sources === 1 ? "" : "s"}
                  {:else}
                    …
                  {/if}
                </span>
              </div>
            </div>
            <div class="set-row">
              <div class="set-row-l">
                <div class="set-row-t">向量索引</div>
                <div class="set-row-d">用于检索的本地向量模型</div>
              </div>
              <div class="set-row-r">
                <span class="mono faint">{stats ? `${stats.chunks} chunk${stats.chunks === 1 ? "" : "s"}` : "…"}</span>
              </div>
            </div>
            <div class="set-row">
              <div class="set-row-l">
                <div class="set-row-t">离线模式</div>
                <div class="set-row-d">阻断所有网络请求，只用 Ollama。</div>
              </div>
              <div class="set-row-r">
                <button type="button" class={"st-toggle" + (offlineMode ? " on" : "")} onclick={toggleOffline} role="switch" aria-checked={offlineMode} aria-label="offline"><span class="st-knob"></span></button>
              </div>
            </div>
          </div>
        </section>

        {#if archivedSubjects.length}
          <section class="set-group">
            <div class="set-group-h">
              <h3 class="set-group-t">已归档科目</h3>
              <p class="set-group-d">不在应用里显示，但保留在存储中。随时可以恢复。</p>
            </div>
            <div class="set-card">
              {#each archivedSubjects as s (s.id)}
                <div class="set-row">
                  <div class="set-row-l">
                    <div class="set-row-t">
                      <span style="color:{app.subjectColor(s)}">{s.glyph}</span> {s.name}
                    </div>
                    <div class="set-row-d">
                      {s.code ? s.code + " · " : ""}{s.sourceCount} source{s.sourceCount === 1 ? "" : "s"}
                    </div>
                  </div>
                  <div class="set-row-r">
                    <button class="btn" onclick={() => restoreSubject(s.id)}>Restore</button>
                  </div>
                </div>
              {/each}
            </div>
          </section>
        {/if}

        {#if undoEntries.length || undoLoading || undoError}
          <section class="set-group">
            <div class="set-group-h">
              <h3 class="set-group-t">回收站</h3>
              <p class="set-group-d">最近删除的科目、主题、资料、笔记和材料。恢复会把行（连同分块和标签）一起找回来；条目 7 天后自动过期。</p>
            </div>
            <div class="set-card">
              {#if undoError}
                <div class="set-row">
                  <div class="set-row-l">
                    <div class="set-row-t" style="color:var(--err)">无法加载撤销记录</div>
                    <div class="set-row-d">{undoError}</div>
                  </div>
                  <div class="set-row-r">
                    <button class="btn" onclick={loadUndo}>Retry</button>
                  </div>
                </div>
              {/if}
              {#if undoLoading && !undoEntries.length}
                <div class="set-row">
                  <div class="set-row-l">
                    <div class="set-row-d mono faint">Loading…</div>
                  </div>
                </div>
              {/if}
              {#each undoEntries as e (e.id)}
                <div class="set-row">
                  <div class="set-row-l">
                    <div class="set-row-t">
                      <span class="mono faint">{e.action.replace("delete_", "")}</span>
                      &nbsp;{e.label}
                    </div>
                    <div class="set-row-d mono faint">
                      {undoAge(e.created_at)} · {undoExpiry(e.expires_at)}
                      {#if e.undone_at}
                        · <span style="color:var(--fg-muted)">已恢复</span>
                      {/if}
                    </div>
                  </div>
                  <div class="set-row-r">
                    <button class="btn" onclick={() => runRestore(e.id)} disabled={undoBusy === e.id || !!e.undone_at}>
                      {undoBusy === e.id ? "Restoring…" : "Restore"}
                    </button>
                  </div>
                </div>
              {/each}
            </div>
          </section>
        {/if}


        <section class="set-group">
          <div class="set-group-h"><h3 class="set-group-t">管理</h3></div>
          <div class="set-card">
            <div class="set-row">
              <div class="set-row-l">
                <div class="set-row-t">导出全部数据</div>
                <div class="set-row-d">科目、资料、速查表 → 一个可携带的归档包。</div>
              </div>
              <div class="set-row-r">
                <button class="btn" onclick={exportData}>
                  <Icon name="external" size={12} /> Export
                </button>
                <button class="btn" onclick={exportObsidian}>Obsidian Markdown</button>
              </div>
            </div>
            <div class="set-row">
              <div class="set-row-l">
                <div class="set-row-t">优化存储</div>
                <div class="set-row-d">回收未使用的磁盘空间（VACUUM），安全。</div>
              </div>
              <div class="set-row-r">
                <button class="btn" onclick={clearCaches}>Optimize</button>
              </div>
            </div>
            <div class="set-row">
              <div class="set-row-l">
                <div class="set-row-t">删除全部数据</div>
                <div class="set-row-d">不可撤销，会清空本地数据库。</div>
              </div>
              <div class="set-row-r">
                <button class="btn btn--danger" onclick={deleteEverything}>Delete…</button>
              </div>
            </div>
          </div>
        </section>
      </div>

    <!-- ===== CRON / AUTOMATION ===== -->
    {:else if tab === "cron"}
      <div class="set-pane">
        <header class="set-head">
          <div class="eyebrow">Automation</div>
          <h1 class="set-title">定时任务</h1>
          <p class="set-sub">应用运行期间按计划自动执行提示词（Khoj Automations 风格）。结果存进运行记录；关联科目的同时归档为该科目的资料源。</p>
        </header>

        <section class="set-group">
          <div class="set-group-h"><h3 class="set-group-t">任务</h3></div>
          <div class="set-card">
            {#if !cronJobs || cronJobs.length === 0}
              <div class="mt-task-d" style="padding:10px 0">
                还没有任务。在下面建一个——比如每天早上让它汇总一次各科待办和临近考试。
              </div>
            {/if}
            {#each cronJobs ?? [] as j (j.id)}
              <div class="xtra-row" style="align-items:flex-start">
                <div class="xtra-label">
                  <div class="mt-task-t">
                    {j.name}
                    <span class="slot-hint mono">{cronScheduleText(j)}</span>
                    {#if j.action === "sync"}<span class="slot-hint">🔗 连接器 {j.action_ref}</span>{/if}
                    {#if j.action === "sync" && j.auto_flashcards}<span class="slot-hint">⚡自动闪卡</span>{/if}
                    {#if !j.enabled}<span class="slot-hint">已暂停</span>{/if}
                  </div>
                  <div class="mt-task-d mono">上次：{cronFmt(j.last_run_at)} · 下次：{cronFmt(j.next_run_at)}</div>
                  <div class="mt-task-d">{j.action === "sync" ? `定时拉取连接器 ${j.action_ref} 的新内容到关联科目` : j.prompt}</div>
                </div>
                <div class="xtra-stack" style="flex-direction:row; gap:6px; align-items:center">
                  <button class="btn btn--sm btn--ghost" onclick={() => cronToggle(j)}>
                    {j.enabled ? "暂停" : "启用"}
                  </button>
                  <button class="btn btn--sm btn--ghost" onclick={() => cronTrigger(j)} disabled={cronBusy}>运行</button>
                  <button class="btn btn--sm btn--ghost" onclick={() => cronToggleRuns(j.id)}>记录</button>
                  <button class="btn btn--sm btn--ghost" onclick={() => cronRemove(j)}>删除</button>
                </div>
              </div>
              {#if cronRunsOpen === j.id}
                <div style="padding:0 0 12px 14px">
                  {#each cronRuns[j.id] ?? [] as r (r.id)}
                    <div class="mt-task-d mono">
                      [{r.status}] {cronFmt(r.started_at)}{r.error ? ` — ${r.error}` : ""}
                    </div>
                    {#if r.output}
                      <pre class="cron-output">{r.output.slice(0, 600)}{r.output.length > 600 ? "…" : ""}</pre>
                    {/if}
                  {:else}
                    <div class="mt-task-d">还没有运行记录</div>
                  {/each}
                </div>
              {/if}
            {/each}
          </div>
        </section>

        <section class="set-group" style="margin-top:14px">
          <div class="set-group-h"><h3 class="set-group-t">MCP Client</h3></div>
          <div class="set-card">
            <div class="xtra-row" style="align-items:flex-start">
              <div class="xtra-label">
                <div class="mt-task-t">外部 MCP servers（stdio）</div>
                <div class="mt-task-d mono">JSON 数组，每项含 name / command / args / env / enabled；name 会拼进工具名 mcp__&lt;name&gt;__&lt;tool&gt;（见输入框示例）</div>
              </div>
              <textarea
                class="input mono"
                rows="5"
                style="min-width:280px"
                value={mcpServersJson}
                onchange={(e) => (mcpServersJson = (e.target as HTMLTextAreaElement).value)}
                placeholder={MCP_PLACEHOLDER}
              ></textarea>
            </div>
          </div>
          <div class="set-foot-actions">
            <button class="btn btn--ghost" onclick={saveMcpServers}>保存配置</button>
            <button class="btn btn--primary" onclick={mcpSync} disabled={mcpBusy}>
              <Icon name="refresh" size={13} /> 连接并同步工具
            </button>
          </div>
          <div class="set-note mono">
            <Icon name="diamond" size={11} color="var(--accent)" />
            同步后工具进入 Agent 工具列表（mcp__ 前缀），在 Agents 视图里给需要的 agent 勾选即可调用。停用的 server 其工具仍会显示但调用时报错。
          </div>
        </section>

        <section class="set-group" style="margin-top:14px">
          <div class="set-group-h"><h3 class="set-group-t">新建任务</h3></div>
          <div class="set-card">
            <div class="xtra-row">
              <div class="xtra-label">
                <div class="mt-task-t">名称</div>
              </div>
              <input class="input" value={cronForm.name} onchange={(e) => (cronForm.name = (e.target as HTMLInputElement).value)} placeholder="比如：每日晨报" />
            </div>
            <div class="xtra-row">
              <div class="xtra-label">
                <div class="mt-task-t">任务类型</div>
                <div class="mt-task-d">提示词 = 跑一次 AI；同步 = 定时拉取研究连接器的新内容</div>
              </div>
              <select class="input" value={cronForm.action} onchange={(e) => (cronForm.action = (e.target as HTMLSelectElement).value)}>
                <option value="prompt">AI 提示词</option>
                <option value="sync">连接器同步</option>
              </select>
            </div>
            {#if cronForm.action === "sync"}
              <div class="xtra-row">
                <div class="xtra-label">
                  <div class="mt-task-t">连接器</div>
                  <div class="mt-task-d mono">已配置的研究连接器（arXiv / PubMed / Zotero…）；没有的话先在「研究与文献」里配置</div>
                </div>
                <select class="input" value={cronForm.connectorId} onchange={(e) => (cronForm.connectorId = (e.target as HTMLSelectElement).value)}>
                  <option value="">选择连接器</option>
                  {#each cronConnectors ?? [] as c (c.id)}
                    <option value={c.id}>{c.label}（{c.id}）</option>
                  {/each}
                </select>
              </div>
              <div class="xtra-row">
                <div class="xtra-label">
                  <div class="mt-task-t">同步后自动生成闪卡</div>
                  <div class="mt-task-d">本次新导入的内容自动提炼 8 张闪卡，直接进入「今日复习」队列（科研 → 学习闭环）</div>
                </div>
                <input type="checkbox" bind:checked={cronForm.autoFlashcards} />
              </div>
            {:else}
              <div class="xtra-row">
                <div class="xtra-label">
                  <div class="mt-task-t">提示词</div>
                  <div class="mt-task-d mono">执行时发给聊天槽位模型，直接输出任务结果</div>
                </div>
                <textarea
                  class="input"
                  rows="3"
                  value={cronForm.prompt}
                  onchange={(e) => (cronForm.prompt = (e.target as HTMLTextAreaElement).value)}
                  placeholder="汇总我所有科目的待办和临近考试，按紧急程度排序输出"
                ></textarea>
              </div>
            {/if}
            <div class="xtra-row">
              <div class="xtra-label">
                <div class="mt-task-t">计划</div>
              </div>
              <div class="xtra-stack" style="flex-direction:row; gap:8px">
                <select class="input" value={cronForm.kind} onchange={(e) => (cronForm.kind = (e.target as HTMLSelectElement).value)}>
                  <option value="daily">每天</option>
                  <option value="interval">间隔</option>
                </select>
                {#if cronForm.kind === "daily"}
                  <input class="input mono" type="time" value={cronForm.dailyTime} onchange={(e) => (cronForm.dailyTime = (e.target as HTMLInputElement).value)} />
                {:else}
                  <input class="input mono" type="number" min="1" style="width:90px" value={cronForm.intervalMinutes} onchange={(e) => (cronForm.intervalMinutes = Number((e.target as HTMLInputElement).value) || 60)} />
                  <span class="mt-task-d">分钟</span>
                {/if}
              </div>
            </div>
            <div class="xtra-row">
              <div class="xtra-label">
                <div class="mt-task-t">关联科目{cronForm.action === "sync" ? "（必选）" : "（可选）"}</div>
                <div class="mt-task-d mono">{cronForm.action === "sync" ? "新内容会拉取到该科目下" : "结果同时归档为该科目下的 txt 资料源"}</div>
              </div>
              <select class="input" value={cronForm.subjectId} onchange={(e) => (cronForm.subjectId = (e.target as HTMLSelectElement).value)}>
                <option value="">不关联</option>
                {#each app.subjects as s (s.id)}
                  <option value={s.id}>{s.name}</option>
                {/each}
              </select>
            </div>
          </div>
          <div class="set-foot-actions">
            <button class="btn btn--primary" onclick={cronCreate} disabled={cronBusy}>
              <Icon name="plus" size={13} /> 创建任务
            </button>
          </div>
        </section>
      </div>

    <!-- ===== ABOUT ===== -->
    {:else if tab === "about"}
      <div class="set-pane">
        <header class="set-head">
          <div class="eyebrow">About</div>
          <h1 class="set-title">Cortex</h1>
          <p class="set-sub">给认真学的人一套桌面学习系统。</p>
        </header>

        <div class="set-card">
          <div class="set-row">
            <div class="set-row-l"><div class="set-row-t">版本</div></div>
            <div class="set-row-r"><span class="mono faint">{appVersion || "…"}</span></div>
          </div>
          <div class="set-row">
            <div class="set-row-l">
              <div class="set-row-t">更新</div>
              <div class="set-row-d">检查 GitHub 上的新版本并安装。</div>
            </div>
            <div class="set-row-r">
              <button class="btn" onclick={() => app.checkForUpdates()} disabled={app.updateChecking}>
                <Icon name="refresh" size={12} /> {app.updateChecking ? "Checking…" : "Check for updates"}
              </button>
            </div>
          </div>
          <div class="set-row">
            <div class="set-row-l">
              <div class="set-row-t">支持 Cortex</div>
              <div class="set-row-d">Cortex 由一个学生独立开发。如果它帮你省了时间，请作者喝杯咖啡就能让更新继续。</div>
            </div>
            <div class="set-row-r">
              <button class="btn" onclick={() => api.openExternal("https://ko-fi.com/aidanmcconnon")}>
                <Icon name="heart" size={12} color="var(--err)" /> Support me on Ko-fi
              </button>
            </div>
          </div>
          <div class="set-row">
            <div class="set-row-l"><div class="set-row-t">引擎</div></div>
            <div class="set-row-r"><span class="mono faint">Rust · Tauri · Svelte</span></div>
          </div>
          <div class="set-row">
            <div class="set-row-l"><div class="set-row-t">主题来源</div></div>
            <div class="set-row-r">
              <span class="mono faint">
                Omarchy · {THEME_LABELS[app.theme]}
              </span>
            </div>
          </div>
          <div class="set-row">
            <div class="set-row-l"><div class="set-row-t">许可证</div></div>
            <div class="set-row-r"><span class="mono faint">Source-available · BYOK</span></div>
          </div>
        </div>

        <div class="set-note mono">
          <Icon name="diamond" size={11} color="var(--accent)" />
          Offline-first. Your notes never leave this machine unless you choose a cloud model.
        </div>
      </div>
    {/if}

  </div>
</div>
