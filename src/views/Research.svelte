<script lang="ts">
  // Research — the reading list and the excerpts taken from it.
  //
  // This is the middle of the research pipeline that used to be missing:
  // literature_search could find a paper and LiteratureReader could display one,
  // but nothing remembered "I plan to read this", "I read it", or "this is the
  // sentence I will cite". A paper here can become a schedulable reading task,
  // and every excerpt is kept verbatim so it can go straight into a draft.
  import * as api from "../lib/api";
  import { app } from "../lib/store.svelte";
  import { jobs } from "../lib/jobs.svelte";
  import Icon from "../components/Icon.svelte";
  import MatrixView from "../components/MatrixView.svelte";
  import OfficeTools from "../components/OfficeTools.svelte";

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

  // ── Deep Research（D2）：课题 → 多源发现 → AI 筛选 → 入库 → 综述初稿 ──
  let drOpen = $state(false);
  let drTopic = $state("");
  let drMax = $state(6);
  let drBusy = $state(false);
  let drResult = $state<api.DeepResearchResult | null>(null);
  async function runDeepResearch() {
    if (!drTopic.trim() || drBusy) return;
    const sid = app.activeSubject?.id;
    if (!sid) { say("先选择一个科目"); return; }
    drBusy = true;
    error = null;
    try {
      drResult = await api.deepResearch(sid, drTopic.trim(), drMax);
      const added = drResult.picks.filter((p) => p.imported).length;
      say(`深度研究完成：新增 ${added} 篇到阅读清单${drResult.overview ? "，综述初稿已存入笔记" : ""}`);
      await load();
    } catch (e) {
      error = String(e);
    } finally {
      drBusy = false;
    }
  }
  let importText = $state("");

  // ----- B4: cross-paper matrix -----
  let matrixSelection = $state<Set<string>>(new Set());
  let matrix = $state<api.PaperMatrix | null>(null);
  let matrixBuilding = $state(false);
  let matrixError = $state<string | null>(null);
  function toggleMatrixPick(id: string) {
    const next = new Set(matrixSelection);
    if (next.has(id)) next.delete(id); else next.add(id);
    matrixSelection = next;
  }
  const matrixSelectionCount = $derived(matrixSelection.size);
  const matrixSelectionIds = $derived(Array.from(matrixSelection));
  function clearMatrixSelection() { matrixSelection = new Set(); }
  async function buildComparison() {
    if (matrixSelectionCount === 0) return;
    matrixBuilding = true;
    matrixError = null;
    try { matrix = await api.paperBuildMatrix(matrixSelectionIds); }
    catch (e) { matrixError = String(e); }
    finally { matrixBuilding = false; }
  }
  function closeMatrix() { matrix = null; matrixError = null; }

  // ----- B5: excerpts → citable draft -----
  // The pick set is GLOBAL and insertion-ordered: the order you check excerpts
  // across papers IS the citation order, so [1] is always the first one picked.
  let pickedExcerpts = $state<Set<string>>(new Set());
  let drafts = $state<api.ResearchDraft[]>([]);
  let draftModal = $state<"builder" | "library" | null>(null);
  let draftResult = $state<api.ResearchDraft | null>(null);
  let viewDraft = $state<api.ResearchDraft | null>(null);
  let builderExcerpts = $state<api.Excerpt[]>([]);
  let draftBusy = $state(false);
  let draftError = $state<string | null>(null);
  let draftTitle = $state("");
  let draftPrompt = $state("");
  const pickedCount = $derived(pickedExcerpts.size);
  const pickedIds = $derived(Array.from(pickedExcerpts));
  const showingDraft = $derived(draftResult ?? viewDraft);

  function toggleExcerptPick(id: string) {
    const next = new Set(pickedExcerpts);
    if (next.has(id)) next.delete(id); else next.add(id);
    pickedExcerpts = next;
  }
  function clearExcerptSelection() { pickedExcerpts = new Set(); }

  async function openDraftBuilder() {
    if (pickedCount === 0) return;
    draftError = null;
    try { builderExcerpts = await api.excerptList(); } catch { builderExcerpts = []; }
    draftModal = "builder";
  }

  async function openDraftLibrary() {
    draftError = null;
    try { drafts = await api.draftList(); } catch { drafts = []; }
    draftModal = "library";
  }

  async function runGenerateDraft() {
    if (pickedCount === 0 || draftBusy) return;
    draftBusy = true;
    draftError = null;
    try {
      draftResult = await api.generateDraft(
        pickedIds,
        draftTitle.trim() || undefined,
        draftPrompt.trim() || undefined,
      );
      pickedExcerpts = new Set();
      draftTitle = "";
      draftPrompt = "";
      try { drafts = await api.draftList(); } catch { /* library refresh is best-effort */ }
    } catch (e) {
      draftError = String(e);
    } finally {
      draftBusy = false;
    }
  }

  async function deleteDraft(id: string) {
    try {
      await api.draftDelete(id);
      if (viewDraft?.id === id) viewDraft = null;
      drafts = await api.draftList();
    } catch (e) {
      draftError = String(e);
    }
  }

  function closeDraftModal() {
    draftModal = null;
    draftResult = null;
    viewDraft = null;
    draftError = null;
  }

  /** Split paragraph text so [n] markers render as highlighted citation chips. */
  function citeTokens(text: string): Array<{ t: string; c?: number }> {
    const out: Array<{ t: string; c?: number }> = [];
    const re = /\[(\d{1,3})\]/g;
    let last = 0;
    for (const m of text.matchAll(re)) {
      const idx = m.index ?? 0;
      if (idx > last) out.push({ t: text.slice(last, idx) });
      out.push({ t: m[0], c: Number(m[1]) });
      last = idx + m[0].length;
    }
    if (last < text.length) out.push({ t: text.slice(last) });
    return out;
  }

  function draftFullText(d: api.ResearchDraft): string {
    const p = d.payload;
    if (!p) return d.title;
    const refs = (p.references ?? [])
      .map((r) => `[${r.n}] ${r.authors ?? "佚名"}${r.year ? ` (${r.year})` : ""}. ${r.title}.`)
      .join("\n");
    const secs = (p.sections ?? []).map(
      (s) => `## ${s.heading}\n\n${(s.paragraphs ?? []).map((x) => x.text).join("\n\n")}`,
    );
    return [p.thesis, ...secs, ...(p.paragraphs ?? []).map((x) => x.text), "", "参考文献", refs]
      .filter(Boolean)
      .join("\n\n");
  }

  async function copyDraft(d: api.ResearchDraft) {
    try {
      await navigator.clipboard.writeText(draftFullText(d));
      say("成稿已复制到剪贴板");
    } catch (e) {
      draftError = String(e);
    }
  }

  // Draft → real .docx with citation numbers + the reference table.
  async function exportDraft(d: api.ResearchDraft) {
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const safe = d.title.replace(/[^\w.-]+/g, "_").replace(/^_+|_+$/g, "") || "draft";
      const dest = await save({
        defaultPath: `${safe}.docx`,
        filters: [{ name: "Word 文档", extensions: ["docx"] }],
      });
      if (!dest) return;
      await api.exportDraftDocx(d.id, dest);
      say(`成稿已导出 → ${dest}`);
    } catch (e) {
      draftError = String(e);
    }
  }

  async function exportDraftTex(d: api.ResearchDraft) {
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const safe = d.title.replace(/[^\w.-]+/g, "_").replace(/^_+|_+$/g, "") || "draft";
      const dest = await save({
        defaultPath: `${safe}.tex`,
        filters: [{ name: "LaTeX (ctexart, xelatex 编译)", extensions: ["tex"] }],
      });
      if (!dest) return;
      await api.exportDraftTex(d.id, dest);
      say(`LaTeX 已导出 → ${dest}`);
    } catch (e) {
      draftError = String(e);
    }
  }

  // ----- D8: extraction templates (Elicit-style field-wise table) -----
  // A template fixes the field list once (样本量 / 方法 / 效应量 …); a run
  // applies it to every checked paper, one LLM call per paper, and every cell
  // is individually editable — a manual edit is flagged `manual` and survives
  // re-runs and retries. Reuses the same paper checkbox selection as the
  // comparison matrix: the checked set IS the extraction workset.
  let extractOpen = $state(false);
  let extractView = $state<"pick" | "runs" | "result">("pick");
  let extractTemplates = $state<api.ExtractTemplate[]>([]);
  let extractRuns = $state<api.ExtractRun[]>([]);
  let extractChosenId = $state<string | null>(null);
  let extractRunTitle = $state("");
  let extractResult = $state<api.ExtractResult | null>(null);
  let extractBusy = $state(false);
  let extractError = $state<string | null>(null);
  let retryingItem = $state<string | null>(null);

  // Template editor state (one form serves create + edit).
  let tplFormOpen = $state(false);
  let tplEditId = $state<string | null>(null);
  let tplName = $state("");
  let tplDesc = $state("");
  let tplFields = $state<api.ExtractField[]>([{ key: "", prompt: "" }]);

  const chosenTemplate = $derived(
    extractTemplates.find((t) => t.id === extractChosenId) ?? null
  );
  const cellMap = $derived.by(() => {
    const m = new Map<string, api.ExtractCell>();
    if (extractResult) {
      for (const c of extractResult.cells) m.set(`${c.item_id}\u0000${c.field_key}`, c);
    }
    return m;
  });
  const extractRunning = $derived(extractResult?.run.status === "running");

  function cellVal(itemId: string, fieldKey: string): string {
    return cellMap.get(`${itemId}\u0000${fieldKey}`)?.value ?? "";
  }
  function cellIsManual(itemId: string, fieldKey: string): boolean {
    return cellMap.get(`${itemId}\u0000${fieldKey}`)?.source === "manual";
  }

  async function loadExtractData() {
    try {
      extractTemplates = await api.extractTemplateList();
      extractRuns = await api.extractRunList();
    } catch (e) {
      extractError = String(e);
    }
  }

  function openExtract() {
    extractOpen = true;
    extractError = null;
    void loadExtractData();
  }

  function closeExtract() {
    extractOpen = false;
    extractResult = null;
    extractError = null;
    tplFormOpen = false;
    extractView = "pick";
  }

  function openTemplateForm(t?: api.ExtractTemplate) {
    tplEditId = t?.id ?? null;
    tplName = t?.name ?? "";
    tplDesc = t?.description ?? "";
    tplFields = t ? t.fields.map((f) => ({ ...f })) : [{ key: "", prompt: "" }];
    tplFormOpen = true;
  }

  async function saveTemplate() {
    extractBusy = true;
    extractError = null;
    try {
      await api.extractTemplateSave(
        tplEditId,
        tplName.trim(),
        tplDesc.trim() || null,
        tplFields
          .map((f) => ({ key: f.key.trim(), prompt: f.prompt.trim() }))
          .filter((f) => f.key && f.prompt)
      );
      tplFormOpen = false;
      await loadExtractData();
      say("模板已保存");
    } catch (e) {
      extractError = String(e);
    } finally {
      extractBusy = false;
    }
  }

  async function deleteTemplate(id: string) {
    try {
      await api.extractTemplateDelete(id);
      if (extractChosenId === id) extractChosenId = null;
      await loadExtractData();
    } catch (e) {
      extractError = String(e);
    }
  }

  async function startExtraction() {
    if (!extractChosenId || matrixSelectionCount === 0 || extractBusy) return;
    extractBusy = true;
    extractError = null;
    try {
      const runId = await api.extractRunCreate(
        extractChosenId,
        matrixSelectionIds,
        extractRunTitle.trim() || undefined
      );
      extractRunTitle = "";
      // Open the (initially empty) table right away — cells fill in live.
      extractResult = await api.extractResultGet(runId);
      extractView = "result";
      jobs.start({
        kind: "extract",
        label: `${chosenTemplate?.name ?? "提取"} · ${matrixSelectionCount} 篇`,
        subjectId: app.activeSubject?.id ?? "",
        run: () => api.extractRunExecute(runId),
        onDone: () => void refreshRun(runId),
      });
    } catch (e) {
      extractError = String(e);
    } finally {
      extractBusy = false;
    }
  }

  async function refreshRun(runId: string) {
    try {
      const fresh = await api.extractResultGet(runId);
      if (extractResult?.run.id === runId) extractResult = fresh;
      extractRuns = await api.extractRunList();
    } catch {
      /* transient poll errors are non-fatal */
    }
  }

  // While the open run is executing, poll so extracted cells appear live.
  $effect(() => {
    if (!extractOpen || !extractRunning || !extractResult) return;
    const runId = extractResult.run.id;
    const t = setInterval(() => void refreshRun(runId), 3000);
    return () => clearInterval(t);
  });

  async function openRun(id: string) {
    extractError = null;
    try {
      extractResult = await api.extractResultGet(id);
      extractView = "result";
    } catch (e) {
      extractError = String(e);
    }
  }

  async function retryRow(itemId: string) {
    if (!extractResult || retryingItem) return;
    retryingItem = itemId;
    extractError = null;
    try {
      await api.extractRunRetry(extractResult.run.id, itemId);
      await refreshRun(extractResult.run.id);
    } catch (e) {
      extractError = String(e);
    } finally {
      retryingItem = null;
    }
  }

  async function saveCell(itemId: string, fieldKey: string, value: string) {
    if (!extractResult) return;
    if (value === cellVal(itemId, fieldKey)) return;
    try {
      await api.extractCellSet(extractResult.run.id, itemId, fieldKey, value);
      await refreshRun(extractResult.run.id);
    } catch (e) {
      extractError = String(e);
    }
  }

  async function deleteRun(id: string) {
    try {
      await api.extractRunDelete(id);
      if (extractResult?.run.id === id) {
        extractResult = null;
        extractView = "runs";
      }
      await loadExtractData();
    } catch (e) {
      extractError = String(e);
    }
  }

  async function exportCsv() {
    if (!extractResult) return;
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const safe =
        extractResult.run.title.replace(/[^\w.-]+/g, "_").replace(/^_+|_+$/g, "") ||
        "extraction";
      const dest = await save({
        defaultPath: `${safe}.csv`,
        filters: [{ name: "CSV 表格", extensions: ["csv"] }],
      });
      if (!dest) return;
      await api.extractResultCsvFile(extractResult.run.id, dest);
      say(`表格已导出 → ${dest}`);
    } catch (e) {
      extractError = String(e);
    }
  }

  // ----- 3.2: PRISMA 8-dimension extraction (systematic-review table) -----
  // Fixed 8-field template (研究问题/方法/贡献/结果/局限/适用人群/使用前提/复现成本),
  // one LLM call per checked paper, no template to configure. Every value
  // carries an evidence sentence back-linked to the excerpt pipeline; the
  // matrix stays the free-form comparison, this is the review-grade table.
  // Shares the same paper checkboxes as matrix/extract: the checked set IS the
  // review workset.
  const PRISMA_LABELS: Record<string, string> = {
    research_question: "研究问题",
    method: "方法",
    contribution: "贡献",
    results: "结果",
    limitations: "局限",
    audience: "适用人群",
    constraints: "使用前提",
    repro_cost: "复现成本",
  };
  function prismaLabel(key: string): string {
    return PRISMA_LABELS[key] ?? key;
  }

  let prismaOpen = $state(false);
  let prismaView = $state<"pick" | "runs" | "result">("pick");
  let prismaRuns = $state<api.PrismaRun[]>([]);
  let prismaRunTitle = $state("");
  let prismaResult = $state<api.PrismaResult | null>(null);
  let prismaBusy = $state(false);
  let prismaError = $state<string | null>(null);
  let prismaRetrying = $state<string | null>(null);

  const prismaCellMap = $derived.by(() => {
    const m = new Map<string, api.PrismaCell>();
    if (prismaResult) {
      for (const c of prismaResult.cells) m.set(`${c.item_id}\u0000${c.field_key}`, c);
    }
    return m;
  });
  const prismaRunning = $derived(prismaResult?.run.status === "running");

  function prismaEvidence(itemId: string, fieldKey: string): api.PrismaCell | null {
    return prismaCellMap.get(`${itemId}\u0000${fieldKey}`) ?? null;
  }

  // ----- 3.3: cross-paper tension analysis (0050) -----
  let tensionOpen = $state(false);
  let tensionView = $state<"pick" | "runs" | "result">("pick");
  let tensionRuns = $state<api.TensionRun[]>([]);
  let tensionResult = $state<api.TensionResult | null>(null);
  let tensionTitle = $state("");
  let tensionBusy = $state(false);
  let tensionError = $state<string | null>(null);
  const tensionRunning = $derived(tensionResult?.run.status === "running");
  const tensionPaperTitle = (id: string) =>
    tensionResult?.papers.find(([pid]) => pid === id)?.[1] ?? id;

  async function loadTensionRuns() {
    try { tensionRuns = await api.tensionRunList(); }
    catch (e) { tensionError = String(e); }
  }
  function openTensions() {
    tensionOpen = true;
    tensionError = null;
    void loadTensionRuns();
  }
  function closeTensions() {
    tensionOpen = false;
    tensionResult = null;
    tensionError = null;
    tensionView = "pick";
  }
  async function startTension() {
    if (matrixSelectionCount < 2 || tensionBusy) return;
    tensionBusy = true;
    tensionError = null;
    try {
      const runId = await api.tensionRunCreate(matrixSelectionIds, tensionTitle.trim() || undefined);
      tensionTitle = "";
      tensionResult = await api.tensionRunResult(runId);
      tensionView = "result";
      jobs.start({
        kind: "tension",
        label: `文献张力 · ${matrixSelectionCount} 篇`,
        subjectId: app.activeSubject?.id ?? "",
        run: () => api.tensionRunExecute(runId),
        onDone: () => void refreshTension(runId),
      });
    } catch (e) { tensionError = String(e); }
    finally { tensionBusy = false; }
  }
  async function refreshTension(runId: string) {
    try {
      const fresh = await api.tensionRunResult(runId);
      if (tensionResult?.run.id === runId) tensionResult = fresh;
      tensionRuns = await api.tensionRunList();
    } catch { /* transient */ }
  }
  $effect(() => {
    if (!tensionOpen || !tensionRunning || !tensionResult) return;
    const iv = setInterval(() => void refreshTension(tensionResult!.run.id), 2500);
    return () => clearInterval(iv);
  });
  async function openTensionRun(id: string) {
    try {
      tensionResult = await api.tensionRunResult(id);
      tensionView = "result";
    } catch (e) { tensionError = String(e); }
  }
  async function deleteTensionRun(id: string) {
    if (!await app.confirm({ title: "删除这次张力分析？", danger: true })) return;
    await api.tensionRunDelete(id).catch(() => {});
    await loadTensionRuns();
    if (tensionResult?.run.id === id) { tensionResult = null; tensionView = "pick"; }
  }
  async function exportTensionMd() {
    if (!tensionResult) return;
    const { save } = await import("@tauri-apps/plugin-dialog");
    const dest = await save({
      defaultPath: `文献张力-${tensionResult.run.title}.md`,
      filters: [{ name: "Markdown", extensions: ["md"] }],
    });
    if (!dest) return;
    try {
      await api.tensionMdFile(tensionResult.run.id, dest);
      app.pushToast({ kind: "success", title: "已导出 Markdown" });
    } catch (e) { app.pushToast({ kind: "error", title: "导出失败", body: String(e) }); }
  }

  async function loadPrismaRuns() {
    try {
      prismaRuns = await api.prismaRunList();
    } catch (e) {
      prismaError = String(e);
    }
  }

  function openPrisma() {
    prismaOpen = true;
    prismaError = null;
    void loadPrismaRuns();
  }

  function closePrisma() {
    prismaOpen = false;
    prismaResult = null;
    prismaError = null;
    prismaView = "pick";
  }

  async function startPrisma() {
    if (matrixSelectionCount === 0 || prismaBusy) return;
    prismaBusy = true;
    prismaError = null;
    try {
      const runId = await api.prismaRunCreate(
        matrixSelectionIds,
        prismaRunTitle.trim() || undefined
      );
      prismaRunTitle = "";
      // Open the (initially empty) table right away — cells fill in live.
      prismaResult = await api.prismaResultGet(runId);
      prismaView = "result";
      jobs.start({
        kind: "prisma",
        label: `PRISMA 8 维 · ${matrixSelectionCount} 篇`,
        subjectId: app.activeSubject?.id ?? "",
        run: () => api.prismaRunExecute(runId),
        onDone: () => void refreshPrisma(runId),
      });
    } catch (e) {
      prismaError = String(e);
    } finally {
      prismaBusy = false;
    }
  }

  async function refreshPrisma(runId: string) {
    try {
      const fresh = await api.prismaResultGet(runId);
      if (prismaResult?.run.id === runId) prismaResult = fresh;
      prismaRuns = await api.prismaRunList();
    } catch {
      /* transient poll errors are non-fatal */
    }
  }

  // While the open run is executing, poll so extracted cells appear live.
  $effect(() => {
    if (!prismaOpen || !prismaRunning || !prismaResult) return;
    const runId = prismaResult.run.id;
    const t = setInterval(() => void refreshPrisma(runId), 3000);
    return () => clearInterval(t);
  });

  async function openPrismaRun(id: string) {
    prismaError = null;
    try {
      prismaResult = await api.prismaResultGet(id);
      prismaView = "result";
    } catch (e) {
      prismaError = String(e);
    }
  }

  async function prismaSaveCell(itemId: string, fieldKey: string, value: string) {
    if (!prismaResult) return;
    const cur = prismaEvidence(itemId, fieldKey)?.value ?? "";
    if (value === cur) return;
    try {
      await api.prismaCellSet(prismaResult.run.id, itemId, fieldKey, value);
      await refreshPrisma(prismaResult.run.id);
    } catch (e) {
      prismaError = String(e);
    }
  }

  async function prismaRetryRow(itemId: string) {
    if (!prismaResult || prismaRetrying) return;
    prismaRetrying = itemId;
    prismaError = null;
    try {
      await api.prismaRunRetry(prismaResult.run.id, itemId);
      await refreshPrisma(prismaResult.run.id);
    } catch (e) {
      prismaError = String(e);
    } finally {
      prismaRetrying = null;
    }
  }

  async function prismaDeleteRun(id: string) {
    try {
      await api.prismaRunDelete(id);
      if (prismaResult?.run.id === id) {
        prismaResult = null;
        prismaView = "runs";
      }
      await loadPrismaRuns();
    } catch (e) {
      prismaError = String(e);
    }
  }

  async function exportPrismaCsv() {
    if (!prismaResult) return;
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const safe =
        prismaResult.run.title.replace(/[^\w.-]+/g, "_").replace(/^_+|_+$/g, "") ||
        "prisma";
      const dest = await save({
        defaultPath: `${safe}.csv`,
        filters: [{ name: "CSV 表格", extensions: ["csv"] }],
      });
      if (!dest) return;
      await api.prismaResultCsvFile(prismaResult.run.id, dest);
      say(`汇总表已导出 → ${dest}`);
    } catch (e) {
      prismaError = String(e);
    }
  }

  async function exportPrismaMd() {
    if (!prismaResult) return;
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const safe =
        prismaResult.run.title.replace(/[^\w.-]+/g, "_").replace(/^_+|_+$/g, "") ||
        "prisma";
      const dest = await save({
        defaultPath: `${safe}.md`,
        filters: [{ name: "Markdown", extensions: ["md"] }],
      });
      if (!dest) return;
      await api.prismaResultMdFile(prismaResult.run.id, dest);
      say(`汇总表已导出 → ${dest}`);
    } catch (e) {
      prismaError = String(e);
    }
  }

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
    void loadExperiments();
  });

  // ── Experiments（复现实验卡片，科研一体化的最后一块）──
  // 轻量形态：假设/代码/环境/结果四要素挂在论文上，可 livesync；执行交给
  // agent 平台，这里只记录与对账。
  let experiments = $state<api.Experiment[]>([]);
  let expOpen = $state<Record<string, boolean>>({});
  let expNewOpen = $state(false);
  let expTitle = $state("");
  let expHypo = $state("");
  let expEnv = $state("");
  let expDraft = $state<Record<string, { code: string; result: string }>>({});
  async function loadExperiments() {
    if (!selected) { experiments = []; return; }
    experiments = await api.experimentList(selected.id, null).catch(() => []);
  }
  async function addExperiment() {
    if (!selected || !expTitle.trim() || busy) return;
    busy = "exp";
    try {
      await api.experimentCreate(selected.id, selected.subject_id ?? null, expTitle.trim(), expHypo.trim() || null, expEnv.trim() || null);
      expTitle = ""; expHypo = ""; expEnv = ""; expNewOpen = false;
      await loadExperiments();
      say("实验卡片已创建");
    } catch (e) { error = String(e); } finally { busy = null; }
  }
  async function setExpStatus(e: api.Experiment, status: api.ExperimentStatus) {
    try { await api.experimentUpdate(e.id, { status }); await loadExperiments(); }
    catch (err) { error = String(err); }
  }
  function expDraftOf(e: api.Experiment) {
    return expDraft[e.id] ?? { code: e.code ?? "", result: e.result ?? "" };
  }
  function setExpDraft(id: string, patch: { code?: string; result?: string }) {
    expDraft[id] = { ...expDraftOf(experiments.find((x) => x.id === id)!), ...patch };
  }
  async function saveExpFields(e: api.Experiment) {
    const d = expDraft[e.id];
    if (!d) return;
    busy = e.id;
    try {
      await api.experimentUpdate(e.id, { code: d.code, result: d.result });
      await loadExperiments();
      say("实验记录已保存");
    } catch (err) { error = String(err); } finally { busy = null; }
  }
  async function dropExperiment(id: string) {
    busy = id;
    try {
      await api.experimentDelete(id);
      await loadExperiments();
    } catch (e) { error = String(e); } finally { busy = null; }
  }

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
      if (pickedExcerpts.has(id)) {
        const next = new Set(pickedExcerpts);
        next.delete(id);
        pickedExcerpts = next;
      }
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

  // ── Zotero import (管理引用: read the LOCAL Zotero library, DOI-deduped) ──
  let zoteroBusy = $state(false);
  async function importZotero() {
    if (zoteroBusy) return;
    zoteroBusy = true;
    error = null;
    try {
      const r = await api.zoteroImport(app.activeSubject?.id ?? null, null);
      say(`Zotero 导入完成：新增 ${r.imported} 篇${r.skipped ? `，跳过已在库 ${r.skipped} 篇` : ""}（共发现 ${r.found}）`);
      await load();
    } catch (e) {
      error = String(e);
    } finally {
      zoteroBusy = false;
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
      <button class="btn" disabled={zoteroBusy} onclick={importZotero}
              title="读取本地 Zotero 数据库（~/Zotero/zotero.sqlite），按 DOI 去重导入阅读清单">
        <Icon name="upload" size={12} /> {zoteroBusy ? "Zotero 导入中…" : "从 Zotero 导入"}
      </button>
      <button class="btn" onclick={() => (importOpen = !importOpen)}>导入 BibTeX</button>
      <button class="btn primary" onclick={() => (drOpen = !drOpen)} title="课题 → 多源检索 → AI 筛选 → 入库 → 综述初稿">
        {drBusy ? "深度研究中…" : "深度研究"}
      </button>
      <button class="btn" disabled={busy === "bib"} onclick={copyBibtex}>导出 BibTeX</button>
      <button
        class="btn primary"
        disabled={matrixSelectionCount === 0 || matrixBuilding}
        onclick={buildComparison}
        title={matrixSelectionCount === 0 ? "勾选至少一篇论文" : "按已勾选的 " + matrixSelectionCount + " 篇生成对比矩阵"}
      >
        {matrixBuilding ? "生成中…" : "对比矩阵 · " + matrixSelectionCount}
      </button>
      <button
        class="btn"
        onclick={openExtract}
        title="结构化提取：按固定字段（样本量 / 方法 / 效应量 …）对勾选的论文逐篇提取成可校对表格"
      >
        提取表格
      </button>
      <button
        class="btn"
        onclick={openPrisma}
        title="PRISMA 8 维抽取：固定维度（研究问题 / 方法 / 贡献 / 结果 / 局限 / 适用人群 / 使用前提 / 复现成本）逐篇抽取，证据句回链摘录，导出 CSV / Markdown"
      >
        PRISMA 抽取
      </button>
      <button
        class="btn"
        onclick={openTensions}
        title="文献张力分析：模型通读勾选的论文，找出文献间互相矛盾/互补/证据冲突的地方，每条张力带双方原话（至少勾选 2 篇）"
      >
        张力分析
      </button>
      {#if matrixSelectionCount > 0}
        <button class="btn btn--ghost" onclick={clearMatrixSelection}>清除选择</button>
      {/if}
      <button
        class="btn primary"
        disabled={pickedCount === 0}
        onclick={openDraftBuilder}
        title={pickedCount === 0 ? "勾选摘录后生成成稿" : "按勾选顺序把 " + pickedCount + " 条摘录写成带引用编号的成稿"}
      >
        成稿 · {pickedCount}
      </button>
      <button class="btn" onclick={openDraftLibrary}>成稿库</button>
      <OfficeTools />
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

  {#if drOpen}
    <div class="import">
      <input class="search" style="width:100%" bind:value={drTopic}
             placeholder="研究课题，例如：检索增强生成在中文教育场景中的应用效果"
             onkeydown={(e) => e.key === "Enter" && void runDeepResearch()} />
      <div class="irow">
        <span class="faint">五源并发检索 → AI 筛选（每篇带理由）→ DOI 去重入库 → 综述初稿存为笔记 · 约 1–3 分钟</span>
        <label class="faint">篇数
          <input class="search" type="number" min="3" max="12" bind:value={drMax} style="width:64px" />
        </label>
        <button class="btn primary" disabled={drBusy || !drTopic.trim()} onclick={() => void runDeepResearch()}>
          {drBusy ? "研究中…" : "开始"}
        </button>
      </div>
      {#if drResult}
        <div style="display:flex;flex-direction:column;gap:8px">
          <div class="faint mono">
            候选 {drResult.candidates} 篇 · 命中源 {drResult.sources_succeeded.join(" / ") || "—"} · {(drResult.took_ms / 1000).toFixed(1)}s
            {#if drResult.note_id} · 综述已存笔记{/if}
          </div>
          {#each drResult.picks as p (p.paper_id + p.title)}
            <div class="prow" style="cursor:default">
              <div style="flex:1;min-width:0">
                <div style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap">
                  {p.title}{#if !p.imported}<span class="faint">（已在库）</span>{/if}
                  {#if p.year}<span class="faint"> · {p.year}</span>{/if}
                </div>
                <div class="faint">{p.why} <span class="mono">— {p.found_by}</span></div>
              </div>
            </div>
          {/each}
          {#if drResult.overview}
            <div style="background:var(--surface-2);border:1px solid var(--border);border-radius:8px;padding:12px 14px;white-space:pre-wrap;font-size:12.5px;line-height:1.75">{drResult.overview}</div>
          {/if}
          {#each drResult.warnings as w}
            <div class="faint" style="color:var(--warn)">{w}</div>
          {/each}
        </div>
      {/if}
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
          {@const isPicked = matrixSelection.has(p.id)}
          <div class="prow" class:picked={isPicked} class:on={selected?.id === p.id}>
            <label class="prow-check">
              <input
                type="checkbox"
                checked={isPicked}
                onchange={() => toggleMatrixPick(p.id)}
                title="勾选以便加入对比矩阵"
              />
            </label>
            <button type="button" class="prow-body" onclick={() => pick(p)}>
              <div class="ptitle">{p.title}</div>
              <div class="pmeta">
                <span class="badge">{STATUS_LABEL[p.status] ?? p.status}</span>
                {#if p.year}<span>{p.year}</span>{/if}
                {#if p.excerpt_count}<span>{p.excerpt_count} 摘录</span>{/if}
                {#if p.open_task_count}<span class="task">{p.open_task_count} 任务</span>{/if}
              </div>
            </button>
          </div>
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
              {@const isPicked = pickedExcerpts.has(ex.id)}
              {@const pickN = pickedIds.indexOf(ex.id) + 1}
              <blockquote class="ex" class:picked={isPicked}>
                <label class="excheck" title="勾选后按顺序作为成稿引用">
                  <input type="checkbox" checked={isPicked} onchange={() => toggleExcerptPick(ex.id)} />
                  {#if isPicked}<span class="cnum">[{pickN}]</span>{/if}
                </label>
                <div class="exmain">
                  <p class="q">{ex.quote}</p>
                  {#if ex.note}<p class="note">{ex.note}</p>{/if}
                  <div class="efoot">
                    {#if ex.page}<span class="pg">p.{ex.page}</span>{/if}
                    <span class="faint">{new Date(ex.created_at).toLocaleDateString("zh-CN")}</span>
                    <button class="x" title="删除摘录" onclick={() => dropExcerpt(ex.id)}>
                      <Icon name="x" size={10} />
                    </button>
                  </div>
                </div>
              </blockquote>
            {/each}
          {:else}
            <p class="none">还没有摘录。读到关键句就存一条——写综述时它比摘要有用。</p>
          {/if}
        </section>

        <section class="excerpts">
          <div class="shead">实验 <span class="n">{experiments.length}</span>
            <button class="x" style="margin-left:auto;font-size:11px" onclick={() => (expNewOpen = !expNewOpen)}>
              {expNewOpen ? "收起" : "+ 新建"}
            </button>
          </div>

          {#if expNewOpen}
            <div class="addbox">
              <input class="pg" style="flex:2" bind:value={expTitle} placeholder="实验标题，例如：复现 Table 2" />
              <input class="nt" style="flex:2" bind:value={expHypo} placeholder="假设 / 预期结果（可选）" />
              <input class="nt" style="flex:2" bind:value={expEnv} placeholder="环境：torch 2.4 · cuda 12.1 · seed 42（可选）" />
              <div class="addrow">
                <span class="faint mono" style="font-size:10px">执行交给 agent；这里记录假设、代码与观测。</span>
                <button class="btn primary" disabled={!expTitle.trim() || busy === "exp"} onclick={addExperiment}>创建</button>
              </div>
            </div>
          {/if}

          {#if experiments.length}
            {#each experiments as e (e.id)}
              {@const open = expOpen[e.id]}
              {@const d = expDraftOf(e)}
              <div class="ex" style="display:flex;flex-direction:column;gap:6px">
                <div style="display:flex;align-items:center;gap:8px">
                  <select class="pg" style="width:auto;font-size:11px;padding:2px 6px" value={e.status}
                          onchange={(ev) => setExpStatus(e, (ev.target as HTMLSelectElement).value as api.ExperimentStatus)}>
                    <option value="planned">计划</option>
                    <option value="running">运行中</option>
                    <option value="done">完成</option>
                    <option value="failed">失败</option>
                  </select>
                  <strong style="flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">{e.title}</strong>
                  <button class="x" title="编辑代码/结果" onclick={() => (expOpen = { ...expOpen, [e.id]: !open })}>
                    <Icon name={open ? "x" : "search"} size={10} />
                  </button>
                  <button class="x" title="删除实验" onclick={() => dropExperiment(e.id)}>
                    <Icon name="x" size={10} />
                  </button>
                </div>
                {#if e.hypothesis}<p class="note">{e.hypothesis}</p>{/if}
                {#if open}
                  <textarea class="mono" style="width:100%;min-height:88px;font-size:11px;background:var(--surface-2);border:1px solid var(--border);border-radius:6px;padding:8px;color:var(--fg)"
                            placeholder="代码 / 复现步骤…" value={d.code}
                            oninput={(ev) => setExpDraft(e.id, { code: (ev.target as HTMLTextAreaElement).value })}></textarea>
                  <textarea style="width:100%;min-height:60px;font-size:12px;background:var(--surface-2);border:1px solid var(--border);border-radius:6px;padding:8px;color:var(--fg)"
                            placeholder="观测结果…" value={d.result}
                            oninput={(ev) => setExpDraft(e.id, { result: (ev.target as HTMLTextAreaElement).value })}></textarea>
                  <div style="display:flex;justify-content:flex-end">
                    <button class="btn primary" disabled={busy === e.id} onclick={() => saveExpFields(e)}>保存记录</button>
                  </div>
                {:else if e.result}
                  <p class="q" style="font-size:12px">{e.result}</p>
                {/if}
                <span class="faint mono" style="font-size:10px">{e.environment ?? ""}</span>
              </div>
            {/each}
          {:else}
            <p class="none">还没有实验卡片。复现 Table、跑消融、验证假设——都记在这里。</p>
          {/if}
        </section>
      {/if}
    </div>
  </div>
</div>


{#if matrix || matrixBuilding || matrixError}
  <div class="matrix-modal-bg" onclick={closeMatrix} role="presentation"></div>
  <div class="matrix-modal" role="dialog" aria-label="跨论文对比矩阵">
    {#if matrixError}
      <div class="matrix-modal-bd err">
        生成失败：{matrixError}
        <div class="matrix-modal-actions">
          <button class="btn" onclick={closeMatrix}>关闭</button>
        </div>
      </div>
    {:else if matrix}
      <MatrixView
        data={{ columns: matrix.columns, rows: matrix.rows }}
        title={`对比 ${matrix.columns.length} 篇 · ${matrix.rows.length} 行`}
        onExit={closeMatrix}
      />
    {:else}
      <div class="matrix-modal-bd">
        <div class="matrix-modal-loading">
          <span class="is-spin" style="width:18px;height:18px"></span>
          <span>正在生成对比矩阵…</span>
        </div>
      </div>
    {/if}
  </div>
{/if}

{#if draftModal}
  <div class="matrix-modal-bg" onclick={closeDraftModal} role="presentation"></div>
  <div class="matrix-modal draft-modal" role="dialog" aria-label="从摘录生成成稿">
    {#if draftError}
      <div class="matrix-modal-bd err">
        生成失败：{draftError}
        <div class="matrix-modal-actions">
          {#if draftModal === "builder" && pickedCount > 0}
            <button class="btn" onclick={() => (draftError = null)}>返回重试</button>
          {/if}
          <button class="btn" onclick={closeDraftModal}>关闭</button>
        </div>
      </div>
    {:else if draftBusy}
      <div class="matrix-modal-bd">
        <div class="matrix-modal-loading">
          <span class="is-spin" style="width:18px;height:18px"></span>
          <span>正在按 {pickedIds.length} 条摘录写稿…</span>
        </div>
      </div>
    {:else if showingDraft}
      <div class="matrix-modal-bd">
        <div class="dtitle">{showingDraft.title}</div>
        <div class="dsub">
          {showingDraft.meta}
          {#if showingDraft !== draftResult}· 来自成稿库{/if}
          · {new Date(showingDraft.created_at).toLocaleString("zh-CN")}
        </div>
        {#if showingDraft.payload?.thesis}
          <p class="thesis">{showingDraft.payload.thesis}</p>
        {/if}
        {#each showingDraft.payload?.sections ?? [] as sec (sec.heading)}
          <h4 class="dsec">{sec.heading}</h4>
          {#each sec.paragraphs ?? [] as para}
            <p class="dpara">
              {#each citeTokens(para.text) as tk, i (i)}{#if tk.c}<span class="cite">{tk.t}</span>{:else}{tk.t}{/if}{/each}
            </p>
          {/each}
        {/each}
        {#each showingDraft.payload?.paragraphs ?? [] as para}
          <p class="dpara">
            {#each citeTokens(para.text) as tk, i (i)}{#if tk.c}<span class="cite">{tk.t}</span>{:else}{tk.t}{/if}{/each}
          </p>
        {/each}
        {#if showingDraft.payload?.references?.length}
          <div class="shead">参考文献</div>
          <div class="refs">
            {#each showingDraft.payload.references as r (r.n)}
              <div class="refitem">
                <span class="cnum">[{r.n}]</span>
                <span>{r.authors ?? "佚名"}{r.year ? ` (${r.year})` : ""}. {r.title}.{#if r.page}（p.{r.page} 摘录）{/if}</span>
              </div>
            {/each}
          </div>
        {/if}
        <div class="matrix-modal-actions">
          <button class="btn" onclick={() => copyDraft(showingDraft)}>复制全文</button>
          <button class="btn" onclick={() => exportDraft(showingDraft)}>导出 docx</button>
          <button class="btn" onclick={() => exportDraftTex(showingDraft)} title="ctexart + thebibliography，xelatex 可直接编译">导出 LaTeX</button>
          <button class="btn primary" onclick={closeDraftModal}>完成</button>
        </div>
      </div>
    {:else if draftModal === "builder"}
      <div class="matrix-modal-bd">
        <div class="shead">从 {pickedCount} 条摘录生成成稿</div>
        <p class="dhint">引用编号按勾选顺序固定：[1] 是最早勾选的那条；参考文献由摘录自动生成，不经模型改写。</p>
        <div class="brefs">
          {#each pickedIds as pid, i (pid)}
            {@const ex = builderExcerpts.find((e) => e.id === pid)}
            <div class="bref">
              <span class="cnum">[{i + 1}]</span>
              <span class="bref-q">{ex ? ex.quote : "（摘录已缺失，生成时会报错）"}</span>
              <span class="bref-meta">{[ex?.paper_title, ex?.page != null ? `p.${ex.page}` : null].filter(Boolean).join(" · ")}</span>
            </div>
          {/each}
        </div>
        <input class="dt" bind:value={draftTitle} placeholder="成稿标题（可选，默认「研究成稿」）" />
        <textarea class="dp" bind:value={draftPrompt} placeholder="补充要求（可选）：例如「两段以内」「侧重新颖性对比」「面向开题报告」"></textarea>
        <div class="matrix-modal-actions">
          <button class="btn" onclick={clearExcerptSelection}>清空选择</button>
          <button class="btn" onclick={closeDraftModal}>取消</button>
          <button class="btn primary" onclick={runGenerateDraft}>生成成稿</button>
        </div>
      </div>
    {:else}
      <div class="matrix-modal-bd">
        <div class="shead">成稿库 <span class="n">{drafts.length}</span></div>
        {#if !drafts.length}
          <p class="none">还没有成稿。勾选摘录后点顶部「成稿」生成第一条。</p>
        {/if}
        {#each drafts as d (d.id)}
          <div class="lrow">
            <button type="button" class="lbody" onclick={() => (viewDraft = d)}>
              <div class="ltitle">{d.title}</div>
              <div class="lsub">{d.meta} · {new Date(d.created_at).toLocaleDateString("zh-CN")}</div>
            </button>
            <button class="x" title="导出 docx" onclick={() => exportDraft(d)}>
              <Icon name="doc" size={10} />
            </button>
            <button class="x" title="删除成稿" onclick={() => deleteDraft(d.id)}>
              <Icon name="x" size={10} />
            </button>
          </div>
        {/each}
        <div class="matrix-modal-actions">
          <button class="btn" onclick={closeDraftModal}>关闭</button>
        </div>
      </div>
    {/if}
  </div>
{/if}

{#if extractOpen}
  <div class="matrix-modal-bg" onclick={closeExtract} role="presentation"></div>
  <div class="matrix-modal extract-modal" role="dialog" aria-label="结构化提取表格">
    {#if extractError}
      <div class="matrix-modal-bd err">
        {extractError}
        <div class="matrix-modal-actions">
          <button class="btn" onclick={() => (extractError = null)}>返回</button>
          <button class="btn" onclick={closeExtract}>关闭</button>
        </div>
      </div>
    {:else if extractView === "pick"}
      <div class="matrix-modal-bd">
        <div class="shead">结构化提取 <span class="n">已勾选 {matrixSelectionCount} 篇</span></div>
        <p class="dhint">和对比矩阵共用同一组勾选。选一个模板 → 对勾选的每篇论文各调用一次模型逐字段提取 → 单元格可手动修正（修正后不会被重跑覆盖）。</p>

        {#if tplFormOpen}
          <div class="tplform">
            <input class="dt" bind:value={tplName} placeholder="模板名称，例如「RCT 方法学提取」" />
            <input class="dt" bind:value={tplDesc} placeholder="模板说明（可选）" />
            <div class="flds">
              <div class="fld fld-head"><span>字段 key</span><span>提取要求（问模型的问题）</span><span></span></div>
              {#each tplFields as f, i (i)}
                <div class="fld">
                  <input bind:value={f.key} placeholder="sample_size" />
                  <input bind:value={f.prompt} placeholder="样本量与数据集规模" />
                  <button class="x" title="删除字段" disabled={tplFields.length <= 1}
                          onclick={() => (tplFields = tplFields.filter((_, j) => j !== i))}>
                    <Icon name="x" size={10} />
                  </button>
                </div>
              {/each}
            </div>
            <div class="matrix-modal-actions">
              <button class="btn" onclick={() => tplFields = [...tplFields, { key: "", prompt: "" }]}>加字段</button>
              <button class="btn" onclick={() => (tplFormOpen = false)}>取消</button>
              <button class="btn primary" disabled={!tplName.trim() || extractBusy} onclick={saveTemplate}>
                {extractBusy ? "保存中…" : "保存模板"}
              </button>
            </div>
          </div>
        {:else}
          {#if !extractTemplates.length}
            <p class="none">还没有模板。先建一个，把要提取的字段固定下来。</p>
          {:else}
            <div class="tpllist">
              {#each extractTemplates as t (t.id)}
                <div class="tplrow">
                  <button type="button" class="tpl" class:on={extractChosenId === t.id} onclick={() => (extractChosenId = t.id)}>
                    <div class="tplname">{t.name}</div>
                    {#if t.description}<div class="tpldesc">{t.description}</div>{/if}
                    <div class="tplflds">{#each t.fields as f (f.key)}<span class="chip">{f.key}</span>{/each}</div>
                  </button>
                  <div class="tpltools">
                    <button class="x" title="编辑模板" onclick={() => openTemplateForm(t)}><Icon name="pencil" size={10} /></button>
                    <button class="x" title="删除模板" onclick={() => deleteTemplate(t.id)}><Icon name="x" size={10} /></button>
                  </div>
                </div>
              {/each}
            </div>
          {/if}
          <input class="dt" bind:value={extractRunTitle} placeholder="本次提取标题（可选，默认「模板名 · N 篇」）" />
          <div class="matrix-modal-actions">
            <button class="btn" onclick={() => openTemplateForm()}>新建模板</button>
            <button class="btn" onclick={() => (extractView = "runs")}>提取历史</button>
            <button class="btn" onclick={closeExtract}>取消</button>
            <button class="btn primary" disabled={!extractChosenId || matrixSelectionCount === 0 || extractBusy}
                    title={!extractChosenId ? "先选择一个模板" : matrixSelectionCount === 0 ? "先在论文列表勾选论文" : ""}
                    onclick={startExtraction}>
              {extractBusy ? "创建中…" : `开始提取 ${matrixSelectionCount} 篇`}
            </button>
          </div>
        {/if}
      </div>
    {:else if extractView === "runs"}
      <div class="matrix-modal-bd">
        <div class="shead">提取历史 <span class="n">{extractRuns.length}</span></div>
        {#if !extractRuns.length}
          <p class="none">还没有提取记录。</p>
        {/if}
        {#each extractRuns as r (r.id)}
          <div class="lrow">
            <button type="button" class="lbody" onclick={() => openRun(r.id)}>
              <div class="ltitle">{r.title}</div>
              <div class="lsub">
                {r.template_name} · {r.item_ids.length} 篇 · {r.fields.length} 字段 ·
                <span class:running={r.status === "running"} class:failed={r.status === "error"}>
                  {r.status === "running" ? "提取中" : r.status === "error" ? "失败" : "完成"}
                </span>
                · {new Date(r.created_at).toLocaleString("zh-CN")}
              </div>
            </button>
            <button class="x" title="删除记录" onclick={() => deleteRun(r.id)}><Icon name="x" size={10} /></button>
          </div>
        {/each}
        <div class="matrix-modal-actions">
          <button class="btn" onclick={() => (extractView = "pick")}>返回</button>
          <button class="btn" onclick={closeExtract}>关闭</button>
        </div>
      </div>
    {:else if extractResult}
      <div class="matrix-modal-bd">
        <div class="shead">
          {extractResult.run.title}
          <span class="n">{extractResult.items.length} 篇 · {extractResult.run.fields.length} 字段</span>
          {#if extractRunning}<span class="runbadge"><span class="is-spin" style="width:10px;height:10px"></span>提取中…</span>
          {:else if extractResult.run.status === "error"}<span class="runbadge failed">失败</span>{/if}
        </div>
        {#if extractResult.run.error}
          <p class="dhint" style="color:var(--warn)">{extractResult.run.error}</p>
        {/if}
        <div class="xtable-wrap">
          <table class="xtable">
            <thead>
              <tr>
                <th class="sticky">论文</th>
                {#each extractResult.run.fields as f (f.key)}
                  <th title={f.prompt}>{f.key}</th>
                {/each}
                <th class="ops"></th>
              </tr>
            </thead>
            <tbody>
              {#each extractResult.items as item (item.id)}
                <tr>
                  <td class="sticky">
                    <div class="ptitle">{item.title}</div>
                    <div class="faint">{[item.authors, item.year].filter(Boolean).join(" · ")}</div>
                  </td>
                  {#each extractResult.run.fields as f (f.key)}
                    <td>
                      <input
                        value={cellVal(item.id, f.key)}
                        placeholder={extractRunning ? "…" : "空"}
                        onchange={(e) => saveCell(item.id, f.key, (e.target as HTMLInputElement).value)}
                        title={cellIsManual(item.id, f.key) ? "手动修正过（重跑不会覆盖）" : "点击编辑并校对"}
                      />
                      {#if cellIsManual(item.id, f.key)}<span class="mflag" title="手动修正">手</span>{/if}
                    </td>
                  {/each}
                  <td class="ops">
                    <button class="x" title="重新提取这一行（手动修正过的格子不会被覆盖）"
                            disabled={retryingItem === item.id || extractRunning}
                            onclick={() => retryRow(item.id)}>
                      <Icon name="refresh" size={11} />
                    </button>
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
        <div class="matrix-modal-actions">
          <button class="btn" onclick={exportCsv}>导出 CSV</button>
          <button class="btn" onclick={() => { extractResult = null; extractView = "pick"; }}>再提一次</button>
          <button class="btn primary" onclick={closeExtract}>完成</button>
        </div>
      </div>
    {/if}
  </div>
{/if}

{#if prismaOpen}
  <div class="matrix-modal-bg" onclick={closePrisma} role="presentation"></div>
  <div class="matrix-modal extract-modal" role="dialog" aria-label="PRISMA 8 维抽取">
    {#if prismaError}
      <div class="matrix-modal-bd err">
        {prismaError}
        <div class="matrix-modal-actions">
          <button class="btn" onclick={() => (prismaError = null)}>返回</button>
          <button class="btn" onclick={closePrisma}>关闭</button>
        </div>
      </div>
    {:else if prismaView === "pick"}
      <div class="matrix-modal-bd">
        <div class="shead">PRISMA 8 维抽取 <span class="n">已勾选 {matrixSelectionCount} 篇</span></div>
        <p class="dhint">系统综述专用表：固定 8 维（研究问题 / 方法 / 贡献 / 结果 / 局限 / 适用人群 / 使用前提 / 复现成本）逐篇抽取，每个值附证据句并回链摘录卡。和对比矩阵、提取表格共用同一组勾选；Matrix 是自由对比，这张表是综述用的。</p>
        <input class="dt" bind:value={prismaRunTitle} placeholder="本次抽取标题（可选，默认「PRISMA 8 维 · N 篇」）" />
        <div class="matrix-modal-actions">
          <button class="btn" onclick={() => (prismaView = "runs")}>抽取历史</button>
          <button class="btn" onclick={closePrisma}>取消</button>
          <button class="btn primary" disabled={matrixSelectionCount === 0 || prismaBusy}
                  title={matrixSelectionCount === 0 ? "先在论文列表勾选论文" : ""}
                  onclick={startPrisma}>
            {prismaBusy ? "创建中…" : `开始抽取 ${matrixSelectionCount} 篇`}
          </button>
        </div>
      </div>
    {:else if prismaView === "runs"}
      <div class="matrix-modal-bd">
        <div class="shead">抽取历史 <span class="n">{prismaRuns.length}</span></div>
        {#if !prismaRuns.length}
          <p class="none">还没有抽取记录。</p>
        {/if}
        {#each prismaRuns as r (r.id)}
          <div class="lrow">
            <button type="button" class="lbody" onclick={() => openPrismaRun(r.id)}>
              <div class="ltitle">{r.title}</div>
              <div class="lsub">
                {r.item_ids.length} 篇 · 8 维 ·
                <span class:running={r.status === "running"} class:failed={r.status === "error"}>
                  {r.status === "running" ? "抽取中" : r.status === "error" ? "失败" : "完成"}
                </span>
                · {new Date(r.created_at).toLocaleString("zh-CN")}
              </div>
            </button>
            <button class="x" title="删除记录" onclick={() => prismaDeleteRun(r.id)}><Icon name="x" size={10} /></button>
          </div>
        {/each}
        <div class="matrix-modal-actions">
          <button class="btn" onclick={() => (prismaView = "pick")}>返回</button>
          <button class="btn" onclick={closePrisma}>关闭</button>
        </div>
      </div>
    {:else if prismaResult}
      <div class="matrix-modal-bd">
        <div class="shead">
          {prismaResult.run.title}
          <span class="n">{prismaResult.items.length} 篇 · 8 维</span>
          {#if prismaRunning}<span class="runbadge"><span class="is-spin" style="width:10px;height:10px"></span>抽取中…</span>
          {:else if prismaResult.run.status === "error"}<span class="runbadge failed">失败</span>{/if}
        </div>
        {#if prismaResult.run.error}
          <p class="dhint" style="color:var(--warn)">{prismaResult.run.error}</p>
        {/if}
        <div class="xtable-wrap">
          <table class="xtable">
            <thead>
              <tr>
                <th class="sticky">论文</th>
                {#each prismaResult.run.fields as f (f.key)}
                  <th title={f.prompt}>{prismaLabel(f.key)}</th>
                {/each}
                <th class="ops"></th>
              </tr>
            </thead>
            <tbody>
              {#each prismaResult.items as item (item.id)}
                <tr>
                  <td class="sticky">
                    <div class="ptitle">{item.title}</div>
                    <div class="faint">{[item.authors, item.year].filter(Boolean).join(" · ")}</div>
                  </td>
                  {#each prismaResult.run.fields as f (f.key)}
                    {@const cell = prismaEvidence(item.id, f.key)}
                    <td>
                      <input
                        value={cell?.value ?? ""}
                        placeholder={prismaRunning ? "…" : "空"}
                        onchange={(e) => prismaSaveCell(item.id, f.key, (e.target as HTMLInputElement).value)}
                        title={cell?.source === "manual" ? "手动修正过（重跑不会覆盖）" : "点击编辑并校对"}
                      />
                      {#if cell?.evidence}
                        <div class="ev" class:linked={!!cell.excerpt_id}
                             title={cell.evidence + (cell.excerpt_id ? "（已回链摘录卡）" : "（未回链：这句证据没能在论文卡片原文中逐字定位）")}>
                          {#if cell.excerpt_id}<span class="evl">摘</span>{/if}{cell.evidence}
                        </div>
                      {/if}
                      {#if cell?.source === "manual"}<span class="mflag" title="手动修正">手</span>{/if}
                    </td>
                  {/each}
                  <td class="ops">
                    <button class="x" title="重新抽取这一行（手动修正过的格子不会被覆盖）"
                            disabled={prismaRetrying === item.id || prismaRunning}
                            onclick={() => prismaRetryRow(item.id)}>
                      <Icon name="refresh" size={11} />
                    </button>
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
        <p class="dhint">带「摘」标的证据句已回链到摘录卡（摘录列表里可见「PRISMA · 维度」标注的条目）；没有标的证据句是模型给出但未能在原文逐字定位的，仅作参考。格子可直接改，改过即标记「手」，重跑不覆盖。</p>
        <div class="matrix-modal-actions">
          <button class="btn" onclick={exportPrismaCsv}>导出 CSV</button>
          <button class="btn" onclick={exportPrismaMd}>导出 Markdown</button>
          <button class="btn" onclick={() => { prismaResult = null; prismaView = "pick"; }}>再抽一次</button>
          <button class="btn primary" onclick={closePrisma}>完成</button>
        </div>
      </div>
    {/if}
  </div>
{/if}

{#if tensionOpen}
  <div class="matrix-modal-bg" onclick={closeTensions} role="presentation"></div>
  <div class="matrix-modal extract-modal" role="dialog" aria-label="文献张力分析">
    {#if tensionError}
      <div class="matrix-modal-bd err">
        {tensionError}
        <div class="matrix-modal-actions">
          <button class="btn" onclick={() => (tensionError = null)}>返回</button>
          <button class="btn" onclick={closeTensions}>关闭</button>
        </div>
      </div>
    {:else if tensionView === "pick"}
      <div class="matrix-modal-bd">
        <div class="shead">文献张力分析 <span class="n">已勾选 {matrixSelectionCount} 篇</span></div>
        <p class="dhint">知源式「整理助手」：模型通读勾选论文的卡片，找出文献之间真正不一致的地方——矛盾断言、互补拼图、证据互斥、适用范围冲突。每条张力必须附双方原话；没有引用的张力不会被记录。张量是待核查的假设，不是事实。</p>
        {#if matrixSelectionCount < 2}
          <p class="dhint" style="color:var(--warn)">至少勾选 2 篇论文（在论文列表勾选）。</p>
        {/if}
        <input class="dt" bind:value={tensionTitle} placeholder="本次分析标题（可选）" />
        <div class="matrix-modal-actions">
          <button class="btn" onclick={() => (tensionView = "runs")}>分析历史</button>
          <button class="btn" onclick={closeTensions}>取消</button>
          <button class="btn primary" disabled={matrixSelectionCount < 2 || tensionBusy} onclick={startTension}>
            {tensionBusy ? "创建中…" : `分析 ${matrixSelectionCount} 篇`}
          </button>
        </div>
      </div>
    {:else if tensionView === "runs"}
      <div class="matrix-modal-bd">
        <div class="shead">分析历史 <span class="n">{tensionRuns.length}</span></div>
        {#if !tensionRuns.length}
          <p class="dhint">还没有分析记录。</p>
        {:else}
          <ul class="runlist">
            {#each tensionRuns as r (r.id)}
              <li>
                <button class="runpick" onclick={() => openTensionRun(r.id)}>
                  <span class="mono">{r.title}</span>
                  <span class="mut mono">{r.item_ids.length} 篇 · {new Date(r.created_at).toLocaleDateString("zh-CN")}</span>
                </button>
                <button class="x" title="删除" onclick={() => deleteTensionRun(r.id)}><Icon name="x" size={11} /></button>
              </li>
            {/each}
          </ul>
        {/if}
        <div class="matrix-modal-actions">
          <button class="btn" onclick={() => (tensionView = "pick")}>返回</button>
          <button class="btn" onclick={closeTensions}>关闭</button>
        </div>
      </div>
    {:else if tensionResult}
      <div class="matrix-modal-bd">
        <div class="shead">
          {tensionResult.run.title}
          {#if tensionResult.run.status === "running"}<span class="runbadge">进行中</span>
          {:else if tensionResult.run.status === "error"}<span class="runbadge failed">失败</span>{/if}
          <span class="n">{tensionResult.tensions.length} 条张力</span>
        </div>
        {#if tensionResult.tensions.length === 0 && !tensionRunning}
          <p class="dhint">{#if tensionResult.run.status === "error"}{tensionResult.run.error}{:else}未检测到显著张力——要么文献高度一致，要么卡片信息不足。{/if}</p>
        {:else}
          {#each tensionResult.tensions as t (t.id)}
            <div class="tension-card">
              <div class="tension-h">
                <span class="tension-topic">{t.topic}</span>
                <span class="tension-tags">
                  <span class="ttag" title="冲突类型">{t.conflict}</span>
                  <span class="ttag sev-{t.severity}" title="严重度">{t.severity}</span>
                </span>
              </div>
              <div class="tension-side">
                <div class="ts-label mono">{tensionPaperTitle(t.paper_a_id)}</div>
                <div class="ts-claim">{t.claim_a}</div>
                <div class="ts-quote">「{t.quote_a}」</div>
              </div>
              <div class="tension-vs mono">vs</div>
              <div class="tension-side">
                <div class="ts-label mono">{tensionPaperTitle(t.paper_b_id)}</div>
                <div class="ts-claim">{t.claim_b}</div>
                <div class="ts-quote">「{t.quote_b}」</div>
              </div>
            </div>
          {/each}
        {/if}
        <div class="matrix-modal-actions">
          <button class="btn" onclick={exportTensionMd}>导出 Markdown</button>
          <button class="btn" onclick={() => { tensionResult = null; tensionView = "pick"; }}>再分析一次</button>
          <button class="btn primary" onclick={closeTensions}>完成</button>
        </div>
      </div>
    {/if}
  </div>
{/if}

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

  /* B4: cross-paper matrix UI */
  .prow { display: flex; align-items: stretch; gap: 0; }
  .prow.picked { background: rgba(124, 92, 240, 0.08); }
  .prow-check {
    display: flex;
    align-items: center;
    padding: 0 6px 0 10px;
    cursor: pointer;
  }
  .prow-check input { cursor: pointer; }
  .prow-body {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: stretch;
    background: transparent;
    border: 0;
    text-align: left;
    color: inherit;
    padding: 10px 8px;
    cursor: pointer;
  }
  .prow-body .ptitle { font-size: 13px; }
  .matrix-modal-bg {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    z-index: 90;
  }
  .matrix-modal {
    position: fixed;
    inset: 4vh 6vw;
    z-index: 91;
    background: var(--surface-1, rgba(20, 20, 28, 0.96));
    border-radius: var(--rad-4, 10px);
    border: 1px solid var(--line-2, rgba(255, 255, 255, 0.1));
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.5);
    overflow: auto;
  }
  .matrix-modal-bd {
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 12px;
    padding: 20px;
  }
  .matrix-modal-bd.err { color: #fca5a5; }
  .matrix-modal-loading {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 20px;
    color: var(--text-3, #b5b9c3);
  }
  .matrix-modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  /* B5: excerpts → citable draft */
  .ex {
    display: flex;
    gap: 8px;
    align-items: stretch;
  }
  .ex.picked {
    background: rgba(124, 92, 240, 0.08);
  }
  .excheck {
    display: flex;
    align-items: flex-start;
    gap: 4px;
    padding-top: 2px;
    cursor: pointer;
    flex-shrink: 0;
  }
  .excheck input { cursor: pointer; }
  .exmain { flex: 1; min-width: 0; }
  .cnum {
    font-family: var(--font-mono, monospace);
    font-size: 11px;
    color: var(--accent);
    background: rgba(124, 92, 240, 0.12);
    border-radius: 4px;
    padding: 0 4px;
    white-space: nowrap;
  }
  .draft-modal { inset: 8vh 12vw; }
  .dtitle {
    font-size: var(--t-lg);
    font-weight: 500;
    color: var(--fg-bright);
    line-height: 1.4;
  }
  .dsub {
    margin-top: 4px;
    font-size: 11px;
    color: var(--fg-faint);
  }
  .thesis {
    margin: 14px 0 0;
    font-size: var(--t-sm);
    color: var(--accent);
    line-height: 1.6;
  }
  .dpara {
    margin: 10px 0 0;
    font-size: var(--t-sm);
    line-height: 1.8;
    color: var(--fg);
  }
  .cite {
    color: var(--accent);
    font-size: 11px;
    vertical-align: super;
    padding: 0 1px;
  }
  .refs { margin-top: 6px; }
  .refitem {
    display: flex;
    gap: 6px;
    font-size: var(--t-xs);
    color: var(--fg-muted);
    line-height: 1.6;
    margin-bottom: 4px;
  }
  .refitem .cnum { flex-shrink: 0; }
  .dhint {
    margin: 0 0 10px;
    font-size: 11px;
    color: var(--fg-faint);
  }
  .brefs {
    max-height: 180px;
    overflow-y: auto;
    border: 1px solid var(--border);
    border-radius: 7px;
    padding: 8px 10px;
    margin-bottom: 10px;
  }
  .bref {
    display: flex;
    gap: 6px;
    align-items: baseline;
    font-size: var(--t-xs);
    line-height: 1.6;
    margin-bottom: 4px;
  }
  .bref .cnum { flex-shrink: 0; }
  .bref-q {
    color: var(--fg);
    overflow: hidden;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
  }
  .bref-meta {
    color: var(--fg-faint);
    white-space: nowrap;
    flex-shrink: 0;
  }
  .dt {
    width: 100%;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 7px;
    color: var(--fg);
    font-size: var(--t-xs);
    padding: 7px 9px;
    outline: none;
    margin-bottom: 8px;
  }
  .dt:focus { border-color: var(--accent-dim); }
  .dp {
    width: 100%;
    min-height: 54px;
    resize: vertical;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 7px;
    color: var(--fg);
    font-size: var(--t-xs);
    line-height: 1.6;
    padding: 7px 9px;
    outline: none;
    margin-bottom: 10px;
  }
  .dp:focus { border-color: var(--accent-dim); }
  .lrow {
    display: flex;
    align-items: center;
    gap: 4px;
    border: 1px solid var(--border);
    border-radius: 8px;
    margin-bottom: 6px;
  }
  .lrow:hover { background: var(--surface-2); }
  .lbody {
    flex: 1;
    background: transparent;
    border: 0;
    text-align: left;
    color: inherit;
    padding: 9px 11px;
    cursor: pointer;
  }
  .ltitle {
    font-size: var(--t-sm);
    color: var(--fg);
    line-height: 1.4;
  }
  .lsub {
    margin-top: 2px;
    font-size: 11px;
    color: var(--fg-faint);
  }
  .lrow .x { margin-right: 10px; }

  /* ----- D8: extraction templates ----- */
  .extract-modal { inset: 7vh 9vw; }
  .tpllist {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 10px;
  }
  .tplrow {
    display: flex;
    align-items: stretch;
    gap: 4px;
  }
  .tpl {
    flex: 1;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 8px;
    text-align: left;
    padding: 9px 11px;
    cursor: pointer;
    color: inherit;
  }
  .tpl:hover { border-color: var(--border-strong); }
  .tpl.on {
    border-color: var(--accent-dim);
    background: rgba(124, 92, 240, 0.08);
  }
  .tplname {
    font-size: var(--t-sm);
    color: var(--fg-bright);
    line-height: 1.4;
  }
  .tpldesc {
    margin-top: 2px;
    font-size: 11px;
    color: var(--fg-faint);
  }
  .tplflds {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin-top: 6px;
  }
  .chip {
    font-family: var(--font-mono, monospace);
    font-size: 10px;
    color: var(--fg-muted);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 1px 5px;
  }
  .tpltools {
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 4px;
  }
  .tplform {
    margin-bottom: 10px;
  }
  .flds {
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 8px;
    margin-bottom: 8px;
  }
  .fld {
    display: grid;
    grid-template-columns: 150px 1fr 26px;
    gap: 6px;
    align-items: center;
    margin-bottom: 6px;
  }
  .fld:last-child { margin-bottom: 0; }
  .fld input {
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--fg);
    font-size: var(--t-xs);
    padding: 5px 8px;
    outline: none;
    width: 100%;
  }
  .fld input:focus { border-color: var(--accent-dim); }
  .fld-head {
    font-size: 10px;
    color: var(--fg-faint);
    margin-bottom: 4px;
  }
  .fld-head span:nth-child(3) { width: 26px; }
  .xtable-wrap {
    overflow: auto;
    max-height: 52vh;
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  .xtable {
    border-collapse: separate;
    border-spacing: 0;
    width: 100%;
    font-size: var(--t-xs);
  }
  .xtable th,
  .xtable td {
    border-bottom: 1px solid var(--border);
    padding: 7px 9px;
    text-align: left;
    vertical-align: top;
  }
  .xtable th {
    position: sticky;
    top: 0;
    background: var(--surface);
    color: var(--fg-muted);
    font-weight: 500;
    font-family: var(--font-mono, monospace);
    font-size: 10.5px;
    z-index: 1;
  }
  .xtable .sticky {
    position: sticky;
    left: 0;
    background: var(--surface);
    min-width: 190px;
    max-width: 240px;
    z-index: 0;
  }
  .xtable thead .sticky { z-index: 2; }
  .xtable td .ptitle {
    font-size: var(--t-xs);
    color: var(--fg);
    line-height: 1.45;
  }
  .xtable td input {
    width: 100%;
    min-width: 130px;
    background: transparent;
    border: 1px solid transparent;
    border-radius: 5px;
    color: var(--fg);
    font-size: var(--t-xs);
    line-height: 1.5;
    padding: 3px 5px;
    outline: none;
  }
  .xtable td input:hover { border-color: var(--border); }
  .xtable td input:focus {
    border-color: var(--accent-dim);
    background: var(--bg);
  }
  .xtable .ops { width: 34px; text-align: center; }
  .mflag {
    font-size: 9px;
    color: var(--accent);
    border: 1px solid var(--accent-dim);
    border-radius: 3px;
    padding: 0 3px;
    margin-left: 4px;
    white-space: nowrap;
  }
  .runbadge {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    margin-left: 10px;
    font-size: 11px;
    color: var(--accent);
    vertical-align: middle;
  }
  .runbadge.failed { color: var(--err); }
  .lsub .running { color: var(--accent); }
  .lsub .failed { color: var(--err); }
  /* PRISMA evidence line under each cell value */
  .ev {
    margin-top: 3px;
    font-size: 10.5px;
    line-height: 1.5;
    color: var(--fg-muted);
    max-width: 220px;
    display: -webkit-box;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .ev.linked { color: var(--fg); opacity: 0.85; }
  .ev .evl {
    font-size: 9px;
    color: var(--accent);
    border: 1px solid var(--accent-dim);
    border-radius: 3px;
    padding: 0 3px;
    margin-right: 4px;
  }
  .tension-card {
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 12px 14px;
    margin-bottom: 10px;
    background: var(--surface);
    display: grid;
    grid-template-columns: 1fr auto 1fr;
    gap: 4px 12px;
    align-items: stretch;
  }
  .tension-h { grid-column: 1 / -1; display: flex; align-items: center; gap: 8px; margin-bottom: 4px; }
  .tension-topic { font-weight: 600; color: var(--fg-bright); font-size: var(--t-s); }
  .tension-tags { display: flex; gap: 4px; }
  .ttag { font-size: 10px; padding: 1px 7px; border-radius: 999px; background: var(--surface-2); border: 1px solid var(--border); color: var(--fg-muted); font-family: var(--font-mono); }
  .ttag.sev-high { color: var(--err, #e5484d); border-color: color-mix(in oklab, var(--err, #e5484d) 45%, transparent); }
  .ttag.sev-medium { color: var(--warn); }
  .tension-side { min-width: 0; }
  .ts-label { font-size: 10.5px; color: var(--fg-faint); margin-bottom: 3px; }
  .ts-claim { font-size: var(--t-s); color: var(--fg); margin-bottom: 5px; }
  .ts-quote { font-size: var(--t-xs); color: var(--fg-muted); border-left: 2px solid var(--border); padding-left: 8px; }
  .tension-vs { align-self: center; color: var(--fg-faint); font-size: 10px; }
  @media (max-width: 760px) {
    .tension-card { grid-template-columns: 1fr; }
    .tension-vs { text-align: center; }
  }
</style>
