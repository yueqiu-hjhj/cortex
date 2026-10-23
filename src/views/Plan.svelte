<script lang="ts">
  // Plan — the week view over persisted plan blocks.
  //
  // Phase 2 replaced the old "sprint report" (a computed paragraph that vanished
  // on navigation) with real rows in `plan_blocks`. That is what makes the plan
  // draggable, completable, and reconcilable against actual minutes.
  //
  // Everything here is deterministic: `plan_week` is a rule engine, not a model
  // call, so pressing 自动排期 twice on unchanged data gives the same week.
  import * as api from "../lib/api";
  import Icon from "../components/Icon.svelte";

  const pad = (n: number) => String(n).padStart(2, "0");

  function localDay(d = new Date()) {
    const start = new Date(d.getFullYear(), d.getMonth(), d.getDate());
    const label = `${start.getFullYear()}-${pad(start.getMonth() + 1)}-${pad(start.getDate())}`;
    return { startMs: start.getTime(), label };
  }

  const DAYS = 7;
  let today = $state(localDay());
  let rows = $state<api.BlockRow[]>([]);
  let outcome = $state<api.PlanOutcome | null>(null);
  let capacity = $state(120);
  let loading = $state(true);
  let planning = $state(false);
  let busy = $state<string | null>(null);
  let error = $state<string | null>(null);
  let dragId = $state<string | null>(null);
  let goals = $state<api.Goal[]>([]);
  let goalMsg = $state<string | null>(null);

  // ── long-term plan (0085) ─────────────────────────────────────────────
  // Picking a goal + week turns the grid into that week of the plan; the
  // rolling 7-day view stays the default so nothing changes for users who
  // never paste a plan.
  let goalPick = $state("");
  let weekNo = $state(1);
  let onlyWeek = $state<number | null>(null);
  let planEdit = $state<string | null>(null);
  let planForm = $state({ text: "", weeks: 12, start: "" });
  let planBusy = $state<string | null>(null);

  const pickedGoal = $derived(goals.find((g) => g.id === goalPick) ?? null);
  const pickedWeeks = $derived(Math.max(1, pickedGoal?.plan_weeks ?? 1));

  function goalAnchorMs(g: api.Goal): number {
    if (g.start_ms != null) return g.start_ms;
    if (g.deadline_ms != null && g.plan_weeks) {
      return g.deadline_ms - g.plan_weeks * 7 * 86_400_000;
    }
    return Date.now();
  }

  function pickWeek() {
    if (!pickedGoal) {
      today = localDay();
      onlyWeek = null;
    } else {
      const ms = goalAnchorMs(pickedGoal) + (weekNo - 1) * 7 * 86_400_000;
      today = localDay(new Date(ms));
      onlyWeek = weekNo;
    }
    void load();
  }

  function openPlan(g: api.Goal) {
    planEdit = planEdit === g.id ? null : g.id;
    planForm = {
      text: g.plan_text ?? "",
      weeks: g.plan_weeks ?? 12,
      start: g.start_ms != null ? localDay(new Date(g.start_ms)).label : "",
    };
  }

  async function savePlan(g: api.Goal) {
    if (!planForm.text.trim()) {
      error = "先粘贴一份长期计划";
      return;
    }
    planBusy = g.id;
    try {
      await api.goalPlanSet(g.id, planForm.text, planForm.weeks, planForm.start || null);
      planEdit = null;
      goalMsg = `「${g.title}」计划已保存（${planForm.weeks} 周）`;
      await loadGoals();
    } catch (e) {
      error = String(e);
    } finally {
      planBusy = null;
    }
  }

  async function buildPlan(g: api.Goal) {
    if (!g.plan_text) {
      openPlan(g);
      error = "先粘贴长期计划，再拆解";
      return;
    }
    if (!confirm(`把「${g.title}」的计划拆成 ${g.plan_weeks ?? 12} 周任务？\n（按周打标签、可重复执行，已存在的同名任务会跳过）`)) return;
    planBusy = g.id;
    goalMsg = null;
    try {
      const out = await api.goalPlanBuild(g.id, today.label, true);
      goalMsg = `「${g.title}」生成 ${out.created} 项任务（${minutesLabel(out.minutes)}）` +
        (out.skipped ? `，跳过 ${out.skipped} 项已有任务` : "");
      await Promise.all([loadGoals(), load()]);
    } catch (e) {
      error = String(e);
    } finally {
      planBusy = null;
    }
  }

  function dayKeys(fromLabel: string, n: number): string[] {
    const [y, m, d] = fromLabel.split("-").map(Number);
    const out: string[] = [];
    for (let i = 0; i < n; i++) {
      const dt = new Date(y, m - 1, d + i);
      out.push(`${dt.getFullYear()}-${pad(dt.getMonth() + 1)}-${pad(dt.getDate())}`);
    }
    return out;
  }

  const week = $derived(dayKeys(today.label, DAYS));

  const byDay = $derived.by(() => {
    const map: Record<string, api.BlockRow[]> = {};
    for (const d of week) map[d] = [];
    for (const r of rows) (map[r.day] ??= []).push(r);
    for (const d of week) map[d].sort((a, b) => a.start_min - b.start_min);
    return map;
  });

  const plannedMinutes = $derived(rows.reduce((n, r) => n + (r.end_min - r.start_min), 0));
  const capacityMinutes = $derived(capacity * DAYS);
  const doneCount = $derived(rows.filter((r) => r.status === "done").length);

  async function load() {
    try {
      rows = await api.planBoard(today.label, DAYS);
      error = null;
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  // ── 估算校准 ──
  // 排期用的是「校准后的时长」而不是你自己填的预估：如果你过去 60 天里平均
  // 要花 1.4 倍的预估时间，排期就按 1.4 倍算。样本不够时保持原样。
  let calib = $state<api.Calibration | null>(null);
  async function loadCalib() {
    try {
      calib = await api.planCalibration(60);
    } catch {
      calib = null;
    }
  }

  async function init() {
    // Pick up the persisted capacity so the plan matches Settings.
    try {
      const saved = await api.getSetting("daily_study_minutes");
      if (saved && Number(saved) > 0) capacity = Number(saved);
    } catch {
      /* first run — keep the default */
    }
    await Promise.all([load(), loadGoals(), loadCalib()]);
  }

  async function loadGoals() {
    try {
      goals = (await api.listGoals()).filter((g) => g.status === "active");
    } catch {
      /* goals are optional */
    }
  }

  // The join between Exam and Plan: a goal becomes per-topic tasks (BKT gaps),
  // which the planner can then place into the week.
  async function breakdown(goal: api.Goal) {
    if (busy) return;
    busy = goal.id;
    goalMsg = null;
    try {
      const out = await api.materializeGoal({
        goal_id: goal.id,
        start_day: today.label,
        daily_minutes: capacity,
      });
      goalMsg =
        out.created > 0
          ? `「${goal.title}」生成 ${out.created} 项任务（${minutesLabel(out.minutes)}）` +
            (out.skipped ? `，跳过 ${out.skipped} 项已有任务` : "")
          : `「${goal.title}」没有新任务（${out.skipped} 项已存在）`;
      await Promise.all([loadGoals(), load()]);
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
    }
  }

  $effect(() => {
    void init();
  });

  // ── D4: time-blocking write-back to Google Calendar ────────────────────
  // Pushes planned blocks as events (idempotent), pulls reschedules made in
  // Google back into the plan, removes slots for done/skipped blocks.
  const tzOffsetMin = new Date().getTimezoneOffset();
  let calSyncing = $state(false);
  let calResult = $state<api.PlanSyncResult | null>(null);
  let calError = $state<string | null>(null);
  async function syncCalendar() {
    if (calSyncing) return;
    calSyncing = true;
    calError = null;
    try {
      calResult = await api.planCalendarSync(tzOffsetMin, DAYS);
      await load();
    } catch (e) {
      calError = String(e);
    } finally {
      calSyncing = false;
    }
  }

  async function autoPlan() {
    if (planning) return;
    planning = true;
    error = null;
    try {
      await api.setSetting("daily_study_minutes", String(capacity));
      outcome = await api.planWeek({
        start_day: today.label,
        days: DAYS,
        daily_minutes: capacity,
        day_start_min: 9 * 60,
        day_end_min: 22 * 60,
        break_min: 10,
        replace: true,
        only_week: onlyWeek,
      });
      await load();
    } catch (e) {
      error = String(e);
    } finally {
      planning = false;
    }
  }

  async function complete(row: api.BlockRow) {
    if (busy || row.status === "done") return;
    busy = row.id;
    try {
      await api.completePlanBlock(row.id);
      await load();
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
    }
  }

  // Drag a block to another day. Times are kept; only the day changes, because
  // re-timing is what the planner is for.
  async function moveTo(row: api.BlockRow, day: string) {
    if (row.day === day) return;
    busy = row.id;
    try {
      await api.upsertPlanBlock(
        row.item_id,
        day,
        row.start_min,
        row.end_min,
        row.id,
        row.status,
      );
      await load();
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
    }
  }

  function onDrop(e: DragEvent, day: string) {
    e.preventDefault();
    const id = e.dataTransfer?.getData("text/plain") || dragId;
    dragId = null;
    const row = rows.find((r) => r.id === id);
    if (row) void moveTo(row, day);
  }

  function hhmm(min: number): string {
    return `${pad(Math.floor(min / 60))}:${pad(min % 60)}`;
  }

  function weekdayLabel(day: string): string {
    const [y, m, d] = day.split("-").map(Number);
    const wd = new Date(y, m - 1, d).getDay();
    return ["周日", "周一", "周二", "周三", "周四", "周五", "周六"][wd];
  }

  function dayNumber(day: string): string {
    return String(Number(day.split("-")[2]));
  }

  function minutesLabel(n: number): string {
    if (n < 60) return `${n} 分钟`;
    const h = Math.floor(n / 60);
    const m = n % 60;
    return m ? `${h} 小时 ${m} 分` : `${h} 小时`;
  }
</script>

<div class="plan">
  <header class="head">
    <div>
      <h1>计划</h1>
      <p class="sub">
        未来 {DAYS} 天 · 已排 {minutesLabel(plannedMinutes)} / 容量 {minutesLabel(capacityMinutes)}
      </p>
    </div>
    <div class="controls">
      {#if goals.some((g) => g.plan_weeks)}
        <label class="cap">
          <span>周次</span>
          <select
            class="pick"
            value={goalPick}
            onchange={(e) => {
              goalPick = (e.target as HTMLSelectElement).value;
              weekNo = 1;
              pickWeek();
            }}
          >
            <option value="">滚动 7 天</option>
            {#each goals.filter((g) => g.plan_weeks) as g (g.id)}
              <option value={g.id}>{g.title}（{g.plan_weeks} 周）</option>
            {/each}
          </select>
        </label>
        {#if pickedGoal}
          <label class="cap">
            <span>第</span>
            <select
              class="pick"
              value={String(weekNo)}
              onchange={(e) => {
                weekNo = Number((e.target as HTMLSelectElement).value);
                pickWeek();
              }}
            >
              {#each Array.from({ length: pickedWeeks }, (_, i) => i + 1) as w (w)}
                <option value={String(w)}>{w}</option>
              {/each}
            </select>
            <span>周</span>
          </label>
        {/if}
      {/if}
      <label class="cap">
        <span>每天</span>
        <input type="number" min="15" max="960" step="15" bind:value={capacity} />
        <span>分钟</span>
      </label>
      <button class="btn primary" disabled={planning} onclick={autoPlan}>
        {planning ? "排期中…" : "自动排期"}
      </button>
      <button
        class="btn"
        disabled={calSyncing}
        onclick={syncCalendar}
        title="把计划块写进 Google 日历（Motion 式时间块）：已完成的自动移除，在日历里拖动的时间会同步回计划"
      >
        {calSyncing ? "同步中…" : "同步到日历"}
      </button>
    </div>
  </header>

  {#if calError}
    <div class="calib-note" style="color:var(--err)">{calError}</div>
  {/if}
  {#if calResult}
    <div class="calib-note">
      <span class="calib-t">日历同步</span>
      <span>
        新建 {calResult.pushed} · 更新 {calResult.updated} · 移除 {calResult.removed} ·
        从日历拖回调 {calResult.pulled} · 清理孤儿 {calResult.cleaned}
      </span>
      {#each calResult.notes as n}
        <span class="calib-kinds" style="color:var(--warn)">{n}</span>
      {/each}
    </div>
  {/if}

  <div class="bar">
    <div class="fill" style="width:{Math.min(100, capacityMinutes ? (plannedMinutes / capacityMinutes) * 100 : 0)}%"></div>
  </div>

  {#if calib && calib.samples >= 3}
    <div class="calib-note">
      <span class="calib-t">估算校准</span>
      <span>
        过去 {calib.window_days} 天完成 {calib.samples} 项，实际用时约为预估的
        <b>{calib.factor.toFixed(2)}×</b>，排期按校准后的时长分配。
      </span>
      {#if calib.per_kind.filter((k) => k.samples >= 3).length > 1}
        <span class="calib-kinds">
          {#each calib.per_kind.filter((k) => k.samples >= 3).slice(0, 4) as k (k.kind)}
            <span class="calib-kind">{k.kind} {k.factor.toFixed(2)}×</span>
          {/each}
        </span>
      {/if}
    </div>
  {/if}

  {#if goals.length}
    <section class="goals">
      <div class="ghead">目标</div>
      {#each goals as g (g.id)}
        <div class="grow">
          <div class="ginfo">
            <span class="gtitle">{g.title}</span>
            <span class="gmeta">
              {g.done_item_count}/{g.item_count} 项完成
              {#if g.deadline_ms}
                · 截止 {new Date(g.deadline_ms).toLocaleDateString("zh-CN")}
              {/if}
              {#if g.plan_weeks}
                · 计划 {g.plan_weeks} 周
              {/if}
            </span>
          </div>
          <div class="gbtns">
            <button class="btn" onclick={() => openPlan(g)}>
              {g.plan_text ? "编辑计划" : "粘贴长期计划"}
            </button>
            {#if g.plan_text}
              <button class="btn primary" disabled={planBusy === g.id} onclick={() => buildPlan(g)}>
                {planBusy === g.id ? "拆解中…" : "拆成周任务"}
              </button>
            {/if}
            <button class="btn" disabled={busy === g.id} onclick={() => breakdown(g)} title="按 BKT 掌握度生成复习任务（需绑定科目）">
              {busy === g.id ? "生成中…" : "生成任务"}
            </button>
          </div>
        </div>
        {#if planEdit === g.id}
          <div class="plan-edit">
            <textarea
              rows="8"
              placeholder="粘贴 4–16 周的长期计划 / 课程大纲 / 题目清单。拆解时每条 30–180 分钟，每周最多 6 条。"
              bind:value={planForm.text}
            ></textarea>
            <div class="plan-row">
              <label>周数
                <input type="number" min="1" max="52" bind:value={planForm.weeks} />
              </label>
              <label>第 1 周开始
                <input type="date" bind:value={planForm.start} />
              </label>
              <span class="faint">留空 = 按截止日期倒推</span>
              <button class="btn primary" disabled={planBusy === g.id} onclick={() => savePlan(g)}>
                {planBusy === g.id ? "保存中…" : "保存"}
              </button>
            </div>
          </div>
        {/if}
      {/each}
      {#if goalMsg}<p class="note">{goalMsg}</p>{/if}
    </section>
  {/if}

  {#if error}
    <div class="err">{error}</div>
  {/if}

  {#if loading}
    <div class="empty">正在加载…</div>
  {:else}
    <div class="grid">
      {#each week as day, i (day)}
        <div
          class="col"
          class:today={i === 0}
          role="group"
          aria-label={`${weekdayLabel(day)} ${day}`}
          ondragover={(e) => e.preventDefault()}
          ondrop={(e) => onDrop(e, day)}
        >
          <div class="colhead">
            <span class="wd">{weekdayLabel(day)}</span>
            <span class="dn">{dayNumber(day)}</span>
          </div>

          {#if byDay[day]?.length}
            {#each byDay[day] as row (row.id)}
              <div
                class="card"
                class:done={row.status === "done"}
                class:moved={row.status === "moved"}
                class:busy={busy === row.id}
                draggable="true"
                ondragstart={(e) => {
                  dragId = row.id;
                  e.dataTransfer?.setData("text/plain", row.id);
                }}
                ondragend={() => (dragId = null)}
                role="button"
                tabindex="0"
                onkeydown={(e) => e.key === "Enter" && complete(row)}
              >
                <div class="ctop">
                  <span class="time">{hhmm(row.start_min)}–{hhmm(row.end_min)}</span>
                  <button
                    class="tick"
                    aria-label="完成"
                    onclick={(e) => {
                      e.stopPropagation();
                      complete(row);
                    }}
                  >
                    {#if row.status === "done"}<Icon name="check" size={9} />{/if}
                  </button>
                </div>
                <div class="ctitle">{row.title}</div>
                {#if row.origin === "manual"}
                  <div class="corigin">手动固定</div>
                {/if}
              </div>
            {/each}
          {:else}
            <div class="blank">—</div>
          {/if}
        </div>
      {/each}
    </div>

    {#if outcome}
      <section class="result">
        <div class="rhead">
          <h2>排期结果</h2>
          <span class="faint">
            {outcome.scheduled_items} 项已排 · {minutesLabel(outcome.minutes_planned)} / 容量
            {minutesLabel(outcome.minutes_capacity)}
            {#if doneCount}· 已完成 {doneCount} 块{/if}
          </span>
        </div>

        {#each outcome.notes as note}
          <p class="note">{note}</p>
        {/each}

        {#if outcome.unscheduled.length}
          <div class="unsched">
            <div class="uhead">排不下的（需要提高容量或推迟目标）</div>
            {#each outcome.unscheduled as u (u.item_id)}
              <div class="urow">
                <span class="utitle">{u.title}</span>
                <span class="umin">{u.minutes} 分钟</span>
              </div>
            {/each}
          </div>
        {/if}
      </section>
    {/if}
  {/if}
</div>

<style>
  .plan {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 24px 28px 60px;
  }
  .head {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: 24px;
    flex-wrap: wrap;
    margin-bottom: 12px;
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
  .controls {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .cap {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--t-xs);
    color: var(--fg-muted);
  }
  .cap input {
    width: 64px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--fg);
    font-size: var(--t-xs);
    padding: 4px 6px;
    outline: none;
  }
  .cap input:focus {
    border-color: var(--accent-dim);
  }
  .btn {
    border: 1px solid var(--border);
    background: transparent;
    color: var(--fg-muted);
    font-size: var(--t-xs);
    padding: 5px 12px;
    border-radius: 7px;
    cursor: pointer;
  }
  .btn:hover:not(:disabled) {
    color: var(--fg-bright);
    border-color: var(--border-strong);
  }
  .btn.primary {
    border-color: var(--accent-dim);
    color: var(--accent);
  }
  .btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .bar {
    height: 3px;
    background: var(--surface-2);
    border-radius: 2px;
    overflow: hidden;
    margin-bottom: 20px;
  }
  .fill {
    height: 100%;
    background: var(--accent-dim);
    transition: width 0.2s;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(7, minmax(120px, 1fr));
    gap: 8px;
    align-items: start;
  }
  @media (max-width: 1100px) {
    .grid {
      grid-template-columns: repeat(3, minmax(140px, 1fr));
    }
  }
  .col {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 8px;
    min-height: 120px;
  }
  .col.today {
    border-color: var(--accent-dim);
  }
  .colhead {
    display: flex;
    align-items: baseline;
    gap: 6px;
    padding: 2px 2px 8px;
  }
  .wd {
    font-size: 11px;
    color: var(--fg-faint);
    letter-spacing: 0.06em;
  }
  .dn {
    font-family: var(--font-mono, monospace);
    font-size: var(--t-md);
    color: var(--fg-bright);
  }
  .col.today .dn {
    color: var(--accent);
  }
  .blank {
    text-align: center;
    color: var(--fg-faint);
    font-size: 11px;
    padding: 12px 0;
  }

  .card {
    border: 1px solid var(--border);
    border-left: 2px solid var(--accent-dim);
    background: var(--surface-2);
    border-radius: 0 7px 7px 0;
    padding: 6px 7px;
    margin-bottom: 6px;
    cursor: grab;
  }
  .card:hover {
    border-color: var(--border-strong);
  }
  .card.done {
    opacity: 0.5;
  }
  .card.done .ctitle {
    text-decoration: line-through;
  }
  .card.moved {
    opacity: 0.4;
    border-left-color: var(--warn);
  }
  .card.busy {
    opacity: 0.35;
  }
  .ctop {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 4px;
  }
  .time {
    font-family: var(--font-mono, monospace);
    font-size: 10px;
    color: var(--fg-faint);
  }
  .tick {
    width: 13px;
    height: 13px;
    flex: none;
    border: 1px solid var(--border-strong);
    border-radius: 3px;
    background: transparent;
    color: var(--accent-fg);
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    padding: 0;
  }
  .card.done .tick {
    background: var(--accent);
    border-color: var(--accent);
  }
  .ctitle {
    font-size: 12px;
    color: var(--fg);
    line-height: 1.4;
    margin-top: 3px;
    word-break: break-word;
  }
  .corigin {
    font-size: 10px;
    color: var(--fg-faint);
    margin-top: 3px;
  }

  .result {
    margin-top: 26px;
    border-top: 1px solid var(--border);
    padding-top: 14px;
  }
  .rhead {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
  }
  .rhead h2 {
    margin: 0;
    font-size: 12px;
    font-weight: 500;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }
  .faint {
    font-size: 11px;
    color: var(--fg-faint);
  }
  .note {
    margin: 8px 0 0;
    font-size: var(--t-xs);
    color: var(--fg-muted);
  }
  .unsched {
    margin-top: 12px;
    border: 1px solid var(--warn);
    border-radius: 8px;
    padding: 8px 10px;
  }
  .uhead {
    font-size: 11px;
    color: var(--warn);
    margin-bottom: 6px;
  }
  .urow {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    font-size: var(--t-xs);
    color: var(--fg);
    padding: 2px 0;
  }
  .umin {
    color: var(--fg-faint);
    font-family: var(--font-mono, monospace);
    font-size: 11px;
  }
  .err {
    margin-bottom: 16px;
    padding: 8px 10px;
    border: 1px solid var(--err);
    border-radius: 8px;
    color: var(--err);
    font-size: var(--t-xs);
  }

  .goals {
    border: 1px solid var(--border);
    border-radius: 10px;
    background: var(--surface);
    padding: 10px 12px;
    margin-bottom: 18px;
  }
  .ghead {
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--fg-faint);
    margin-bottom: 8px;
  }
  .grow {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 4px 0;
  }
  .ginfo {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .gtitle {
    font-size: var(--t-sm);
    color: var(--fg);
  }
  .gmeta {
    font-size: 11px;
    color: var(--fg-faint);
  }
  .gbtns {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    justify-content: flex-end;
  }
  .plan-edit {
    margin: 6px 0 10px;
    padding: 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .plan-edit textarea {
    width: 100%;
    resize: vertical;
    font: inherit;
    font-size: var(--t-sm);
    line-height: 1.5;
    padding: 8px;
    border-radius: 6px;
    border: 1px solid var(--border);
    background: var(--bg);
    color: var(--fg);
  }
  .plan-row {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
    font-size: 12px;
    color: var(--fg-faint);
  }
  .plan-row input,
  .pick {
    background: var(--bg);
    color: var(--fg);
    border: 1px solid var(--border);
    border-radius: 5px;
    padding: 3px 6px;
    font: inherit;
    font-size: 12px;
  }
  .plan-row input[type="number"] {
    width: 64px;
  }
  .pick {
    max-width: 190px;
  }
  .empty {
    padding: 40px 0;
    text-align: center;
    color: var(--fg-muted);
    font-size: var(--t-sm);
  }
</style>
