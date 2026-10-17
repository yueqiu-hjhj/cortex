<script lang="ts">
  // Sprint 规划 — 目标分 × 掌握度差距 × 剩余天数 × 每日投入。
  // 后端 sprint_plan 用 BKT mastery（无则回退近期正确率，再回退 coverage）
  // 估出每个知识点的差距与所需分钟数，并判断每日预算是否吃得下。
  import { app } from "../lib/store.svelte";
  import * as api from "../lib/api";
  import Icon from "../components/Icon.svelte";
  import DatePicker from "../components/DatePicker.svelte";

  function isoDate(d: Date) {
    const p = (n: number) => String(n).padStart(2, "0");
    return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
  }
  function defaultDeadline() {
    const d = new Date();
    d.setDate(d.getDate() + 21);
    return isoDate(d);
  }
  function deadlineMs(iso: string) {
    const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(iso);
    if (!m) return Date.now() + 21 * 86_400_000;
    return new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]), 23, 59, 0).getTime();
  }

  let subjectId = $state<string | null>(app.activeSubjectId);
  let targetScore = $state(85);
  let deadline = $state(defaultDeadline());
  let dailyMinutes = $state(60);

  let plan = $state<api.SprintPlan | null>(null);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let seq = 0;

  // 首帧 activeSubjectId 可能仍是 null（init 尚未落位）——科目到位后补选。
  $effect(() => {
    if (!subjectId && app.subjects.length) {
      subjectId = app.activeSubjectId ?? app.subjects[0].id;
    }
  });

  // 任一输入变化 → 抖动 180ms 后重算（本地 SQLite，很快）。
  $effect(() => {
    const sid = subjectId;
    const ts = targetScore;
    const dl = deadline;
    const dm = dailyMinutes;
    if (!sid) return;
    const t = setTimeout(() => { void generate(sid, ts, dl, dm); }, 180);
    return () => clearTimeout(t);
  });

  async function generate(sid: string, ts: number, dl: string, dm: number) {
    const mine = ++seq;
    loading = true;
    error = null;
    try {
      const score = Math.min(100, Math.max(0, Number.isFinite(ts) ? ts : 85));
      const mins = Math.max(10, Math.round(Number.isFinite(dm) ? dm : 60));
      const res = await api.sprintPlan(sid, score, deadlineMs(dl), mins);
      if (mine === seq) plan = res;
    } catch (e) {
      if (mine === seq) { error = String(e); plan = null; }
    } finally {
      if (mine === seq) loading = false;
    }
  }

  const pct = (v: number) => `${Math.round(Math.max(0, Math.min(1, v)) * 100)}%`;

  function statusMeta(status: string) {
    if (status === "critical") return { label: "关键", color: "var(--err)" };
    if (status === "focus") return { label: "重点", color: "var(--warn)" };
    return { label: "稳固", color: "var(--ok)" };
  }
  function fmtMin(m: number) {
    if (m < 60) return `${m} 分`;
    const h = Math.floor(m / 60);
    const r = m % 60;
    return r ? `${h} 时 ${r} 分` : `${h} 时`;
  }
</script>

