<script lang="ts">
  // Today — the cockpit that makes this one platform instead of five apps.
  //
  // Everything else in the app starts from a subject; this view deliberately
  // does not. It answers one question — "what do I do now?" — by merging four
  // sources that previously lived in four different screens:
  //   • overdue work items (were buried in a per-subject kanban)
  //   • items due today, including recurring ones
  //   • the capture inbox (nothing like it existed before)
  //   • today's plan blocks (empty until the Phase 2 planner ships)
  //
  // The day boundary is computed HERE from the local clock, so the backend
  // never has to guess a timezone: we hand it [startMs, endMs] plus the
  // YYYY-MM-DD label that plan blocks are keyed by.
  import { app } from "../lib/store.svelte";
  import * as api from "../lib/api";
  import Icon from "../components/Icon.svelte";

  const pad = (n: number) => String(n).padStart(2, "0");

  function localDay(d = new Date()) {
    const start = new Date(d.getFullYear(), d.getMonth(), d.getDate());
    const end = new Date(start.getTime() + 86_400_000);
    const label = `${start.getFullYear()}-${pad(start.getMonth() + 1)}-${pad(start.getDate())}`;
    return { startMs: start.getTime(), endMs: end.getTime(), label };
  }

  let day = $state(localDay());
  let board = $state<api.TodayBoard | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let draft = $state("");
  let busy = $state<string | null>(null);
  let rolledOver = false;

  async function refresh() {
    // Yesterday's unstarted blocks are marked `moved` once per launch, so the
    // next planner run re-schedules them instead of letting them rot on a day
    // that has already passed. Today is the default view, which makes this the
    // natural startup hook.
    if (!rolledOver) {
      rolledOver = true;
      await api.planRollover(day.label).catch(() => {});
    }
    try {
      board = await api.todayBoard(day.startMs, day.endMs, day.label);
      error = null;
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    void refresh();
  });

  // ----- B2: work-items streak chip ---------------------------------------
  // Pulls the same analytics payload the AnalyticsView uses; cached client-side
  // so we do not pay for it twice when both views are mounted.
  let summary = $state<api.AnalyticsSummary | null>(null);
  $effect(() => {
    api.analyticsSummary(30).then((s) => { summary = s; }).catch(() => { summary = null; });
  });
  const workStreak = $derived(summary?.work_streak ?? 0);
  const workWeek = $derived.by(() => {
    const d = summary?.work_week_done ?? 0;
    const t = summary?.work_week_total ?? 0;
    return { done: d, total: t };
  });
  const weekRate = $derived(workWeek.total > 0 ? Math.round((workWeek.done / workWeek.total) * 100) : null);

  const counts = $derived(board?.counts ?? null);
  const totalToday = $derived(board ? board.overdue.length + board.today.length : 0);
  const isEmpty = $derived(
    !!board && board.overdue.length === 0 && board.today.length === 0 && board.inbox.length === 0,
  );

  // ----- 动线条: what the night automation did + what's due now -----------
  // Review Today's due queue (reps===0 → never-studied cards, e.g. the ones
  // cron's auto-flashcards seeded overnight) and the latest finished run per
  // cron job. Non-fatal loads: the bar just doesn't render when empty.
  let dueCards = $state<api.ReviewTodayCard[] | null>(null);
  let cronLatest = $state<api.CronRecentRun[] | null>(null);
  $effect(() => {
    api.reviewToday(500).then((cs) => { dueCards = cs; }).catch(() => { dueCards = null; });
    api.cronRecentRuns(3).then((rs) => { cronLatest = rs; }).catch(() => { cronLatest = null; });
  });
  const dueCount = $derived(dueCards?.length ?? 0);
  const dueNew = $derived(dueCards?.filter((c) => c.reps === 0).length ?? 0);
  function whenLabel(ms: number | null): string {
    if (ms == null) return "";
    const d = new Date(ms);
    const now = new Date();
    const sameDay = d.toDateString() === now.toDateString();
    const hhmm = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
    if (sameDay) return `今天 ${hhmm}`;
    const yest = new Date(now.getTime() - 86_400_000);
    if (d.toDateString() === yest.toDateString()) return `昨天 ${hhmm}`;
    return `${d.getMonth() + 1}/${d.getDate()} ${hhmm}`;
  }
  function runBrief(r: api.CronRecentRun): string {
    const first = (r.output ?? "").split("\n")[0] ?? "";
    const t = first.trim() || (r.status === "error" ? "运行失败" : "完成");
    return t.length > 72 ? t.slice(0, 72) + "…" : t;
  }

  // Capture is the one interaction that must never fail: Enter files it into
  // the inbox, Cmd/Ctrl+Enter commits it straight to today.
  async function capture(intoTodaysLane: boolean) {
    const title = draft.trim();
    if (!title || busy) return;
    busy = "capture";
    try {
      await api.createWorkItem({
        title,
        status: intoTodaysLane ? "open" : "inbox",
        due_ms: intoTodaysLane ? day.startMs + 12 * 3600_000 : null,
      });
      draft = "";
      await refresh();
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
    }
  }

  function onCaptureKey(e: KeyboardEvent) {
    if (e.key !== "Enter" || e.isComposing) return;
    e.preventDefault();
    void capture(e.metaKey || e.ctrlKey);
  }

  async function toggle(item: api.WorkItem) {
    if (busy) return;
    busy = item.id;
    try {
      await api.setWorkItemStatus(item.id, item.status === "done" ? "open" : "done");
      await refresh();
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
    }
  }

  async function moveToToday(item: api.WorkItem) {
    if (busy) return;
    busy = item.id;
    try {
      // Keep the original time of day when the item already had a date; a
      // captured item gets "this afternoon" so it lands in today's window.
      const hhmm = item.due_ms ? new Date(item.due_ms) : null;
      const target = hhmm
        ? new Date(day.startMs).setHours(hhmm.getHours(), hhmm.getMinutes())
        : day.startMs + 12 * 3600_000;
      await api.updateWorkItem(item.id, {
        title: item.title,
        body: item.body,
        kind: item.kind,
        status: item.status === "inbox" ? "open" : item.status,
        priority: item.priority,
        due_ms: target,
        start_ms: item.start_ms,
        estimate_min: item.estimate_min,
        parent_id: item.parent_id,
        rrule: item.rrule,
        subject_id: item.subject_id,
        topic_id: item.topic_id,
        goal_id: item.goal_id,
      });
      await refresh();
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
    }
  }

  async function remove(item: api.WorkItem) {
    if (busy) return;
    busy = item.id;
    try {
      await api.deleteWorkItem(item.id);
      await refresh();
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
    }
  }

  function timeLabel(ms: number | null): string {
    if (!ms) return "";
    const d = new Date(ms);
    const hhmm = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
    if (ms >= day.startMs && ms < day.endMs) return hhmm;
    const sameYear = d.getFullYear() === new Date(day.startMs).getFullYear();
    const date = sameYear
      ? `${d.getMonth() + 1}/${d.getDate()}`
      : `${d.getFullYear()}/${d.getMonth() + 1}/${d.getDate()}`;
    return `${date} ${hhmm}`;
  }

  function overdueLabel(ms: number | null): string {
    if (!ms) return "无日期";
    const days = Math.floor((day.startMs - ms) / 86_400_000);
    if (days <= 0) return timeLabel(ms);
    if (days === 1) return "逾期 1 天";
    return `逾期 ${days} 天`;
  }

  const KIND_LABEL: Record<string, string> = {
    task: "任务",
    reading: "待读",
    writing: "待写",
    question: "待问",
    excerpt: "摘录",
    milestone: "里程碑",
    material: "材料",
  };

  function minutesLabel(n: number): string {
    if (n < 60) return `${n} 分钟`;
    const h = Math.floor(n / 60);
    const m = n % 60;
    return m ? `${h} 小时 ${m} 分` : `${h} 小时`;
  }

  function blockTime(min: number): string {
    return `${pad(Math.floor(min / 60))}:${pad(min % 60)}`;
  }
