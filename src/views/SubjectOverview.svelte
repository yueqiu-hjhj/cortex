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
  {/if}
</div>
{/if}

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
</style>
