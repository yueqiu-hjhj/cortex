<script lang="ts">
  // Neural Graph — a star-map view of the whole knowledge base.
  //
  // Data source: app.subjects (Subject > Topic > Source). Mastery colouring
  // comes from analyticsSummary().per_subject (accuracy) and weak_topics.
  //
  // Rendered with @antv/g6 v5 (MIT). Positions/layout are G6's, but every
  // colour is resolved from the live CSS custom properties on <html>, so the
  // graph follows the active Cortex theme instead of hard-coding a palette.
  import { Graph } from "@antv/g6";
  import { app } from "../lib/store.svelte";
  import * as api from "../lib/api";
  import Icon from "../components/Icon.svelte";

  type Kind = "root" | "subject" | "topic";

  interface Sel {
    id: string;
    kind: Kind;
    label: string;
    sub: string;
    color: string;
    subjectId?: string;
    topicId?: string;
    sources?: number;
    topics?: number;
    accuracy?: number | null;
    reviews?: number;
  }

  // ── live theme tokens ──────────────────────────────────────────────────────
  function tokens() {
    const cs = getComputedStyle(document.documentElement);
    const v = (n: string, fb: string) => cs.getPropertyValue(n).trim() || fb;
    return {
      bg: v("--bg", "#0e1813"),
      sunken: v("--bg-sunken", "#0a120e"),
      surface: v("--surface", "#111c18"),
      border: v("--border", "#213429"),
      fg: v("--fg", "#c1c497"),
      bright: v("--fg-bright", "#f6f5dd"),
      muted: v("--fg-muted", "#8a9a7e"),
      faint: v("--fg-faint", "#53685b"),
      accent: v("--accent", "#2dd5b7"),
      ok: v("--ok", "#63b07a"),
      warn: v("--warn", "#e5c736"),
      err: v("--err", "#ff5345"),
      info: v("--info", "#acd4cf"),
    };
  }

  let T = $state(tokens());

  function rgba(hex: string, a: number) {
    const h = hex.replace("#", "").trim();
    const full = h.length === 3 ? h.split("").map((c) => c + c).join("") : h;
    const n = parseInt(full.slice(0, 6), 16);
    if (Number.isNaN(n)) return `rgba(45,213,183,${a})`;
    const r = (n >> 16) & 255,
      g = (n >> 8) & 255,
      b = n & 255;
    return `rgba(${r},${g},${b},${a})`;
  }

  // ── mastery bands (KnowOrigin-style four-step bar, mapped to Cortex tokens) ─
  const BANDS = [
    { min: 0.85, key: "solid", label: "Solid" },
    { min: 0.7, key: "steady", label: "Steady" },
    { min: 0.45, key: "shaky", label: "Shaky" },
    { min: -1, key: "cold", label: "Cold" },
  ] as const;

  function bandColor(acc: number | null | undefined, t: typeof T) {
    if (acc == null) return t.faint;
    if (acc >= 0.85) return t.ok;
    if (acc >= 0.7) return t.accent;
    if (acc >= 0.45) return t.warn;
    return t.err;
  }
  function bandLabel(acc: number | null | undefined) {
    if (acc == null) return "No data";
    return (BANDS.find((b) => acc >= b.min) ?? BANDS[3]).label;
  }

  // ── state ──────────────────────────────────────────────────────────────────
  let host = $state<HTMLDivElement | null>(null);
  let graph: Graph | null = null;
  let mode = $state<"radial" | "force">("radial");
  let labels = $state(true);
  let selected = $state<Sel | null>(null);
  let query = $state("");
  let ready = $state(false);
  let failed = $state<string | null>(null);
  let accuracy = $state<Record<string, number>>({});
  let weak = $state<Record<string, string>>({});
  let counts = $state({ subjects: 0, topics: 0, sources: 0, nodes: 0 });

  // Ambient starfield. Generated once; positions are pure decoration.
  const stars = Array.from({ length: 170 }, (_, i) => {
    const r = (s: number) => ((Math.sin(i * 12.9898 + s * 78.233) * 43758.5453) % 1 + 1) % 1;
    return {
      x: r(1) * 100,
      y: r(2) * 100,
      s: 0.6 + r(3) * 1.7,
      d: r(4) * 6,
      o: 0.14 + r(5) * 0.5,
    };
  });

  const SUBJ_PALETTE = ["#7fbbb3", "#dbbc7f", "#e67e80", "#a7c080", "#d699b6", "#83c092"];

  // ── build G6 data from the store ───────────────────────────────────────────
  function build() {
    const t = T;
    const subs = app.subjects.filter((s) => !s.archived);
    const nodes: any[] = [];
    const edges: any[] = [];
    let nSources = 0,
      nTopics = 0;

    nodes.push({
      id: "__root__",
      data: {
        kind: "root" as Kind,
        label: "Your Knowledge",
        sub: `${subs.length} subjects`,
        size: 74,
        color: t.accent,
      },
    });

    subs.forEach((s, si) => {
      const acc = accuracy[s.id];
      const col = s.color || SUBJ_PALETTE[si % SUBJ_PALETTE.length];
      const size = 20 + Math.min(s.sourceCount, 14) * 2.1;
      nSources += s.sourceCount;
      nTopics += s.topics.length;
      nodes.push({
        id: "s:" + s.id,
        data: {
          kind: "subject" as Kind,
          label: s.name,
          sub: `${s.sourceCount} src · ${s.topics.length} topics`,
          size,
          color: col,
          mastery: bandColor(acc, t),
          glyph: s.glyph,
          subjectId: s.id,
          accuracy: acc ?? null,
          reviews: null,
        },
      });
      edges.push({
        source: "__root__",
        target: "s:" + s.id,
        data: { kind: "spine", color: col, weak: weak[s.id] ? 1 : 0 },
      });

      s.topics.forEach((tp) => {
        const n = tp.sources.length;
        const weakTopic = weak[s.id] === tp.id;
        nodes.push({
          id: "t:" + tp.id,
          data: {
            kind: "topic" as Kind,
            label: tp.name,
            sub: `${n} source${n === 1 ? "" : "s"}`,
            size: 8.5 + Math.min(n, 10) * 1.5,
            color: weakTopic ? t.warn : col,
            mastery: weakTopic ? t.warn : rgba(col, 0.55),
            subjectId: s.id,
            topicId: tp.id,
            weak: weakTopic,
          },
        });
        edges.push({
          source: "s:" + s.id,
          target: "t:" + tp.id,
          data: { kind: "branch", color: col, weak: weakTopic ? 1 : 0 },
        });
      });
    });

    counts = { subjects: subs.length, topics: nTopics, sources: nSources, nodes: nodes.length };
    return { nodes, edges };
  }

  function nodeStyle(d: any) {
    const t = T;
    const kind: Kind = d.data.kind;
    const col = d.data.color;
    if (kind === "root") {
      return {
        size: d.data.size,
        fill: `l(90) 0:${rgba(t.accent, 0.95)} 0.5:${rgba(t.bg, 0.9)} 1:${rgba(t.accent, 0.25)}`,
        stroke: t.accent,
        lineWidth: 1.6,
        shadowColor: rgba(t.accent, 0.75),
        shadowBlur: 52,
        zIndex: 30,
      };
    }
    if (kind === "subject") {
      return {
        size: d.data.size,
        fill: `l(90) 0:${rgba(col, 0.95)} 0.55:${rgba(t.bg, 0.85)} 1:${rgba(col, 0.35)}`,
        stroke: col,
        lineWidth: 1.3,
        shadowColor: rgba(col, 0.55),
        shadowBlur: 26,
        zIndex: 20,
      };
    }
    return {
      size: d.data.size,
      fill: d.data.weak ? rgba(t.warn, 0.85) : rgba(t.bg, 0.92),
      stroke: d.data.weak ? t.warn : rgba(col, 0.8),
      lineWidth: 1,
      shadowColor: rgba(col, 0.4),
      shadowBlur: 12,
      zIndex: 10,
    };
  }

  function edgeStyle(d: any) {
    const t = T;
    const col = d.data.color || t.accent;
    const spine = d.data.kind === "spine";
    return {
      stroke: d.data.weak ? rgba(t.warn, 0.55) : rgba(col, spine ? 0.42 : 0.24),
      lineWidth: spine ? 1.35 : 0.9,
      lineDash: d.data.weak ? [2, 6] : [2, 14],
      lineDashOffset: 0,
      shadowColor: d.data.weak ? rgba(t.warn, 0.5) : rgba(col, 0.3),
      shadowBlur: spine ? 8 : 4,
      endArrow: false,
    };
  }

  function layoutCfg() {
    if (mode === "force") {
      return {
        type: "d3-force",
        link: { distance: (d: any) => (d.data?.kind === "spine" ? 180 : 80), strength: 0.6 },
        manyBody: { strength: -320 },
        collide: { radius: (d: any) => (d.data?.size ?? 20) / 2 + 34, strength: 0.9 },
        alphaDecay: 0.022,
      } as any;
    }
    return {
      type: "radial",
      focusNode: "__root__",
      unitRadius: 240,
      preventOverlap: false,
      nodeSize: 60,
      maxIteration: 200,
      sortBy: "data.size",
    } as any;
  }

  function makeGraph() {
    if (!host) return;
    const data = build();
    const t = T;
    graph = new Graph({
      container: host,
      autoResize: true,
      padding: 90,
      background: "transparent",
      data,
      layout: layoutCfg(),
      node: {
        type: "circle",
        style: {
          size: (d: any) => nodeStyle(d).size,
          fill: (d: any) => nodeStyle(d).fill,
          stroke: (d: any) => nodeStyle(d).stroke,
          lineWidth: (d: any) => nodeStyle(d).lineWidth,
          shadowColor: (d: any) => nodeStyle(d).shadowColor,
          shadowBlur: (d: any) => nodeStyle(d).shadowBlur,
          shadowOffsetX: 0,
          shadowOffsetY: 0,
          zIndex: (d: any) => nodeStyle(d).zIndex,
          cursor: "pointer",
          labelText: (d: any) => (labels && d.data.kind !== "root" ? d.data.label : ""),
          labelFill: (d: any) => (d.data.kind === "topic" ? t.muted : t.bright),
          labelFontSize: (d: any) => (d.data.kind === "subject" ? 11.5 : 10),
          labelFontFamily: "JetBrains Mono, ui-monospace, monospace",
          labelPlacement: "bottom",
          labelOffsetY: 5,
          labelBackground: true,
          labelBackgroundFill: rgba(t.sunken, 0.82),
          labelBackgroundStroke: rgba(t.border, 0.9),
          labelBackgroundLineWidth: 1,
          labelBackgroundRadius: 4,
          labelPadding: [2, 6],
          labelMaxWidth: 132,
          labelWordWrap: true,
        },
        state: {
          hover: {
            lineWidth: 2,
            shadowBlur: 40,
          },
          selected: {
            lineWidth: 2.6,
            shadowBlur: 46,
          },
        },
      },
      edge: {
        type: "line",
        style: {
          stroke: (d: any) => edgeStyle(d).stroke,
          lineWidth: (d: any) => edgeStyle(d).lineWidth,
          lineDash: (d: any) => edgeStyle(d).lineDash,
          lineDashOffset: 0,
          shadowColor: (d: any) => edgeStyle(d).shadowColor,
          shadowBlur: (d: any) => edgeStyle(d).shadowBlur,
          endArrow: false,
          zIndex: 1,
        },
        // Traveling light along each edge: an infinite keyframe on the dash
        // offset reads as particles flowing from the core out to the leaves.
        animation: {
          enter: [
            {
              fields: ["lineDashOffset"],
              duration: 2600,
              iterations: Infinity,
              easing: "linear",
            },
          ],
        } as any,
      },
      behaviors: ["drag-canvas", "zoom-canvas", "drag-element", "hover-activate", "click-select"],
      plugins: [{ type: "tooltip", key: "tip" } as any],
    });

    graph.on("node:click", (e: any) => {
      const id: string = e.target?.id ?? e.item?.id ?? "";
      pick(id);
    });
    graph.on("node:dblclick", (e: any) => {
      const id: string = e.target?.id ?? e.item?.id ?? "";
      openNode(id);
    });
    graph.on("canvas:click", () => (selected = null));

    void graph.render();
    return undefined;
  }

  function findNode(id: string): any {
    const d: any = graph?.getData();
    return d?.nodes?.find((n: any) => n.id === id);
  }

  function pick(id: string) {
    const n = findNode(id);
    if (!n) return;
    selected = {
      id,
      kind: n.data.kind,
      label: n.data.label,
      sub: n.data.sub,
      color: n.data.color,
      subjectId: n.data.subjectId,
      topicId: n.data.topicId,
      sources: n.data.sources,
      topics: n.data.topics,
      accuracy: n.data.accuracy ?? null,
      reviews: n.data.reviews ?? null,
    };
  }

  function openNode(id: string) {
    const n = findNode(id);
    if (!n) return;
    const s = n.data.subjectId;
    if (!s) return;
    if (n.data.topicId) app.openTopicSheet(s, n.data.topicId);
    else app.openSubject(s);
  }

  function refit() {
    const g: any = graph;
    if (!g) return;
    try {
      g.fitView?.([80, 80, 80, 80]);
      g.fitCenter?.();
    } catch {
      /* zoom APIs vary by minor version — a failed fit is harmless */
    }
  }

  // Re-layout when the user flips radial/force or the data actually changes.
  let sig = $derived(
    app.subjects
      .map((s) => `${s.id}:${s.sourceCount}:${s.topics.length}`)
      .join("|") + `#${mode}`
  );
  $effect(() => {
    sig;
    if (!graph) return;
    try {
      (graph as any).setOptions?.({ layout: layoutCfg() });
      (graph as any).setData?.(build());
      (graph as any).render?.();
      refit();
    } catch (e) {
      failed = String(e);
    }
  });

  // Labels toggle without a re-layout.
  $effect(() => {
    labels;
    if (graph) void (graph as any).draw?.();
  });

  $effect(() => {
    const q = query.trim().toLowerCase();
    if (!graph) return;
    const d: any = (graph as any).getData?.();
    const hits = (d?.nodes ?? []).filter(
      (n: any) => n.data.kind !== "root" && n.data.label?.toLowerCase().includes(q)
    );
    (graph as any).setElementState?.(
      (d?.nodes ?? []).map((n: any) => ({ id: n.id, states: q && !hits.includes(n) ? [] : [] }))
    );
    if (q && hits[0]) {
      try {
        (graph as any).focusElement?.(hits[0].id, { duration: 240 });
      } catch {
        /* ignore */
      }
    }
  });

  // ── lifecycle ──────────────────────────────────────────────────────────────
  const matches = $derived.by(() => {
    const q = query.trim().toLowerCase();
    if (!q) return [];
    return (Array.isArray(app.subjects) ? app.subjects : [])
      .flatMap((s) => [
        { id: "s:" + s.id, label: s.name, kind: "subject" },
        ...s.topics.map((t) => ({ id: "t:" + t.id, label: `${t.name}`, kind: "topic" })),
      ])
      .filter((n) => n.label.toLowerCase().includes(q))
      .slice(0, 6);
  });

  $effect(() => { if (host && !graph) { try { makeGraph(); ready = true; setTimeout(() => refit(), 800); setTimeout(() => refit(), 2500); setTimeout(() => refit(), 5500); } catch (e) { failed = String(e); } } });

  // Refresh mastery data + repaint when the theme flips.
  $effect(() => {
    let alive = true;
    void api
      .analyticsSummary(90)
      .then((s) => {
        if (!alive) return;
        const acc: Record<string, number> = {};
        for (const p of s.per_subject) acc[p.subject_id] = p.accuracy;
        const wk: Record<string, string> = {};
        for (const w of s.weak_topics) wk[w.subject_id] = w.topic_id;
        accuracy = acc;
        weak = wk;
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  });

  $effect(() => {
    const obs = new MutationObserver(() => {
      T = tokens();
      if (graph) {
        try {
          (graph as any).setOptions?.({
            node: { style: {} },
          });
          (graph as any).render?.();
        } catch {
          /* ignore */
        }
      }
    });
    obs.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    return () => obs.disconnect();
  });

  $effect(() => {
    return () => {
      try {
        (graph as any)?.destroy?.();
      } catch {
        /* ignore */
      }
      graph = null;
    };
  });

  function masteryBar(acc: number | null | undefined) {
    const a = acc ?? 0;
    const filled = acc == null ? 0 : Math.max(1, Math.round(a * 4));
    return filled;
  }

  // ── knowledge framework (BKT + calibration) ──────────────────────────────
  let knNodes = $state<api.KnowledgeNode[]>([]);
  let knCalib = $state<api.CalibrationSummary | null>(null);
  let knLoading = $state(false);
  let knErr = $state<string | null>(null);

  async function loadKnowledge(subjectId: string) {
    knLoading = true;
    knErr = null;
    try {
      // Make sure the framework exists for this subject, then read it back.
      await api.syncKnowledgeGraph(subjectId);
      const [nodes, calib] = await Promise.all([
        api.listKnowledgeNodes(subjectId),
        api.calibrationSummary(subjectId),
      ]);
      knNodes = nodes;
      knCalib = calib;
    } catch (e: any) {
      knErr = String(e?.message ?? e);
      knNodes = [];
      knCalib = null;
    } finally {
      knLoading = false;
    }
  }

  // Reload the framework data when the user picks a different subject node.
  $effect(() => {
    if (selected?.kind === "subject" && selected.subjectId) {
      void loadKnowledge(selected.subjectId);
    } else if (selected && selected.kind !== "subject") {
      knNodes = [];
      knCalib = null;
    }
  });

  // BKT mastery → Cortex band; mirrors the BANDS used in the legend.
  function bktBand(m: number) {
    if (m >= 0.85) return { txt: "Solid",  col: T.ok };
    if (m >= 0.7)  return { txt: "Steady", col: T.accent };
    if (m >= 0.45) return { txt: "Shaky",  col: T.warn };
    return { txt: "Cold", col: T.err };
  }

  function fmtPct(n: number | null | undefined) {
    if (n == null || Number.isNaN(n)) return "—";
    return Math.round(n * 100) + "%";
  }

  function knAvgMastery() {
    const concept = knNodes.filter((n) => n.kind === "concept" || n.kind === "source");
    if (!concept.length) return null;
    const s = concept.reduce((acc, n) => acc + n.mastery, 0);
    return s / concept.length;
  }

  // Concepts only — subjects/topics/sources each get their own row, so showing
  // them again here would just duplicate the detail panel.
  let knConcepts = $derived(knNodes.filter((n) => n.kind === "concept"));

</script>

<div class="ng">
  <!-- ambient starfield -->
  <div class="ng-stars" aria-hidden="true">
    {#each stars as st}
      <i
        style:left="{st.x}%"
        style:top="{st.y}%"
        style:width="{st.s}px"
        style:height="{st.s}px"
        style:opacity={st.o}
        style:animation-delay="{st.d}s"></i>
    {/each}
  </div>
  <div class="ng-glow" aria-hidden="true"></div>

  <!-- header -->
  <div class="ng-head">
    <div class="ng-title">
      <Icon name="graph" size={15} />
      <div>
        <h2>知识图谱</h2>
        <p>每个模块是一簇知识，连线是它的来源。点击展开，双击直达。</p>
      </div>
    </div>
    <div class="ng-chips">
      <span class="chip"><b>{counts.subjects}</b> 科目</span>
      <span class="chip"><b>{counts.topics}</b> 主题</span>
      <span class="chip"><b>{counts.sources}</b> 资料</span>
      <span class="chip"><b>{Object.keys(accuracy).length}</b> 已跟踪</span>
    </div>
  </div>

  <!-- canvas -->
  <div class="ng-stage">
    <div class="ng-canvas" bind:this={host}></div>

    {#if !ready && !failed}
      <div class="ng-boot">正在生成图谱…</div>
    {/if}
    {#if failed}
      <div class="ng-boot err">graph failed: {failed}</div>
    {/if}
    {#if ready && counts.nodes <= 1}
      <div class="ng-boot">还没有科目。先添加一个科目，图谱就会长出来。</div>
    {/if}
  </div>

  <!-- controls -->
  <div class="ng-ctl">
    <div class="seg">
      <button class:on={mode === "radial"} onclick={() => (mode = "radial")} title="放射布局">
        Radial
      </button>
      <button class:on={mode === "force"} onclick={() => (mode = "force")} title="力导向布局">
        Force
      </button>
    </div>
    <button class="tgl" class:on={labels} onclick={() => (labels = !labels)} title="显示/隐藏标签">
      <Icon name="reader" size={13} /> Labels
    </button>
    <button class="tgl" onclick={refit} title="重置视图"><Icon name="refresh" size={13} /> 适配</button>
    <div class="ng-search">
      <Icon name="search" size={13} />
      <input placeholder="搜索节点…" bind:value={query} />
      {#if query && matches.length}
        <div class="ng-hits">
          {#each matches as m}
            <button
              onclick={() => {
                pick(m.id);
                try {
                  (graph as any)?.focusElement?.(m.id, { duration: 260 });
                } catch {
                  /* ignore */
                }
                query = "";
              }}>
              <span class="k">{m.kind === "subject" ? "SU" : "TP"}</span>{m.label}
            </button>
          {/each}
        </div>
      {/if}
    </div>
  </div>

  <!-- legend -->
  <div class="ng-legend">
    <span class="lg-t">掌握度</span>
    {#each BANDS as b}
      <span class="lg-i"><i style:background={bandColor(b.min === -1 ? 0.3 : b.min + 0.01, T)}></i>{b.label}</span>
    {/each}
    <span class="lg-i dim"><i></i>无数据</span>
  </div>

  <!-- detail panel -->
  {#if selected}
    {@const sel = selected}
    <aside class="ng-side">
      <button class="ng-x" onclick={() => (selected = null)} aria-label="关闭"><Icon name="x" size={13} /></button>
      <div class="ng-side-k">{selected.kind}</div>
      <h3 style:color={selected.color}>{selected.label}</h3>
      <p class="ng-side-sub">{selected.sub}</p>

      {#if selected.kind !== "root"}
        <div class="ng-side-sec">
          <span class="lbl">掌握度</span>
          <div class="mbar">
            {#each [0, 1, 2, 3] as i}
              <i
                class:fill={i < masteryBar(selected.accuracy)}
                style:background={i < masteryBar(selected.accuracy) ? bandColor(selected.accuracy, T) : undefined}
              ></i>
            {/each}
            <span class="mv">{selected.accuracy == null ? bandLabel(null) : Math.round(selected.accuracy * 100) + "%"}</span>
          </div>
        </div>
      {/if}

      <div class="ng-side-acts">
      {#if selected.kind === "subject"}
        <div class="ng-side-sec">
          <span class="lbl">知识框架</span>
          {#if knLoading}
            <div class="kn-mut">正在同步节点与掌握度…</div>
          {:else if knErr}
            <div class="kn-mut err">{knErr}</div>
          {:else}
            {@const avg = knAvgMastery()}
            <div class="kn-summary">
              <div class="kn-stat">
                <span class="kn-stat-n">{knNodes.length}</span>
                <span class="kn-stat-l">节点</span>
              </div>
              <div class="kn-stat">
                <span class="kn-stat-n">{knConcepts.length}</span>
                <span class="kn-stat-l">概念</span>
              </div>
              <div class="kn-stat">
                <span class="kn-stat-n">{fmtPct(avg)}</span>
                <span class="kn-stat-l">平均掌握度</span>
              </div>
            </div>

            {#if knCalib && knCalib.observations > 0}
              <div class="kn-cal">
                <div class="kn-cal-t">
                  <span class="lbl">校准</span>
                  <span class="kn-cal-b">Brier {knCalib.brier_score.toFixed(3)}</span>
                </div>
                {#each knCalib.buckets as b}
                  <div class="kn-bucket">
                    <span class="kn-bk-l">
                      {b.label}
                    </span>
                    <div class="kn-bk-bar">
                      <i style:left="{Math.round(b.mean_confidence * 100)}%" class="conf"></i>
                      <i style:left="{Math.round(b.accuracy * 100)}%" class="acc"></i>
                    </div>
                    <span class="kn-bk-r">
                      <span class="mono">{b.observations > 0 ? Math.round(b.accuracy * 100) + "%" : "—"}</span>
                      {#if b.observations > 0}
                        <span class="kn-gap" class:over={b.gap > 0.05} class:under={b.gap < -0.05}>
                          {b.gap > 0 ? "+" : ""}{Math.round(b.gap * 100)}%
                        </span>
                      {/if}
                    </span>
                  </div>
                {/each}
                <div class="kn-cal-legend">
                  <span><i class="conf"></i> 预测</span>
                  <span><i class="acc"></i> 实际</span>
                </div>
              </div>
            {/if}

            {#if knConcepts.length}
              <div class="kn-concepts">
                <span class="lbl">概念</span>
                {#each knConcepts.slice(0, 6) as c}
                  {@const band = bktBand(c.mastery)}
                  <div class="kn-concept">
                    <span class="kn-c-t">{c.title}</span>
                    <span class="kn-c-m" style:color={band.col}>{fmtPct(c.mastery)}</span>
                  </div>
                {/each}
                {#if knConcepts.length > 6}
                  <div class="kn-more">+{knConcepts.length - 6} more</div>
                {/if}
              </div>
            {/if}

            <button class="kn-refresh" onclick={() => selected?.subjectId && loadKnowledge(selected.subjectId)}>
              <Icon name="refresh" size={11} /> Refresh
            </button>
          {/if}
        </div>
      {/if}

        {#if sel.kind === "subject"}
          <button class="pri" onclick={() => openNode(sel.id)}><Icon name="arrowR" size={13} /> 打开科目</button>
        {:else if sel.kind === "topic"}
          <button class="pri" onclick={() => openNode(sel.id)}><Icon name="arrowR" size={13} /> 打开速查表</button>
        {:else}
          <button class="pri" onclick={() => app.openSubject(app.subjects[0]?.id ?? "")} disabled={!app.subjects.length}>
            <Icon name="arrowR" size={13} /> Start with {app.subjects[0]?.name ?? "a subject"}
          </button>
        {/if}
        <button class="gho" onclick={refit}><Icon name="refresh" size={13} /> 居中</button>
      </div>
    </aside>
  {/if}
</div>

<style>
  .ng {
    position: relative;
    flex: 1;
    min-height: 0;
    overflow: hidden;
    background:
      radial-gradient(120% 90% at 50% 42%, var(--surface) 0%, var(--bg) 46%, var(--bg-sunken) 100%);
  }
  .ng-stars {
    position: absolute;
    inset: 0;
    pointer-events: none;
  }
  .ng-stars i {
    position: absolute;
    border-radius: 50%;
    background: var(--fg-bright);
    animation: twinkle 5.2s var(--ease) infinite;
  }
  @keyframes twinkle {
    0%,
    100% {
      opacity: 0.16;
      transform: scale(0.85);
    }
    50% {
      opacity: 0.75;
      transform: scale(1.15);
    }
  }
  .ng-glow {
    position: absolute;
    inset: 0;
    pointer-events: none;
    background: radial-gradient(42% 42% at 50% 50%, color-mix(in srgb, var(--accent) 16%, transparent), transparent 70%);
  }

  .ng-head {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    z-index: 6;
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    padding: 16px 20px;
    pointer-events: none;
  }
  .ng-title {
    display: flex;
    gap: 10px;
    color: var(--accent);
    pointer-events: auto;
  }
  .ng-title h2 {
    margin: 0;
    font-family: var(--font-mono);
    font-size: 15px;
    letter-spacing: 0.02em;
    color: var(--fg-bright);
  }
  .ng-title p {
    margin: 3px 0 0;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-faint);
    max-width: 420px;
  }
  .ng-chips {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    justify-content: flex-end;
    pointer-events: auto;
  }
  .chip {
    font-family: var(--font-mono);
    font-size: 10.5px;
    color: var(--fg-muted);
    background: color-mix(in srgb, var(--surface-2) 80%, transparent);
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 3px 9px;
  }
  .chip b {
    color: var(--fg-bright);
    font-weight: 600;
    margin-right: 3px;
  }

  .ng-stage {
    position: absolute;
    inset: 0;
  }
  .ng-canvas {
    position: absolute;
    inset: 0;
  }
  .ng-boot {
    position: absolute;
    left: 50%;
    top: 50%;
    transform: translate(-50%, -50%);
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--fg-faint);
  }
  .ng-boot.err {
    color: var(--err);
    max-width: 60ch;
    text-align: center;
  }

  .ng-ctl {
    position: absolute;
    left: 16px;
    bottom: 16px;
    z-index: 6;
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    max-width: calc(100% - 320px);
  }
  .seg {
    display: flex;
    border: 1px solid var(--border);
    border-radius: 7px;
    overflow: hidden;
    background: color-mix(in srgb, var(--surface) 88%, transparent);
  }
  .seg button {
    font-family: var(--font-mono);
    font-size: 11px;
    padding: 5px 11px;
    border: 0;
    background: transparent;
    color: var(--fg-muted);
    cursor: pointer;
  }
  .seg button.on {
    background: color-mix(in srgb, var(--accent) 20%, transparent);
    color: var(--fg-bright);
  }
  .tgl {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-family: var(--font-mono);
    font-size: 11px;
    padding: 5px 10px;
    border: 1px solid var(--border);
    border-radius: 7px;
    background: color-mix(in srgb, var(--surface) 88%, transparent);
    color: var(--fg-muted);
    cursor: pointer;
  }
  .tgl.on {
    color: var(--fg-bright);
    border-color: var(--border-strong);
  }
  .ng-search {
    position: relative;
    display: flex;
    align-items: center;
    gap: 6px;
    border: 1px solid var(--border);
    border-radius: 7px;
    background: color-mix(in srgb, var(--surface) 88%, transparent);
    padding: 4px 9px;
    color: var(--fg-faint);
  }
  .ng-search input {
    border: 0;
    background: transparent;
    outline: none;
    color: var(--fg);
    font-family: var(--font-mono);
    font-size: 11px;
    width: 128px;
  }
  .ng-hits {
    position: absolute;
    bottom: calc(100% + 6px);
    left: 0;
    min-width: 200px;
    display: flex;
    flex-direction: column;
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    border-radius: 8px;
    box-shadow: var(--shadow-pop);
    overflow: hidden;
  }
  .ng-hits button {
    display: flex;
    align-items: center;
    gap: 8px;
    text-align: left;
    border: 0;
    background: transparent;
    color: var(--fg);
    font-family: var(--font-mono);
    font-size: 11px;
    padding: 6px 10px;
    cursor: pointer;
  }
  .ng-hits button:hover {
    background: var(--surface-3);
  }
  .ng-hits .k {
    color: var(--accent);
    font-size: 9px;
  }

  .ng-legend {
    position: absolute;
    right: 16px;
    bottom: 16px;
    z-index: 6;
    display: flex;
    align-items: center;
    gap: 10px;
    font-family: var(--font-mono);
    font-size: 10.5px;
    color: var(--fg-faint);
    background: color-mix(in srgb, var(--surface) 82%, transparent);
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 5px 12px;
  }
  .lg-t {
    color: var(--fg-muted);
  }
  .lg-i {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .lg-i i {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--fg-faint);
    display: inline-block;
  }
  .lg-i.dim {
    opacity: 0.6;
  }

  .ng-side {
    position: absolute;
    right: 16px;
    top: 74px;
    width: 252px;
    z-index: 7;
    background: color-mix(in srgb, var(--surface) 94%, transparent);
    border: 1px solid var(--border-strong);
    border-radius: 10px;
    box-shadow: var(--shadow-pop);
    padding: 14px 15px 15px;
    backdrop-filter: blur(10px);
  }
  .ng-x {
    position: absolute;
    top: 9px;
    right: 9px;
    border: 0;
    background: transparent;
    color: var(--fg-faint);
    cursor: pointer;
    padding: 3px;
  }
  .ng-side-k {
    font-family: var(--font-mono);
    font-size: 9.5px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }
  .ng-side h3 {
    margin: 5px 0 2px;
    font-family: var(--font-mono);
    font-size: 15px;
    color: var(--fg-bright);
    padding-right: 18px;
  }
  .ng-side-sub {
    margin: 0;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-muted);
  }
  .ng-side-sec {
    margin-top: 13px;
  }
  .ng-side-sec .lbl {
    font-family: var(--font-mono);
    font-size: 9.5px;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }
  .mbar {
    display: flex;
    align-items: center;
    gap: 4px;
    margin-top: 6px;
  }
  .mbar i {
    flex: 1;
    height: 6px;
    border-radius: 3px;
    background: var(--surface-3);
  }
  .mbar .mv {
    font-family: var(--font-mono);
    font-size: 10.5px;
    color: var(--fg-muted);
    min-width: 46px;
    text-align: right;
  }
  .ng-side-acts {
    margin-top: 15px;
    display: flex;
    gap: 7px;
    flex-wrap: wrap;
  }
  .ng-side-acts button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-family: var(--font-mono);
    font-size: 11px;
    padding: 6px 11px;
    border-radius: 7px;
    cursor: pointer;
    border: 1px solid var(--border-strong);
    background: var(--surface-2);
    color: var(--fg);
  }
  .ng-side-acts .pri {
    background: color-mix(in srgb, var(--accent) 22%, transparent);
    border-color: color-mix(in srgb, var(--accent) 45%, transparent);
    color: var(--fg-bright);
  }
  .ng-side-acts button:disabled {
    opacity: 0.45;
    cursor: default;
  }
  /* Knowledge framework + calibration panel inside the side detail card. */
  .kn-mut {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-faint);
    padding: 6px 0;
  }
  .kn-mut.err { color: var(--err); }

  .kn-summary {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 6px;
    margin: 7px 0 4px;
  }
  .kn-stat {
    display: flex;
    flex-direction: column;
    align-items: center;
    padding: 7px 4px;
    background: color-mix(in srgb, var(--surface-2) 65%, transparent);
    border: 1px solid var(--border);
    border-radius: 7px;
  }
  .kn-stat-n {
    font-family: var(--font-mono);
    font-size: 13px;
    font-weight: 600;
    color: var(--fg-bright);
  }
  .kn-stat-l {
    font-family: var(--font-mono);
    font-size: 9.5px;
    color: var(--fg-faint);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    margin-top: 2px;
  }

  .kn-cal {
    margin-top: 12px;
    padding-top: 10px;
    border-top: 1px dashed var(--border);
  }
  .kn-cal-t {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    margin-bottom: 6px;
  }
  .kn-cal-b {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-muted);
  }
  .kn-bucket {
    display: grid;
    grid-template-columns: 56px 1fr 64px;
    align-items: center;
    gap: 7px;
    margin: 5px 0;
    font-family: var(--font-mono);
    font-size: 10.5px;
    color: var(--fg-muted);
  }
  .kn-bk-l { font-size: 10px; color: var(--fg-faint); }
  .kn-bk-bar {
    position: relative;
    height: 8px;
    background: color-mix(in srgb, var(--surface) 75%, var(--bg) 25%);
    border-radius: 4px;
    border: 1px solid var(--border);
    overflow: hidden;
  }
  .kn-bk-bar i {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 2px;
    margin-left: -1px;
  }
  .kn-bk-bar i.conf { background: var(--accent); opacity: 0.85; }
  .kn-bk-bar i.acc { background: var(--fg-bright); }
  .kn-bk-r {
    display: flex;
    gap: 5px;
    justify-content: flex-end;
    align-items: baseline;
  }
  .kn-bk-r .mono { color: var(--fg); font-size: 11px; }
  .kn-gap { font-size: 9.5px; padding: 1px 5px; border-radius: 4px; }
  .kn-gap.over { background: color-mix(in srgb, var(--err) 22%, transparent); color: var(--err); }
  .kn-gap.under { background: color-mix(in srgb, var(--info) 22%, transparent); color: var(--info); }
  .kn-cal-legend {
    display: flex;
    gap: 12px;
    margin-top: 7px;
    font-family: var(--font-mono);
    font-size: 9.5px;
    color: var(--fg-faint);
  }
  .kn-cal-legend i {
    display: inline-block;
    width: 8px;
    height: 2px;
    margin-right: 4px;
    vertical-align: middle;
  }
  .kn-cal-legend i.conf { background: var(--accent); }
  .kn-cal-legend i.acc { background: var(--fg-bright); }

  .kn-concepts {
    margin-top: 12px;
    padding-top: 10px;
    border-top: 1px dashed var(--border);
  }
  .kn-concept {
    display: flex;
    justify-content: space-between;
    padding: 3px 0;
    font-size: 11.5px;
    color: var(--fg);
  }
  .kn-c-t {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 170px;
  }
  .kn-c-m { font-family: var(--font-mono); font-size: 10.5px; font-weight: 600; }
  .kn-more {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-faint);
    margin-top: 3px;
  }

  .kn-refresh {
    margin-top: 10px;
    width: 100%;
    display: inline-flex;
    justify-content: center;
    align-items: center;
    gap: 5px;
    padding: 5px;
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-muted);
    background: transparent;
    border: 1px solid var(--border);
    border-radius: 6px;
    cursor: pointer;
  }
  .kn-refresh:hover { color: var(--accent); border-color: var(--accent); }

</style>
