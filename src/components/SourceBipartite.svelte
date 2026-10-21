<script lang="ts">
  import * as api from "../lib/api";

  let { subjectId }: { subjectId: string } = $props();

  let data = $state<api.Bipartite | null>(null);
  let err = $state<string | null>(null);
  let hover = $state<string | null>(null);

  $effect(() => {
    if (!subjectId) return;
    let cancelled = false;
    data = null;
    err = null;
    api
      .provenanceBipartite(subjectId)
      .then((r) => { if (!cancelled) data = r; })
      .catch((e) => { if (!cancelled) err = String(e); });
    return () => { cancelled = true; };
  });

  // Deterministic layout — no force-directed anything: the same library must
  // draw the same picture every time, or the user loses spatial memory.
  const ROW = 26;
  const NODE_H = 20;
  const L_X = 142; // left node column
  const L_W = 118;
  const R_X = 428; // right node column
  const R_W = 118;

  const VERDICT_LABEL: Record<string, string> = {
    cross: "交叉验证",
    single: "孤证",
    notes_only: "只有笔记",
    orphan: "无来源",
  };
  const VIA_LABEL: Record<string, string> = {
    topic: "归档在该主题下",
    text: "正文提到",
    mention: "笔记提到",
  };

  interface Row { id: string; name: string; kind: string; verdict?: string; sub?: string; }

  const layout = $derived.by(() => {
    if (!data) return null;
    const materials: Row[] = [
      ...data.materials.filter((m) => m.kind === "source").map((m) => ({
        id: m.id, name: m.name, kind: "source",
        sub: m.parsed === false ? "未解析" : `${m.supports} 个点`,
      })),
      ...data.materials.filter((m) => m.kind === "note").map((m) => ({
        id: m.id, name: m.name, kind: "note", sub: `${m.supports} 个点`,
      })),
    ];
    // 孤证 first: the points that need a second source are the reason this
    // view exists, they belong at the top, not buried alphabetically.
    const rank: Record<string, number> = { single: 0, notes_only: 1, orphan: 2, cross: 3 };
    const points: Row[] = [...data.points]
      .sort((a, b) => rank[a.verdict] - rank[b.verdict] || a.title.localeCompare(b.title))
      .map((p) => ({
        id: p.id, name: p.title, kind: p.kind, verdict: p.verdict,
        sub: `${p.source_count} 来源 · ${p.note_count} 笔记`,
      }));
    const rows = Math.max(materials.length, points.length, 1);
    return { materials, points, rows, h: rows * ROW + 16 };
  });

  function y(i: number): number {
    return 8 + i * ROW + NODE_H / 2;
  }

  function trunc(s: string, n: number): string {
    return s.length > n ? s.slice(0, n - 1) + "…" : s;
  }

  function nodeFill(kind: string, verdict?: string): string {
    if (kind === "source") return "var(--accent)";
    if (kind === "note") return "var(--fg-muted)";
    if (verdict === "cross") return "var(--ok)";
    if (verdict === "single") return "var(--warn)";
    if (verdict === "notes_only") return "var(--fg-muted)";
    return "var(--fg-faint)";
  }

  // The actionable list: every 孤证 with the ONE material that backs it —
  // "再找一个来源" needs to name the source you already have.
  const singles = $derived.by(() => {
    if (!data) return [];
    return data.points
      .filter((p) => p.verdict === "single" || p.verdict === "notes_only")
      .map((p) => {
        const mats = data!.edges
          .filter((e) => e.point_id === p.id)
          .map((e) => data!.materials.find((m) => m.id === e.material_id))
          .filter((m): m is api.ProvenanceMaterial => !!m);
        return { point: p, mats };
      });
  });
</script>