</script>

{#snippet itemRow(item: api.WorkItem, lane: "overdue" | "today" | "inbox")}
  <div class="row" class:done={item.status === "done"} class:busy={busy === item.id}>
    <button
      class="tick"
      aria-label={item.status === "done" ? "标记未完成" : "标记完成"}
      onclick={() => toggle(item)}
    >
      {#if item.status === "done"}<Icon name="check" size={11} />{/if}
    </button>

    <div class="body">
      <div class="title">
        {item.title}
        {#if item.priority === "urgent" || item.priority === "high"}
          <span class="prio {item.priority}"></span>
        {/if}
        {#if item.child_count > 0}
          <span class="chip">{item.child_count - item.open_child_count}/{item.child_count}</span>
        {/if}
        {#if item.rrule}
          <span class="chip rec" title={item.rrule}>
            <Icon name="refresh" size={9} /> 重复
          </span>
        {/if}
      </div>
      <div class="meta">
        {#if lane === "overdue"}
          <span class="warn">{overdueLabel(item.due_ms)}</span>
        {:else if item.due_ms}
          <span>{timeLabel(item.due_ms)}</span>
        {/if}
        {#if item.next_due_ms && item.rrule}
          <span class="faint">下次 {timeLabel(item.next_due_ms)}</span>
        {/if}
        <span class="faint">{KIND_LABEL[item.kind] ?? item.kind}</span>
        {#if item.estimate_min}
          <span class="faint">预计 {minutesLabel(item.estimate_min)}</span>
        {/if}
        {#if item.actual_min > 0}
          <span class="faint">已用 {minutesLabel(item.actual_min)}</span>
        {/if}
      </div>
    </div>

    <div class="acts">
      {#if lane !== "today"}
        <button class="act" title="移到今天" onclick={() => moveToToday(item)}>
          今天
        </button>
      {/if}
      <button class="act danger" title="删除" onclick={() => remove(item)}>
        <Icon name="x" size={11} />
      </button>
    </div>
  </div>
{/snippet}

<div class="today">
  <header class="head">
    <div>
      <h1>今天</h1>
      <p class="sub">
        {new Date(day.startMs).toLocaleDateString("zh-CN", {
          month: "long",
          day: "numeric",
          weekday: "long",
        })}
      </p>
    </div>
    <div class="streak-chip" class:streak-chip--hot={workStreak >= 3} title={workStreak > 0 ? `连续 ${workStreak} 天有任务完成` : "今天还没有完成的任务 — 标记一件开始连续"}>
      <span class="streak-chip-flame">🔥</span>
      <span class="streak-chip-n">{workStreak}</span>
      <span class="streak-chip-l">连续</span>
      {#if weekRate !== null}
        <span class="streak-chip-sep">·</span>
        <span class="streak-chip-l">本周 {weekRate}%</span>
      {/if}
    </div>
    <div class="stats">
      <div class="stat"><b class:hot={totalToday > 0}>{totalToday}</b><span>待办</span></div>
      <div class="stat"><b class:hot={(counts?.overdue ?? 0) > 0}>{counts?.overdue ?? 0}</b><span>逾期</span></div>
      <div class="stat"><b>{counts?.done_today ?? 0}</b><span>今日完成</span></div>
      <div class="stat">
        <b>{counts?.planned_minutes ? minutesLabel(counts.planned_minutes) : "—"}</b>
        <span>计划投入</span>
      </div>
    </div>
  </header>

  {#if dueCount > 0 || (cronLatest?.length ?? 0) > 0}
    <div class="flowbar">
      {#if dueCount > 0}
        <button class="flow-chip flow-review" title="进入今日复习" onclick={() => app.setView("review")}>
          <Icon name="cards" size={13} />
          <b>复习 {dueCount} 张</b>
          {#if dueNew > 0}<span class="flow-sub mono">· 含新卡 {dueNew}</span>{/if}
        </button>
      {/if}
      {#each cronLatest ?? [] as r (r.job_id)}
        <button
          class="flow-chip flow-cron"
          class:err={r.status === "error"}
          title={(r.output ?? "").slice(0, 400)}
          onclick={() => app.openSettings("cron")}
        >
          <span class="flow-dot" class:err={r.status === "error"}></span>
          <b>{r.job_name}</b>
          <span class="flow-sub">{whenLabel(r.finished_at)} · {runBrief(r)}</span>
        </button>
      {/each}
    </div>
  {/if}

  <div class="capture">
    <Icon name="plus" size={13} />
    <input
      bind:value={draft}
      onkeydown={onCaptureKey}
      placeholder="记一笔…  Enter 收进收件箱，⌘/Ctrl+Enter 直接加到今天"
      aria-label="快速记录"
    />
    <button class="go" disabled={!draft.trim() || busy === "capture"} onclick={() => capture(false)}>
      收件箱
    </button>
    <button class="go primary" disabled={!draft.trim() || busy === "capture"} onclick={() => capture(true)}>
      今天
    </button>
  </div>

  {#if error}
    <div class="err">{error}</div>
  {/if}

  {#if loading}
    <div class="empty">正在加载…</div>
  {:else if isEmpty && !board?.blocks.length}
    <div class="empty">
      <p>今天还没有安排。</p>
      <p class="hint">在上面记一条，或去「复习」看看到期卡片，去「冲刺」生成备考计划。</p>
    </div>
  {:else}
    {#if board?.blocks.length}
      <section>
        <h2>今日计划</h2>
        <div class="blocks">
          {#each board.blocks as b (b.id)}
            <div class="block" class:done={b.status === "done"}>
              <span class="btime">{blockTime(b.start_min)}–{blockTime(b.end_min)}</span>
              <span class="btitle">
                {board.today.find((i) => i.id === b.item_id)?.title ??
                  board.overdue.find((i) => i.id === b.item_id)?.title ??
                  "已排期任务"}
              </span>
            </div>
          {/each}
        </div>
      </section>
    {/if}

    {#if board?.overdue.length}
      <section>
        <h2 class="warnhead">逾期 <span class="n">{board.overdue.length}</span></h2>
        {#each board.overdue as item (item.id)}
          {@render itemRow(item, "overdue")}
        {/each}
      </section>
    {/if}

    <section>
      <h2>今天 <span class="n">{board?.today.length ?? 0}</span></h2>
      {#if board?.today.length}
        {#each board.today as item (item.id)}
          {@render itemRow(item, "today")}
        {/each}
      {:else}
        <p class="none">今天没有到期任务。</p>
      {/if}
    </section>

    <section>
      <h2>收件箱 <span class="n">{board?.inbox.length ?? 0}</span></h2>
      {#if board?.inbox.length}
        {#each board.inbox as item (item.id)}
          {@render itemRow(item, "inbox")}
        {/each}
      {:else}
        <p class="none">空。想到什么先扔进来，之后再归类。</p>
      {/if}
    </section>
  {/if}
</div>

<style>
  .today {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 24px 28px 60px;
    max-width: 860px;
  }
  .head {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: 24px;
    margin-bottom: 18px;
    flex-wrap: wrap;
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
  .stats {
    display: flex;
    gap: 18px;
  }
  .stat {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 2px;
  }
  .stat b {
    font-family: var(--font-mono, monospace);
    font-size: var(--t-md);
    font-weight: 500;
    color: var(--fg-bright);
  }
  .stat b.hot {
    color: var(--accent);
  }
  .stat span {
    font-size: 11px;
    color: var(--fg-faint);
  }

  /* 动线条 — night automation report + due-now review, one row of chips */
  .flowbar {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-bottom: 10px;
  }
  .flow-chip {
    display: flex;
    align-items: center;
    gap: 7px;
    max-width: 100%;
    padding: 6px 12px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: transparent;
    font-size: var(--t-xs, 12px);
    color: var(--fg-muted);
    cursor: pointer;
    text-align: left;
  }
  .flow-chip b {
    font-weight: 500;
    color: var(--fg-bright);
    white-space: nowrap;
  }
  .flow-chip:hover {
    border-color: var(--accent);
    color: var(--fg-bright);
  }
  .flow-review {
    border-color: var(--accent);
  }
  .flow-review b {
    color: var(--accent);
  }
  .flow-sub {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg-faint);
    font-size: 11px;
  }
  .flow-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--ok, #639922);
    flex: none;
  }
  .flow-dot.err {
    background: var(--danger, #e24b4a);
  }
  .flow-chip.err b {
    color: var(--danger, #e24b4a);
  }

  .capture {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: 10px;    background: var(--surface);
    margin-bottom: 22px;
  }
  .capture:focus-within {
    border-color: var(--accent-dim);
  }
  .capture input {
    flex: 1;
    min-width: 0;
    background: transparent;
    border: 0;
    outline: none;
    color: var(--fg);
    font-size: var(--t-sm);
  }
  .capture input::placeholder {
    color: var(--fg-faint);
  }
  .go {
    border: 1px solid var(--border);
    background: transparent;
    color: var(--fg-muted);
    font-size: 11px;
    padding: 3px 9px;
    border-radius: 6px;
    cursor: pointer;
    white-space: nowrap;
  }
  .go:hover:not(:disabled) {
    color: var(--fg-bright);
    border-color: var(--border-strong);
  }
  .go.primary {
    border-color: var(--accent-dim);
    color: var(--accent);
  }
  .go:disabled {
    opacity: 0.4;
    cursor: default;
  }

  section {
    margin-bottom: 26px;
  }
  h2 {
    margin: 0 0 8px;
    font-size: 12px;
    font-weight: 500;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--fg-faint);
    display: flex;
    align-items: center;
    gap: 8px;
  }
  h2.warnhead {
    color: var(--err);
  }
  .n {
    font-family: var(--font-mono, monospace);
    font-size: 11px;
    color: var(--fg-faint);
    background: var(--surface-2);
    border-radius: 20px;
    padding: 0 7px;
  }

  .row {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 9px 10px;
    border-radius: 8px;
    border: 1px solid transparent;
  }
  .row:hover {
    background: var(--surface);
    border-color: var(--border);
  }
  .row.busy {
    opacity: 0.5;
  }
  .row.done .title {
    text-decoration: line-through;
    color: var(--fg-faint);
  }
  .tick {
    width: 15px;
    height: 15px;
    margin-top: 2px;
    flex: none;
    border: 1px solid var(--border-strong);
    border-radius: 4px;
    background: transparent;
    color: var(--accent-fg);
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    padding: 0;
  }
  .row.done .tick {
    background: var(--accent);
    border-color: var(--accent);
  }
  .body {
    flex: 1;
    min-width: 0;
  }
  .title {
    font-size: var(--t-sm);
    color: var(--fg);
    line-height: 1.45;
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  .meta {
    display: flex;
    gap: 10px;
    flex-wrap: wrap;
    margin-top: 3px;
    font-size: 11px;
    color: var(--fg-muted);
  }
  .faint {
    color: var(--fg-faint);
  }
  .warn {
    color: var(--err);
  }
  .chip {
    font-size: 10px;
    color: var(--fg-faint);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0 4px;
    display: inline-flex;
    align-items: center;
    gap: 3px;
  }
  .streak-chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    align-self: flex-start;
    margin-top: 4px;
    padding: 4px 10px;
    border-radius: 999px;
    font-size: 12px;
    background: rgba(255, 255, 255, 0.06);
    color: var(--fg-muted, #b5b9c3);
    border: 1px solid rgba(255, 255, 255, 0.08);
  }
  .streak-chip--hot {
    background: linear-gradient(135deg, rgba(249, 115, 22, 0.18), rgba(249, 115, 22, 0.08));
    border-color: rgba(249, 115, 22, 0.45);
    color: #fed7aa;
  }
  .streak-chip-flame { font-size: 13px; line-height: 1; }
  .streak-chip-n { font-weight: 600; color: inherit; }
  .streak-chip-l { opacity: 0.9; }
  .streak-chip-sep { opacity: 0.5; }

  .chip.rec {
    color: var(--info);
  }
  .prio {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    flex: none;
  }
  .prio.high {
    background: var(--warn);
  }
  .prio.urgent {
    background: var(--err);
  }

  .acts {
    display: flex;
    gap: 4px;
    opacity: 0;
    transition: opacity 0.12s;
  }
  .row:hover .acts {
    opacity: 1;
  }
  .act {
    border: 1px solid var(--border);
    background: transparent;
    color: var(--fg-muted);
    font-size: 11px;
    border-radius: 5px;
    padding: 2px 6px;
    cursor: pointer;
    display: flex;
    align-items: center;
  }
  .act:hover {
    color: var(--fg-bright);
    border-color: var(--border-strong);
  }
  .act.danger:hover {
    color: var(--err);
    border-color: var(--err);
  }

  .blocks {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .block {
    display: flex;
    gap: 12px;
    align-items: baseline;
    padding: 7px 10px;
    border-left: 2px solid var(--accent-dim);
    background: var(--surface);
    border-radius: 0 8px 8px 0;
    font-size: var(--t-sm);
  }
  .block.done {
    opacity: 0.55;
  }
  .btime {
    font-family: var(--font-mono, monospace);
    font-size: 11px;
    color: var(--fg-faint);
    flex: none;
  }
  .btitle {
    color: var(--fg);
  }

  .empty {
    padding: 48px 0;
    text-align: center;
    color: var(--fg-muted);
    font-size: var(--t-sm);
  }
  .empty .hint {
    margin-top: 6px;
    font-size: var(--t-xs);
    color: var(--fg-faint);
  }
  .none {
    margin: 0;
    padding: 8px 10px;
    font-size: var(--t-xs);
    color: var(--fg-faint);
  }
  .err {
    margin-bottom: 16px;
    padding: 8px 10px;
    border: 1px solid var(--err);
    border-radius: 8px;
    color: var(--err);
    font-size: var(--t-xs);
  }
</style>
