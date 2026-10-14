<script lang="ts">
  // Pomodoro Timeline — "今天学了什么" 的时间块视图。
  // 主轨：0:00–24:00 水平时间线，每个 Focus/Break 段按真实起止时间定位成块，
  //       正在跑的番茄钟是一块会生长的 live 块，"现在"是一条竖线。
  // 下方：今天的时间块明细列表（倒序）+ 当前番茄钟控制面板。
  // 数据：pomodoro_segments()（今天已完成段）+ analyticsSummary(1)（KPI）+ 实时 app.pomo。
  import { app } from "../lib/store.svelte";
  import * as api from "../lib/api";
  import Icon from "../components/Icon.svelte";

  const DAY_MIN = 24 * 60;

  // 今天的时间块（默认窗口 = 本地 0 点 → 现在）
  let segs = $state<api.PomodoroSegment[]>([]);
  let loaded = $state(false);
  async function refresh() {
    try { segs = await api.pomodoroSegments(); }
    catch { /* non-fatal — keep last list */ }
    finally { loaded = true; }
  }
  $effect(() => { void refresh(); });
  // 完成/跳过一个段（completedSessions 变化）或开始暂停（running 变化）后，
  // 稍等落库完成再拉新列表。
  let refreshTimer: ReturnType<typeof setTimeout> | null = null;
  function refreshSoon() {
    if (refreshTimer) clearTimeout(refreshTimer);
    refreshTimer = setTimeout(refresh, 400);
  }
  $effect(() => {
    void app.pomo.completedSessions;
    void app.pomo.running;
    refreshSoon();
    return () => { if (refreshTimer) clearTimeout(refreshTimer); };
  });

  // KPI 仍来自统计接口（与 Insights 口径一致）
  let stats = $state<api.PomodoroStats | null>(null);
  $effect(() => {
    api.analyticsSummary(1)
      .then((s) => { stats = s.pomodoro; })
      .catch(() => {});
  });
  const focusCount = $derived(stats?.focus_sessions ?? 0);
  const totalMin = $derived(stats?.focus_minutes ?? 0);

  // 当天 0 点（跨午夜时随时间刷新重算）
  let midnightMs = $state(getMidnight());
  let nowMin = $state(getNowMin());
  $effect(() => {
    const id = setInterval(() => {
      nowMin = getNowMin();
      midnightMs = getMidnight();
    }, 30_000);
    return () => clearInterval(id);
  });
  function getMidnight() {
    const d = new Date();
    d.setHours(0, 0, 0, 0);
    return d.getTime();
  }
  function getNowMin() {
    const d = new Date();
    return d.getHours() * 60 + d.getMinutes();
  }
  function pctOf(ms: number) {
    return Math.max(0, Math.min(100, ((ms - midnightMs) / 60_000 / DAY_MIN) * 100));
  }

  // 正在跑的段：起点 = 现在 − 已流逝，块随秒针生长
  const liveSeg = $derived.by(() => {
    if (!app.pomo.running) return null;
    const total = app.pomo.totalMs();
    const started = Date.now() - (total - app.pomo.remainingMs);
    return { started, kind: app.pomo.phase as "work" | "break" | "long" };
  });
  const liveTone = { work: "accent", break: "ok", long: "info" } as const;
  const PHASE_LABEL = { work: "Focus", break: "Break", long: "Long break" } as const;

  function colorOf(seg: api.PomodoroSegment): string | null {
    if (!seg.subject_id) return null;
    const s = app.subjects.find((x: { id: string }) => x.id === seg.subject_id);
    return s ? app.subjectColor(s) : null;
  }
  function tipOf(seg: api.PomodoroSegment) {
    const dur = fmtDur(seg.ended_ms - seg.started_ms);
    const kind = seg.kind === "work" ? "Focus" : seg.kind === "break" ? "Break" : seg.kind;
    return `${fmtClock(seg.started_ms)}–${fmtClock(seg.ended_ms)} · ${kind} ${dur}${seg.subject_name ? " · " + seg.subject_name : ""}`;
  }
  function fmtClock(ms: number) {
    const d = new Date(ms);
    return `${d.getHours().toString().padStart(2, "0")}:${d.getMinutes().toString().padStart(2, "0")}`;
  }
  function fmtDur(ms: number) {
    const m = Math.max(1, Math.round(ms / 60_000));
    return m >= 60 ? `${(m / 60).toFixed(m % 60 === 0 ? 0 : 1)}h` : `${m}min`;
  }

  // 明细列表：倒序，最多 12 条
  const listSegs = $derived(
    [...segs].sort((a, b) => b.started_ms - a.started_ms).slice(0, 12)
  );

  const today = $derived(formatToday(new Date()));
  function formatToday(d: Date) {
    const w = ["日", "一", "二", "三", "四", "五", "六"][d.getDay()];
    return `${d.getMonth() + 1}月${d.getDate()}日 · 周${w}`;
  }
  const activeSubjectId = $derived(app.activeSubjectId);
