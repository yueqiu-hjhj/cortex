<script lang="ts">
  import { app } from "../lib/store.svelte";
  import * as api from "../lib/api";
  import Mascot from "../components/Mascot.svelte";
  import Icon from "../components/Icon.svelte";
  import { moveItem, reorderable } from "../lib/dnd";

  // ── pointer-based card reordering (no native HTML5 drag — see lib/dnd.ts) ──
  function reorderSubject(from: number, to: number) {
    app.reorderSubjects(moveItem(app.subjects.map((s) => s.id), from, to));
  }

  // Derive a simple day/week string for the eyebrow
  const now = new Date();
  const dayName = now.toLocaleDateString(undefined, { weekday: "long" });
  // Simple week number calculation
  const startOfYear = new Date(now.getFullYear(), 0, 1);
  const weekNum = Math.ceil(((now.getTime() - startOfYear.getTime()) / 86400000 + startOfYear.getDay() + 1) / 7);
  // ── hero 第二句：只用真实数据 ──────────────────────────────
  // 拉 30 天汇总，用 reviews_per_day 自己切出「本周 / 上周」两段 7 天，
  // 比正确率。数据不足两周（<14 天）时返回 null，hero 就不显示任何数字，
  // 绝不编造统计值。
  let summary = $state<api.AnalyticsSummary | null>(null);
  $effect(() => {
    api.analyticsSummary(30)
      .then((s) => { summary = s; })
      .catch(() => { summary = null; });
  });
  const heroMetric = $derived.by<string | null>(() => {
    const days = summary?.reviews_per_day;
    if (!days || days.length < 14) return null;
    const accOf = (seg: api.DayReviews[]): number | null => {
      const r = seg.reduce((a, d) => a + (d.reviews ?? 0), 0);
      const c = seg.reduce((a, d) => a + (d.correct ?? 0), 0);
      return r > 0 ? c / r : null;
    };
    const thisWeek = accOf(days.slice(-7));
    const lastWeek = accOf(days.slice(-14, -7));
    if (thisWeek == null || lastWeek == null) return null;
    const delta = Math.round((thisWeek - lastWeek) * 100);
    if (delta === 0) return "正确率和上周持平";
    if (delta > 0) return `这周比上周稳 ${delta}%`;
    return `这周比上周波动 ${delta}%`;
  });
</script>

