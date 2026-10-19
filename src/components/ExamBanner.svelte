<script lang="ts">
  // Exam countdown banner (roadmap 2.11) — the nearest upcoming exam pulses at
  // the top-right of the main column: orange (--warn) at ≤7 days, red (--err)
  // with a faster pulse at ≤3 days, plus a once-a-day push toast once an exam
  // enters the red zone. Sourced from the local calendar (`kind === "exam"`),
  // NOT Moodle deadlines — those already surface in the notification centre.
  //
  // The banner is cosmetic by design: fetch failures stay silent (the calendar
  // view surfaces real errors), and dismissal only hides it until the next
  // local day. Clicking the body jumps to the calendar view.
  import { app } from "../lib/store.svelte";
  import * as api from "../lib/api";
  import Icon from "./Icon.svelte";

  const WINDOW_MS = 7 * 86_400_000; // banner horizon: one week out
  const GRACE_MS = 60 * 60_000; // keep showing an exam for 1h after it starts

  let events = $state<api.CalEvent[]>([]);
  let now = $state(Date.now());
  let dismissed = $state<string | null>(null); // "{id}:{localDay}" hidden for today

  // Local calendar day ("2026-10-25") — date diff must use LOCAL days, not UTC
  // or epoch days, so "today 23:00" vs "now 16:00" counts as 0 days, not 1.
  function localDay(ms: number): string {
    const d = new Date(ms);
    const p = (n: number) => String(n).padStart(2, "0");
    return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
  }
  function daysUntil(startMs: number, nowMs: number): number {
    const a = new Date(startMs);
    const b = new Date(nowMs);
    const da = Date.UTC(a.getFullYear(), a.getMonth(), a.getDate());
    const db = Date.UTC(b.getFullYear(), b.getMonth(), b.getDate());
    return Math.round((da - db) / 86_400_000);
  }
  function daysLabel(d: number): string {
    if (d < 0) return "进行中";
    if (d === 0) return "今天开考";
    if (d === 1) return "明天开考";
    return `${d} 天后开考`;
  }
  function clockLabel(ms: number): string {
    const d = new Date(ms);
    const p = (n: number) => String(n).padStart(2, "0");
    return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
  }

  // Reload on subject switch; a 60s refetch keeps the list fresh without
  // depending on `now` (that would tear down the interval every tick).
  $effect(() => {
    const sid = app.activeSubject?.id ?? null;
    let cancelled = false;
    const load = () => {
      const t = Date.now();
      // Windowed fetch: only what the banner can ever display (started ≤1h ago
      // through one week out), so big calendars don't refetch everything.
      api.listEvents(sid, t - GRACE_MS, t + WINDOW_MS + 3_600_000)
        .then((r) => { if (!cancelled) events = r ?? []; })
        .catch(() => { /* banner is cosmetic; the calendar shows real errors */ });
    };
    load();
    const t = setInterval(load, 60_000);
    return () => { cancelled = true; clearInterval(t); };
  });

  // Wall-clock tick — drives the countdown/pulse re-derivation every 30s.
  $effect(() => {
    const t = setInterval(() => { now = Date.now(); }, 30_000);
    return () => clearInterval(t);
  });

  let upcoming = $derived.by(() => {
    const cutoff = now - GRACE_MS;
    return events
      .filter((e) => e.kind === "exam" && !e.done && e.start_ms > cutoff && e.start_ms <= now + WINDOW_MS)
      .sort((a, b) => a.start_ms - b.start_ms);
  });
  let nearest = $derived(upcoming[0] ?? null);
  let daysLeft = $derived(nearest ? daysUntil(nearest.start_ms, now) : 99);
  let level = $derived(daysLeft <= 3 ? "hot" : daysLeft <= 7 ? "warm" : null);
  let dismissKey = $derived(nearest ? `${nearest.id}:${localDay(now)}` : "");

  // Red-zone push: once per exam per local day, even across restarts
  // (localStorage guard — the toast itself auto-dismisses after 5.2s).
  $effect(() => {
    const e = nearest;
    if (!e || level !== "hot" || e.start_ms < now) return;
    const key = `cortex-exampush:${e.id}:${localDay(now)}`;
    try {
      if (localStorage.getItem(key)) return;
      localStorage.setItem(key, "1");
    } catch { /* private mode etc. — toast once per session is fine */ }
    app.pushToast({
      kind: "error",
      title: `⛔ ${e.title} · ${daysLabel(daysLeft)}`,
      body: `考试 ${clockLabel(e.start_ms)}，只剩 ${daysLeft} 天——把复习排上日程。`,
    });
  });

  function dismiss() {
    dismissed = dismissKey;
  }
