<script lang="ts">
  import * as api from "../lib/api";
  import type { MaterialRec } from "../lib/api";
  import { app } from "../lib/store.svelte";
  import Icon from "../components/Icon.svelte";
  import AudioPlayer from "../components/AudioPlayer.svelte";
  import Flashcards from "./Flashcards.svelte";
  import Quiz from "./Quiz.svelte";
  import InfographicView from "../components/InfographicView.svelte";
  import SlideshowView from "../components/SlideshowView.svelte";
  import MindMapView from "../components/MindMapView.svelte";
  import MatrixView from "../components/MatrixView.svelte";
  import GuideView from "../components/GuideView.svelte";
  import GeneratingCard from "../components/GeneratingCard.svelte";
  import { jobs } from "../lib/jobs.svelte";

  // material type metadata
  const MAT_TYPES: Record<string, { label: string; group: string; icon: string; color: string }> = {
    flashcards:  { label: "闪卡",      group: "闪卡",       icon: "cards", color: "var(--accent)"       },
    quiz:        { label: "测验",             group: "测验",          icon: "check", color: "var(--info)"         },
    audio:       { label: "音频概览",   group: "音频概览",  icon: "music", color: "var(--mode-select)"  },
    slideshow:   { label: "幻灯片",           group: "幻灯片",           icon: "grid",  color: "var(--warn)"         },
    infographic: { label: "信息图",      group: "信息图",     icon: "grid",  color: "var(--ok)"           },
    mindmap:     { label: "思维导图",          group: "思维导图",        icon: "link",  color: "var(--info)"         },
    matrix:      { label: "对比矩阵", group: "对比矩阵", icon: "grid", color: "var(--mode-select)" },
    guide:       { label: "学习指南",       group: "学习指南",     icon: "cards", color: "var(--ok)"           },
  };
  const MAT_ORDER = ["flashcards", "quiz", "audio", "slideshow", "infographic", "mindmap", "matrix", "guide"];

  // ── unified card shape ────────────────────────────────────────
  interface Card {
    id: string;
    type: string;
    title: string;
    topic: string;
    meta: string;
    status: string;
    payload?: any;
  }

  // ── real materials loaded from backend ────────────────────────
  let materials = $state<Card[]>([]);
  let loading = $state(false);

  function loadMaterials(subjectId: string) {
    loading = true;
    api.listMaterials(subjectId).then((recs: MaterialRec[]) => {
      materials = recs.map((r) => ({
        id: r.id,
        type: r.kind,
        title: r.title,
        topic: r.topic,
        meta: r.meta,
        status: r.status,
        payload: r.payload,
      }));
    }).catch(() => { materials = []; }).finally(() => { loading = false; });
  }

  $effect(() => {
    const sub = app.activeSubject;
    if (!sub) { materials = []; return; }
    loadMaterials(sub.id);
  });

  // Background material-generation jobs for this subject (cheatsheet lives in its
  // own view). Running cards show "generating…"; errors stay until dismissed.
  const matJobs = $derived(
    jobs.forSubject(app.activeSubjectId).filter(
      (j) => j.kind !== "cheatsheet" && (j.status === "running" || j.status === "error")
    )
  );
  const runningCount = $derived(
    jobs.running(app.activeSubjectId).filter((j) => j.kind !== "cheatsheet").length
  );

  // When the running count drops (a job finished), reload the persisted list so
  // the freshly-generated material appears — even if generation was kicked off
  // from another view and we navigated here afterward.
  let lastRunning = $state(-1);
  $effect(() => {
    const n = runningCount;
    const sub = app.activeSubject;
    if (lastRunning !== -1 && n < lastRunning && sub) loadMaterials(sub.id);
    lastRunning = n;
  });

  let filter   = $state("all");
  // null = materials grid; otherwise the launched material
  let matLaunch = $state<Card | null>(null);

  const topics = $derived(Array.from(new Set(materials.map(m => m.topic))));
  const shown  = $derived(filter === "all" ? materials : materials.filter(m => m.topic === filter));
  const groups = $derived(
    MAT_ORDER
      .map(type => ({ type, items: shown.filter(m => m.type === type) }))
      .filter(g => g.items.length)
  );

  function isLaunchable(type: string): boolean {
    return ["flashcards", "quiz", "audio", "infographic", "slideshow", "mindmap", "matrix", "guide"].includes(type);
  }

  function launchLabel(type: string): string {
    const map: Record<string, string> = {
      flashcards: "开始复习",
      quiz: "开始测验",
      audio: "播放",
      infographic: "查看",
      slideshow: "看幻灯片",
      mindmap: "看导图",
      matrix: "看表格",
      guide: "读指南",
    };
    return map[type] ?? "打开";
  }

  function launch(m: Card) {
    if (!isLaunchable(m.type) || m.status === "draft") return;
    matLaunch = m;
  }

  async function renameMaterial(e: Event, m: Card) {
    e.stopPropagation();
    const name = await app.prompt({ title: "Rename material", label: "Name", value: m.title, placeholder: m.title });
    if (name && name.trim() && name.trim() !== m.title) {
      try {
        await api.renameMaterial(m.id, name.trim());
        if (app.activeSubject) loadMaterials(app.activeSubject.id);
      } catch (err) {
        app.pushToast({ kind: "error", title: "Rename failed", body: String(err) });
      }
    }
  }
  async function deleteMaterial(e: Event, m: Card) {
    e.stopPropagation();
    if (!(await app.confirm({ title: `Delete "${m.title}"?`, danger: true, okLabel: "Delete" }))) return;
    try {
      await api.deleteMaterial(m.id);
      if (app.activeSubject) loadMaterials(app.activeSubject.id);
    } catch (err) {
      app.pushToast({ kind: "error", title: "Delete failed", body: String(err) });
    }
  }
  // Export a flashcard deck to an Anki .apkg via a native save dialog.
  async function exportAnki(e: Event, m: Card) {
    e.stopPropagation();
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const safe = m.title.replace(/[^\w.-]+/g, "_").replace(/^_+|_+$/g, "") || "deck";
      const dest = await save({
        defaultPath: `${safe}.apkg`,
        filters: [{ name: "Anki deck", extensions: ["apkg"] }],
      });
      if (!dest) return;
      const n = await api.exportAnki(m.id, dest);
      app.pushToast({ kind: "success", title: "Exported to Anki", body: `${n} card${n !== 1 ? "s" : ""} → ${dest}` });
    } catch (err) {
      app.pushToast({ kind: "error", title: "Anki export failed", body: String(err) });
    }
  }

  // Export any material as a real Office file. One native save dialog, three
  // formats: the extension the user settles on picks the backend command.
  async function exportOffice(e: Event, m: Card) {
    e.stopPropagation();
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const safe = m.title.replace(/[^\w.-]+/g, "_").replace(/^_+|_+$/g, "") || "material";
      const dest = await save({
        defaultPath: `${safe}.docx`,
        filters: [{ name: "Word / Excel / PowerPoint", extensions: ["docx", "xlsx", "pptx"] }],
      });
      if (!dest) return;
      const ext = dest.split(".").pop()?.toLowerCase();
      if (ext === "xlsx") await api.exportMaterialXlsx(m.id, dest);
      else if (ext === "pptx") await api.exportMaterialPptx(m.id, dest);
      else await api.exportMaterialDocx(m.id, dest);
      app.pushToast({ kind: "success", title: "已导出 Office 文档", body: dest });
    } catch (err) {
      app.pushToast({ kind: "error", title: "导出失败", body: String(err) });
    }
  }

  // Import an Anki .apkg into this subject as flashcard material(s) via a native
  // open dialog. Lives here (not in the Flashcards player) because this view owns
  // the deck list + reload, and mirrors the per-card export affordance above.
  let importing = $state(false);
  async function importAnki() {
    const sub = app.activeSubject;
    if (!sub) { app.pushToast({ kind: "warning", title: "Select a subject first" }); return; }
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const picked = await open({
        multiple: false,
        filters: [{ name: "Anki deck", extensions: ["apkg"] }],
      });
      const path = Array.isArray(picked) ? picked[0] : picked;
      if (!path) return;
      importing = true;
      const r = await api.importAnki(sub.id, path);
      const decks = `${r.deck_count} deck${r.deck_count !== 1 ? "s" : ""}`;
      const cards = `${r.card_count} card${r.card_count !== 1 ? "s" : ""}`;
      app.pushToast(
        r.skipped > 0
          ? { kind: "warning", title: "Imported with skips", body: `${cards} across ${decks} · ${r.skipped} duplicate${r.skipped !== 1 ? "s" : ""} skipped` }
          : { kind: "success", title: "Imported from Anki", body: `${cards} across ${decks}` }
      );
      loadMaterials(sub.id);
    } catch (err) {
      app.pushToast({ kind: "error", title: "Anki import failed", body: String(err) });
    } finally {
      importing = false;
    }
  }