<div class="workspace-scroll">
  <div class="dash">
    <header class="dash-head">
      <div>
        <div class="eyebrow">{dayName} · week {weekNum}</div>
        <h1 class="dash-title">Your subjects</h1>
      </div>
      <div class="row gap-2">
        <button class="btn btn--sm btn--primary" onclick={() => app.setView("add-subject")}>
          <Icon name="plus" size={13} /> New subject
        </button>
      </div>
    </header>

    <!-- Hero: mascot + conic 渐变 + 今日一句话 (P0) -->
    <section class="hero" aria-label="Greeting">
      <div class="hero-mascot"><Mascot state="curious" size="lg" title="cortex mascot" /></div>
      <div class="hero-body">
        <div class="hero-eyebrow">今天 {dayName}</div>
        <div class="hero-line">
          先从今天第一道题开始吧{#if heroMetric}<span class="hero-dot">·</span> {heroMetric}{/if}
        </div>
        <div class="hero-tags">
          <span class="hero-tag tone-lavender">学习</span>
          <span class="hero-tag tone-peach">复习</span>
          <span class="hero-tag tone-sky">统计</span>
          <span class="hero-tag tone-amber">个人</span>
        </div>
      </div>
      <div class="hero-blob" aria-hidden="true"></div>
    </section>

    <div class="subj-grid">
      {#if app.subjects.length === 0}
        <div style:grid-column="1 / -1" style:text-align="center" style:padding="48px 0" style:color="var(--fg-faint)">
          Add your first subject.
        </div>
      {/if}

      {#each app.subjects as s, i (s.id)}
        <button
          class="subj-card{i === app.dashFocus ? ' kb-focus' : ''}"
          onclick={() => app.openSubject(s.id)}
          style:box-shadow="inset 3px 0 0 {app.subjectColor(s)}"
          use:reorderable={{ index: i, group: "subjects", onReorder: reorderSubject }}
        >
          <div class="sc-top">
            <span class="subj-glyph" style="font-size:15px;line-height:1">{s.glyph || "◆"}</span>
            <span class="subj-code mono">{s.code ?? ""}</span>
            <div class="grow"></div>
            {#if s.streak > 0}
              <span class="streak mono" title="study streak">
                <Icon name="bolt" size={11} color="var(--warn)" /> {s.streak}
              </span>
            {/if}
          </div>
          <h3 class="subj-name read">{s.name}</h3>
          <div class="subj-stat mono">{s.sourceCount} sources · {s.topics.length} topics</div>
          <div class="subj-foot">
            {#if s.sourceCount === 0}
              <span class="status-pill status-pill--review"><span class="dot"></span>No sources yet</span>
            {:else}
              <span class="status-pill status-pill--ready"><span class="dot"></span>{s.sourceCount} source{s.sourceCount === 1 ? "" : "s"}</span>
            {/if}
          </div>
        </button>
      {/each}

      <button class="subj-card subj-card--add" onclick={() => app.setView("add-subject")}>
        <div class="add-inner">
          <Icon name="plus" size={20} color="var(--fg-faint)" />
          <span>Add subject</span>
        </div>
      </button>
    </div>
  </div>
</div>

<style>
  /* ── Hero (P0 mascot + conic 渐变) ──────────────────── */
  .hero {
    position: relative;
    display: grid;
    grid-template-columns: auto 1fr;
    align-items: center;
    gap: var(--sp-5);
    padding: var(--sp-5) var(--sp-5) var(--sp-5) var(--sp-5);
    margin: 0 0 var(--sp-5);
    border: 1px solid var(--border);
    border-radius: 14px;
    background: var(--grad-hero);
    overflow: hidden;
    animation: hero-in 0.45s var(--ease-out-back) both;
  }
  @keyframes hero-in {
    0%   { opacity: 0; transform: translateY(8px) scale(0.985); }
    100% { opacity: 1; transform: translateY(0) scale(1); }
  }
  .hero-mascot { flex: none; line-height: 0; }
  .hero-body { min-width: 0; }
  .hero-eyebrow {
    font-size: var(--t-xs);
    color: var(--fg-faint);
    text-transform: uppercase;
    letter-spacing: 0.12em;
    margin-bottom: 4px;
  }
  .hero-line {
    font-family: var(--font-cn);
    font-size: var(--t-xl);
    color: var(--fg-bright);
    font-weight: 500;
    line-height: 1.35;
  }
  .hero-dot { color: var(--accent); margin: 0 6px; }
  .hero-tags {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 10px;
  }
  .hero-tag {
    display: inline-flex;
    align-items: center;
    height: 22px;
    padding: 0 9px;
    border-radius: var(--rad-pill);
    font-size: var(--t-2xs);
    color: var(--fg);
    background: var(--surface-2);
    border: 1px solid var(--border);
    transition: transform var(--dur) var(--ease), background var(--dur) var(--ease);
  }
  .hero-tag.tone-lavender { background: var(--tone-lavender-bg); color: var(--tone-lavender); border-color: transparent; }
  .hero-tag.tone-peach    { background: var(--tone-peach-bg);    color: var(--tone-peach);    border-color: transparent; }
  .hero-tag.tone-sky      { background: var(--tone-sky-bg);      color: var(--tone-sky);      border-color: transparent; }
  .hero-tag.tone-amber    { background: var(--tone-amber-bg);    color: var(--tone-amber);    border-color: transparent; }
  .hero-blob {
    position: absolute;
    right: -60px; top: -40px;
    width: 240px; height: 240px;
    background: var(--grad-conic);
    filter: blur(50px);
    opacity: 0.35;
    pointer-events: none;
    animation: blob-drift 14s ease-in-out infinite;
  }
  @keyframes blob-drift {
    0%, 100% { transform: translate(0, 0) rotate(0); }
    50%      { transform: translate(-12px, 8px) rotate(8deg); }
  }
  .hero:hover .hero-blob { opacity: 0.5; }

  /* 窄屏（手机 / 收窄的窗口）：mascot 收到文字上方，保持可读。 */
  @media (max-width: 640px) {
    .hero {
      grid-template-columns: 1fr;
      gap: var(--sp-4);
      padding: var(--sp-4);
    }
    .hero-mascot { justify-self: start; }
    .hero-mascot :global(svg) { width: var(--mascot-md); height: var(--mascot-md); }
    .hero-line { font-size: var(--t-lg); }
    .hero-blob { width: 180px; height: 180px; right: -50px; top: -30px; }
  }

</style>
