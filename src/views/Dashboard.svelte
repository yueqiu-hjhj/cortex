<script lang="ts">
  import { app } from "../lib/store.svelte";
  import * as api from "../lib/api";
  import Icon from "../components/Icon.svelte";
  import Mascot from "../components/Mascot.svelte";
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

  // 工具 / Workspace — Reviva-style tool grid. Each tool is a card that links to
  // the most relevant view in cortex. Tool-specific actions land on a sensible tab
  // (e.g. Flashcards → subject · materials tab where the deck lives).
  type Tool = {
    id: string;
    name: string;
    desc: string;
    icon: string;
    /** tag used to color the icon background */
    tone: "accent" | "ok" | "warn" | "info" | "err";
    run: () => void;
  };
  function goToSubjectTab(tab: string) {
    const s = app.subjects[0];
    if (!s) { app.setView("add-subject"); return; }
    app.openSubject(s.id);
    app.setTab(tab);
  }
  const TOOLS: Tool[] = [
    { id: "cheat",     name: "Cheatsheet",  desc: "自动生成的复习提纲",          icon: "book",    tone: "accent", run: () => goToSubjectTab("cheatsheet") },
    { id: "cards",     name: "Flashcards",  desc: "FSRS 间隔重复抽认卡",         icon: "cards",    tone: "ok",     run: () => goToSubjectTab("materials") },
    { id: "quiz",      name: "Quiz",        desc: "边做边检验掌握度",            icon: "check",    tone: "warn",   run: () => goToSubjectTab("materials") },
    { id: "graph",     name: "Neural Graph",desc: "知识图谱 / 神经星图",          icon: "graph",    tone: "accent", run: () => app.setView("neural-graph") },
    { id: "pomo",      name: "Pomodoro",   desc: "今天的专注时间线",                icon: "bolt",     tone: "accent", run: () => app.setView("pomodoro") },
    { id: "materials", name: "Materials",   desc: "学习材料 + 讲义 + 导图",        icon: "grid",     tone: "info",   run: () => goToSubjectTab("materials") },
    { id: "notes",     name: "Notes",       desc: "Markdown 笔记 + 闪念",         icon: "reader",   tone: "ok",     run: () => app.setView("notes") },
    { id: "cite",      name: "Citations",   desc: "引用 + 大纲 + 学习计划",        icon: "cards",    tone: "info",   run: () => goToSubjectTab("citations") },
    { id: "insights",  name: "Insights",    desc: "时间线 + 准确率 + 弱项",         icon: "chart",    tone: "accent", run: () => app.setView("analytics") },
    { id: "calendar",  name: "Calendar",    desc: "日历 + 任务 + 截止日",           icon: "calendar", tone: "warn",   run: () => app.setView("calendar") },
    { id: "record",    name: "Recorder",    desc: "录课 + 转写 + AI 摘要",        icon: "record",   tone: "err",    run: () => app.setView("recorder") },
  ];

  // ── quick panel: today's due count + live pomodoro, one tap each ──
  let dueCount = $state<number | null>(null);
  $effect(() => {
    api.reviewToday(500)
      .then((cs) => { dueCount = cs.length; })
      .catch(() => { dueCount = null; });
  });

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

    <!-- Quick panel: Pomodoro 一键开始 + 今日 FSRS 复习 -->
    <section class="quick" aria-label="Quick actions">
      <button class="q-card q-focus" onclick={() => app.setView("pomodoro")}>
        <span class="q-icon"><Icon name="bolt" size={16} /></span>
        <span class="q-body">
          <b>{app.pomo.running ? app.pomo.phaseLabel : "开始专注"}</b>
          <span class="mono">{app.pomo.running ? "进行中 — 查看时间线" : "番茄钟 · 今天的时间块"}</span>
        </span>
        <em class="mono">{app.pomo.running ? app.pomo.mmss : `${app.pomo.workMin}:00`}</em>
      </button>
      <button class="q-card q-review" onclick={() => app.setView("review")}>
        <span class="q-icon"><Icon name="cards" size={16} /></span>
        <span class="q-body">
          <b>今日复习</b>
          <span class="mono">FSRS 到期队列</span>
        </span>
        <em class="mono">{dueCount == null ? "…" : dueCount === 0 ? "清零" : `${dueCount} 张`}</em>
      </button>
    </section>

    <!-- 工具 / Workspace: Reviva-style tool grid. Each card jumps to the most
         relevant existing view (cortex already has all these tools — the grid is
         a visual index of what is available). Cards show icon + name + 1-line desc
         and react on hover with an accent border + soft lift. -->
    <section class="tools">
      <div class="tools-head">
        <h2>工具</h2>
        <span class="tools-sub">Workspace · {TOOLS.length} 个</span>
      </div>
      <div class="tools-grid">
        {#each TOOLS as t, i (t.id)}
          <button
            class="tool tone-{t.tone}"
            onclick={t.run}
            style:animation-delay="{i * 30}ms"
            aria-label="{t.name} — {t.desc}"
          >
            <span class="tool-icon">
              <Icon name={t.icon} size={16} />
            </span>
            <div class="tool-body">
              <span class="tool-name">{t.name}</span>
              <span class="tool-desc">{t.desc}</span>
            </div>
            <span class="tool-go"><Icon name="arrowR" size={12} /></span>
          </button>
        {/each}
      </div>
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

  /* ── Quick panel: two one-tap actions above the tool grid ── */
  .quick {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
    gap: 9px;
    margin: 0 0 18px;
  }
  .q-card {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 13px 15px;
    border: 1px solid var(--border);
    border-radius: 11px;
    background: var(--surface);
    cursor: pointer;
    text-align: left;
    transition: transform var(--dur-fast) var(--ease),
                border-color var(--dur-fast) var(--ease),
                background var(--dur-fast) var(--ease);
  }
  .q-card:hover {
    transform: translateY(-1px);
    background: var(--surface-2);
  }
  .q-focus:hover { border-color: color-mix(in srgb, var(--accent) 55%, transparent); }
  .q-review:hover { border-color: color-mix(in srgb, var(--ok) 55%, transparent); }
  .q-icon {
    display: grid;
    place-items: center;
    width: 36px;
    height: 36px;
    border-radius: 9px;
    flex: none;
  }
  .q-focus .q-icon {
    background: color-mix(in srgb, var(--accent) 20%, transparent);
    color: var(--accent);
  }
  .q-review .q-icon {
    background: color-mix(in srgb, var(--ok) 18%, transparent);
    color: var(--ok);
  }
  .q-body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .q-body b {
    font-family: var(--font-display);
    font-size: 13.5px;
    font-weight: 600;
    color: var(--fg-bright);
  }
  .q-body .mono {
    font-size: 10.5px;
    color: var(--fg-muted);
  }
  .q-card em {
    font-style: normal;
    font-size: 15px;
    font-weight: 600;
    color: var(--fg-bright);
    flex: none;
  }

  /* ── Tools / Workspace grid (Reviva-inspired) ── */
  .tools { margin: 4px 0 22px; }
  .tools-head {
    display: flex;
    align-items: baseline;
    gap: 10px;
    margin: 0 0 10px;
  }
  .tools-head h2 {
    margin: 0;
    font-family: var(--font-display);
    font-size: 14px;
    font-weight: 600;
    letter-spacing: 0.01em;
    color: var(--fg-bright);
  }
  .tools-sub {
    font-family: var(--font-mono);
    font-size: 10.5px;
    color: var(--fg-faint);
  }
  .tools-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: 9px;
  }
  .tool {
    position: relative;
    display: flex;
    align-items: center;
    gap: 11px;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: 9px;
    background: var(--surface);
    color: var(--fg);
    cursor: pointer;
    text-align: left;
    font-family: var(--font-mono);
    transition: transform var(--dur-fast) var(--ease),
                border-color var(--dur-fast) var(--ease),
                background var(--dur-fast) var(--ease);
    animation: tool-in 320ms var(--ease) both;
  }
  .tool:hover {
    border-color: var(--border-strong);
    background: var(--surface-2);
    transform: translateY(-1px);
  }
  .tool:focus-visible {
    outline: none;
    border-color: var(--accent);
  }
  .tool-icon {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    border-radius: 7px;
    background: var(--surface-2);
    color: var(--fg-bright);
    flex: none;
    transition: background var(--dur-fast) var(--ease), color var(--dur-fast) var(--ease);
  }
  .tool-body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .tool-name {
    font-family: var(--font-display);
    font-size: 12.5px;
    font-weight: 500;
    color: var(--fg-bright);
  }
  .tool-desc {
    font-family: var(--font-mono);
    font-size: 10.5px;
    color: var(--fg-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tool-go {
    color: var(--fg-faint);
    flex: none;
    transition: color var(--dur-fast) var(--ease), transform var(--dur-fast) var(--ease);
  }
  .tool:hover .tool-go { color: var(--accent); transform: translateX(2px); }

  /* Per-tone accent on the icon chip. The hover border picks the same tone. */
  .tool.tone-accent:hover { border-color: color-mix(in srgb, var(--accent) 55%, transparent); }
  .tool.tone-accent:hover .tool-icon {
    background: color-mix(in srgb, var(--accent) 22%, transparent);
    color: var(--accent);
  }
  .tool.tone-ok:hover { border-color: color-mix(in srgb, var(--ok) 55%, transparent); }
  .tool.tone-ok:hover .tool-icon {
    background: color-mix(in srgb, var(--ok) 20%, transparent);
    color: var(--ok);
  }
  .tool.tone-warn:hover { border-color: color-mix(in srgb, var(--warn) 55%, transparent); }
  .tool.tone-warn:hover .tool-icon {
    background: color-mix(in srgb, var(--warn) 18%, transparent);
    color: var(--warn);
  }
  .tool.tone-info:hover { border-color: color-mix(in srgb, var(--info) 55%, transparent); }
  .tool.tone-info:hover .tool-icon {
    background: color-mix(in srgb, var(--info) 18%, transparent);
    color: var(--info);
  }
  .tool.tone-err:hover .tool-icon {
    background: color-mix(in srgb, var(--err) 18%, transparent);
    color: var(--err);
  }


  @keyframes tool-in {
    from { opacity: 0; transform: translateY(4px); }
    to   { opacity: 1; transform: translateY(0); }
  }
</style>
