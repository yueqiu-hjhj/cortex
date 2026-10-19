<script lang="ts">
  import { app } from "../lib/store.svelte";
  import * as api from "../lib/api";
  import Icon from "../components/Icon.svelte";
  import { jobs, type JobKind } from "../lib/jobs.svelte";

  // ── Material type definitions ──────────────────────────────
  const GEN_TYPES = [
    { id: "flashcards", label: "闪卡",      ico: "cards", desc: "间隔重复抽认卡",    color: "var(--accent)"      },
    { id: "quiz",       label: "测验",             ico: "check", desc: "单选 · 简答 · 填空", color: "var(--info)"        },
    { id: "audio",      label: "音频概览",   ico: "music", desc: "双人播客式讲解",           color: "var(--mode-select)" },
    { id: "slideshow",  label: "幻灯片",           ico: "grid",  desc: "演示大纲",     color: "var(--warn)"        },
    { id: "infographic",label: "信息图",      ico: "grid",  desc: "一页纸摘要海报",         color: "var(--ok)"          },
    { id: "mindmap",    label: "思维导图",          ico: "link",  desc: "主题的概念结构",    color: "var(--info)"        },
    { id: "matrix",     label: "对比矩阵", ico: "grid",  desc: "概念并排对照表",  color: "var(--mode-select)" },
    { id: "guide",      label: "学习指南",       ico: "cards", desc: "要点 · 易错点 · 自测", color: "var(--ok)" },
  ] as const;

  const srcLabel: Record<string, string> = {
    pdf: "PDF", pptx: "PPTX", docx: "DOCX", web: "WEB", yt: "YT", audio: "AUD", image: "IMG",
  };

  // Count-based formats and their clamps; other formats have no "how many".
  const COUNT_LIMITS: Record<string, { min: number; max: number; def: number }> = {
    flashcards: { min: 4, max: 40, def: 14 },
    quiz: { min: 3, max: 30, def: 10 },
  };

  // ── Derived from real active subject ─────────────────────────
  const subjectTopics = $derived(app.activeSubject?.topics ?? []);
  const allSources = $derived(
    subjectTopics.flatMap(t =>
      t.sources.map(s => ({ ...s, topicId: t.id, topicName: t.name }))
    )
  );

  // ── State ─────────────────────────────────────────────────────
  let type  = $state<string>("flashcards");
  let sel   = $state<string[]>([]);
  let title = $state("");
  let customPrompt = $state("");
  // Per-type item count (flashcards / quiz). Seeded from the defaults.
  let cardCount = $state(COUNT_LIMITS.flashcards.def);
  let quizCount = $state(COUNT_LIMITS.quiz.def);

  // ── 2.7: slides two-round confirm (outline → edit → render per slide) ──
  let outline = $state<api.SlidePlan[] | null>(null);
  let outlineBusy = $state(false);
  let renderProgress = $state<api.SlidesProgress | null>(null);

  // ── Derived ───────────────────────────────────────────────────
  const selSources = $derived(allSources.filter(s => sel.includes(s.id)));
  const countLimit = $derived(COUNT_LIMITS[type] ?? null);
  const countValue = $derived(type === "flashcards" ? cardCount : type === "quiz" ? quizCount : null);

  const counts = $derived.by(() => {
    const c: Record<string, number> = {};
    for (const s of selSources) c[s.topicName] = (c[s.topicName] ?? 0) + 1;
    return c;
  });
  const topicNames = $derived(Object.keys(counts));
  const autoTopic  = $derived(
    topicNames.length === 0
      ? null
      : [...topicNames].sort((a, b) => counts[b] - counts[a])[0]
  );
  const dominantTopicId = $derived.by(() => {
    if (!autoTopic) return app.activeSubject?.topics[0]?.id;
    const t = subjectTopics.find(t => t.name === autoTopic);
    return t?.id ?? app.activeSubject?.topics[0]?.id;
  });
  const multi = $derived(topicNames.length > 1);

  // Plugin-contributed material types (plugins/*.json): label + steering prompt
  // for an EXISTING generator (base_kind). The dropdown shows them inline; the
  // submit maps them back onto the built-in kind with the plugin's prompt.
  let pluginTypes = $state<api.PluginMaterialType[]>([]);
  $effect(() => {
    api.pluginsList()
      .then((pl) => { pluginTypes = pl.plugins.flatMap((p) => p.material_types); })
      .catch(() => {});
  });
  // Per-slide render progress (2.7): the backend emits one event per page.
  $effect(() => {
    let un: (() => void) | null = null;
    let alive = true;
    api.onSlidesProgress((p) => { renderProgress = p; })
      .then((u) => { if (alive) un = u; else u(); });
    return () => { alive = false; un?.(); };
  });
  type GenType = { id: string; label: string; ico: string; desc: string; color: string; plugin?: api.PluginMaterialType };
  const allTypes = $derived<GenType[]>([
    ...GEN_TYPES,
    ...pluginTypes.map((t): GenType => ({
      id: t.id, label: t.label, ico: "cards", color: "var(--mode-select)",
      desc: `插件预设 · 运行${GEN_TYPES.find((g) => g.id === t.base_kind)?.label ?? t.base_kind}`,
      plugin: t,
    })),
  ]);
  const tm = $derived(allTypes.find(t => t.id === type));
  const activePlugin = $derived(tm?.plugin ?? null);
  const suffixFor = (t: string) => {
    const plugin = pluginTypes.find((p) => p.id === t);
    if (plugin) return ` — ${plugin.label}`;
    return ({ flashcards: " — flashcards", quiz: " — quiz", audio: " — deep dive", slideshow: " — slides", infographic: " — infographic", mindmap: " — mind map", matrix: " — comparison matrix", guide: " — study guide" } as Record<string, string>)[t] ?? "";
  };
  const suggested = $derived.by(() => {
    const suffix = suffixFor(type);
    if (selSources.length === 1) {
      const base = selSources[0].name.replace(/\.[^.]+$/, "").trim();
      if (base) return base + suffix;
    }
    return autoTopic ? autoTopic + suffix : "";
  });
  const finalTitle = $derived(title.trim() || suggested);
  const ready = $derived(sel.length > 0 && !!app.activeSubject);

  // ── Actions ───────────────────────────────────────────────────
  function toggle(id: string) {
    sel = sel.includes(id) ? sel.filter(y => y !== id) : [...sel, id];
  }

  function toggleTopic(topicId: string) {
    const topic = subjectTopics.find(t => t.id === topicId);
    if (!topic) return;
    const ids = topic.sources.map(s => s.id);
    const allOn = ids.every(i => sel.includes(i));
    sel = allOn ? sel.filter(i => !ids.includes(i)) : Array.from(new Set([...sel, ...ids]));
  }

  function setCount(n: number) {
    if (!countLimit) return;
    const v = Math.max(countLimit.min, Math.min(countLimit.max, Math.round(n) || countLimit.def));
    if (type === "flashcards") cardCount = v;
    else if (type === "quiz") quizCount = v;
  }

  function generate() {
    const sub = app.activeSubject;
    if (!sub) {
      app.pushToast({ kind: "error", title: "No active subject", body: "Select a subject first." });
      return;
    }

    // Plugin preset: run the built-in base_kind, steering it with the plugin's
    // prompt (+ whatever the user typed on top).
    const plugin = activePlugin;
    const kind = (plugin?.base_kind ?? type) as JobKind;
    const steerPrompt = [plugin?.prompt, customPrompt.trim()].filter(Boolean).join("\n\n");
    const subjectId = sub.id;
    const topicId = dominantTopicId;
    const matTitle = finalTitle || undefined;
    const sourceIds = [...sel];
    const count = countValue ?? undefined;

    jobs.start({
      kind,
      label: finalTitle || tm?.label || type,
      subjectId,
      topicId,
      run: () =>
        api.generateMaterial(
          subjectId,
          kind as "flashcards" | "quiz" | "audio" | "infographic" | "slideshow" | "mindmap" | "matrix" | "guide",
          topicId,
          matTitle,
          steerPrompt || undefined,
          sourceIds,
          count,
        ),
    });

    app.setView("subject");
    app.setTab("materials");
  }

  function cancel() {
    app.setView("subject");
    app.setTab("materials");
  }

  // ── 2.7: slides two-round confirm ──────────────────────────────
  // Round 1: one call → editable outline (nothing saved). The user edits,
  // then round 2 renders one focused LLM call per slide via the jobs system.
  function steerText(): string {
    return [activePlugin?.prompt, customPrompt.trim()].filter(Boolean).join("\n\n");
  }

  async function planOutline() {
    const sub = app.activeSubject;
    if (!sub || outlineBusy) return;
    outlineBusy = true;
    try {
      const r = await api.slidesOutline(sub.id, dominantTopicId, steerText() || undefined, [...sel]);
      outline = r.slides;
      if (!title.trim()) title = finalTitle || r.suggested_title;
    } catch (e) {
      app.pushToast({ kind: "error", title: "大纲生成失败", body: String(e) });
    } finally {
      outlineBusy = false;
    }
  }

  function renderDeck() {
    const sub = app.activeSubject;
    if (!sub || !outline?.length) return;
    const subjectId = sub.id;
    const plan = outline;
    renderProgress = null;
    jobs.start({
      kind: "slideshow",
      label: `渲染 ${plan.length} 页 · ${finalTitle || type}`,
      subjectId,
      topicId: dominantTopicId,
      // Stay on this view so the per-page progress line is visible; jump on done.
      run: () =>
        api.renderSlides(subjectId, plan, finalTitle || undefined, dominantTopicId, steerText() || undefined),
      onDone: () => {
        outline = null;
        app.setView("subject");
        app.setTab("materials");
      },
    });
  }

  function moveSlide(i: number, d: number) {
    if (!outline) return;
    const j = i + d;
    if (j < 0 || j >= outline.length) return;
    const arr = [...outline];
    [arr[i], arr[j]] = [arr[j], arr[i]];
    outline = arr;
  }
  function removeSlide(i: number) {
    if (outline) outline = outline.filter((_, k) => k !== i);
  }
  function addSlide() {
    outline = [...(outline ?? []), { title: "", focus: "" }];
  }
