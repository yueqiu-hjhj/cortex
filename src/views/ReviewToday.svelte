<script lang="ts">
  // Review Today — FSRS 的"今天该复习什么"。
  // review_today 拉全库到期卡（跨科目、quiz+flashcard，最久未复习优先），
  // flashcard 直接在本视图翻面自评（srs_grade 走 FSRS 重排），quiz 跳回
  // 对应科目的材料页去答。空队列 = 今天全部还清。
  import { app } from "../lib/store.svelte";
  import * as api from "../lib/api";
  import Icon from "../components/Icon.svelte";
  import RichText from "../components/RichText.svelte";

  let cards = $state<api.ReviewTodayCard[]>([]);
  let loaded = $state(false);
  async function refresh() {
    try { cards = await api.reviewToday(500); }
    catch { /* non-fatal — keep last list */ }
    finally { loaded = true; }
  }
  $effect(() => { void refresh(); });

  // Group by subject, preserving oldest-due order inside each group.
  const bySubject = $derived.by(() => {
    const map = new Map<string, { name: string; cards: api.ReviewTodayCard[] }>();
    for (const c of cards) {
      let g = map.get(c.subject_id);
      if (!g) { g = { name: c.subject_name, cards: [] }; map.set(c.subject_id, g); }
      g.cards.push(c);
    }
    return [...map.entries()];
  });
  const totalDue = $derived(cards.length);

  function subjectOf(id: string) {
    return app.subjects.find((s: { id: string }) => s.id === id);
  }
  function glyphOf(id: string) {
    return subjectOf(id)?.glyph || "◆";
  }
  function colorOf(id: string) {
    const s = subjectOf(id);
    return s ? app.subjectColor(s) : "var(--accent)";
  }
  function quizDue(g: { cards: api.ReviewTodayCard[] }) {
    return g.cards.filter((c) => c.kind === "quiz").length;
  }
  function cardDue(g: { cards: api.ReviewTodayCard[] }) {
    return g.cards.filter((c) => c.kind === "flashcard").length;
  }
  // "逾期 N" 相对标签：>1d 显示天，>1h 显示小时，否则分钟。
  function overdueLabel(dueAt: number) {
    const ms = Date.now() - dueAt;
    if (ms >= 86_400_000) return `逾期 ${Math.floor(ms / 86_400_000)} 天`;
    if (ms >= 3_600_000) return `逾期 ${Math.floor(ms / 3_600_000)} 小时`;
    return `逾期 ${Math.max(1, Math.floor(ms / 60_000))} 分钟`;
  }

  function quizJump(subjectId: string) {
    app.openSubject(subjectId);
    app.setTab("materials");
  }

  // ── study session: flashcards only, graded inline against FSRS ──
  type Session = {
    subjectId: string;
    subjectName: string;
    queue: api.ReviewTodayCard[];
    i: number;
    flipped: boolean;
    done: boolean;
    rated: number;
    intervals: number[] | null;
  };
  let session = $state<Session | null>(null);
  // item_key (front) → back text, resolved from the subject's flashcard materials.
  let backs = $state<Map<string, string>>(new Map());

  async function startSubject(subjectId: string) {
    const g = bySubject.find(([id]) => id === subjectId)?.[1];
    if (!g) return;
    const queue = g.cards.filter((c) => c.kind === "flashcard");
    if (queue.length === 0) return;
    session = { subjectId, subjectName: g.name, queue, i: 0, flipped: false, done: false, rated: 0, intervals: null };
    backs = new Map();
    try {
      const mats = await api.listMaterials(subjectId);
      for (const m of mats) {
        if (m.kind !== "flashcards") continue;
        const arr: any[] = Array.isArray(m.payload) ? m.payload : (m.payload?.cards ?? []);
        for (const c of arr) {
          if (c?.q != null && !backs.has(String(c.q))) backs.set(String(c.q), String(c.a ?? ""));
        }
      }
    } catch { /* backs stay empty — front-only self-grade still reschedules */ }
  }

  const cur = $derived(session && !session.done ? session.queue[session.i] : null);
  const backOf = $derived(cur ? (backs.get(cur.item_key) ?? "") : "");

  // ── 遮挡自测：正面含 {{c1::答案}} 标记时，渲染成可点击的遮挡块 ──
  type ClozePart = { t: "text"; s: string } | { t: "cloze"; answer: string };
  function parseCloze(front: string): ClozePart[] | null {
    const re = /\{\{c\d+::(.*?)\}\}/g;
    if (!re.test(front)) return null;
    const parts: ClozePart[] = [];
    let last = 0;
    re.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = re.exec(front))) {
      if (m.index > last) parts.push({ t: "text", s: front.slice(last, m.index) });
      parts.push({ t: "cloze", answer: m[1] });
      last = m.index + m[0].length;
    }
    if (last < front.length) parts.push({ t: "text", s: front.slice(last) });
    return parts;
  }
  let revealed = $state<number[]>([]);
  // 换卡时重置揭示状态
  $effect(() => { void cur; revealed = []; });
  const clozeParts = $derived(cur ? parseCloze(cur.item_key) : null);
  const maskStr = (len: number) => "█".repeat(Math.max(3, Math.min(len, 10)));
  function toggleReveal(i: number) {
    revealed = revealed.includes(i) ? revealed.filter((x) => x !== i) : [...revealed, i];
  }
  function revealAll() {
    revealed = (clozeParts ?? []).map((_, i) => i).filter((i) => clozeParts![i].t === "cloze");
  }

  // Next-interval preview per grade [again, hard, good, easy] for the current card.
  $effect(() => {
    const s = session;
    const c = cur;
    if (!s || !c) { if (s) s.intervals = null; return; }
    let cancelled = false;
    api.srsPreview(s.subjectId, "flashcard", c.item_key)
      .then((v) => { if (!cancelled && session) session.intervals = v; })
      .catch(() => { if (!cancelled && session) session.intervals = null; });
    return () => { cancelled = true; };
  });

  const RATINGS = [
    { id: "again", label: "Again", key: "1", cls: "again", q: 1 },
    { id: "hard",  label: "Hard",  key: "2", cls: "hard",  q: 3 },
    { id: "good",  label: "Good",  key: "3", cls: "good",  q: 4 },
    { id: "easy",  label: "Easy",  key: "4", cls: "easy",  q: 5 },
  ] as const;
  function fmtInterval(d: number): string {
    if (d <= 0) return "<1d";
    if (d < 7) return `${d}d`;
    if (d < 30) return `${Math.round(d / 7)}w`;
    if (d < 365) return `${Math.round(d / 30)}mo`;
    return `${(d / 365).toFixed(d < 730 ? 1 : 0)}y`;
  }

  let glow = $state<string | null>(null);
  async function grade(cls: string) {
    const s = session;
    const c = cur;
    if (!s || !c || glow) return;
    const q = RATINGS.find((r) => r.cls === cls)?.q ?? 4;
    glow = cls;
    try {
      await api.srsGrade(s.subjectId, "flashcard", c.item_index, c.item_key, q, c.material_id);
    } catch (e) {
      glow = null;
      app.pushToast({ kind: "error", title: "记录失败", body: String(e) });
      return;
    }
    setTimeout(() => {
      glow = null;
      if (session !== s) return;
      session.rated += 1;
      if (session.i + 1 >= session.queue.length) session.done = true;
      else { session.i += 1; session.flipped = false; }
    }, 380);
  }

  function exitSession() {
    session = null;
    void refresh();
  }

  // Keyboard: Space 翻面 · 1-4 评分 · Esc 退出会话。会话期间屏蔽全局引擎。
  $effect(() => {
    window.__cortexModalOpen = session != null;
    return () => { window.__cortexModalOpen = false; };
  });
  $effect(() => {
    if (!session) return;
    const s = session;
    function onKey(e: KeyboardEvent) {
      const el = document.activeElement as HTMLElement | null;
      if (el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA")) return;
      if (e.key === "Escape") { exitSession(); return; }
      if (s.done) return;
      if (e.key === " ") { e.preventDefault(); if (session) session.flipped = !session.flipped; return; }
      const hit = RATINGS.find((r) => r.key === e.key);
      if (hit && session?.flipped) grade(hit.cls);
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const today = $derived.by(() => {
    const d = new Date();
    const w = ["日", "一", "二", "三", "四", "五", "六"][d.getDay()];
    return `${d.getMonth() + 1}月${d.getDate()}日 · 周${w}`;
  });
</script>

<div class="rv">
  <!-- Header -->
  <header class="rv-head">
    <div>
      <div class="eyebrow">{today} · FSRS 复习队列</div>
      <h1 class="rv-title">今天该复习什么</h1>
    </div>
    <div class="rv-head-stats">
      <span class="kpi"><b>{totalDue}</b><i>张到期</i></span>
      <span class="kpi"><b>{bySubject.length}</b><i>个科目</i></span>
    </div>
  </header>

  {#if session}
    <!-- ── inline study session ── -->
    <section class="rv-study">
      <div class="rv-study-top">
        <button class="rv-btn" onclick={exitSession} title="退出本组复习 (Esc)">
          <Icon name="chevron" size={12} style="transform:rotate(180deg)" /> 队列
        </button>
        <span class="rv-study-prog mono">
          {session.subjectName ?? ""} · {session.i + (session.done ? 0 : 1)} / {session.queue.length}
          {#if session.rated > 0}· 已评 {session.rated}{/if}
        </span>
      </div>

      {#if session.done}
        <div class="rv-done">
          <div class="rv-done-big">这组还清了 🎉</div>
          <p class="rv-done-sub">评了 {session.rated} 张，FSRS 已按你的反馈重排下次到期时间。</p>
          <div class="rv-done-actions">
            <button class="rv-btn primary" onclick={exitSession}>回到队列</button>
          </div>
        </div>
      {:else if cur}
        <div
          class="rv-card{session.flipped ? ' flipped' : ''}"
          role="button"
          tabindex="0"
          onclick={() => (session!.flipped = true)}
          onkeydown={(e) => e.key === "Enter" && (session!.flipped = true)}
          title="点击翻面 (Space)"
        >
          <div class="rv-card-side">
            <span class="rv-card-tag eyebrow">正面</span>
            <div class="rv-card-text read">
              {#if clozeParts}
                <div class="rv-cloze">
                  {#each clozeParts as p, i}
                    {#if p.t === "text"}<span>{p.s}</span>{:else}
                      <button
                        class="rv-mask"
                        class:open={revealed.includes(i)}
                        onclick={(e) => { e.stopPropagation(); toggleReveal(i); }}
                        title={revealed.includes(i) ? "再点一下重新遮挡" : "点击揭示"}
                      >{revealed.includes(i) ? p.answer : maskStr(p.answer.length)}</button>
                    {/if}
                  {/each}
                </div>
                <button class="rv-cloze-all" onclick={(e) => { e.stopPropagation(); revealAll(); }}>
                  全部揭示
                </button>
              {:else}
                <RichText text={cur.item_key} />
              {/if}
            </div>
          </div>
          {#if session.flipped}
            <div class="rv-card-side back">
              <span class="rv-card-tag eyebrow">背面</span>
              <div class="rv-card-text read">
                {#if backOf}<RichText text={backOf} />{:else}<span class="faint">（未找到对应卡背 — 凭记忆自评即可）</span>{/if}
              </div>
            </div>
          {:else}
            <div class="rv-card-hint mono">Space / 点击 翻面</div>
          {/if}
        </div>

        <div class="rv-ratings" class:hidden={!session.flipped}>
          {#each RATINGS as r (r.id)}
            <button
              class="rv-rate {r.cls}{glow === r.cls ? ' glow' : ''}"
              onclick={() => grade(r.cls)}
              disabled={!session!.flipped}
            >
              <span class="rv-rate-label">{r.label}</span>
              {#if session!.intervals}
                <span class="rv-rate-next mono">{fmtInterval(session!.intervals[RATINGS.indexOf(r)])}</span>
              {/if}
              <span class="rv-rate-key mono">{r.key}</span>
            </button>
          {/each}
        </div>
        {#if !session.flipped}
          <p class="rv-ratings-hint mono">先回忆，再翻面对照 — 诚实评分 FSRS 才能排得准。</p>
        {/if}
      {/if}
    </section>
  {:else}
    <!-- ── queue grouped by subject ── -->
    {#if loaded && totalDue === 0}
      <section class="rv-empty">
        <div class="rv-empty-big">今天没有到期的卡片 🎉</div>
        <p class="rv-empty-sub">
          FSRS 队列已还清。学新材料（生成闪卡 / 做题）后，到期的卡会自动出现在这里。
        </p>
        <div class="rv-done-actions">
          <button class="rv-btn" onclick={() => app.setView("dashboard")}>回首页</button>
          <button class="rv-btn primary" onclick={() => app.pomo.pomoToggle()}>
            <Icon name="play" size={12} /> 顺手来个番茄钟
          </button>
        </div>
      </section>
    {:else}
      {#each bySubject as [sid, g] (sid)}
        <section class="rv-group" style="--subj: {colorOf(sid)}">
          <div class="rv-group-head">
            <span class="rv-group-glyph">{glyphOf(sid)}</span>
            <h3 class="rv-group-name read">{g.name}</h3>
            <span class="rv-group-counts mono">
              {#if cardDue(g)}闪卡 {cardDue(g)}{/if}
              {#if cardDue(g) && quizDue(g)} · {/if}
              {#if quizDue(g)}Quiz {quizDue(g)}{/if}
            </span>
            <div class="grow"></div>
            {#if cardDue(g) > 0}
              <button class="rv-btn primary" onclick={() => startSubject(sid)}>
                <Icon name="play" size={11} /> 开始复习
              </button>
            {/if}
            {#if quizDue(g) > 0}
              <button class="rv-btn" onclick={() => quizJump(sid)} title="去该科目的材料页答题">
                <Icon name="check" size={11} /> 去 Quiz
              </button>
            {/if}
          </div>
          <div class="rv-group-list">
            {#each g.cards as c (c.subject_id + c.kind + c.item_key)}
              <div class="rv-row" class:quiz={c.kind === "quiz"}>
                <span class="rv-row-kind mono">{c.kind === "quiz" ? "Quiz" : "Card"}</span>
                <div class="rv-row-front read">{c.item_key}</div>
                <span class="rv-row-due mono">{overdueLabel(c.due_at)}</span>
                <span class="rv-row-meta mono" title="重复 {c.reps} 次 · 当前间隔 {c.interval_d} 天 · 稳定性 {c.stability?.toFixed(1) ?? '—'}">
                  ×{c.reps} · {fmtInterval(c.interval_d)}
                </span>
              </div>
            {/each}
          </div>
        </section>
      {/each}
    {/if}
  {/if}
</div>

<style>
  .rv {
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
  .faint { color: var(--fg-faint); }
  .mono { font-family: var(--font-mono); }

  /* Header */
  .rv-head {
    display: flex; align-items: center; justify-content: space-between; gap: 12px;
    border-bottom: 1px solid var(--border); padding-bottom: 14px;
  }
  .rv-title {
    margin: 4px 0 0;
    font-family: var(--font-display);
    font-size: 22px; font-weight: 600; letter-spacing: -0.01em;
    color: var(--fg-bright);
  }
  .rv-head-stats { display: flex; gap: 8px; }
  .kpi {
    display: inline-flex; align-items: baseline; gap: 4px;
    font-family: var(--font-mono); font-size: 11px; color: var(--fg-faint);
    background: var(--surface); border: 1px solid var(--border);
    border-radius: 8px; padding: 5px 11px;
  }
  .kpi b { font-size: 18px; color: var(--fg-bright); font-weight: 600; }
  .kpi i { font-style: normal; color: var(--fg-muted); }

  /* Buttons */
  .rv-btn {
    display: inline-flex; align-items: center; gap: 6px;
    font-family: var(--font-mono); font-size: 11.5px;
    padding: 7px 12px; border-radius: 7px; cursor: pointer;
    border: 1px solid var(--border); background: var(--surface-2); color: var(--fg);
  }
  .rv-btn:hover { color: var(--fg-bright); border-color: var(--border-strong); }
  .rv-btn.primary {
    background: color-mix(in srgb, var(--accent) 22%, transparent);
    border-color: color-mix(in srgb, var(--accent) 45%, transparent);
    color: var(--fg-bright);
  }
  .rv-btn.primary:hover { background: color-mix(in srgb, var(--accent) 32%, transparent); }

  /* Empty state */
  .rv-empty {
    border: 1px dashed var(--border); border-radius: 12px;
    background: var(--surface);
    padding: 48px 24px; text-align: center;
  }
  .rv-empty-big {
    font-family: var(--font-display); font-size: 18px; font-weight: 600;
    color: var(--fg-bright);
  }
  .rv-empty-sub, .rv-done-sub {
    margin: 8px auto 0; max-width: 460px;
    font-family: var(--font-mono); font-size: 11.5px; color: var(--fg-muted);
  }
  .rv-done-actions {
    margin-top: 16px; display: flex; gap: 8px; justify-content: center;
  }

  /* Subject groups */
  .rv-group {
    border: 1px solid var(--border); border-radius: 12px;
    background: var(--surface); overflow: hidden;
  }
  .rv-group-head {
    display: flex; align-items: center; gap: 9px;
    padding: 11px 14px;
    border-bottom: 1px solid var(--border);
    background: color-mix(in srgb, var(--subj) 7%, transparent);
  }
  .rv-group-glyph { color: var(--subj); font-size: 14px; line-height: 1; }
  .rv-group-name {
    margin: 0; font-family: var(--font-display); font-size: 13.5px; font-weight: 600;
    color: var(--fg-bright);
  }
  .rv-group-counts { font-size: 10.5px; color: var(--fg-muted); }
  .grow { flex: 1; }

  .rv-group-list { display: flex; flex-direction: column; }
  .rv-row {
    display: flex; align-items: baseline; gap: 10px;
    padding: 8px 14px;
    border-bottom: 1px solid color-mix(in srgb, var(--border) 55%, transparent);
  }
  .rv-row:last-child { border-bottom: none; }
  .rv-row:hover { background: var(--surface-2); }
  .rv-row-kind {
    flex: none; width: 40px;
    font-size: 9.5px; color: var(--fg-faint); text-transform: uppercase;
  }
  .rv-row.quiz .rv-row-kind { color: var(--warn); }
  .rv-row-front {
    flex: 1; min-width: 0;
    font-size: 12.5px; color: var(--fg);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .rv-row-due { flex: none; font-size: 10px; color: var(--err, #e5484d); opacity: 0.85; }
  .rv-row-meta { flex: none; font-size: 10px; color: var(--fg-faint); }

  /* Study session */
  .rv-study {
    display: flex; flex-direction: column; gap: 14px;
  }
  .rv-study-top { display: flex; align-items: center; justify-content: space-between; }
  .rv-study-prog { font-size: 11px; color: var(--fg-muted); }

  .rv-card {
    border: 1px solid var(--border); border-radius: 14px;
    background: var(--surface);
    padding: 26px 30px; min-height: 220px;
    display: flex; flex-direction: column; gap: 16px;
    cursor: pointer;
    transition: border-color var(--dur-fast) var(--ease), transform var(--dur-fast) var(--ease);
  }
  .rv-card:hover { border-color: color-mix(in srgb, var(--accent) 45%, transparent); }
  .rv-card.flipped { cursor: default; border-color: color-mix(in srgb, var(--accent) 30%, transparent); }
  .rv-card-side { display: flex; flex-direction: column; gap: 10px; }
  .rv-card-side.back {
    padding-top: 16px; border-top: 1px dashed var(--border);
  }
  .rv-card-tag { color: var(--accent); }
  .rv-card-side.back .rv-card-tag { color: var(--ok); }
  .rv-card-text { font-size: 17px; line-height: 1.55; color: var(--fg-bright); }
  .rv-card-hint {
    margin-top: auto; text-align: center;
    font-size: 10.5px; color: var(--fg-faint);
  }

  .rv-ratings { display: grid; grid-template-columns: repeat(4, 1fr); gap: 8px; }
  .rv-ratings.hidden { visibility: hidden; }
  .rv-rate {
    position: relative;
    display: flex; flex-direction: column; align-items: center; gap: 3px;
    padding: 10px 8px 12px; border-radius: 9px; cursor: pointer;
    font-family: var(--font-mono);
    border: 1px solid var(--border); background: var(--surface-2); color: var(--fg);
    transition: transform var(--dur-fast) var(--ease), border-color var(--dur-fast) var(--ease);
  }
  .rv-rate:hover:not(:disabled) { border-color: var(--border-strong); transform: translateY(-1px); }
  .rv-rate:disabled { opacity: 0.35; cursor: default; }
  .rv-rate-label { font-size: 12px; font-weight: 600; }
  .rv-rate-next { font-size: 10px; color: var(--fg-faint); }
  .rv-rate-key {
    position: absolute; top: 4px; right: 6px;
    font-size: 8.5px; color: var(--fg-faint);
    border: 1px solid var(--border); border-radius: 3px; padding: 0 3px;
  }
  .rv-rate.again:hover:not(:disabled) { border-color: var(--err, #e5484d); color: var(--err, #e5484d); }
  .rv-rate.hard:hover:not(:disabled)  { border-color: var(--warn); color: var(--warn); }
  .rv-rate.good:hover:not(:disabled)  { border-color: var(--ok); color: var(--ok); }
  .rv-rate.easy:hover:not(:disabled)  { border-color: var(--info); color: var(--info); }
  .rv-rate.glow { animation: rv-glow 380ms var(--ease); }
  @keyframes rv-glow {
    0% { box-shadow: 0 0 0 0 color-mix(in srgb, var(--accent) 45%, transparent); }
    100% { box-shadow: 0 0 0 10px transparent; }
  }
  .rv-ratings-hint { text-align: center; font-size: 10.5px; color: var(--fg-faint); margin: 0; }

  /* Done panel */
  .rv-done {
    border: 1px solid color-mix(in srgb, var(--ok) 35%, transparent);
    border-radius: 14px; background: color-mix(in srgb, var(--ok) 7%, transparent);
    padding: 44px 24px; text-align: center;
  }
  .rv-done-big {
    font-family: var(--font-display); font-size: 18px; font-weight: 600;
    color: var(--fg-bright);
  }

  /* ── 遮挡自测（cloze）卡面 ── */
  .rv-cloze { display: inline; line-height: 2.1; }
  .rv-mask {
    font: inherit; cursor: pointer; display: inline-block;
    margin: 0 2px; padding: 0 7px; border-radius: 5px;
    background: var(--surface-2, rgba(127, 127, 127, 0.12));
    border: 1px dashed var(--border-strong, var(--border));
    color: transparent; user-select: none; vertical-align: baseline;
  }
  .rv-mask.open {
    border-style: solid; color: var(--accent); font-weight: 600;
    background: transparent; user-select: text;
  }
  .rv-cloze-all {
    display: block; margin-top: 10px;
    font-size: var(--t-xs); color: var(--fg-muted);
    background: none; border: 1px solid var(--border); border-radius: 6px;
    padding: 3px 10px; cursor: pointer;
  }
  .rv-cloze-all:hover { color: var(--fg-bright); border-color: var(--border-strong, var(--border)); }
</style>