<div class="sp">
  <header class="sp-head">
    <div>
      <div class="eyebrow">冲刺规划</div>
      <h1 class="sp-title">自适应冲刺规划</h1>
      <p class="sp-sub mono">目标分 × 掌握度差距 × 剩余天数 × 每日投入{#if loading} · 重算中…{/if}</p>
    </div>
    <div class="sp-controls">
      <label class="fld">
        <span class="fld-l">科目</span>
        <select bind:value={subjectId} class="ipt sel">
          {#each app.subjects as s (s.id)}
            <option value={s.id}>{s.name}</option>
          {/each}
        </select>
      </label>
      <label class="fld">
        <span class="fld-l">目标分</span>
        <input class="ipt num" type="number" min="0" max="100" step="1" bind:value={targetScore} />
      </label>
      <label class="fld">
        <span class="fld-l">截止</span>
        <DatePicker value={deadline} onChange={(v: string) => (deadline = v)} placeholder="选择日期" />
      </label>
      <label class="fld">
        <span class="fld-l">每日投入 (分)</span>
        <input class="ipt num" type="number" min="10" max="600" step="5" bind:value={dailyMinutes} />
      </label>
    </div>
  </header>

  {#if !app.subjects.length}
    <div class="sp-empty">
      <div class="sp-empty-big">还没有科目</div>
      <div class="sp-empty-sub mono">先创建一个科目并导入材料，规划器才有知识点可以排。</div>
    </div>
  {:else if error}
    <div class="sp-empty">
      <div class="sp-empty-big">无法生成计划</div>
      <div class="sp-empty-sub mono">{error}</div>
    </div>
  {:else if plan}
    <div class="banner" class:bad={!plan.feasible}>
      <Icon
        name={plan.feasible ? "check" : "bolt"}
        size={15}
        color={plan.feasible ? "var(--ok)" : "var(--warn)"}
      />
      <div class="banner-body">
        <div class="banner-t">{plan.feasible ? "预算可行" : "预算吃紧"}</div>
        <div class="banner-s mono">
          需要 {fmtMin(plan.minutes_required)} · 可用 {fmtMin(plan.minutes_available)}
          （{plan.days_left} 天 × {plan.daily_minutes} 分/天）
        </div>
      </div>
      <div class="banner-when mono">
        <b>{plan.days_left}</b> 天
      </div>
    </div>

    <div class="kpis">
      <div class="kpi"><i>当前掌握</i><b>{pct(plan.mastery_now)}</b></div>
      <div class="kpi"><i>目标掌握</i><b>{pct(plan.mastery_target)}</b><span class="faint">({plan.target_score} 分)</span></div>
      <div class="kpi"><i>每日需投入</i><b>{plan.per_day_minutes}</b><span class="faint">分</span></div>
      <div class="kpi"><i>待补知识点</i><b>{plan.topics.filter((t) => t.gap > 0).length}</b><span class="faint">/ {plan.topics.length}</span></div>
    </div>

    <section class="tops">
      <div class="tops-h">
        <span class="eyebrow">优先级</span>
        <span class="mono faint">{plan.subject_name} · {plan.topics.length} 个知识点</span>
      </div>
      {#if plan.topics.length === 0}
        <div class="sp-empty-sub mono" style="padding:14px 2px">该科目还没有知识点——先导入材料并生成大纲。</div>
      {:else}
        <div class="top-head mono">
          <span>#</span><span>知识点</span><span>掌握度</span><span>差距</span><span>预计</span><span>状态</span>
        </div>
        {#each plan.topics as t (t.topic_id)}
          {@const meta = statusMeta(t.status)}
          <div class="top-row">
            <span class="top-rank mono">{t.rank}</span>
            <span class="top-name read" title={t.topic_name}>{t.topic_name}</span>
            <span class="top-bar">
              <span class="bar"><span class="bar-fill" style:width={pct(t.mastery)} style:background={meta.color}></span></span>
              <span class="bar-n mono">{pct(t.mastery)}</span>
            </span>
            <span class="top-gap mono">{t.gap > 0 ? `−${Math.round(t.gap * 100)}` : "0"}</span>
            <span class="top-min mono">{t.minutes > 0 ? `${t.minutes}分` : "—"}</span>
            <span class="chip mono" style:color={meta.color} style:border-color={meta.color}>{meta.label}</span>
          </div>
        {/each}
      {/if}
    </section>

    {#if plan.notes.length}
      <ul class="notes mono">
        {#each plan.notes as n}<li>{n}</li>{/each}
      </ul>
    {/if}
  {:else}
    <div class="sp-empty">
      <div class="sp-empty-big">正在生成计划…</div>
    </div>
  {/if}
</div>

<style>
  .sp {
    flex: 1; min-height: 0; overflow: auto;
    padding: 22px 28px 32px;
    display: flex; flex-direction: column; gap: 16px;
  }
  .eyebrow {
    font-family: var(--font-mono);
    font-size: 10.5px;
    color: var(--fg-faint);
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }
  .faint { color: var(--fg-faint); }
  .mono { font-family: var(--font-mono); }
  .read { font-family: var(--font-read); }

  /* Header */
  .sp-head {
    display: flex; align-items: flex-end; justify-content: space-between; gap: 16px;
    border-bottom: 1px solid var(--border); padding-bottom: 14px; flex-wrap: wrap;
  }
  .sp-title {
    margin: 4px 0 0;
    font-family: var(--font-display);
    font-size: 22px; font-weight: 600; letter-spacing: -0.01em;
    color: var(--fg-bright);
  }
  .sp-sub { margin: 6px 0 0; font-size: 11.5px; color: var(--fg-muted); }
  .sp-controls { display: flex; align-items: flex-end; gap: 10px; flex-wrap: wrap; }
  .fld { display: flex; flex-direction: column; gap: 4px; }
  .fld-l { font-family: var(--font-mono); font-size: 10px; color: var(--fg-faint); text-transform: uppercase; letter-spacing: 0.04em; }
  .ipt {
    font-family: var(--font-mono); font-size: 12px; color: var(--fg);
    background: var(--surface-2); border: 1px solid var(--border);
    border-radius: 7px; padding: 6px 9px; outline: none;
  }
  .ipt:focus { border-color: var(--accent-dim); color: var(--fg-bright); }
  .ipt.num { width: 84px; }
  .ipt.sel { min-width: 140px; }

  /* Feasibility banner */
  .banner {
    display: flex; align-items: center; gap: 12px;
    padding: 12px 15px; border-radius: 8px;
    background: color-mix(in srgb, var(--ok) 8%, var(--surface));
    border: 1px solid color-mix(in srgb, var(--ok) 34%, transparent);
  }
  .banner.bad {
    background: color-mix(in srgb, var(--warn) 9%, var(--surface));
    border-color: color-mix(in srgb, var(--warn) 40%, transparent);
  }
  .banner-body { flex: 1; min-width: 0; }
  .banner-t { font-family: var(--font-display); font-size: 14px; font-weight: 600; color: var(--fg-bright); }
  .banner-s { margin-top: 3px; font-size: 11.5px; color: var(--fg-muted); }
  .banner-when { font-size: 12px; color: var(--fg-muted); white-space: nowrap; }
  .banner-when b { font-size: 18px; color: var(--fg-bright); font-weight: 600; }

  /* KPI strip */
  .kpis { display: flex; gap: 8px; flex-wrap: wrap; }
  .kpi {
    display: inline-flex; align-items: baseline; gap: 5px;
    font-family: var(--font-mono); font-size: 11px; color: var(--fg-faint);
    background: var(--surface); border: 1px solid var(--border);
    border-radius: 8px; padding: 6px 12px;
  }
  .kpi b { font-size: 17px; color: var(--fg-bright); font-weight: 600; }
  .kpi i { font-style: normal; color: var(--fg-muted); }

  /* Topic table */
  .tops { border: 1px solid var(--border); border-radius: 8px; background: var(--surface); overflow: hidden; }
  .tops-h {
    display: flex; align-items: center; justify-content: space-between; gap: 10px;
    padding: 11px 14px; border-bottom: 1px solid var(--border);
  }
  .top-head, .top-row {
    display: grid;
    grid-template-columns: 34px minmax(160px, 1fr) 168px 52px 68px 60px;
    align-items: center; gap: 10px;
    padding: 9px 14px;
  }
  .top-head {
    font-size: 10px; color: var(--fg-faint); text-transform: uppercase; letter-spacing: 0.04em;
    border-bottom: 1px solid var(--border);
  }
  .top-row { border-bottom: 1px solid color-mix(in srgb, var(--border) 55%, transparent); }
  .top-row:last-child { border-bottom: none; }
  .top-row:hover { background: var(--surface-2); }
  .top-rank { font-size: 12px; color: var(--fg-faint); }
  .top-name {
    font-size: 13px; color: var(--fg-bright);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .top-bar { display: flex; align-items: center; gap: 8px; }
  .bar {
    flex: 1; height: 6px; border-radius: 999px;
    background: var(--surface-3); overflow: hidden;
  }
  .bar-fill { display: block; height: 100%; border-radius: 999px; transition: width var(--dur-slow); }
  .bar-n { font-size: 11px; color: var(--fg-muted); min-width: 34px; text-align: right; }
  .top-gap { font-size: 12px; color: var(--fg-muted); }
  .top-min { font-size: 11.5px; color: var(--fg-muted); }
  .chip {
    justify-self: start;
    font-size: 10.5px; padding: 2px 8px; border-radius: 999px;
    border: 1px solid currentColor;
    background: color-mix(in srgb, currentColor 10%, transparent);
  }

  /* Empty / notes */
  .sp-empty {
    border: 1px dashed var(--border); border-radius: 8px;
    background: var(--surface);
    padding: 48px 24px; text-align: center;
  }
  .sp-empty-big {
    font-family: var(--font-display); font-size: 18px; font-weight: 600;
    color: var(--fg-bright);
  }
  .sp-empty-sub {
    margin: 8px auto 0; max-width: 460px;
    font-size: 11.5px; color: var(--fg-muted);
  }
  .notes {
    margin: 0; padding: 12px 14px 12px 30px;
    border: 1px solid var(--border); border-radius: 8px;
    background: var(--surface); color: var(--fg-muted);
    font-size: 11.5px; line-height: 1.7;
  }
</style>