</script>

<!-- ── Side-by-side launcher (mirrors AddSource: header · two-column · sticky foot,
     no page scroll — only the long source list scrolls inside its panel). ── -->
<div class="genmat2">
  <!-- Header -->
  <div class="gm2-head">
    <button class="btn btn--icon btn--sm btn--ghost" onclick={cancel} title="Back">
      <span style="display:inline-flex;transform:rotate(180deg)"><Icon name="chevron" size={14} /></span>
    </button>
    <div>
      <div class="eyebrow">生成学习材料</div>
      <h1 class="addpage-title">新建学习材料</h1>
      <div class="mono faint" style="font-size: var(--t-xs)">
        from {app.activeSubject?.name ?? "your subject"} · pick a format and sources
      </div>
    </div>
  </div>

  {#if outline}
  <!-- Round 1.5 — the editable deck outline. Render runs ONE focused call per
       slide, so a sharper "focus" line here directly buys a better page. -->
  <div class="gm2-outline">
    <div class="gm2-outline-head">
      <div>
        <div class="eyebrow">第 1 / 2 轮 · 大纲已生成</div>
        <h2 class="ol-h">确认或改写每一页</h2>
        <p class="mono faint ol-sub">渲染时每页单独调用模型——把 focus 写具体，那一页就更准。</p>
      </div>
      <button class="btn btn--sm" onclick={addSlide}><Icon name="plus" size={12} /> 添加一页</button>
    </div>
    <div class="gm2-panel ol-list">
      {#each outline as s, i}
        <div class="ol-row">
          <span class="ol-idx mono">{i + 1}</span>
          <div class="ol-fields">
            <input class="input ol-title" bind:value={s.title} placeholder="页标题" />
            <input class="input ol-focus" bind:value={s.focus} placeholder="这一页要讲透什么（一句话）" />
          </div>
          <div class="ol-ops">
            <button class="btn btn--icon btn--sm btn--ghost" disabled={i === 0} onclick={() => moveSlide(i, -1)} title="上移">
              <span style="display:inline-flex;transform:rotate(-90deg)"><Icon name="chevron" size={12} /></span>
            </button>
            <button class="btn btn--icon btn--sm btn--ghost" disabled={i === outline.length - 1} onclick={() => moveSlide(i, 1)} title="下移">
              <span style="display:inline-flex;transform:rotate(90deg)"><Icon name="chevron" size={12} /></span>
            </button>
            <button class="btn btn--icon btn--sm btn--ghost" onclick={() => removeSlide(i)} title="删除本页">
              <Icon name="x" size={12} />
            </button>
          </div>
        </div>
      {/each}
    </div>
  </div>
  {:else}
  <div class="gm2-grid">
    <!-- LEFT — format, count, details -->
    <div class="gm2-left">
      <div class="gm2-block">
        <div class="onb-label mono">格式</div>
        <div class="gm2-formats">
          {#each allTypes as t (t.id)}
            <button class="gm2-format{type === t.id ? ' on' : ''}" onclick={() => (type = t.id)}>
              <span class="gm2-format-ico" style:color={t.color}><Icon name={t.ico} size={16} /></span>
              <span class="gm2-format-txt">
                <span class="gm2-format-label">{t.label}</span>
                <span class="gm2-format-desc mono">{t.desc}</span>
              </span>
              {#if type === t.id}<Icon name="check" size={13} color="var(--accent)" />{/if}
            </button>
          {/each}
        </div>
      </div>

      {#if countLimit && countValue !== null}
        <div class="gm2-block">
          <div class="onb-label mono">{type === "quiz" ? "QUESTIONS" : "CARDS"}</div>
          <div class="gm2-count">
            <div class="gm2-step">
              <button class="btn btn--icon btn--sm" onclick={() => setCount(countValue - 1)} aria-label="fewer" disabled={countValue <= countLimit.min}>−</button>
              <span class="mono gm2-step-v">{countValue}</span>
              <button class="btn btn--icon btn--sm" onclick={() => setCount(countValue + 1)} aria-label="增加" disabled={countValue >= countLimit.max}>+</button>
            </div>
            <div class="seg gm2-count-presets">
              {#each [Math.round(countLimit.def / 2), countLimit.def, Math.min(countLimit.max, countLimit.def * 2)] as p}
                <button class="seg-opt{countValue === p ? ' on' : ''}" onclick={() => setCount(p)}>{p}</button>
              {/each}
            </div>
          </div>
        </div>
      {/if}

      <div class="gm2-block">
        <div class="field">
          <!-- svelte-ignore a11y_label_has_associated_control -->
          <label class="onb-label mono">标题 <span class="gm2-label-hint">自动建议</span></label>
          <input class="input" bind:value={title} placeholder={suggested || "Select sources first…"} />
        </div>
        <div class="field">
          <!-- svelte-ignore a11y_label_has_associated_control -->
          <label class="onb-label mono">自定义要求 <span class="gm2-label-hint">可选</span></label>
          <textarea
            class="input set-textarea"
            bind:value={customPrompt}
            rows="3"
            placeholder={`e.g. “Focus on exam-likely topics”, “Explain like I'm new to ${app.activeSubject?.name ?? "this"}”, “Emphasise dates and order”…`}
          ></textarea>
        </div>
        <div class="gm-autotag">
          <div class="gm-autotag-l">
            <Icon name="lock" size={13} color="var(--fg-faint)" />
            <span class="mono">自动归入主题</span>
          </div>
          {#if autoTopic}
            <div class="gm-autotag-r">
              <span class="topic-tag mono"><Icon name="chevron" size={9} /> {autoTopic}</span>
              {#if multi}<span class="mono faint">spans {topicNames.length} topics</span>{/if}
            </div>
          {:else}
            <span class="mono faint">选择资料后可指定主题</span>
          {/if}
        </div>
      </div>
    </div>

    <!-- RIGHT — source selection (scrolls within the panel) -->
    <div class="gm2-right">
      <div class="gm2-right-head">
        <span class="onb-label mono" style="margin:0">资料</span>
        <span class="faint mono">{sel.length} selected</span>
      </div>
      <div class="gm2-panel">
        {#if subjectTopics.length === 0}
          <p class="mono faint" style="font-size: var(--t-sm); padding: 8px;">这个科目还没有资料，请先添加资料。</p>
        {:else}
          <div class="gm-sources">
            {#each subjectTopics as topic (topic.id)}
              {@const ids    = topic.sources.map(s => s.id)}
              {@const allOn  = ids.length > 0 && ids.every(i => sel.includes(i))}
              {@const someOn = ids.some(i => sel.includes(i))}
              <div class="gm-topic">
                <div class="gm-topic-h">
                  <button class="gm-check{allOn ? ' on' : someOn ? ' some' : ''}" onclick={() => toggleTopic(topic.id)}>
                    {#if allOn}<Icon name="check" size={11} />{:else if someOn}<span class="gm-dash"></span>{/if}
                  </button>
                  <span class="gm-topic-name mono">{topic.name}</span>
                  <span class="faint mono">{topic.sources.length}</span>
                </div>
                <div class="gm-src-list">
                  {#each topic.sources as s (s.id)}
                    {@const on = sel.includes(s.id)}
                    <button class="gm-src{on ? ' on' : ''}" onclick={() => toggle(s.id)}>
                      <span class="gm-check{on ? ' on' : ''}">
                        {#if on}<Icon name="check" size={11} />{/if}
                      </span>
                      <span class="badge badge--{s.kind === 'audio' ? 'audio' : s.kind}">
                        <span class="dot"></span>{srcLabel[s.kind] ?? s.kind.toUpperCase()}
                      </span>
                      <span class="gm-src-name mono">{s.name}</span>
                      <span class="gm-src-meta mono faint">{s.meta ?? ""}</span>
                    </button>
                  {/each}
                </div>
              </div>
            {/each}
          </div>
        {/if}
      </div>
    </div>
  </div>
  {/if}

  <!-- Sticky footer — always visible, no scrolling to reach Generate -->
  <div class="add-foot gm2-foot">
    <button class="btn btn--ghost" onclick={outline ? () => (outline = null) : cancel}>
      {outline ? "← 返回配置" : "取消"}
    </button>
    {#if outline}
      <div class="gm2-foot-r">
        {#if renderProgress}
          <span class="mono faint gm2-prog">第 {renderProgress.idx}/{renderProgress.total} 页 · {renderProgress.title}</span>
        {/if}
        <button class="btn" disabled={outlineBusy} onclick={planOutline}>
          <Icon name="refresh" size={12} /> 重新出大纲
        </button>
        <button class="btn btn--primary" disabled={outline.length === 0 || !app.activeSubject} onclick={renderDeck}>
          <Icon name="bolt" size={13} /> 第二步 · 渲染 {outline.length} 页
        </button>
      </div>
    {:else}
      <button class="btn btn--primary" disabled={!ready || outlineBusy}
              onclick={type === "slideshow" && !activePlugin ? planOutline : generate}>
        <Icon name="bolt" size={13} />
        {outlineBusy ? "出大纲中…" : type === "slideshow" && !activePlugin ? "第一步 · 生成大纲" : `生成${tm?.label ?? type}`}
      </button>
    {/if}
  </div>
</div>

<style>
  .genmat2 {
    height: 100%;
    display: flex;
    flex-direction: column;
    padding: clamp(16px, 3vh, 30px) clamp(20px, 4vw, 52px);
    gap: 16px;
    overflow: hidden;
  }
  .gm2-head { display: flex; align-items: center; gap: 12px; flex: 0 0 auto; }

  .gm2-grid {
    flex: 1 1 auto;
    min-height: 0;
    display: grid;
    grid-template-columns: minmax(300px, 380px) 1fr;
    gap: clamp(18px, 3vw, 38px);
    align-items: stretch;
  }
  .gm2-left { display: flex; flex-direction: column; gap: 16px; min-height: 0; overflow-y: auto; padding-right: 4px; }
  .gm2-block { display: flex; flex-direction: column; gap: 8px; }

  /* format picker — vertical rows */
  .gm2-formats { display: flex; flex-direction: column; gap: 6px; }
  .gm2-format {
    display: flex; align-items: center; gap: 11px;
    padding: 9px 11px; text-align: left; cursor: pointer;
    background: var(--surface); border: 1px solid var(--border); border-radius: var(--rad-3);
    color: var(--fg-muted); transition: border-color var(--dur-fast), background var(--dur-fast);
  }
  .gm2-format:hover { border-color: var(--border-strong); color: var(--fg-bright); }
  .gm2-format.on { border-color: var(--accent-dim); background: color-mix(in oklab, var(--accent) 8%, var(--surface)); color: var(--fg-bright); }
  .gm2-format-ico { display: grid; place-items: center; width: 20px; flex: none; }
  .gm2-format-txt { display: flex; flex-direction: column; gap: 1px; flex: 1; min-width: 0; }
  .gm2-format-label { font-size: var(--t-sm); font-weight: 600; }
  .gm2-format-desc { font-size: var(--t-2xs); color: var(--fg-faint); }

  /* count stepper — mirrors the ExamView / Settings pomodoro stepper, with the
     7/14/28 quick-picks as a .seg segmented control. */
  .gm2-count { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  .gm2-step { display: flex; align-items: center; gap: 8px; }
  .gm2-step-v { min-width: 36px; text-align: center; color: var(--fg-bright); font-variant-numeric: tabular-nums; }
  .gm2-count-presets { margin-left: auto; }

  /* label qualifier — quietly subordinate to the uppercase mono eyebrow label */
  .gm2-label-hint { text-transform: none; letter-spacing: normal; color: var(--fg-faint); font-weight: 400; }

  /* right column / source panel */
  .gm2-right { display: flex; flex-direction: column; min-height: 0; gap: 8px; }
  .gm2-right-head { display: flex; align-items: center; justify-content: space-between; flex: 0 0 auto; }
  .gm2-panel {
    flex: 1 1 auto; min-height: 0; overflow: auto;
    border: 1px solid var(--border); border-radius: var(--rad-3);
    background: var(--surface); padding: 12px;
  }

  .gm2-foot { flex: 0 0 auto; margin: 0; }

  /* ---- 2.7: outline editor (two-round confirm) ---- */
  .gm2-outline { flex: 1 1 auto; min-height: 0; display: flex; flex-direction: column; gap: 12px; }
  .gm2-outline-head { display: flex; align-items: flex-end; justify-content: space-between; gap: 12px; }
  .ol-h { margin: 2px 0 0; font-size: var(--t-md); }
  .ol-sub { margin: 4px 0 0; font-size: var(--t-xs); }
  .ol-list { flex: 1 1 auto; overflow: auto; padding: 10px 12px; display: flex; flex-direction: column; gap: 8px; }
  .ol-row { display: flex; align-items: center; gap: 10px; padding: 8px 10px; border: 1px solid var(--border); border-radius: var(--rad-3, 8px); background: var(--surface-2); }
  .ol-idx { flex: none; width: 22px; text-align: right; color: var(--fg-faint); font-size: 11px; }
  .ol-fields { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 6px; }
  .ol-title { font-weight: 600; }
  .ol-ops { flex: none; display: flex; gap: 4px; }
  .gm2-foot-r { display: flex; align-items: center; gap: 10px; }
  .gm2-prog { font-size: 11px; }

  @media (max-width: 860px) {
    .genmat2 { overflow: auto; }
    .gm2-grid { grid-template-columns: 1fr; }
    .gm2-left { overflow: visible; }
    .gm2-panel { max-height: 50vh; }
  }
</style>
