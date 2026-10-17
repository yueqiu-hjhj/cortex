<script lang="ts">
  import { app } from "../lib/store.svelte";
  import * as api from "../lib/api";
  import Icon from "../components/Icon.svelte";

  // The whole dashboard arrives in ONE call (see repo::analytics_summary) so a
  // single DB lock backs every chart below. Reloaded when the view mounts and
  // whenever the active subject changes (cheap; keeps figures fresh after study).
  let data = $state<api.AnalyticsSummary | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  // Selectable window for the per-day charts + tables (the heatmap is always a year).
  let range = $state(30);
  const RANGES = [
    { d: 7, label: "7d" },
    { d: 30, label: "30d" },
    { d: 90, label: "90d" },
  ];
  async function load() {
    loading = true;
    error = null;
    try {
      data = await api.analyticsSummary(range);
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }
  $effect(() => {
    void range; // reload when the window changes
    void app.activeSubjectId;
    load();
  });

  // ── 计划 vs 实际 ──
  // 计划的分钟来自 plan_blocks，实际分钟来自被勾选完成的块（完成时会写回
  // work_items.actual_min）。两个数字都在同一个库里，所以这里是纯读。
  let planVs = $state<api.PlanVsActual | null>(null);
  const PLAN_WINDOW = 14;
  function localDay(): string {
    const d = new Date();
    const p = (n: number) => String(n).padStart(2, "0");
    return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
  }
  async function loadPlanVs() {
    try {
      planVs = await api.planVsActual(localDay(), PLAN_WINDOW);
    } catch {
      planVs = null;
    }
  }
  $effect(() => {
    void app.view;
    void loadPlanVs();
  });
  const planMax = $derived(
    Math.max(1, ...(planVs?.days ?? []).map((d) => Math.max(d.planned_min, d.done_min)))
  );
  const hasPlan = $derived((planVs?.planned_min ?? 0) > 0);
  function planDayNum(day: string): string {
    return String(Number(day.split("-")[2]));
  }

  // ── per-day time-series (study minutes + reviews/accuracy) ──
  const mpd = $derived(data?.minutes_per_day ?? []);
  const maxMin = $derived(Math.max(1, ...mpd.map((d) => d.minutes)));
  const rpd = $derived(data?.reviews_per_day ?? []);
  const maxRev = $derived(Math.max(1, ...rpd.map((d) => d.reviews)));
  // Consistency: how many days in the window you studied at all.
  const daysStudied = $derived(mpd.filter((d) => d.minutes > 0).length);
  const consistency = $derived(mpd.length ? daysStudied / mpd.length : 0);
  // Label every Nth bar so a 90-day axis doesn't turn into a smear.
  const labelEvery = $derived(mpd.length > 45 ? 14 : mpd.length > 14 ? 7 : 1);
  function dayNum(iso: string): string {
    const [, m, d] = iso.split("-").map(Number);
    return `${d}/${m}`;
  }
  // ── pomodoro / focus sessions ──
  const pomo = $derived(data?.pomodoro ?? null);
  const maxHour = $derived(Math.max(1, ...(pomo?.by_hour ?? [0])));

  // ── subject pill-switcher → per-topic stats (radar) ──
  let topicSubject = $state<string>("");
  $effect(() => { if (!topicSubject && app.activeSubjectId) topicSubject = app.activeSubjectId; });
  let topics = $state<api.TopicStat[]>([]);
  $effect(() => {
    const sid = topicSubject;
    void range;
    if (!sid) { topics = []; return; }
    let cancelled = false;
    api.topicStats(sid, range).then((t) => { if (!cancelled) topics = t; }).catch(() => { if (!cancelled) topics = []; });
    return () => { cancelled = true; };
  });
  // Top topics (by activity) for the radar — keep it readable.
  const radarTopics = $derived(
    [...topics].sort((a, b) => (b.reviews + b.cards) - (a.reviews + a.cards)).slice(0, 8)
  );

  // ── tiny SVG helpers (donuts, radial clock, radar) ──
  const TAU = Math.PI * 2;
  function polar(cx: number, cy: number, r: number, frac: number): [number, number] {
    const a = frac * TAU - Math.PI / 2; // 0 = top, clockwise
    return [cx + r * Math.cos(a), cy + r * Math.sin(a)];
  }
  // accuracy/retention donut ring (fraction 0..1) → stroke-dasharray on r=normRadius
  function ringDash(frac: number, r: number): string {
    const c = TAU * r;
    return `${Math.max(0, Math.min(1, frac)) * c} ${c}`;
  }
  // radar polygon points string for a set of values (0..1) evenly spaced
  function radarPoints(vals: number[], cx: number, cy: number, r: number): string {
    return vals.map((v, i) => polar(cx, cy, r * Math.max(0.02, v), i / vals.length).join(",")).join(" ");
  }
  // Radar metric — default "engagement" (content + study), since review accuracy
  // alone is near-zero before you've reviewed much.
  let radarMetric = $state<"engagement" | "accuracy" | "cards">("engagement");
  const engScore = (t: api.TopicStat) => t.sources + t.materials * 1.5 + t.cards + t.reviews * 0.5;
  const radarValues = $derived.by(() => {
    const ts = radarTopics;
    let raw: number[];
    if (radarMetric === "accuracy") {
      raw = ts.map((t) => (t.reviews > 0 ? t.accuracy : 0));
      return raw; // accuracy is already a meaningful 0..1 grade — keep it linear
    } else if (radarMetric === "cards") {
      const mx = Math.max(1, ...ts.map((t) => t.cards));
      raw = ts.map((t) => t.cards / mx);
    } else {
      const mx = Math.max(1, ...ts.map(engScore));
      raw = ts.map((t) => engScore(t) / mx);
    }
    // Perceptual (sqrt) scaling for the count-based metrics: amplifies small
    // values so sparse early data is still legible, and compresses as it grows —
    // the top topic still reaches the rim. A topic with ANY data clears the centre.
    return raw.map((v) => (v > 0 ? Math.max(0.12, Math.sqrt(v)) : 0));
  });
  const radarMetricLabel = $derived(
    radarMetric === "accuracy" ? "review accuracy" : radarMetric === "cards" ? "flashcards" : "engagement (sources · materials · cards)"
  );
  const accentColor = $derived(topicSubject ? app.subjectColor(app.subjects.find((s) => s.id === topicSubject) ?? null) : "var(--accent)");

  // Subject lookups come from the already-loaded subject list, not extra queries.
  function subjectName(id: string): string {
    return app.subjects.find((s) => s.id === id)?.name ?? "Unknown subject";
  }
  function subjectColor(id: string): string {
    return app.subjectColor(app.subjects.find((s) => s.id === id) ?? null);
  }

  // ── headline figures ──
  const streak = $derived(data?.streak ?? 0);
  const longestStreak = $derived(data?.longest_streak ?? 0);
  // Today last cell: any study minutes today means streak is safe.
  const studiedToday = $derived(((data?.year_minutes ?? []).slice(-1)[0]?.minutes ?? 0) > 0);
  const minutesWeek = $derived(data?.minutes_week ?? 0);
  const reviewsWeek = $derived(data?.reviews_week ?? 0);
  const accuracyWeek = $derived(data?.accuracy_week ?? 0);

  // Format minutes as "Hh Mm" / "Mm" so the cards read naturally.
  function fmtMins(m: number): string {
    if (m <= 0) return "0m";
    const h = Math.floor(m / 60);
    const min = m % 60;
    return h > 0 ? (min > 0 ? `${h}h ${min}m` : `${h}h`) : `${min}m`;
  }
  const pct = (x: number) => `${Math.round(x * 100)}%`;

  // Short weekday/day label for an ISO "YYYY-MM-DD" (local date string).
  function dayLabel(iso: string): string {
    const [y, m, d] = iso.split("-").map(Number);
    return new Date(y, m - 1, d).toLocaleDateString(undefined, { weekday: "short" });
  }

  // ── focus-hours contributions heatmap (GitHub-style; pure CSS grid) ──
  // The backend hands us a full rolling year (366 days, oldest → newest, gaps
  // already filled with 0). We bucket it into week-columns of 7 day-rows
  // (Mon–Sun), today in the last column, the first column padded so each row is
  // a fixed weekday. No chart library — just a CSS grid of cells.
  type Cell = { date: string; minutes: number; level: number } | null;
  const yearDays = $derived(data?.year_minutes ?? []);

  // Weekday index with Monday = 0 … Sunday = 6 (JS getDay() has Sunday = 0).
  function mondayIdx(iso: string): number {
    const [y, m, d] = iso.split("-").map(Number);
    return (new Date(y, m - 1, d).getDay() + 6) % 7;
  }

  // Intensity thresholds from the user's OWN nonzero distribution (quartiles of
  // the max), so a light user still sees 4 shades rather than one faint band.
  const yearMax = $derived(Math.max(0, ...yearDays.map((d) => d.minutes)));
  function levelFor(min: number): number {
    if (min <= 0 || yearMax <= 0) return 0;
    const q = min / yearMax;
    if (q <= 0.25) return 1;
    if (q <= 0.5) return 2;
    if (q <= 0.75) return 3;
    return 4;
  }

  // Build week-columns. Pad the FIRST column with leading nulls so the oldest
  // day lands on its true weekday row (keeps Mon/Wed/Fri labels aligned).
  const weeks = $derived.by<Cell[][]>(() => {
    if (!yearDays.length) return [];
    const cols: Cell[][] = [];
    let col: Cell[] = [];
    const firstRow = mondayIdx(yearDays[0].day);
    for (let i = 0; i < firstRow; i++) col.push(null); // pad partial first week
    for (const d of yearDays) {
      col.push({ date: d.day, minutes: d.minutes, level: levelFor(d.minutes) });
      if (col.length === 7) {
        cols.push(col);
        col = [];
      }
    }
    if (col.length) {
      while (col.length < 7) col.push(null); // pad trailing partial week
      cols.push(col);
    }
    return cols;
  });

  // Month labels along the top: the column index where each month first appears
  // (placed above that week so labels read like GitHub's).
  const monthLabels = $derived.by<{ col: number; label: string }[]>(() => {
    const out: { col: number; label: string }[] = [];
    let lastMonth = -1;
    weeks.forEach((wk, ci) => {
      // Use the first real cell in the column to date the week.
      const cell = wk.find((c) => c !== null);
      if (!cell) return;
      const month = Number(cell.date.split("-")[1]) - 1;
      if (month !== lastMonth) {
        // Skip a label crammed into the very first partial column (GitHub does too).
        if (ci > 0 || mondayIdx(cell.date) === 0) {
          out.push({ col: ci, label: MONTHS[month] });
        }
        lastMonth = month;
      }
    });
    return out;
  });
  const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

  const yearMinutesTotal = $derived(yearDays.reduce((a, d) => a + d.minutes, 0));
  const yearHours = $derived(yearMinutesTotal / 60);
  const heatmapEmpty = $derived(yearDays.length > 0 && yearMinutesTotal === 0);

  // Tooltip — a positioned div (not a title attr), shown on cell hover.
  let tip = $state<{ x: number; y: number; below: boolean; text: string } | null>(null);
  // "2.4h · Tue 14 May" — hours with one decimal when ≥60m, else "45m".
  function tipLabel(c: NonNullable<Cell>): string {
    const [y, m, d] = c.date.split("-").map(Number);
    const dt = new Date(y, m - 1, d);
    const when = dt.toLocaleDateString(undefined, { weekday: "short", day: "numeric", month: "short" });
    const amount = c.minutes >= 60 ? `${(c.minutes / 60).toFixed(1)}h` : `${c.minutes}m`;
    return `${amount} · ${when}`;
  }
  function showTip(e: MouseEvent, c: Cell) {
    if (!c) return;
    const cell = e.currentTarget as HTMLElement;
    const cr = cell.getBoundingClientRect();
    // Viewport coords + position:fixed → the card's overflow can't clip it.
    // Flip below near the top edge; clamp x so it never runs off either side.
    const below = cr.top < 48;
    tip = {
      x: Math.max(64, Math.min(window.innerWidth - 64, cr.left + cr.width / 2)),
      y: below ? cr.bottom + 6 : cr.top - 6,
      below,
      text: tipLabel(c),
    };
  }
  function hideTip() {
    tip = null;
  }

  // ── due-forecast mini bars (next 7 days) ──
  const forecast = $derived(data?.due_forecast ?? []);
  const maxDue = $derived(Math.max(1, ...forecast.map((d) => d.due)));

  // Whole-dashboard empty state: nothing studied AND nothing scheduled.
  const isEmpty = $derived(
    !!data &&
      data.minutes_week === 0 &&
      data.reviews_week === 0 &&
      data.fsrs.cards === 0 &&
      data.per_subject.length === 0 &&
      data.streak === 0
  );

  // ── topics needing the most work ──
  const weakTopics = $derived(data?.weak_topics ?? []);
  function weakSubjectName(t: api.WeakTopic): string {
    return subjectName(t.subject_id);
  }
</script>

<div class="workspace-scroll">
  <div class="dash an">
    <header class="dash-head">
      <div>
        <div class="eyebrow">学习分析</div>
        <h1 class="dash-title">学习分析</h1>
      </div>
    </header>

    {#if loading}
      <div class="an-note">正在加载学习数据…</div>
    {:else if error}
      <div class="an-note an-note--err">Couldn't load analytics: {error}</div>
    {:else if isEmpty}
      <div class="an-empty">
        <div class="an-empty-glyph">📊</div>
        <div class="an-empty-t read">还没有学习数据</div>
        <div class="an-empty-d">
          Start a pomodoro or review some cards — your focus minutes, review
          accuracy and upcoming due cards will show up here.
        </div>
      </div>
    {:else if data}
      <!-- ── headline stat cards ── -->
      <div class="an-stats">
        <div class="an-stat">
          <div class="an-stat-k mono">连续天数</div>
          <div class="an-stat-v">
            {streak}<span class="an-stat-u"> day{streak === 1 ? "" : "s"}</span>
          </div>
          <div class="an-stat-sub mono">
            {#if streak > 0 && !studiedToday}
              <span class="an-stat-warn">快断了 · 今天学一会儿</span>
            {:else if longestStreak > streak}
              best {longestStreak}
            {:else}
              <span class="faint">best {longestStreak}</span>
            {/if}
          </div>
        </div>
        <div class="an-stat">
          <div class="an-stat-k mono">本周专注</div>
          <div class="an-stat-v">{fmtMins(minutesWeek)}</div>
        </div>
        <div class="an-stat">
          <div class="an-stat-k mono">本周复习</div>
          <div class="an-stat-v">{reviewsWeek}</div>
        </div>
        <div class="an-stat">
          <div class="an-stat-k mono">本周正确率</div>
          <div class="an-stat-v">{reviewsWeek > 0 ? pct(accuracyWeek) : "—"}</div>
        </div>
      </div>

      <!-- ── window selector ── -->
      <div class="an-rangebar">
        <span class="an-range-lbl mono">区间</span>
        <div class="seg an-range">
          {#each RANGES as r}
            <button type="button" class={"seg-opt" + (range === r.d ? " on" : "")} onclick={() => (range = r.d)}>{r.label}</button>
          {/each}
        </div>
      </div>

      <!-- ── radial summary row (donuts + focus clock) ── -->
      <div class="an-radials">
        <section class="an-card an-radial">
          <div class="an-card-h"><h3 class="an-card-t mono">正确率 · 7 天</h3></div>
          <svg class="an-donut" viewBox="0 0 120 120">
            <circle cx="60" cy="60" r="48" class="an-donut-bg" />
            <circle cx="60" cy="60" r="48" class="an-donut-fg" style:stroke="var(--ok)" stroke-dasharray={ringDash(accuracyWeek, 48)} transform="rotate(-90 60 60)" />
            <text x="60" y="58" class="an-donut-v">{reviewsWeek > 0 ? pct(accuracyWeek) : "—"}</text>
            <text x="60" y="76" class="an-donut-k">{reviewsWeek} reviews</text>
          </svg>
        </section>

        {#if pomo}
          <section class="an-card an-radial">
            <div class="an-card-h"><h3 class="an-card-t mono">你的专注时段</h3></div>
            <svg class="an-clock" viewBox="0 0 120 120">
              <circle cx="60" cy="60" r="52" class="an-donut-bg" />
              {#each pomo.by_hour as min, h (h)}
                {@const p1 = polar(60, 60, 16, h / 24)}
                {@const p2 = polar(60, 60, 16 + (min > 0 ? Math.max(0.32, min / maxHour) : 0.07) * 38, h / 24)}
                <line x1={p1[0]} y1={p1[1]} x2={p2[0]} y2={p2[1]} class="an-spoke" class:zero={min === 0} style:stroke={min > 0 ? accentColor : "var(--border)"}><title>{min > 0 ? fmtMins(min) : "无专注"} · {h}:00</title></line>
              {/each}
              <text x="60" y="64" class="an-clock-c mono">24h</text>
            </svg>
          </section>
        {/if}

        <section class="an-card an-radial">
          <div class="an-card-h"><h3 class="an-card-t mono">Consistency · {range}d</h3></div>
          <svg class="an-donut" viewBox="0 0 120 120">
            <circle cx="60" cy="60" r="48" class="an-donut-bg" />
            <circle cx="60" cy="60" r="48" class="an-donut-fg" style:stroke="var(--accent)" stroke-dasharray={ringDash(consistency, 48)} transform="rotate(-90 60 60)" />
            <text x="60" y="58" class="an-donut-v">{pct(consistency)}</text>
            <text x="60" y="76" class="an-donut-k">{daysStudied}/{mpd.length} days</text>
          </svg>
        </section>
      </div>

      <!-- ── trends: focus minutes + reviews per day ── -->
      <section class="an-card">
        <div class="an-card-h"><h3 class="an-card-t mono">Focus minutes · last {range}d</h3></div>
        <div class="an-ts">
          {#each mpd as d, i (d.day)}
            <div class="an-ts-col">
              <div class="an-ts-track"><div class="an-ts-fill" style:height={pct(d.minutes / maxMin)} class:zero={d.minutes === 0}><title>{fmtMins(d.minutes)} · {d.day}</title></div></div>
              {#if i % labelEvery === 0}<span class="an-ts-x mono">{planDayNum(d.day)}</span>{/if}
            </div>
          {/each}
        </div>
      </section>

      <section class="an-card">
        <div class="an-card-h"><h3 class="an-card-t mono">Reviews · last {range}d</h3></div>
        <div class="an-ts">
          {#each rpd as d, i (d.day)}
            <div class="an-ts-col">
              <div class="an-ts-track"><div class="an-ts-fill rev" style:height={pct(d.reviews / maxRev)} class:zero={d.reviews === 0}><title>{d.reviews} reviews · {d.reviews > 0 ? pct(d.accuracy) : "—"} acc · {d.day}</title></div></div>
              {#if i % labelEvery === 0}<span class="an-ts-x mono">{dayNum(d.day)}</span>{/if}
            </div>
          {/each}
        </div>
      </section>

      <!-- ── pomodoro / focus sessions ── -->
      {#if pomo && pomo.focus_sessions > 0}
        <section class="an-card">
          <div class="an-card-h"><h3 class="an-card-t mono">专注记录</h3><span class="an-card-sub mono">{fmtMins(pomo.focus_minutes)} 已专注</span></div>
          <div class="an-pomo">
            <div class="an-pomo-stat"><div class="an-pomo-v">{pomo.focus_sessions}</div><div class="an-pomo-k mono">段数</div></div>
            <div class="an-pomo-stat"><div class="an-pomo-v">{Math.round(pomo.avg_session_min)}<span class="an-stat-u">m</span></div><div class="an-pomo-k mono">平均时长</div></div>
            <div class="an-pomo-stat"><div class="an-pomo-v">{pomo.longest_session_min}<span class="an-stat-u">m</span></div><div class="an-pomo-k mono">最长</div></div>
            <div class="an-pomo-stat"><div class="an-pomo-v">{fmtMins(pomo.break_minutes)}</div><div class="an-pomo-k mono">休息</div></div>
          </div>
        </section>
      {/if}

      <!-- ── 计划 vs 实际 ── -->
      {#if planVs && hasPlan}
        <section class="an-card">
          <div class="an-card-h">
            <h3 class="an-card-t mono">计划 vs 实际</h3>
            <span class="an-card-sub mono">
              近 {PLAN_WINDOW} 天完成率 {Math.round(planVs.adherence * 100)}% ·
              {fmtMins(planVs.done_min)} / {fmtMins(planVs.planned_min)}
            </span>
          </div>
          <div class="pva">
            {#each planVs.days as d (d.day)}
              <div class="pva-col" title="{d.day} · 计划 {fmtMins(d.planned_min)} · 完成 {fmtMins(d.done_min)}">
                <div class="pva-bars">
                  <div class="pva-bar planned" style:height="{Math.max(2, (d.planned_min / planMax) * 100)}%"></div>
                  <div class="pva-bar done" style:height="{Math.max(2, (d.done_min / planMax) * 100)}%"></div>
                </div>
                <span class="pva-d mono">{dayNum(d.day)}</span>
              </div>
            {/each}
          </div>
          <div class="pva-legend">
            <span><i class="sw planned"></i> 计划</span>
            <span><i class="sw done"></i> 完成</span>
            <span class="faint">估算 {fmtMins(planVs.estimate_min)} · 实际 {fmtMins(planVs.actual_min)}</span>
          </div>
        </section>
      {/if}

      <!-- ── topic mastery radar (subject pill switcher) ── -->
      {#if app.subjects.length}
        <section class="an-card">
          <div class="an-card-h">
            <h3 class="an-card-t mono">主题掌握度</h3>
            <div class="an-pills">
              {#each app.subjects as s (s.id)}
                <button type="button" class={"an-pill" + (topicSubject === s.id ? " on" : "")} onclick={() => (topicSubject = s.id)} style:--pill={app.subjectColor(s)}>{s.name}</button>
              {/each}
            </div>
          </div>
          <div class="an-metricbar">
            {#each [{ id: "engagement", label: "Engagement" }, { id: "accuracy", label: "Accuracy" }, { id: "cards", label: "Cards" }] as m}
              <button type="button" class={"an-metric" + (radarMetric === m.id ? " on" : "")} onclick={() => (radarMetric = m.id as typeof radarMetric)}>{m.label}</button>
            {/each}
          </div>
          {#if radarTopics.length >= 3}
            <div class="an-radar-wrap">
              <svg class="an-radar" viewBox="0 0 240 240">
                {#each [0.25, 0.5, 0.75, 1] as g}<circle cx="120" cy="120" r={90 * g} class="an-radar-ring" />{/each}
                {#each radarTopics as t, i (t.topic_id)}
                  {@const p = polar(120, 120, 90, i / radarTopics.length)}
                  {@const lp = polar(120, 120, 106, i / radarTopics.length)}
                  <line x1="120" y1="120" x2={p[0]} y2={p[1]} class="an-radar-axis" />
                  <text x={lp[0]} y={lp[1]} class="an-radar-lbl" text-anchor="middle">{t.topic_name.length > 11 ? t.topic_name.slice(0, 10) + "…" : t.topic_name}</text>
                {/each}
                <polygon points={radarPoints(radarValues, 120, 120, 90)} class="an-radar-poly" style:fill={accentColor} style:stroke={accentColor} />
                {#each radarTopics as t, i (t.topic_id)}
                  {@const pt = polar(120, 120, 90 * Math.max(0.02, radarValues[i]), i / radarTopics.length)}
                  <circle cx={pt[0]} cy={pt[1]} r="3.5" class="an-radar-dot" style:fill={accentColor}><title>{t.topic_name}: {t.sources} sources · {t.materials} materials · {t.cards} cards · {t.reviews > 0 ? pct(t.accuracy) + " acc" : "no reviews"}</title></circle>
                {/each}
              </svg>
              <div class="an-radar-note mono">distance from centre = {radarMetricLabel} · hover a point for detail</div>
            </div>
          {:else}
            <p class="an-empty-d">主题活动还不够 — 复习这个科目各主题的卡片，掌握度雷达就会长出来。</p>
          {/if}
        </section>
      {/if}

      <!-- ── focus-hours contributions heatmap (last year) ── -->
      <section class="an-card hm-card">
        <div class="an-card-h">
          <h3 class="an-card-t mono">专注时长 · 近一年</h3>
          <span class="an-card-sub mono">{yearHours.toFixed(1)}h total</span>
        </div>

        <!-- Scroll wrapper: the full year is ~53 weeks wide; if it can't fit the
             panel it scrolls horizontally inside the card rather than the page. -->
        <div class="hm-scroll">
          <div class="hm-grid-wrap">
            <!-- month labels along the top edge -->
            <div class="hm-months">
              {#each monthLabels as ml (ml.col)}
                <span class="hm-month mono" style:grid-column={ml.col + 2}>{ml.label}</span>
              {/each}
            </div>

            <div class="hm-body">
              <!-- weekday labels (Mon/Wed/Fri) down the left, faint mono -->
              <div class="hm-wd">
                <span class="hm-wd-l mono" style:grid-row="2">一</span>
                <span class="hm-wd-l mono" style:grid-row="4">三</span>
                <span class="hm-wd-l mono" style:grid-row="6">五</span>
              </div>

              <div class="hm-grid" role="img" aria-label="Focus hours over the last year">
                {#each weeks as wk, ci (ci)}
                  {#each wk as cell, ri (ri)}
                    {#if cell}
                      <!-- svelte-ignore a11y_no_static_element_interactions -->
                      <div
                        class="hm-cell"
                        data-lvl={cell.level}
                        style:grid-column={ci + 1}
                        style:grid-row={ri + 1}
                        onmouseenter={(e) => showTip(e, cell)}
                        onmouseleave={hideTip}
                      ></div>
                    {:else}
                      <div class="hm-cell hm-cell--pad" style:grid-column={ci + 1} style:grid-row={ri + 1}></div>
                    {/if}
                  {/each}
                {/each}
              </div>
            </div>
          </div>

          {#if tip}
            <div class="hm-tip mono" class:below={tip.below} style:left="{tip.x}px" style:top="{tip.y}px">{tip.text}</div>
          {/if}
        </div>

        <!-- legend bottom-right + (when empty) the tracking hint bottom-left -->
        <div class="hm-foot">
          {#if heatmapEmpty}
            <span class="hm-hint">还没有记录学习时间 — 现在应用内的专注时长会自动计入。</span>
          {:else}
            <span></span>
          {/if}
          <div class="hm-legend mono">
            less
            <span class="hm-cell hm-leg" data-lvl="0"></span>
            <span class="hm-cell hm-leg" data-lvl="1"></span>
            <span class="hm-cell hm-leg" data-lvl="2"></span>
            <span class="hm-cell hm-leg" data-lvl="3"></span>
            <span class="hm-cell hm-leg" data-lvl="4"></span>
            more
          </div>
        </div>
      </section>

      <!-- ── due forecast (next 7 days) ── -->
      <section class="an-card">
        <div class="an-card-h">
          <h3 class="an-card-t mono">未来 7 天到期卡片</h3>
          <span class="an-card-sub mono">
            {forecast.reduce((a, d) => a + d.due, 0)} scheduled
          </span>
          <button class="btn btn--sm btn--ghost" onclick={() => app.setView("review")} title="进入今日复习队列">
            <Icon name="bolt" size={12} /> 去复习
          </button>
        </div>
        <div class="an-forecast">
          {#each forecast as d, i (d.day)}
            <div class="an-fc">
              <div class="an-fc-track">
                <div
                  class="an-fc-fill"
                  style:height={pct(d.due / maxDue)}
                  class:zero={d.due === 0}
                ></div>
              </div>
              <div class="an-fc-n mono">{d.due}</div>
              <div class="an-fc-d mono">{i === 0 ? "Today" : dayLabel(d.day)}</div>
            </div>
          {/each}
        </div>
      </section>

      <!-- ── per-subject breakdown ── -->
      {#if data.per_subject.length}
        <section class="an-card">
          <div class="an-card-h">
            <h3 class="an-card-t mono">按科目</h3>
          </div>
          <div class="an-table">
            <div class="an-tr an-tr--head mono">
              <span class="an-th an-th--name">科目</span>
              <span class="an-th">专注</span>
              <span class="an-th">复习</span>
              <span class="an-th">正确率</span>
            </div>
            {#each data.per_subject as s (s.subject_id)}
              <div class="an-tr">
                <span class="an-td an-td--name">
                  <span class="an-dot" style:background={subjectColor(s.subject_id)}></span>
                  <span class="an-name read">{subjectName(s.subject_id)}</span>
                </span>
                <span class="an-td mono">{fmtMins(s.minutes)}</span>
                <span class="an-td mono">{s.reviews}</span>
                <span class="an-td mono">{s.reviews > 0 ? pct(s.accuracy) : "—"}</span>
              </div>
            {/each}
          </div>
        </section>
      {/if}

      <!-- ── topics needing the most work ── -->
      {#if weakTopics.length}
        <section class="an-card">
          <div class="an-card-h">
            <h3 class="an-card-t mono">需要加强的主题</h3>
          <div class="an-podium">
            {#each weakTopics.slice(0, 3) as t, i (t.topic_id)}
              {@const w = Math.max(0.05, (t.reviews > 0 ? 1 - t.accuracy : 0) + (t.lapses > 0 ? t.lapses / (t.lapses + 3) : 0))}
              {@const pctTxt = t.reviews > 0 ? Math.round(t.accuracy * 100) + "%" : "—"}
              <button
                type="button"
                class={"an-podium-card rank-" + (i + 1)}
                onclick={() => app.openTopicSheet(t.subject_id, t.topic_id)}
                onkeydown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); app.openTopicSheet(t.subject_id, t.topic_id); } }}
                aria-label={"Open topic: " + t.topic_name}
                style:--subj-c={subjectColor(t.subject_id)}
                style:--w={w}
              >
                <div class="an-podium-rank mono">#{i + 1}</div>
                <div class="an-podium-name read">{t.topic_name}</div>
                <div class="an-podium-subj mono faint">{subjectName(t.subject_id)}</div>
                <div class="an-podium-bar"><div class="an-podium-bar-fill"></div></div>
                <div class="an-podium-stats mono">
                  <span>{pctTxt} acc</span>
                  {#if t.lapses > 0}<span>· {t.lapses} lapse{t.lapses === 1 ? "" : "s"}</span>{/if}
                </div>
              </button>
            {/each}
            {#if weakTopics.length > 3}
              <div class="an-podium-more mono faint">+{weakTopics.length - 3} more below</div>
            {/if}
          </div>
            <span class="an-card-sub mono">最弱的在前</span>
          </div>
          <div class="an-weak">
            {#each weakTopics as t (t.topic_id)}
              <!-- svelte-ignore a11y_click_events_have_key_events -->
              <!-- svelte-ignore a11y_no_static_element_interactions -->
              <div
                class="an-weak-row"
                role="button"
                tabindex="0"
                title="打开这个主题"
                onclick={() => app.openTopicSheet(t.subject_id, t.topic_id)}
                onkeydown={(e) => e.key === "Enter" && app.openTopicSheet(t.subject_id, t.topic_id)}
              >
                <span class="an-dot" style:background={subjectColor(t.subject_id)}></span>
                <div class="an-weak-main">
                  <div class="an-weak-name read">
                    <span class="an-weak-subj">{weakSubjectName(t)}</span>
                    <span class="an-weak-sep">·</span>
                    {t.topic_name}
                  </div>
                  <div class="an-weak-reason mono">{t.reason}</div>
                </div>
                <div class="an-weak-stats mono">
                  {#if t.reviews > 0}<span class="an-weak-stat">{pct(t.accuracy)} acc</span>{/if}
                  {#if t.lapses > 0}<span class="an-weak-stat">{t.lapses} lapse{t.lapses === 1 ? "" : "s"}</span>{/if}
                </div>
              </div>
            {/each}
          </div>
        </section>
      {/if}

      <!-- ── FSRS memory state ── -->
      <section class="an-card">
        <div class="an-card-h">
          <h3 class="an-card-t mono">间隔重复记忆</h3>
        </div>
        <div class="an-fsrs">
          <div class="an-fsrs-stat">
            <div class="an-fsrs-v">{data.fsrs.cards}</div>
            <div class="an-fsrs-k mono">已排期卡片</div>
          </div>
          <div class="an-fsrs-stat">
            <div class="an-fsrs-v">
              {data.fsrs.cards > 0 ? data.fsrs.avg_stability.toFixed(1) : "—"}<span class="an-stat-u">{data.fsrs.cards > 0 ? " d" : ""}</span>
            </div>
            <div class="an-fsrs-k mono">平均稳定度</div>
          </div>
          <div class="an-fsrs-stat">
            <div class="an-fsrs-v">{data.fsrs.lapses}</div>
            <div class="an-fsrs-k mono">遗忘次数</div>
          </div>
        </div>
      </section>
    {/if}
  </div>
</div>

<style>
  /* The view reuses .workspace-scroll / .dash / .dash-head / .eyebrow /
     .dash-title from the design system (see Dashboard.svelte); everything below
     is analytics-specific, styled purely with the shared tokens. */
  .an {
    display: flex;
    flex-direction: column;
    gap: var(--sp-6);
  }

  .an-note {
    padding: 40px 0;
    text-align: center;
    color: var(--fg-faint);
    font-size: var(--t-sm);
  }
  .an-note--err {
    color: var(--err);
  }

  /* ── empty state ── */
  .an-empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    padding: 56px 24px;
    text-align: center;
  }
  .an-empty-glyph {
    font-size: 40px;
    opacity: 0.85;
  }
  .an-empty-t {
    font-size: var(--r-lg);
    color: var(--fg-bright);
  }
  .an-empty-d {
    max-width: 420px;
    font-size: var(--t-sm);
    color: var(--fg-muted);
    line-height: 1.5;
  }

  /* ── headline stat cards ── */
  .an-stats {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: var(--sp-4);
  }
  /* Reflow 4→2 under ~1000px, 2→1 under ~560px — same cards, never crushed. */
  @media (max-width: 1000px) {
    .an-stats { grid-template-columns: repeat(2, 1fr); }
  }
  @media (max-width: 560px) {
    .an-stats { grid-template-columns: 1fr; }
  }
  .an-stat {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--rad-3);
    padding: 14px 16px;
  }
  .an-stat-k {
    font-size: var(--t-2xs);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }
  .an-stat-v {
    margin-top: 8px;
    font-size: var(--r-xl);
    font-weight: 600;
    color: var(--fg-bright);
    font-variant-numeric: tabular-nums;
  }
  .an-stat-u {
    font-size: var(--t-sm);
    font-weight: 400;
    color: var(--fg-faint);
  }
  .an-stat-sub {
    font-size: var(--t-xs);
    color: var(--fg-muted);
    margin-top: 4px;
    line-height: 1.2;
  }
  .an-stat-warn { color: var(--warn); font-weight: 600; }
  .an-stat .faint { color: var(--fg-faint); }

  /* ── card frame shared by chart / forecast / table / fsrs ── */
  .pva {
    display: flex;
    align-items: flex-end;
    gap: 6px;
    height: 110px;
    padding: 8px 2px 0;
  }
  .pva-col {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    height: 100%;
    gap: 4px;
  }
  .pva-bars {
    flex: 1;
    width: 100%;
    display: flex;
    align-items: flex-end;
    justify-content: center;
    gap: 2px;
  }
  .pva-bar {
    width: 42%;
    border-radius: 2px 2px 0 0;
  }
  .pva-bar.planned {
    background: var(--surface-3);
  }
  .pva-bar.done {
    background: var(--accent-dim);
  }
  .pva-d {
    font-size: 10px;
    color: var(--fg-faint);
  }
  .pva-legend {
    display: flex;
    align-items: center;
    gap: 14px;
    margin-top: 8px;
    font-size: 11px;
    color: var(--fg-muted);
  }
  .pva-legend .sw {
    display: inline-block;
    width: 8px;
    height: 8px;
    border-radius: 2px;
    margin-right: 4px;
  }
  .pva-legend .sw.planned {
    background: var(--surface-3);
  }
  .pva-legend .sw.done {
    background: var(--accent-dim);
  }

  .an-card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--rad-3);
    padding: 16px;
  }
  .an-card-h {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 14px;
  }
  .an-card-t {
    font-size: var(--t-xs);
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--accent);
  }
  .an-card-sub {
    font-size: var(--t-2xs);
    color: var(--fg-faint);
  }

  /* ── focus-hours contributions heatmap ── */
  /* Cell sizing — kept as custom props so cells, gaps and label tracks stay in
     lockstep (the weekday-label rows must match the grid rows exactly). */
  .hm-card {
    --hm-cell: 11px;
    --hm-gap: 3px;
    --hm-wd: 26px; /* width of the weekday-label gutter */
  }
  /* The grid can be ~53 weeks wide; scroll inside the card, never the page. */
  .hm-scroll {
    position: relative;
    overflow-x: auto;
    overflow-y: hidden;
    padding-bottom: 2px;
    scrollbar-width: thin;
    scrollbar-color: var(--border-strong) transparent;
  }
  .hm-scroll::-webkit-scrollbar {
    height: 7px;
  }
  .hm-scroll::-webkit-scrollbar-thumb {
    background: var(--border-strong);
    border-radius: var(--rad-pill);
  }
  .hm-grid-wrap {
    /* Just wide enough for the whole year so the scroll wrapper can pan it. */
    width: max-content;
  }

  /* month labels: a single-row grid whose columns mirror the heatmap columns,
     offset by 1 to clear the weekday gutter. */
  .hm-months {
    display: grid;
    grid-auto-columns: calc(var(--hm-cell) + var(--hm-gap));
    grid-auto-flow: column;
    margin-left: var(--hm-wd);
    height: 15px;
  }
  .hm-month {
    font-size: var(--t-2xs);
    color: var(--fg-faint);
    white-space: nowrap;
  }

  .hm-body {
    display: flex;
    gap: 0;
  }
  /* weekday labels aligned to grid rows via a matching 7-row grid */
  .hm-wd {
    display: grid;
    grid-template-rows: repeat(7, var(--hm-cell));
    grid-auto-flow: row;
    gap: var(--hm-gap);
    width: var(--hm-wd);
    padding-right: 4px;
  }
  .hm-wd-l {
    font-size: var(--t-2xs);
    color: var(--fg-faint);
    line-height: var(--hm-cell);
    align-self: center;
  }

  .hm-grid {
    display: grid;
    grid-auto-flow: column;
    grid-template-rows: repeat(7, var(--hm-cell));
    grid-auto-columns: var(--hm-cell);
    gap: var(--hm-gap);
  }
  .hm-cell {
    width: var(--hm-cell);
    height: var(--hm-cell);
    border-radius: 2px;
    background: var(--surface-2);
    transition: outline-color var(--dur-fast) var(--ease);
    outline: 1px solid transparent;
  }
  .hm-cell[data-lvl="1"] { background: color-mix(in oklab, var(--accent) 25%, var(--surface-2)); }
  .hm-cell[data-lvl="2"] { background: color-mix(in oklab, var(--accent) 50%, var(--surface-2)); }
  .hm-cell[data-lvl="3"] { background: color-mix(in oklab, var(--accent) 75%, var(--surface-2)); }
  .hm-cell[data-lvl="4"] { background: var(--accent); }
  .hm-cell:not(.hm-cell--pad):hover {
    outline-color: var(--fg-muted);
  }
  .hm-cell--pad {
    background: transparent;
  }

  /* tooltip: positioned div anchored to the card (rides horizontal scroll) */
  .hm-tip {
    position: fixed;
    transform: translate(-50%, -100%);
    z-index: 40;
  }
  .hm-tip.below {
    transform: translate(-50%, 0);
  }
  .hm-tip {
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--rad-2);
    padding: 4px 8px;
    font-size: var(--t-2xs);
    color: var(--fg-bright);
    white-space: nowrap;
    pointer-events: none;
    box-shadow: var(--shadow-pop, 0 4px 14px rgba(0, 0, 0, 0.3));
    z-index: 5;
  }

  .hm-foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-top: 12px;
  }
  .hm-hint {
    font-size: var(--t-2xs);
    color: var(--fg-faint);
  }
  .hm-legend {
    display: flex;
    align-items: center;
    gap: 3px;
    font-size: var(--t-2xs);
    color: var(--fg-faint);
    margin-left: auto;
  }
  .hm-leg {
    /* legend swatches are independent of the grid sizing vars */
    width: 11px;
    height: 11px;
  }

  /* ── due forecast (7 mini columns) ── */
  .an-forecast {
    display: grid;
    grid-template-columns: repeat(7, 1fr);
    gap: 8px;
    align-items: end;
  }
  .an-fc {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
  }
  .an-fc-track {
    width: 100%;
    height: 84px;
    display: flex;
    align-items: flex-end;
    justify-content: center;
    background: var(--bg-sunken);
    border-radius: var(--rad-2);
    overflow: hidden;
  }
  .an-fc-fill {
    width: 100%;
    min-height: 2px;
    background: var(--accent);
    opacity: 0.78;
    border-radius: var(--rad-2) var(--rad-2) 0 0;
    transition: height var(--dur) var(--ease);
  }
  .an-fc-fill.zero {
    background: var(--border-strong);
    opacity: 0.5;
    min-height: 2px;
  }
  .an-fc-n {
    font-size: var(--t-sm);
    color: var(--fg-bright);
    font-variant-numeric: tabular-nums;
  }
  .an-fc-d {
    font-size: var(--t-2xs);
    color: var(--fg-faint);
  }

  /* ── per-subject table ── */
  .an-table {
    display: flex;
    flex-direction: column;
  }
  .an-tr {
    display: grid;
    grid-template-columns: 2fr 1fr 1fr 1fr;
    align-items: center;
    gap: 10px;
    padding: 10px 4px;
    border-top: 1px solid var(--border);
  }
  .an-tr--head {
    border-top: none;
    padding-top: 0;
  }
  .an-th {
    font-size: var(--t-2xs);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--fg-faint);
    text-align: right;
  }
  .an-th--name {
    text-align: left;
  }
  .an-td {
    font-size: var(--t-sm);
    color: var(--fg-muted);
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .an-td--name {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    text-align: left;
  }
  .an-dot {
    flex: none;
    width: 9px;
    height: 9px;
    border-radius: 50%;
  }
  .an-name {
    color: var(--fg-bright);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* ── FSRS memory state ── */
  .an-fsrs {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: var(--sp-4);
  }
  .an-fsrs-stat {
    text-align: center;
    padding: 8px 0;
  }
  .an-fsrs-v {
    font-size: var(--r-xl);
    font-weight: 600;
    color: var(--fg-bright);
    font-variant-numeric: tabular-nums;
  }
  .an-fsrs-k {
    margin-top: 6px;
    font-size: var(--t-2xs);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }

  /* ── topics needing work ── */
  .an-weak {
    display: flex;
    flex-direction: column;
  }
  .an-weak-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 11px 4px;
    border-top: 1px solid var(--border);
    cursor: pointer;
    border-radius: 6px;
  }
  .an-weak-row:hover {
    background: var(--surface);
  }
  .an-weak-row:focus-visible {
    outline: 1px solid var(--accent);
  }
  .an-weak-row:first-child {
    border-top: none;
  }
  .an-weak-main {
    flex: 1;
    min-width: 0;
  }
  .an-weak-name {
    font-size: var(--t-md);
    color: var(--fg-bright);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .an-weak-subj {
    color: var(--fg-muted);
  }
  .an-weak-sep {
    color: var(--fg-faint);
    margin: 0 2px;
  }
  .an-weak-reason {
    margin-top: 3px;
    font-size: var(--t-2xs);
    color: var(--fg-faint);
  }
  .an-weak-stats {
    flex: none;
    display: flex;
    gap: 8px;
  }
  .an-weak-stat {
    font-size: var(--t-2xs);
    color: var(--warn);
    background: color-mix(in oklab, var(--warn) 12%, transparent);
    border-radius: var(--rad-pill);
    padding: 2px 8px;
    white-space: nowrap;
  }

  /* ── window selector ── */
  .an-rangebar { display: flex; align-items: center; gap: 10px; }
  .an-range-lbl { font-size: var(--t-2xs); color: var(--fg-faint); text-transform: uppercase; letter-spacing: 0.1em; }

  /* ── radial summary row ── */
  .an-radials { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 12px; }
  .an-radial { display: flex; flex-direction: column; align-items: center; }
  .an-donut, .an-clock { width: 130px; height: 130px; margin-top: 4px; }
  .an-donut-bg { fill: none; stroke: var(--surface-3, var(--surface-2)); stroke-width: 9; }
  .an-donut-fg { fill: none; stroke-width: 9; stroke-linecap: round; transition: stroke-dasharray 0.6s var(--ease, ease); }
  .an-donut-v { fill: var(--fg-bright); font-size: 22px; font-weight: 700; text-anchor: middle; }
  .an-donut-k { fill: var(--fg-faint); font-size: 8.5px; text-anchor: middle; font-family: var(--font-mono); }
  .an-spoke { stroke-width: 2.4; stroke-linecap: round; transition: opacity 0.15s, stroke-width 0.15s; }
  .an-spoke.zero { stroke: var(--border) !important; stroke-width: 1.4; }
  .an-spoke:hover { stroke-width: 4; }
  .an-clock-c { fill: var(--fg-faint); font-size: 9px; text-anchor: middle; }

  /* ── per-day time-series bars ── */
  .an-ts { display: flex; align-items: flex-end; gap: 2px; height: 96px; margin-top: 6px; }
  .an-ts-col { flex: 1 1 0; min-width: 0; display: flex; flex-direction: column; align-items: center; gap: 3px; }
  .an-ts-track { width: 100%; height: 80px; display: flex; align-items: flex-end; }
  .an-ts-fill { width: 100%; background: var(--accent); border-radius: 2px 2px 0 0; min-height: 2px; transition: height 0.5s var(--ease, ease), filter 0.12s; }
  .an-ts-fill.rev { background: var(--info, #5a7fd6); }
  .an-ts-fill.zero { background: var(--border); min-height: 2px; }
  .an-ts-col:hover .an-ts-fill { filter: brightness(1.35); }
  .an-ts-x { font-size: 8.5px; color: var(--fg-faint); white-space: nowrap; }

  /* ── pomodoro stats ── */
  .an-pomo { display: grid; grid-template-columns: repeat(auto-fit, minmax(96px, 1fr)); gap: 10px; }
  .an-pomo-stat { background: var(--surface-2); border: 1px solid var(--border); border-radius: var(--rad-3); padding: 12px; text-align: center; }
  .an-pomo-v { font-size: 22px; font-weight: 700; color: var(--fg-bright); }
  .an-pomo-k { font-size: var(--t-2xs); color: var(--fg-faint); margin-top: 2px; }

  /* ── subject pill switcher ── */
  .an-pills { display: flex; flex-wrap: wrap; gap: 6px; }
  .an-pill { background: var(--surface-2); border: 1px solid var(--border); color: var(--fg-faint); border-radius: var(--rad-pill); padding: 3px 11px; font-size: var(--t-xs); cursor: pointer; transition: all 0.12s; max-width: 160px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .an-pill:hover { color: var(--fg-bright); border-color: var(--border-strong); }
  .an-pill.on { color: var(--accent-fg); background: var(--pill, var(--accent)); border-color: transparent; }

  /* ── radar metric switcher ── */
  .an-metricbar { display: flex; gap: 6px; margin: 4px 0 10px; }
  .an-metric { background: var(--surface-2); border: 1px solid var(--border); color: var(--fg-faint); border-radius: var(--rad-2); padding: 3px 12px; font-size: var(--t-xs); cursor: pointer; transition: all 0.12s; }
  .an-metric:hover { color: var(--fg-bright); }
  .an-metric.on { color: var(--accent); border-color: var(--accent-dim, var(--accent)); background: color-mix(in oklab, var(--accent) 12%, transparent); }

  /* ── topic radar ── */
  .an-radar-wrap { display: flex; flex-direction: column; align-items: center; padding: 4px 40px 0; }
  /* overflow visible so axis labels near the rim aren't clipped by the viewBox. */
  .an-radar { width: min(280px, 100%); height: auto; overflow: visible; }
  .an-radar-ring { fill: none; stroke: var(--border); stroke-width: 1; }
  .an-radar-axis { stroke: var(--border); stroke-width: 1; }
  .an-radar-lbl { fill: var(--fg-faint); font-size: 8px; font-family: var(--font-mono); }
  .an-radar-poly { fill-opacity: 0.18; stroke-width: 2; stroke-linejoin: round; transition: all 0.5s var(--ease, ease); }
  .an-radar-dot { stroke: var(--surface); stroke-width: 1.5; transition: r 0.12s; }
  .an-radar-dot:hover { r: 5; }
  .an-radar-note { font-size: var(--t-2xs); color: var(--fg-faint); margin-top: 8px; text-align: center; }
  /* ── weakness podium (top 3) ── */
  .an-podium {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 8px;
    margin: 10px 0 4px;
  }
  @media (max-width: 720px) {
    .an-podium { grid-template-columns: 1fr; }
  }
  .an-podium-card {
    appearance: none;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 10px 12px;
    text-align: left;
    cursor: pointer;
    color: var(--fg);
    font: inherit;
    transition: border-color 120ms ease, transform 120ms ease;
    position: relative;
    overflow: hidden;
  }
  .an-podium-card:hover {
    border-color: var(--subj-c, var(--accent));
    transform: translateY(-1px);
  }
  .an-podium-card.rank-1 { border-color: var(--subj-c, var(--accent)); }
  .an-podium-rank {
    font-size: var(--t-xs);
    color: var(--fg-faint);
    margin-bottom: 4px;
  }
  .an-podium-card.rank-1 .an-podium-rank { color: var(--subj-c, var(--accent)); font-weight: 700; }
  .an-podium-name {
    font-size: var(--t-md);
    color: var(--fg-bright);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .an-podium-subj { font-size: var(--t-xs); margin-top: 2px; }
  .an-podium-bar {
    height: 4px;
    border-radius: 2px;
    background: var(--surface-2);
    margin: 8px 0 6px;
    overflow: hidden;
  }
  .an-podium-bar-fill {
    height: 100%;
    width: calc(var(--w) * 100%);
    background: var(--subj-c, var(--accent));
    border-radius: 2px;
  }
  .an-podium-stats {
    font-size: var(--t-xs);
    color: var(--fg-muted);
    display: flex;
    gap: 6px;
  }
  .an-podium-more { margin-top: 6px; font-size: var(--t-xs); }

</style>