<div class="srcv">
  <div class="srcv-head">
    <h3>来源视图</h3>
    <p>
      材料（左）× 知识点（右）的支撑关系。<b>孤证</b> = 只有一个来源支撑，
      别当结论用；<b>交叉验证</b> = 两个以上来源共同支撑。全部本地计算，不调用模型。
    </p>
  </div>

  {#if err}
    <p class="srcv-err">{err}</p>
  {:else if !layout}
    <p class="srcv-empty">加载中…</p>
  {:else if layout.materials.length === 0}
    <p class="srcv-empty">这个学科还没有任何材料。导入来源、写笔记之后，这里会出现支撑关系。</p>
  {:else if layout.points.length === 0}
    <p class="srcv-empty">
      还没有知识点节点。生成第一篇知识点文档（框架里点「文档」）或完成一次陪练后，
      这里会显示每个点被哪些材料支撑。
    </p>
  {:else}
    <div class="srcv-legend">
      <span class="srcv-chip"><i style="background:var(--ok)"></i>交叉验证 {layout.points.filter((p) => p.verdict === "cross").length}</span>
      <span class="srcv-chip"><i style="background:var(--warn)"></i>孤证 {layout.points.filter((p) => p.verdict === "single").length}</span>
      <span class="srcv-chip"><i style="background:var(--fg-muted)"></i>只有笔记 {layout.points.filter((p) => p.verdict === "notes_only").length}</span>
      <span class="srcv-chip"><i style="background:var(--fg-faint)"></i>无来源 {layout.points.filter((p) => p.verdict === "orphan").length}</span>
    </div>

    <svg class="srcv-svg" viewBox="0 0 680 {layout.h}" role="img" aria-label="材料与知识点的支撑关系图">
      <!-- edges first, nodes on top -->
      {#each data!.edges as e (e.material_id + "\u0000" + e.point_id)}
        {@const mi = layout.materials.findIndex((m) => m.id === e.material_id)}
        {@const pi = layout.points.findIndex((p) => p.id === e.point_id)}
        {#if mi >= 0 && pi >= 0}
          <path
            d="M {L_X + L_W} {y(mi)} C {L_X + L_W + 90} {y(mi)}, {R_X - 90} {y(pi)}, {R_X} {y(pi)}"
            fill="none"
            stroke={e.via === "text" ? "var(--accent)" : e.via === "mention" ? "var(--fg-muted)" : "var(--fg-faint)"}
            stroke-width={1 + Math.min(e.hits, 6) * 0.35}
            stroke-opacity={hover == null || hover === e.material_id || hover === e.point_id ? 0.5 : 0.06}
          >
            <title>{VIA_LABEL[e.via] ?? e.via} · {e.hits} 处</title>
          </path>
        {/if}
      {/each}

      {#each layout.materials as m, i (m.id)}
        <g
          class="srcv-node"
          role="img"
          opacity={hover == null || hover === m.id ? 1 : 0.35}
          onmouseenter={() => (hover = m.id)}
          onmouseleave={() => (hover = null)}
        >
          <text class="srcv-lab srcv-lab-l" x={L_X - 8} y={y(i) + 4}>{trunc(m.name, 11)}</text>
          <rect x={L_X} y={y(i) - NODE_H / 2} width={L_W} height={NODE_H} rx={4}
            fill={nodeFill(m.kind)} fill-opacity={m.kind === "source" ? 0.22 : 0.12}
            stroke={nodeFill(m.kind)} stroke-opacity={0.65} />
          <text class="srcv-sub" x={L_X + 7} y={y(i) + 4}>{trunc(m.sub ?? "", 12)}</text>
          <title>{m.name}（{m.kind === "source" ? "来源" : "笔记"}）· {m.sub}</title>
        </g>
      {/each}

      {#each layout.points as p, i (p.id)}
        <g
          class="srcv-node"
          role="img"
          opacity={hover == null || hover === p.id ? 1 : 0.35}
          onmouseenter={() => (hover = p.id)}
          onmouseleave={() => (hover = null)}
        >
          <rect x={R_X} y={y(i) - NODE_H / 2} width={R_W} height={NODE_H} rx={4}
            fill={nodeFill(p.kind, p.verdict)} fill-opacity={p.verdict === "cross" ? 0.16 : p.verdict === "orphan" ? 0.08 : 0.2}
            stroke={nodeFill(p.kind, p.verdict)} stroke-opacity={0.7} />
          <text class="srcv-lab" x={R_X + R_W + 8} y={y(i) + 4}>{trunc(p.name, 10)}</text>
          {#if p.verdict !== "cross"}
            <text class="srcv-tag" x={R_X + R_W + 8} y={y(i) + 4} dx={trunc(p.name, 10).length * 12 + 6}
              fill={p.verdict === "single" ? "var(--warn)" : "var(--fg-muted)"}>
              {VERDICT_LABEL[p.verdict ?? ""] ?? ""}
            </text>
          {/if}
          <title>{p.name}（{p.kind === "concept" ? "知识点" : "主题"}）· {VERDICT_LABEL[p.verdict ?? ""] ?? ""} · {p.sub}</title>
        </g>
      {/each}
    </svg>

    {#if singles.length > 0}
      <div class="srcv-singles">
        <h4>需要补来源</h4>
        <ul>
          {#each singles as s (s.point.id)}
            <li>
              <b>{s.point.title}</b>
              {#if s.point.verdict === "single"}
                只被「{s.mats.map((m) => m.name).join("、")}」支撑 —— 再找一个来源，或者先别把它当结论用。
              {:else}
                只有你自己的笔记提到，没有任何导入材料支撑 —— 补个来源更稳。
              {/if}
            </li>
          {/each}
        </ul>
      </div>
    {/if}
  {/if}
</div>

<style>
  .srcv {
    border: 1px solid var(--line, rgba(128, 128, 128, 0.25));
    border-radius: 10px;
    padding: 14px 16px;
    background: var(--bg-sunken);
  }
  .srcv-head h3 { margin: 0 0 4px; font-size: 14px; }
  .srcv-head p { margin: 0 0 10px; font-size: 12px; color: var(--fg-muted); line-height: 1.5; }
  .srcv-empty, .srcv-err { font-size: 12px; color: var(--fg-muted); margin: 4px 0; }
  .srcv-err { color: var(--warn); }
  .srcv-legend { display: flex; flex-wrap: wrap; gap: 10px; margin-bottom: 8px; }
  .srcv-chip { display: inline-flex; align-items: center; gap: 5px; font-size: 11px; color: var(--fg-muted); }
  .srcv-chip i { width: 8px; height: 8px; border-radius: 2px; display: inline-block; }
  .srcv-svg { width: 100%; height: auto; display: block; }
  .srcv-node { cursor: default; }
  .srcv-lab { font-size: 11px; fill: var(--fg); text-anchor: end; }
  .srcv-lab-l { fill: var(--fg-muted); }
  .srcv-svg text.srcv-lab:not(.srcv-lab-l) { text-anchor: start; }
  .srcv-sub { font-size: 9px; fill: var(--fg-faint); pointer-events: none; }
  .srcv-tag { font-size: 9px; pointer-events: none; }
  .srcv-singles { margin-top: 12px; border-top: 1px dashed var(--line, rgba(128, 128, 128, 0.25)); padding-top: 10px; }
  .srcv-singles h4 { margin: 0 0 6px; font-size: 12px; color: var(--warn); }
  .srcv-singles ul { margin: 0; padding-left: 18px; }
  .srcv-singles li { font-size: 12px; color: var(--fg-muted); line-height: 1.7; }
  .srcv-singles li b { color: var(--fg); }
</style>
