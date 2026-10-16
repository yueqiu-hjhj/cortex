<script lang="ts">
  // Subject Overview — the KnowOrigin-style "what is the state of this subject,
  // and what should I do next?" panel that lives on the first tab of every
  // subject. The four-step mastery bar follows KnowOrigins review band
  // (Solid / Steady / Shaky / Cold) so students see the same vocabulary as
  // the neural graph.
  import { app } from "../lib/store.svelte";
  import * as api from "../lib/api";
  import Icon from "../components/Icon.svelte";
  import MindMapView from "../components/MindMapView.svelte";
  import { loadGlossary } from "../lib/glossary";

  type Band = "solid" | "steady" | "shaky" | "cold";
  interface NextItem {
    id: string;
    name: string;
    accuracy: number | null;
    reviews: number;
    lapses: number;
    reasons: string[];
  }

  const BANDS: { key: Band; label: string; min: number; color: string }[] = [
    { key: "solid",  label: "Solid",  min: 0.85, color: "var(--ok)" },
    { key: "steady", label: "Steady", min: 0.7,  color: "var(--accent)" },
    { key: "shaky",  label: "Shaky",  min: 0.45, color: "var(--warn)" },
    { key: "cold",   label: "Cold",   min: -1,   color: "var(--fg-faint)" },
  ];

  const subj = $derived(app.activeSubject);

  // Pull topic stats for this subject on subject-switch + on tab open. 90-day
  // window matches the neural graph so the next-up reasoning is consistent
  // with the other surfaces.
  let stats = $state<api.TopicStat[]>([]);
  let loaded = $state(false);
  $effect(() => {
    const id = subj?.id;
    if (!id) { stats = []; loaded = false; return; }
    let cancelled = false;
    stats = [];
    loaded = false;
    api.topicStats(id, 90)
      .then((r) => { if (!cancelled) { stats = r ?? []; loaded = true; } })
      .catch(() => { if (!cancelled) { stats = []; loaded = true; } });
    return () => { cancelled = true; };
  });

  function bandFor(acc: number | null | undefined): Band {
    if (acc == null) return "cold";
    if (acc >= 0.85) return "solid";
    if (acc >= 0.7)  return "steady";
    if (acc >= 0.45) return "shaky";
    return "cold";
  }

  const distribution = $derived.by(() => {
    const buckets: Record<Band, number> = { solid: 0, steady: 0, shaky: 0, cold: 0 };
    const total = stats.length;
    for (const s of stats) buckets[bandFor(s.accuracy)]++;
    return { buckets, total };
  });

  const nextUp = $derived.by(() => {
    const items: NextItem[] = stats.map((s) => {
      const reasons: string[] = [];
      if (s.accuracy == null) reasons.push("还没复习过");
      else if (s.accuracy < 0.7) reasons.push(`正确率 ${Math.round(s.accuracy * 100)}%`);
      if (s.lapses >= 2) reasons.push(`遗忘 ${s.lapses} 次`);
      if (s.reviews === 0) reasons.push("本主题还没有学习记录");
      if (s.cards === 0) reasons.push("还没有抽认卡");
      if (s.avg_stability && s.avg_stability < 7) reasons.push(`稳定度 ${s.avg_stability.toFixed(1)}d`);
      return {
        id: s.topic_id,
        name: s.topic_name,
        accuracy: s.accuracy ?? null,
        reviews: s.reviews,
        lapses: s.lapses,
        reasons,
      };
    });
    const rank: Record<Band, number> = { cold: 0, shaky: 1, steady: 2, solid: 3 };
    return items
      .filter((i) => i.reasons.length > 0)
      .sort((a, b) => {
        const ra = rank[bandFor(a.accuracy)];
        const rb = rank[bandFor(b.accuracy)];
        if (ra !== rb) return ra - rb;
        return b.lapses - a.lapses;
      })
      .slice(0, 5);
  });

  const subjectAccuracy = $derived.by(() => {
    if (!stats.length) return null;
    const withReviews = stats.filter((s) => s.reviews > 0);
    if (!withReviews.length) return null;
    return withReviews.reduce((n, s) => n + s.accuracy, 0) / withReviews.length;
  });

  function openTopic(id: string) {
    const s = subj?.id;
    if (!s) return;
    app.openTopicSheet(s, id);
  }

    // ── 知识框架树（KnowOrigin-style：foldable tree + node-level BKT mastery）──
    // sync_knowledge_graph rebuilds the subject > topic > source hierarchy from
    // the live library. Coverage rolls up from sources; mastery is the BKT
    // estimate at the topic level, falling back to 90-day review accuracy when
    // no observations have been recorded yet (cold start).
    interface TreeNode {
      node: api.KnowledgeNode;
      children: TreeNode[];
      depth: number;
    }
    let knTree = $state<TreeNode[]>([]);
    let knLoading = $state(false);
    let knErr = $state<string | null>(null);
    let knExpanded = $state<Record<string, boolean>>({}); // node id -> open?
    let knSyncNote = $state<string | null>(null);
    const knTopicAccuracy = $derived.by(() => {
      const m: Record<string, number | null> = {};
      for (const s of stats) m[s.topic_id] = s.accuracy ?? null;
      return m;
    });
    $effect(() => {
      const id = subj?.id;
      if (!id) { knTree = []; knErr = null; knLoading = false; return; }
      let cancelled = false;
      knLoading = true;
      knErr = null;
      (async () => {
        try {
          const n = await api.syncKnowledgeGraph(id);
          const nodes = await api.listKnowledgeNodes(id);
          if (cancelled) return;
          knTree = buildTree(nodes);
          // Auto-expand the subject root + topics with sources, collapse the rest.
          const exp: Record<string, boolean> = {};
          for (const t of knTree) {
            exp[t.node.id] = true;
            for (const c of t.children) exp[c.node.id] = (c.children.length > 0);
          }
          knExpanded = exp;
          knSyncNote = `${n} 个节点已同步`;
        } catch (e) {
          if (!cancelled) knErr = String(e);
        } finally {
          if (!cancelled) knLoading = false;
        }
      })();
      return () => { cancelled = true; };
    });
    function buildTree(nodes: api.KnowledgeNode[]): TreeNode[] {
      const byId = new Map<string, TreeNode>();
      for (const n of nodes) byId.set(n.id, { node: n, children: [], depth: 0 });
      const roots: TreeNode[] = [];
      for (const n of nodes) {
        const t = byId.get(n.id)!;
        if (n.parent_id && byId.has(n.parent_id)) {
          const p = byId.get(n.parent_id)!;
          t.depth = p.depth + 1;
          p.children.push(t);
        } else {
          roots.push(t);
        }
      }
      // Sort siblings by position (then title for stable display).
      const sortRec = (list: TreeNode[]) => {
        list.sort((a, b) => a.node.position - b.node.position || a.node.title.localeCompare(b.node.title));
        for (const t of list) sortRec(t.children);
      };
      sortRec(roots);
      return roots;
    }
    // Effective mastery: BKT (knowledge_nodes.mastery) for nodes that have
    // observations; otherwise the 90-day topic accuracy, otherwise the rolled-up
    // coverage. This keeps the visual meaningful across cold start.
    function effectiveMastery(n: api.KnowledgeNode): number {
      if (n.observations > 0) return n.mastery;
      if (n.kind === "topic" && n.topic_id && knTopicAccuracy[n.topic_id] != null) {
        return knTopicAccuracy[n.topic_id]!;
      }
      return n.coverage > 0 ? 0.5 + n.coverage * 0.4 : 0.3;
    }
    function bktBandFor(m: number): Band {
      if (m >= 0.85) return "solid";
      if (m >= 0.7)  return "steady";
      if (m >= 0.45) return "shaky";
      return "cold";
    }
    function bktColor(b: Band) {
      return BANDS.find((x) => x.key === b)?.color ?? "var(--fg-faint)";
    }
    function openSourceNode(n: api.KnowledgeNode) {
      if (!n.source_id) return;
      const s = subj;
      if (!s) return;
      const src: api.Source = {
        id: n.source_id,
        subject_id: s.id,
        topic_id: n.topic_id ?? null,
        name: n.title,
        kind: "source",
        status: n.coverage > 0 ? "ready" : "pending",
        meta: n.summary ?? null,
        origin: null,
        error: null,
        content: null,
        stored_path: null,
        tags: [],
        created_at: n.created_at,
        updated_at: n.updated_at,
      };
      app.openSource(src, null);
    }
    function toggleNode(id: string) { knExpanded = { ...knExpanded, [id]: !knExpanded[id] }; }

    // ── calibration panel (BKT confidence vs actual accuracy) ──
    // KnowOrigin-style: per-bucket predicted vs actual, with Brier score for
    // an at-a-glance calibration quality. The backend already bins observations
    // into 0.2-wide confidence brackets and computes the (accuracy - mean_conf)
    // gap per bucket. This panel reads that summary and renders the gap bars.
    let knCalib = $state<api.CalibrationSummary | null>(null);
    let knCalibLoading = $state(false);
    $effect(() => {
      const id = subj?.id;
      if (!id) { knCalib = null; knCalibLoading = false; return; }
      let cancelled = false;
      knCalibLoading = true;
      api.calibrationSummary(id)
        .then((c) => { if (!cancelled) knCalib = c ?? null; })
        .catch(() => { if (!cancelled) knCalib = null; })
        .finally(() => { if (!cancelled) knCalibLoading = false; });
      return () => { cancelled = true; };
    });
    // Brier ← 0 is well-calibrated; >0.25 is meaningfully off. We surface a
    // 3-bucket label (well / fair / off) so the headline stays scannable.
    function calibLabel(brier: number, n: number): { txt: string; col: string } {
      if (n === 0) return { txt: "no data", col: "var(--fg-faint)" };
      if (brier < 0.1) return { txt: "well-calibrated", col: "var(--ok)" };
      if (brier < 0.25) return { txt: "fair", col: "var(--accent)" };
      return { txt: "off", col: "var(--warn)" };
    }

  // ── 知识脑图 ──
  // 从本科目的库结构（主题→来源）+ 本地术语表拼一棵只读树，复用 AI 材料
  // 的 MindMapView 渲染器。零模型调用；与 NeuralGraph 不重叠——那张是
  // 跨科目的掌握度星图，这张是单科目的内容清单。
  interface MapNode { label: string; children?: MapNode[] }
  let showMap = $state(false);
  let mapData = $state<{ central: string; branches: MapNode[] } | null>(null);
  const MAP_MAX_KIDS = 12;
  function kidsOf(labels: string[]): MapNode[] {
    const out = labels.slice(0, MAP_MAX_KIDS).map((l) => ({ label: l }));
    if (labels.length > MAP_MAX_KIDS) out.push({ label: `…还有 ${labels.length - MAP_MAX_KIDS} 个` });
    return out;
  }
  async function openMap() {
    const s = subj;
    if (!s) return;
    const branches: MapNode[] = (s.topics ?? []).map((t) => ({
      label: `${t.glyph ?? "•"} ${t.name}`.trim(),
      children: kidsOf((t.sources ?? []).map((x) => x.name)),
    }));
    try {
      const loose = ((await api.listSources(s.id)) ?? [])
        .filter((x) => !x.topic_id)
        .map((x) => x.name);
      if (loose.length) branches.push({ label: `未归类来源 · ${loose.length}`, children: kidsOf(loose) });
    } catch { /* 库树已足够渲染，未归类来源拿不到就跳过 */ }
    const terms = loadGlossary(s.id);
    if (terms.length) {
      const known = terms.filter((t) => t.status === "known").length;
      branches.push({ label: `术语表 · ${terms.length}（已知 ${known}）`, children: kidsOf(terms.map((t) => t.term)) });
    }
    mapData = { central: `${s.glyph} ${s.name}`.trim(), branches };
    showMap = true;
  }