</script>

{#if nearest && level && dismissed !== dismissKey}
  <div class="exam-banner" class:hot={level === "hot"} role="alert">
    <span class="eb-dot" aria-hidden="true"></span>
    <button class="eb-body" onclick={() => app.setView("calendar")} title="打开日历查看考试安排">
      <Icon name="calendar" size={12} />
      <span class="eb-title">{nearest.title}</span>
      <span class="eb-when mono">{clockLabel(nearest.start_ms)}</span>
      <span class="eb-count mono">{daysLabel(daysLeft)}</span>
      {#if upcoming.length > 1}
        <span class="eb-more mono">+{upcoming.length - 1} 场</span>
      {/if}
    </button>
    <button class="eb-close" onclick={dismiss} title="今天不再显示">
      <Icon name="x" size={12} />
    </button>
  </div>
{/if}

<style>
  /* Anchored top-right of the main column so it never collides with the
     startup self-check banner (top-left, App.svelte .selfcheck). */
  .exam-banner {
    --eb-c: var(--warn);
    position: absolute; top: 10px; right: 12px; z-index: 35;
    display: flex; align-items: center; gap: 8px;
    max-width: min(460px, calc(100% - 24px));
    padding: 7px 8px 7px 12px;
    background: var(--surface); color: var(--fg);
    border: 1px solid color-mix(in oklab, var(--eb-c) 50%, transparent);
    border-radius: var(--rad-3, 8px);
    box-shadow: 0 8px 28px color-mix(in oklab, var(--bg-sunken) 55%, transparent);
    font-size: var(--t-sm);
  }
  .exam-banner.hot { --eb-c: var(--err); }

  .eb-dot {
    flex: none; width: 8px; height: 8px; border-radius: 50%;
    background: var(--eb-c);
    animation: eb-pulse 2.4s ease-out infinite;
  }
  .exam-banner.hot .eb-dot { animation-duration: 1.2s; }
  @keyframes eb-pulse {
    0% { box-shadow: 0 0 0 0 color-mix(in oklab, var(--eb-c) 50%, transparent); }
    70% { box-shadow: 0 0 0 8px transparent; }
    100% { box-shadow: 0 0 0 0 transparent; }
  }

  .eb-body {
    display: flex; align-items: center; gap: 7px; min-width: 0;
    border: 0; background: transparent; color: var(--fg-bright);
    font: inherit; cursor: pointer; text-align: left; padding: 0;
  }
  .eb-body:hover .eb-title { text-decoration: underline; }
  .eb-body > :global(svg) { color: var(--eb-c); flex: none; }
  .eb-title { font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .eb-when { color: var(--fg-faint); font-size: 10.5px; white-space: nowrap; }
  .eb-count { color: var(--eb-c); font-weight: 600; font-size: 11px; white-space: nowrap; }
  .eb-more { color: var(--fg-faint); font-size: 10.5px; white-space: nowrap; }

  .eb-close {
    flex: none; display: inline-flex; align-items: center; justify-content: center;
    width: 22px; height: 22px; border: 0; border-radius: 6px;
    background: transparent; color: var(--fg-faint); cursor: pointer;
  }
  .eb-close:hover { background: color-mix(in oklab, var(--eb-c) 12%, transparent); color: var(--fg-bright); }

  @media (max-width: 1080px) {
    .eb-when { display: none; }
  }
</style>