</script>

{#if matLaunch}
  {#if matLaunch.type === "flashcards"}
    <Flashcards
      onExit={() => (matLaunch = null)}
      deck={matLaunch.payload ?? undefined}
    />
  {:else if matLaunch.type === "quiz"}
    <Quiz
      onExit={() => (matLaunch = null)}
      questions={matLaunch.payload ?? undefined}
    />
  {:else if matLaunch.type === "audio"}
    <AudioPlayer
      title={matLaunch.title}
      script={matLaunch.payload?.segments ?? []}
      materialId={matLaunch.id}
      onExit={() => (matLaunch = null)}
    />
  {:else if matLaunch.type === "infographic"}
    <InfographicView
      data={matLaunch.payload ?? undefined}
      onExit={() => (matLaunch = null)}
    />
  {:else if matLaunch.type === "slideshow"}
    <SlideshowView
      slides={matLaunch.payload?.slides ?? []}
      title={matLaunch.title}
      onExit={() => (matLaunch = null)}
    />
  {:else if matLaunch.type === "mindmap"}
    <MindMapView
      data={matLaunch.payload ?? undefined}
      onExit={() => (matLaunch = null)}
    />
  {:else if matLaunch.type === "matrix"}
    <MatrixView
      data={matLaunch.payload ?? undefined}
      title={matLaunch.title}
      onExit={() => (matLaunch = null)}
    />
  {:else if matLaunch.type === "guide"}
    <GuideView
      data={matLaunch.payload ?? undefined}
      title={matLaunch.title}
      onExit={() => (matLaunch = null)}
    />
  {/if}
{:else}
  <div class="workspace-scroll">
    <div class="materials-page">
      <!-- toolbar -->
      <div class="sources-toolbar">
        <span class="label">{materials.length} materials · generated from this subject</span>
        <div class="grow"></div>
        <button
          class="btn btn--sm"
          onclick={importAnki}
          disabled={importing}
          title="Import an Anki .apkg deck as flashcards"
        >
          <Icon name="upload" size={12} /> {importing ? "Importing…" : "Import Anki"}
        </button>
        <button
          class="btn btn--sm"
          onclick={() => app.setView("exam")}
          title="Take a timed, graded practice exam"
        >
          <Icon name="check" size={12} /> Exam mode
        </button>
        <button
          class="btn btn--sm btn--primary"
          onclick={() => app.setView("gen-material")}
        >
          <Icon name="bolt" size={12} /> Generate material
        </button>
      </div>

      {#each matJobs as job (job.id)}
        <GeneratingCard {job} />
      {/each}

      {#if loading}
        <div class="mat-empty">
          <span class="mono faint">正在加载材料…</span>
        </div>
      {:else if materials.length === 0}
        <!-- empty state -->
        <div class="mat-empty">
          <div class="mat-empty-ico">
            <Icon name="bolt" size={26} color="var(--fg-faint)" />
          </div>
          <h2 class="read mat-empty-h">还没有学习材料</h2>
          <p class="mono faint mat-empty-sub">从你的资料生成闪卡、测验、学习指南、幻灯片或信息图。</p>
          <button
            class="btn btn--primary"
            onclick={() => app.setView("gen-material")}
          >
            <Icon name="bolt" size={13} /> Generate material
          </button>
        </div>
      {:else}
        <!-- topic filter chips -->
        <div class="mat-filter">
          <button
            class="filter-chip{filter === 'all' ? ' on' : ''}"
            onclick={() => (filter = "all")}
          >全部主题</button>
          {#each topics as t (t)}
            <button
              class="filter-chip{filter === t ? ' on' : ''}"
              onclick={() => (filter = t)}
            >{t}</button>
          {/each}
        </div>

        <!-- grouped material cards -->
        {#each groups as g (g.type)}
          <div class="mat-group">
            <div class="mat-group-h">
              <span class="mat-group-ico" style:color={MAT_TYPES[g.type]?.color}>
                <Icon name={MAT_TYPES[g.type]?.icon ?? "grid"} size={13} />
              </span>
              <span class="mat-group-t">{MAT_TYPES[g.type]?.group ?? g.type}</span>
              <span class="mono faint">{g.items.length}</span>
            </div>
            <div class="mat-grid">
              {#each g.items as m (m.id)}
                {@const meta = MAT_TYPES[m.type]}
                {@const playable = isLaunchable(m.type) && m.status !== "draft"}
                <div
                  class="mat-card"
                  onclick={() => launch(m)}
                  role="button"
                  tabindex="0"
                  onkeydown={(e) => { if (e.key === "Enter") launch(m); }}
                >
                  <div class="mat-top">
                    <span class="mat-type" style:color={meta?.color}>
                      <Icon name={meta?.icon ?? "grid"} size={13} /> {meta?.label ?? m.type}
                    </span>
                    <span class="status-pill status-pill--{m.status === 'draft' ? 'draft' : 'ready'}">
                      <span class="dot"></span>
                    </span>
                    {#if m.type === "flashcards"}
                      <button class="btn btn--icon btn--sm btn--ghost mat-act" title="Export to Anki (.apkg)" aria-label="Export deck to Anki" onclick={(e) => exportAnki(e, m)}>
                        <Icon name="upload" size={12} />
                      </button>
                    {/if}
                    <button class="btn btn--icon btn--sm btn--ghost mat-act" title="导出 Word / Excel / PowerPoint" aria-label="Export as Office document" onclick={(e) => exportOffice(e, m)}>
                      <Icon name="doc" size={12} />
                    </button>
                    <button class="btn btn--icon btn--sm btn--ghost mat-act" title="Rename" aria-label="Rename material" onclick={(e) => renameMaterial(e, m)}>
                      <Icon name="pencil" size={12} />
                    </button>
                    <button class="btn btn--icon btn--sm btn--ghost mat-act" title="Delete" aria-label="Delete material" onclick={(e) => deleteMaterial(e, m)}>
                      <Icon name="x" size={12} />
                    </button>
                  </div>
                  <div class="mat-title read">{m.title}</div>
                  <div class="mat-foot">
                    <span class="topic-tag mono">
                      <Icon name="chevron" size={9} /> {m.topic}
                    </span>
                    <span class="mat-meta mono">{m.meta}</span>
                  </div>
                  {#if playable}
                    <div class="mat-launch mono">
                      <Icon name="play" size={11} />
                      {launchLabel(m.type)}
                    </div>
                  {/if}
                </div>
              {/each}
            </div>
          </div>
        {/each}
      {/if}
    </div>
  </div>
{/if}

<style>
  /* card action buttons (rename/delete) — reveal on hover */
  .mat-act { opacity: 0; transition: opacity var(--dur-fast); }
  .mat-card:hover .mat-act, .mat-card:focus-within .mat-act { opacity: 0.85; }
  .mat-act:hover { opacity: 1; }

  .mat-empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--sp-4);
    padding: var(--sp-10) var(--sp-7);
    text-align: center;
  }

  .mat-empty-ico {
    width: 56px;
    height: 56px;
    border-radius: var(--rad-4);
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    display: grid;
    place-items: center;
    margin-bottom: var(--sp-2);
  }

  .mat-empty-h {
    font-size: var(--r-lg);
    color: var(--fg-bright);
    font-weight: 600;
    margin: 0;
  }

  .mat-empty-sub {
    font-size: var(--t-sm);
    max-width: 380px;
    line-height: 1.5;
  }
</style>