</script>

<div class="pt">
  <!-- Header -->
  <header class="pt-head">
    <div>
      <div class="eyebrow">{today} · 番茄钟</div>
      <h1 class="pt-title">今天学了什么</h1>
    </div>
    <div class="pt-head-stats">
      <span class="kpi"><b>{totalMin}</b><i>min</i></span>
      <span class="kpi"><b>{focusCount}</b><i>段</i></span>
      <span class="kpi"><b>{app.pomo.completedSessions}</b><i>本会话</i></span>
    </div>
  </header>

  <!-- Time-block timeline -->
  <section class="pt-tl">
    <div class="pt-tl-l">
      <h3>时间块</h3>
      <p>每一块是一次真实的 Focus / Break 段，按起止时间落在 24 小时轨道上。亮色块正在生长。</p>
    </div>

    <div class="pt-tl-r">
      <div class="pt-tl-axis">
        <span>0:00</span><span>6:00</span><span>12:00</span><span>18:00</span><span>24:00</span>
      </div>
      <div class="pt-tl-track">
        <!-- hour gridlines -->
        {#each Array(25) as _, h (h)}
          <div class="pt-tl-grid" style:left="{(h / 24) * 100}%"></div>
        {/each}

        <!-- finished segments -->
        {#each segs as seg (seg.id)}
          {@const l = pctOf(seg.started_ms)}
          {@const w = Math.max(0.35, pctOf(seg.ended_ms) - l)}
          <div
            class="pt-tl-blk {seg.kind === 'break' ? 'is-break' : ''}"
            class:has-subj={!!colorOf(seg)}
            style:left="{l}%"
            style:width="{w}%"
            style:border-color={colorOf(seg) ?? undefined}
            title={tipOf(seg)}
          ></div>
        {/each}

        <!-- live segment (growing) -->
        {#if liveSeg}
          {@const l = pctOf(liveSeg.started)}
          {@const w = Math.max(0.35, (nowMin / DAY_MIN) * 100 - l)}
          <div
            class="pt-tl-live tone-{liveTone[liveSeg.kind]}"
            style:left="{l}%"
            style:width="{w}%"
          ></div>
        {/if}

        <!-- now marker -->
        {#if loaded}
          <div class="pt-tl-now" style:left="{(nowMin / DAY_MIN) * 100}%"></div>
        {/if}

        <!-- empty state -->
        {#if loaded && segs.length === 0 && !liveSeg}
          <div class="pt-tl-empty">今天还没有时间块 — 点下方 Start 开始第一个 Focus。</div>
        {/if}
      </div>
    </div>
  </section>

  <!-- Segment list -->
  <section class="pt-list">
    <div class="pt-list-head">
      <h3>今天的时间块</h3>
      <button class="pt-btn slim" onclick={refresh} title="重新拉取">
        <Icon name="refresh" size={12} /> 刷新
      </button>
    </div>
    {#if listSegs.length === 0}
      <p class="pt-list-empty mono">{loaded ? "还没有记录。" : "加载中…"}</p>
    {:else}
      <div class="pt-rows">
        {#each listSegs as seg (seg.id)}
          <div class="pt-row">
            <span class="pt-row-dot {seg.kind === 'break' ? 'is-break' : ''}" style:background={colorOf(seg) ?? undefined}></span>
            <span class="pt-row-time mono">{fmtClock(seg.started_ms)}–{fmtClock(seg.ended_ms)}</span>
            <span class="pt-row-kind">{seg.kind === "work" ? "Focus" : seg.kind === "break" ? "Break" : seg.kind}</span>
            <span class="pt-row-subj">{seg.subject_name ?? "未绑定科目"}</span>
            <span class="pt-row-dur mono">{fmtDur(seg.ended_ms - seg.started_ms)}</span>
          </div>
        {/each}
      </div>
    {/if}
  </section>

  <!-- Live control -->
  <section class="pt-live">
    <div class="pt-live-l">
      <div class="pt-live-phase tone-{liveTone[app.pomo.phase]}">
        <Icon name={app.pomo.phase === "work" ? "bolt" : "music"} size={14} />
        <span>{PHASE_LABEL[app.pomo.phase]}</span>
        <span class="pt-live-cycle">第 {app.pomo.cycle} 段</span>
      </div>
      <div class="pt-live-time mono">{app.pomo.mmss}</div>
      <div class="pt-live-bar">
        <div
          class="pt-live-bar-fill tone-{liveTone[app.pomo.phase]}"
          style:width="{app.pomo.progress * 100}%"
        ></div>
      </div>
      <p class="pt-live-sub">
        {app.pomo.running ? "进行中 — 保持专注" : "空闲 — 点 Start 开始一个 Focus 段"}
      </p>
    </div>

    <div class="pt-live-r">
      <button class="pt-btn primary" onclick={() => app.pomo.pomoToggle()} title="开始 / 暂停 (Space p)">
        <Icon name={app.pomo.running ? "pause" : "play"} size={14} />
        {app.pomo.running ? "Pause" : "Start"}
      </button>
      <button class="pt-btn" onclick={() => app.pomo.pomoReset()} title="重置当前段">
        <Icon name="refresh" size={13} /> Reset
      </button>
      <button class="pt-btn" onclick={() => app.pomo.pomoSkip()} title="跳到下一段">
        <Icon name="arrowR" size={13} /> Skip
      </button>
      {#if app.subjects.length}
        <div class="pt-live-subject">
          <span class="eyebrow">Bind to</span>
          <select
            value={activeSubjectId ?? ""}
            onchange={(e) => {
              const id = (e.currentTarget as HTMLSelectElement).value || null;
              if (id) app.openSubject(id);
            }}
          >
            <option value="">（不绑定）</option>
            {#each app.subjects as s (s.id)}
              <option value={s.id}>{s.glyph} {s.name}</option>
            {/each}
          </select>
        </div>
      {/if}
    </div>
  </section>

  <!-- Tip strip -->
  <section class="pt-tip">
    <Icon name="bolt" size={12} color="var(--accent)" />
    <span>每完成 4 段 Focus 自动奖励一个长休息。段结束会自动落库到这里的时间线（绑定了科目则计入该科目的学习时间）。</span>
  </section>
</div>

<style>
  .pt {
    flex: 1; min-height: 0; overflow: auto;
    padding: 22px 28px 32px;
    display: flex; flex-direction: column; gap: 14px;
  }
  .eyebrow {
    font-family: var(--font-mono);
    font-size: 10.5px;
    color: var(--fg-faint);
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }
  .mono { font-family: var(--font-mono); }

  /* Header */
  .pt-head {
    display: flex; align-items: center; justify-content: space-between; gap: 12px;
    border-bottom: 1px solid var(--border); padding-bottom: 14px;
  }
  .pt-title {
    margin: 4px 0 0;
    font-family: var(--font-display);
    font-size: 22px; font-weight: 600; letter-spacing: -0.01em;
    color: var(--fg-bright);
  }
  .pt-head-stats { display: flex; gap: 8px; }
  .kpi {
    display: inline-flex; align-items: baseline; gap: 4px;
    font-family: var(--font-mono); font-size: 11px; color: var(--fg-faint);
    background: var(--surface); border: 1px solid var(--border);
    border-radius: 8px; padding: 5px 11px;
  }
  .kpi b { font-size: 18px; color: var(--fg-bright); font-weight: 600; }
  .kpi i { font-style: normal; color: var(--fg-muted); }

  /* Timeline */
  .pt-tl {
    display: grid; grid-template-columns: 220px 1fr; gap: 18px;
    border: 1px solid var(--border); border-radius: 12px;
    background: var(--surface); padding: 16px 18px;
  }
  .pt-tl-l h3 {
    margin: 0; font-family: var(--font-display); font-size: 13px; font-weight: 600;
    color: var(--fg-bright);
  }
  .pt-tl-l p {
    margin: 6px 0 0; font-family: var(--font-mono); font-size: 11px; color: var(--fg-faint);
  }
  .pt-tl-axis {
    display: flex; justify-content: space-between;
    font-family: var(--font-mono); font-size: 10.5px; color: var(--fg-faint);
    margin-bottom: 6px;
  }
  .pt-tl-track {
    position: relative; height: 84px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow: hidden;
  }
  .pt-tl-grid {
    position: absolute; top: 0; bottom: 0; width: 1px;
    background: var(--border); opacity: 0.45; pointer-events: none;
  }
  .pt-tl-blk {
    position: absolute; top: 26%; height: 48%;
    background: var(--accent);
    border-left: 2px solid transparent;
    border-radius: 2px;
    opacity: 0.85;
    cursor: default;
    transition: opacity var(--dur-fast) var(--ease);
  }
  .pt-tl-blk:hover { opacity: 1; }
  .pt-tl-blk.is-break { background: var(--ok); opacity: 0.5; }
  .pt-tl-live {
    position: absolute; top: 26%; height: 48%;
    border-radius: 3px; pointer-events: none;
    opacity: 0.75;
    animation: live-pulse 2.4s ease-in-out infinite;
  }
  .pt-tl-live.tone-accent { background: var(--accent); box-shadow: 0 0 10px var(--accent); }
  .pt-tl-live.tone-ok     { background: var(--ok);     box-shadow: 0 0 10px var(--ok); }
  .pt-tl-live.tone-info   { background: var(--info);   box-shadow: 0 0 10px var(--info); }
  @keyframes live-pulse {
    0%, 100% { opacity: 0.55; }
    50%      { opacity: 0.9; }
  }
  .pt-tl-now {
    position: absolute; top: -2px; bottom: -2px; width: 2px;
    background: var(--fg-bright); opacity: 0.8; pointer-events: none;
    transform: translateX(-1px);
  }
  .pt-tl-empty {
    position: absolute; inset: 0;
    display: grid; place-items: center;
    font-family: var(--font-mono); font-size: 11px; color: var(--fg-faint);
  }

  /* Segment list */
  .pt-list {
    border: 1px solid var(--border); border-radius: 12px;
    background: var(--surface); padding: 14px 18px;
  }
  .pt-list-head {
    display: flex; align-items: center; justify-content: space-between;
    margin-bottom: 8px;
  }
  .pt-list-head h3 {
    margin: 0; font-family: var(--font-display); font-size: 13px; font-weight: 600;
    color: var(--fg-bright);
  }
  .pt-list-empty { margin: 4px 0 2px; font-size: 11px; color: var(--fg-faint); }
  .pt-rows { display: flex; flex-direction: column; }
  .pt-row {
    display: flex; align-items: center; gap: 12px;
    padding: 7px 2px;
    border-bottom: 1px solid var(--border);
    font-size: 11.5px; color: var(--fg);
  }
  .pt-row:last-child { border-bottom: none; }
  .pt-row-dot {
    flex: none; width: 8px; height: 8px; border-radius: 3px;
    background: var(--accent);
  }
  .pt-row-dot.is-break { background: var(--ok); opacity: 0.6; }
  .pt-row-time { flex: none; width: 96px; color: var(--fg-muted); font-size: 11px; }
  .pt-row-kind {
    flex: none; width: 64px;
    font-family: var(--font-mono); font-size: 10px;
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 14%, transparent);
    border-radius: 999px; padding: 2px 8px; text-align: center;
  }
  .pt-row .pt-row-dot.is-break + .pt-row-time + .pt-row-kind { color: var(--ok); background: color-mix(in srgb, var(--ok) 14%, transparent); }
  .pt-row-subj {
    flex: 1; min-width: 0;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    color: var(--fg-bright);
  }
  .pt-row-dur { flex: none; color: var(--fg-muted); font-size: 11px; }

  /* Live control panel */
  .pt-live {
    display: grid; grid-template-columns: 1fr auto; gap: 20px;
    border: 1px solid var(--border); border-radius: 12px;
    background: var(--surface); padding: 18px 20px;
    align-items: center;
  }
  .pt-live-phase {
    display: inline-flex; align-items: center; gap: 6px;
    font-family: var(--font-mono); font-size: 11.5px;
    padding: 3px 10px; border-radius: 999px;
    background: color-mix(in srgb, var(--accent) 18%, transparent);
    color: var(--accent);
  }
  .pt-live-phase.tone-ok   { background: color-mix(in srgb, var(--ok)    18%, transparent); color: var(--ok); }
  .pt-live-phase.tone-info { background: color-mix(in srgb, var(--info)  18%, transparent); color: var(--info); }
  .pt-live-cycle { color: var(--fg-muted); margin-left: 6px; }
  .pt-live-time {
    margin-top: 10px; font-size: 44px; font-weight: 600; line-height: 1;
    color: var(--fg-bright); letter-spacing: -0.02em;
  }
  .pt-live-bar {
    margin-top: 12px; height: 6px; background: var(--surface-2);
    border-radius: 3px; overflow: hidden;
  }
  .pt-live-bar-fill {
    height: 100%; background: var(--accent);
    transition: width 250ms linear;
  }
  .pt-live-bar-fill.tone-ok   { background: var(--ok); }
  .pt-live-bar-fill.tone-info { background: var(--info); }
  .pt-live-sub {
    margin: 10px 0 0; font-family: var(--font-mono); font-size: 11px; color: var(--fg-muted);
  }
  .pt-live-r { display: flex; flex-direction: column; gap: 8px; min-width: 200px; }
  .pt-btn {
    display: inline-flex; align-items: center; gap: 6px;
    font-family: var(--font-mono); font-size: 11.5px;
    padding: 7px 12px; border-radius: 7px; cursor: pointer;
    border: 1px solid var(--border); background: var(--surface-2); color: var(--fg);
  }
  .pt-btn:hover { color: var(--fg-bright); border-color: var(--border-strong); }
  .pt-btn.primary {
    background: color-mix(in srgb, var(--accent) 22%, transparent);
    border-color: color-mix(in srgb, var(--accent) 45%, transparent);
    color: var(--fg-bright);
  }
  .pt-btn.primary:hover { background: color-mix(in srgb, var(--accent) 32%, transparent); }
  .pt-btn.slim { padding: 4px 9px; font-size: 10.5px; border-radius: 6px; }
  .pt-live-subject {
    display: flex; flex-direction: column; gap: 4px;
    margin-top: 4px;
  }
  .pt-live-subject select {
    background: var(--surface-2); color: var(--fg);
    border: 1px solid var(--border); border-radius: 6px;
    padding: 5px 8px; font-family: var(--font-mono); font-size: 11px;
  }

  .pt-tip {
    display: flex; align-items: center; gap: 8px;
    padding: 10px 14px; border-radius: 9px;
    background: color-mix(in srgb, var(--accent) 8%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 30%, transparent);
    color: var(--fg-muted); font-family: var(--font-mono); font-size: 11px;
  }
</style>