</script>

{#if showMap && mapData}
  <MindMapView data={mapData} onExit={() => (showMap = false)} />
{:else}
<div class="ov">
  {#if !subj}
    <div class="ov-empty">
      <Icon name="book" size={18} />
      <p>选一个科目看它的掌握度概览。</p>
    </div>
  {:else}
    <header class="ov-head">
      <div class="ov-head-l">
        <span class="ov-head-glyph">{subj.glyph}</span>
        <div>
          <h2>{subj.name}</h2>
          <p>
            {subj.topics.length} 个主题 · {subj.sourceCount} 个来源
            {#if subjectAccuracy != null} · 整体正确率 <b>{Math.round(subjectAccuracy * 100)}%</b>{/if}
          </p>
        </div>
      </div>
      <div class="ov-head-r">
        {#if loaded}
          <span class="ov-head-pill">过去 90 天</span>
        {/if}
        <button class="btn btn--sm" onclick={openMap} title="把主题、来源、术语表整理成一张知识脑图">
          <Icon name="link" size={13} /> 知识脑图
        </button>
      </div>
    </header>

    <section class="ov-band">
      <div class="ov-band-l">
        <h3>掌握度</h3>
        <p>
          {#if distribution.total === 0}
            还没有复习数据。先做几张抽认卡，自动归类。
          {:else}
            按最近 90 天的正确率划分 — Solid 85% 以上、Steady 70%、Shaky 45%、Cold 45% 以下
          {/if}
        </p>
      </div>

      <div class="ov-band-bar" role="img" aria-label="掌握度分布">
        {#if distribution.total > 0}
          {#each BANDS as b}
            {@const n = distribution.buckets[b.key]}
            {@const pct = (n / distribution.total) * 100}
            <div
              class="ov-band-seg seg-{b.key}"
              style:width="{pct}%"
              style:opacity={n === 0 ? 0.18 : 1}
              title="{b.label}: {n} 个主题"
            >
              {#if pct >= 8}<span>{n}</span>{/if}
            </div>
          {/each}
        {:else}
          <div class="ov-band-empty">—</div>
        {/if}
      </div>

      <div class="ov-band-leg">
        {#each BANDS as b}
          <span class="ov-leg">
            <i class="leg-i leg-{b.key}" style:opacity={distribution.buckets[b.key] === 0 ? 0.25 : 1}></i>
            <span class="ov-leg-l">{b.label}</span>
            <span class="ov-leg-n">{distribution.buckets[b.key]}</span>
          </span>
        {/each}
      </div>
    </section>

    <section class="ov-next">
      <div class="ov-next-l">
        <h3>下一步学什么</h3>
        <p>
          {#if nextUp.length === 0}
            全部主题都稳了！试着加新主题或换个科目。
          {:else}
            按"为什么"排好的前 {nextUp.length} 个建议 — 点开就是对应 cheatsheet
          {/if}
        </p>
      </div>

      <ol class="ov-next-list">
        {#each nextUp as it, i (it.id)}
          <li class="ov-next-item">
            <span class="ov-next-i">{i + 1}</span>
            <div class="ov-next-body">
              <button class="ov-next-name" onclick={() => openTopic(it.id)} title="打开 cheatsheet">
                {it.name}
              </button>
              <div class="ov-next-meta">
                <span class="ov-next-band band-{bandFor(it.accuracy)}">
                  {BANDS.find((b) => b.key === bandFor(it.accuracy))?.label ?? "—"}
                </span>
                <span class="ov-next-sep">·</span>
                {#each it.reasons as r, ri}
                  <span class="ov-next-reason">
                    {r}{#if ri < it.reasons.length - 1}<span class="ov-next-sep">, </span>{/if}
                  </span>
                {/each}
              </div>
            </div>
            <button class="ov-next-go" onclick={() => openTopic(it.id)} title="开始">
              <Icon name="arrowR" size={13} />
            </button>
          </li>
        {/each}
      </ol>
    </section>

    <section class="ov-tree">
      <div class="ov-tree-l">
        <h3>知识框架树</h3>
        <p>
          本科目的层级树 — 主题 → 来源。颜色按 BKT 掌握的估计值（Solid ≥ 85% ，
          Steady ≥ 70%，Shaky ≥ 45%，Cold 低于）。点叶节点直接打开来源。
        </p>
      </div>
      <div class="ov-tree-card">
        {#if knLoading}
          <div class="ov-tree-mut">同步知识框架…</div>
        {:else if knErr}
          <div class="ov-tree-mut err">{knErr}</div>
        {:else if knTree.length === 0 || (knTree[0]?.children.length ?? 0) === 0}
          <div class="ov-tree-mut">还没有主题。先加几个来源和主题。</div>
        {:else}
          <ul class="ov-tree-list">
            {#each knTree as root (root.node.id)}
              {@render knRow(root)}
            {/each}
          </ul>
          {#if knSyncNote}<div class="ov-tree-foot mono">{knSyncNote}</div>{/if}
        {/if}
      </div>
    </section>

    <section class="ov-calib">
      <div class="ov-calib-l">
        <h3>掌握度校准</h3>
        <p>
          按 BKT 估计的掌握概率（你表示的“能记住”）分桶，
          对比实际正确率。柱子高于 0 线：你不够自信；
          柱子低于 0 线：你过于自信。
        </p>
      </div>
      <div class="ov-calib-card">
        {#if knCalibLoading}
          <div class="ov-calib-mut">加载中…</div>
        {:else if !knCalib || knCalib.observations === 0}
          <div class="ov-calib-mut">
            还没有记录。从抽认卡、测验或复习里报告你的自信度、
            与实际结果，这里会出现校准图。
          </div>
        {:else}
          {@const lbl = calibLabel(knCalib.brier_score, knCalib.observations)}
          <div class="ov-calib-head">
            <div class="ov-calib-stat">
              <span class="ov-calib-stat-n">{knCalib.observations}</span>
              <span class="ov-calib-stat-l">观测</span>
            </div>
            <div class="ov-calib-stat">
              <span class="ov-calib-stat-n">{Math.round(knCalib.accuracy * 100)}%</span>
              <span class="ov-calib-stat-l">实际正确率</span>
            </div>
            <div class="ov-calib-stat">
              <span class="ov-calib-stat-n">{Math.round(knCalib.mean_confidence * 100)}%</span>
              <span class="ov-calib-stat-l">平均自信</span>
            </div>
            <div class="ov-calib-stat">
              <span class="ov-calib-stat-n mono" style:color={lbl.col}>{lbl.txt}</span>
              <span class="ov-calib-stat-l">Brier {knCalib.brier_score.toFixed(3)}</span>
            </div>
          </div>
          <div class="ov-calib-bars" role="img" aria-label="每个自信度桶的实际正确率与预期偏差">
            <div class="ov-calib-zero"></div>
            {#each knCalib.buckets as b}
              <div class="ov-calib-col" title="{b.label}: predicted {Math.round(b.mean_confidence * 100)}%, actual {Math.round(b.accuracy * 100)}% (gap {b.gap >= 0 ? '+' : ''}{(b.gap * 100).toFixed(0)}%)">
                <div class="ov-calib-col-pred mono">{Math.round(b.mean_confidence * 100)}%</div>
                <div class="ov-calib-col-bar">
                  {#if b.observations > 0}
                    <div class="ov-calib-col-actual" style:height="{Math.max(4, Math.round(b.accuracy * 100))}%"></div>
                    <div class="ov-calib-col-gap" style:height="{Math.max(2, Math.abs(b.gap) * 100)}%" class:up={b.gap > 0} class:down={b.gap < 0}></div>
                  {:else}
                    <div class="ov-calib-col-empty"></div>
                  {/if}
                </div>
                <div class="ov-calib-col-lbl mono">{b.label}</div>
                <div class="ov-calib-col-n mono">{b.observations}</div>
              </div>
            {/each}
          </div>
        {/if}
      </div>
    </section>
  {/if}
</div>
{/if}

{#snippet knRow(t: TreeNode)}
  {@const band = bktBandFor(effectiveMastery(t.node))}
  {@const open = !!knExpanded[t.node.id]}
  {@const hasKids = t.children.length > 0}
  <li class="ov-tree-row" class:has-kids={hasKids}>
    <button
      class="ov-tree-row-h"
      type="button"
      onclick={() => hasKids ? toggleNode(t.node.id) : (t.node.source_id ? openSourceNode(t.node) : null)}
    >
      <span class="ov-tree-band" style:background={bktColor(band)} title="BKT mastery band"></span>
      <span class="ov-tree-kind kind-{t.node.kind}">{t.node.kind}</span>
      <span class="ov-tree-title">{t.node.title}</span>
      <span class="ov-tree-m mono" style:color={bktColor(band)}>{Math.round(effectiveMastery(t.node) * 100)}%</span>
      {#if t.node.observations > 0}
        <span class="ov-tree-n mono">{t.node.observations} obs</span>
      {/if}
      {#if hasKids}
        <Icon name={open ? "x" : "arrowR"} size={11} />
      {:else if t.node.source_id}
        <Icon name="arrowR" size={11} />
      {/if}
    </button>
    {#if hasKids && open}
      <ul class="ov-tree-list">
        {#each t.children as child (child.node.id)}
          {@render knRow(child)}
        {/each}
      </ul>
    {/if}
  </li>
{/snippet}

<style>
  .ov {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 22px 26px 32px;
    display: flex;
    flex-direction: column;
    gap: 14px;
    background: transparent;
  }
  .ov-empty {
    margin: auto;
    text-align: center;
    color: var(--fg-faint);
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .ov-empty p { margin: 8px 0 0; }

  .ov-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    border-bottom: 1px solid var(--border);
    padding: 0 0 14px;
  }
  .ov-head-l { display: flex; align-items: center; gap: 12px; }
  .ov-head-glyph {
    width: 40px; height: 40px;
    border-radius: 10px;
    display: grid; place-items: center;
    background: var(--surface-2);
    color: var(--fg-bright);
    font-family: var(--font-mono);
    font-size: 19px;
  }
  .ov-head h2 {
    margin: 0;
    font-family: var(--font-display);
    font-size: 18px;
    font-weight: 600;
    letter-spacing: -0.01em;
    color: var(--fg-bright);
  }
  .ov-head p {
    margin: 3px 0 0;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-muted);
  }
  .ov-head p b { color: var(--fg-bright); }
  .ov-head-pill {
    font-family: var(--font-mono);
    font-size: 10.5px;
    padding: 3px 9px;
    border: 1px solid var(--border);
    border-radius: 999px;
    color: var(--fg-muted);
  }

  .ov-band, .ov-next {
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 16px 18px;
    background: var(--surface);
  }
  .ov-band-l h3, .ov-next-l h3 {
    margin: 0;
    font-family: var(--font-display);
    font-size: 13px;
    font-weight: 600;
    letter-spacing: 0.01em;
    color: var(--fg-bright);
  }
  .ov-band-l p, .ov-next-l p {
    margin: 4px 0 12px;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--fg-faint);
  }

  .ov-band-bar {
    display: flex;
    height: 28px;
    border-radius: 8px;
    overflow: hidden;
    background: var(--surface-2);
  }
  .ov-band-seg {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    min-width: 0;
    transition: opacity 200ms var(--ease);
  }
  .ov-band-seg span { font-family: var(--font-mono); font-size: 11.5px; font-weight: 600; color: var(--bg); }
  .seg-solid  { background: var(--ok); }
  .seg-steady { background: var(--accent); }
  .seg-shaky  { background: var(--warn); }
  .seg-cold   { background: var(--fg-faint); }
  .ov-band-empty {
    width: 100%;
    display: grid; place-items: center;
    font-family: var(--font-mono);
    color: var(--fg-faint);
    background: var(--surface-2);
  }

  .ov-band-leg {
    display: flex;
    flex-wrap: wrap;
    gap: 12px 18px;
    margin-top: 12px;
  }
  .ov-leg { display: inline-flex; align-items: center; gap: 6px; font-family: var(--font-mono); font-size: 10.5px; }
  .leg-i { width: 8px; height: 8px; border-radius: 50%; display: inline-block; }
  .leg-solid  { background: var(--ok); }
  .leg-steady { background: var(--accent); }
  .leg-shaky  { background: var(--warn); }
  .leg-cold   { background: var(--fg-faint); }
  .ov-leg-l { color: var(--fg-muted); }
  .ov-leg-n { color: var(--fg-faint); }

  .ov-next-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .ov-next-item {
    display: flex;
    align-items: center;
    gap: 12px;
    border: 1px solid var(--border);
    border-radius: 9px;
    padding: 9px 12px;
    transition: border-color var(--dur-fast) var(--ease), background var(--dur-fast) var(--ease);
  }
  .ov-next-item:hover { border-color: var(--border-strong); background: var(--surface-2); }
  .ov-next-i {
    width: 22px; height: 22px;
    border-radius: 50%;
    display: grid; place-items: center;
    font-family: var(--font-mono);
    font-size: 10.5px;
    background: var(--surface-2);
    color: var(--fg-muted);
    flex: none;
  }
  .ov-next-body { flex: 1; min-width: 0; }
  .ov-next-name {
    background: transparent;
    border: 0;
    padding: 0;
    font-family: var(--font-display);
    font-size: 13px;
    font-weight: 500;
    color: var(--fg-bright);
    cursor: pointer;
    text-align: left;
  }
  .ov-next-name:hover { color: var(--accent); }
  .ov-next-meta {
    margin-top: 3px;
    display: flex;
    flex-wrap: wrap;
    gap: 4px 6px;
    font-family: var(--font-mono);
    font-size: 10.5px;
  }
  .ov-next-band { font-weight: 600; }
  .band-solid  { color: var(--ok); }
  .band-steady { color: var(--accent); }
  .band-shaky  { color: var(--warn); }
  .band-cold   { color: var(--fg-faint); }
  .ov-next-sep { color: var(--fg-faint); padding: 0 2px; }
  .ov-next-reason { color: var(--fg-muted); white-space: normal; }
  .ov-next-go {
    background: transparent;
    border: 0;
    padding: 4px;
    cursor: pointer;
    border-radius: 6px;
    color: var(--accent);
  }
  .ov-next-go:hover { background: var(--surface-2); }

  /* ---- knowledge framework tree ---- */
  .ov-tree {
    display: grid;
    grid-template-columns: 220px 1fr;
    gap: 14px;
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 14px 16px 16px;
    background: transparent;
  }
  .ov-tree-l h3 { margin: 0 0 4px; font-size: 13px; }
  .ov-tree-l p { margin: 0; color: var(--fg-muted); font-size: 11.5px; line-height: 1.55; }
  .ov-tree-card {
    min-width: 0;
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: 9px;
    padding: 8px 6px;
  }
  .ov-tree-mut {
    padding: 14px 12px;
    text-align: center;
    color: var(--fg-faint);
    font-family: var(--font-mono);
    font-size: 11.5px;
  }
  .ov-tree-mut.err { color: var(--warn); }
  .ov-tree-foot {
    padding: 4px 12px 6px;
    color: var(--fg-faint);
    font-size: 10.5px;
  }
  .ov-tree-list { list-style: none; margin: 0; padding: 0; }
  .ov-tree-row { margin: 0; }
  .ov-tree-row-h {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 5px 10px;
    background: transparent;
    border: 0;
    border-radius: 6px;
    color: var(--fg);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .ov-tree-row-h:hover { background: var(--surface-2); }
  .ov-tree-row.has-kids > .ov-tree-row-h { font-weight: 500; }
  .ov-tree-band {
    width: 4px; height: 18px; border-radius: 2px; flex: none;
  }
  .ov-tree-kind {
    flex: none;
    font-family: var(--font-mono);
    font-size: 9.5px;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--fg-faint);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 1px 4px;
  }
  .ov-tree-kind.kind-subject { color: var(--accent); border-color: var(--accent); }
  .ov-tree-kind.kind-topic   { color: var(--fg-muted); }
  .ov-tree-title {
    flex: 1; min-width: 0;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .ov-tree-m {
    flex: none; font-weight: 600; font-size: 11px;
  }
  .ov-tree-n {
    flex: none; color: var(--fg-faint); font-size: 10px;
  }
  .ov-tree-row > .ov-tree-list {
    margin: 0 0 0 18px;
    border-left: 1px dashed var(--border);
    padding-left: 4px;
  }
  @media (max-width: 760px) {
    .ov-tree { grid-template-columns: 1fr; }
  }

  /* ---- calibration panel ---- */
  .ov-calib {
    display: grid;
    grid-template-columns: 220px 1fr;
    gap: 14px;
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 14px 16px 16px;
    background: transparent;
  }
  .ov-calib-l h3 { margin: 0 0 4px; font-size: 13px; }
  .ov-calib-l p { margin: 0; color: var(--fg-muted); font-size: 11.5px; line-height: 1.55; }
  .ov-calib-card {
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: 9px;
    padding: 12px 14px;
  }
  .ov-calib-mut {
    color: var(--fg-faint);
    text-align: center;
    font-family: var(--font-mono);
    font-size: 11.5px;
    padding: 18px 0;
  }
  .ov-calib-head {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 14px;
    margin-bottom: 14px;
  }
  .ov-calib-stat { display: flex; flex-direction: column; gap: 2px; }
  .ov-calib-stat-n { font-family: var(--font-display); font-size: 20px; color: var(--fg-bright); }
  .ov-calib-stat-l { font-size: 10.5px; color: var(--fg-faint); letter-spacing: 0.04em; }
  .ov-calib-bars {
    position: relative;
    display: grid;
    grid-template-columns: repeat(5, 1fr);
    height: 110px;
    align-items: end;
    padding: 16px 6px 22px;
    border-top: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
    background:
      linear-gradient(180deg,
        transparent 0%, transparent 19%,
        var(--border) 19%, var(--border) 20%,
        transparent 20%, transparent 39%,
        var(--border) 39%, var(--border) 40%,
        transparent 40%, transparent 59%,
        var(--border) 59%, var(--border) 60%,
        transparent 60%, transparent 79%,
        var(--border) 79%, var(--border) 80%,
        transparent 80%);
  }
  .ov-calib-zero {
    position: absolute;
    left: 0; right: 0; bottom: 22px;
    height: 1px;
    background: var(--border-strong);
    pointer-events: none;
  }
  .ov-calib-col { display: flex; flex-direction: column; align-items: center; gap: 4px; }
  .ov-calib-col-pred { font-size: 10px; color: var(--fg-faint); }
  .ov-calib-col-bar {
    position: relative;
    width: 18px;
    height: 100%;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 3px;
    overflow: hidden;
  }
  .ov-calib-col-actual {
    position: absolute;
    left: 0; right: 0; bottom: 0;
    background: var(--accent);
    opacity: 0.65;
  }
  .ov-calib-col-gap {
    position: absolute;
    left: 0; right: 0;
    border-radius: 1px;
  }
  .ov-calib-col-gap.up { bottom: 100%; background: var(--ok); }
  .ov-calib-col-gap.down { top: 100%; background: var(--warn); }
  .ov-calib-col-empty { position: absolute; inset: 0; background: var(--surface-2); }
  .ov-calib-col-lbl { font-size: 10px; color: var(--fg-muted); }
  .ov-calib-col-n { font-size: 10px; color: var(--fg-faint); }
  @media (max-width: 760px) {
    .ov-calib { grid-template-columns: 1fr; }
    .ov-calib-head { grid-template-columns: repeat(2, 1fr); }
  }
</style>
