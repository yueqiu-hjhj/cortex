<script lang="ts">
  // Tutor bench — 支架式陪练。
  //
  // 知源的两条设计搬到这里：
  //   1. 五档支架梯子，会升也会降：答对且自信 → 升一档；答错 → 降一档；
  //      部分对 → 不动。低信心答对不升——猜对不是会了。
  //   2. 学情校准：答错但当时很确定（信心 ≥4）记入「先修」队列，次日提醒。
  //      样本不足 8 轮时界面拒绝下任何水平结论。
  //
  // 档位移动是后端规则（next_level），不是模型说的；模型只负责出题、
  // 判定和按档位决定讲多少。

  import { app } from "../lib/store.svelte";
  import * as api from "../lib/api";
  import Icon from "../components/Icon.svelte";

  const sid = $derived(app.activeSubjectId);

  let nodes = $state<api.KnowledgeNode[]>([]);
  let pickedNode = $state<string | null>(null);
  let sessions = $state<api.TutorSession[]>([]);
  let activeId = $state<string | null>(null);
  let turns = $state<api.TutorTurn[]>([]);
  let busy = $state(false);
  let err = $state<string | null>(null);
  let draft = $state("");
  let confidence = $state(3);
  let misjudged = $state<api.TutorMisjudged[]>([]);

  const active = $derived(sessions.find((s) => s.id === activeId) ?? null);
  const sampleNote = $derived(
    active
      ? active.turns < 8
        ? `样本 ${active.turns}/8 轮 —— 太少，还看不出水平`
        : `样本 ${active.turns} 轮`
      : "",
  );
  const dueMisjudged = $derived(misjudged.filter((m) => m.due));
  const laterMisjudged = $derived(misjudged.filter((m) => !m.due));

  $effect(() => { void loadNodes(); });
  $effect(() => { void loadSessions(); });
  $effect(() => { void loadMisjudged(); });

  async function loadNodes() {
    if (!sid) { nodes = []; return; }
    const s = sid;
    try { const ns = await api.listKnowledgeNodes(s); if (s === sid) nodes = ns.filter((n) => n.kind !== "subject"); }
    catch { /* the picker just stays empty */ }
  }
  async function loadSessions() {
    if (!sid) { sessions = []; return; }
    const s = sid;
    try { const ss = await api.tutorSessions(s); if (s === sid) sessions = ss; }
    catch { /* ignore */ }
  }
  async function loadMisjudged() {
    try { misjudged = await api.tutorMisjudged(); } catch { /* ignore */ }
  }
  async function loadHistory(id: string) {
    try { turns = await api.tutorHistory(id); } catch { /* ignore */ }
  }

  async function begin(nodeId: string | null) {
    if (!sid || busy) return;
    busy = true; err = null;
    try {
      const out = await api.tutorStart(sid, nodeId);
      activeId = out.session.id;
      turns = [{
        id: "opening", role: "tutor", text: out.opening,
        level: out.session.level, confidence: null, verdict: null, created_at: Date.now(),
      }];
      await loadSessions();
    } catch (e) { err = String(e); }
    finally { busy = false; }
  }

  async function resume(s: api.TutorSession) {
    activeId = s.id;
    await loadHistory(s.id);
  }

  async function send() {
    if (!activeId || !draft.trim() || busy) return;
    busy = true; err = null;
    const answer = draft.trim();
    draft = "";
    try {
      const ex = await api.tutorReply(activeId, answer, confidence);
      turns = await api.tutorHistory(activeId);
      await loadSessions();
      if (ex.misjudged) await loadMisjudged();
      app.pushToast({
        kind: ex.misjudged ? "warning" : "info",
        title: ex.misjudged
          ? `答错且自信 —— 已记入「先修」队列`
          : `判定：${verdictLabel(ex.verdict)} · 档位 ${ex.level}`,
      });
    } catch (e) { err = String(e); }
    finally { busy = false; }
  }

  async function finish() {
    if (!activeId) return;
    try { await api.tutorEnd(activeId); } catch { /* ignore */ }
    activeId = null; turns = [];
    await loadSessions();
  }

  async function resolveSlot(id: string) {
    try { await api.tutorMisjudgedResolve(id); await loadMisjudged(); } catch { /* ignore */ }
  }

  function verdictLabel(v: string | null): string {
    if (v === "correct") return "对";
    if (v === "wrong") return "错";
    if (v === "partial") return "半对";
    return "";
  }
  function verdictColor(v: string | null): string {
    if (v === "correct") return "var(--ok, #639922)";
    if (v === "wrong") return "var(--danger, #e24b4a)";
    if (v === "partial") return "var(--warn, #BA7517)";
    return "var(--fg-faint)";
  }
  function confLabel(c: number | null): string {
    if (c == null) return "";
    return ["", "完全没底", "没底", "一般", "有底", "非常确定"][c] ?? "";
  }
  function whenLabel(ms: number): string {
    const d = new Date(ms);
    return `${d.getMonth() + 1}/${d.getDate()}`;
  }
</script>

<div class="tu">
  <header class="tu-head">
    <div>
      <div class="eyebrow">陪练台</div>
      <h1 class="tu-title">{app.activeSubject?.name ?? "未选中科目"}</h1>
      <p class="tu-sub mono">五档支架 · 会升也会降 · 答错且自信进「先修」队列</p>
    </div>
  </header>

  {#if !sid}
    <div class="tu-empty">
      <div class="tu-empty-big">先选一个科目</div>
      <div class="tu-empty-sub mono">陪练挂在学科的知识框架上；没有科目就没有可练的点。</div>
    </div>
  {:else}
    <div class="tu-cols">
      <aside class="tu-side">
        <section class="tu-card">
          <h2 class="tu-h2">练什么</h2>
          <select
            class="ipt"
            bind:value={pickedNode}
            onchange={() => { pickedNode = pickedNode; }}
          >
            <option value={null}>整个学科（广撒网）</option>
            {#each nodes as n (n.id)}
              <option value={n.id}>{n.title}{n.kind === "concept" ? " · 概念" : ""}</option>
            {/each}
          </select>
          <button class="btn primary wide" disabled={busy} onclick={() => begin(pickedNode)}>
            <Icon name="bolt" size={12} /> {busy ? "准备中…" : "开始陪练"}
          </button>
          {#if nodes.length === 0}
            <p class="tu-hint">这个学科还没有知识点。打开一次「学科总览」生成框架，或直接按整个学科练。</p>
          {/if}
        </section>

        {#if sessions.length > 0}
          <section class="tu-card">
            <h2 class="tu-h2">进行中的陪练</h2>
            {#each sessions as s (s.id)}
              <div class="tu-sess" class:on={s.id === activeId}>
                <button class="tu-sess-main" onclick={() => resume(s)}>
                  <span class="tu-sess-title">{s.node_title ?? s.subject_name}</span>
                  <span class="tu-sess-meta mono">第 {s.level} 档 · {s.turns} 轮</span>
                </button>
              </div>
            {/each}
          </section>
        {/if}

        {#if dueMisjudged.length > 0 || laterMisjudged.length > 0}
          <section class="tu-card">
            <h2 class="tu-h2">先修这些</h2>
            <p class="tu-hint">答错、但当时很确定的点。印象最深的时候错的，最值得回头。</p>
            {#each [...dueMisjudged, ...laterMisjudged] as m (m.id)}
              <div class="tu-slot" class:due={m.due}>
                <div class="tu-slot-head">
                  <span class="tu-slot-node">{m.node_title ?? m.subject_id}</span>
                  {#if !m.due}<span class="tu-slot-when mono">{whenLabel(m.scheduled_for)} 先修</span>{/if}
                </div>
                <div class="tu-slot-q">{m.question.split("\n")[0]}</div>
                <div class="tu-slot-a">你当时答：{m.answer}</div>
                <div class="tu-slot-acts">
                  {#if m.node_id}
                    <button class="btn tiny" onclick={() => begin(m.node_id)}>再练一次</button>
                  {/if}
                  <button class="btn tiny ghost" onclick={() => resolveSlot(m.id)}>解决了</button>
                </div>
              </div>
            {/each}
          </section>
        {/if}
      </aside>

      <main class="tu-main">
        {#if err}<div class="err mono">{err}</div>{/if}
        {#if !active}
          <div class="tu-empty">
            <div class="tu-empty-big">还没有开始</div>
            <div class="tu-empty-sub mono">
              左边选一个知识点开始。第一档由框架里的掌握度决定：越是不懂的点，支架给得越足。
            </div>
          </div>
        {:else}
          <div class="tu-ladder">
            {#each [1, 2, 3, 4, 5] as lv (lv)}
              {@const label = ["", "直接讲", "给线索", "提问引导", "只给目标", "纯自测"][lv]}
              <div
                class="tu-rung"
                class:passed={lv < active.level}
                class:cur={lv === active.level}
                title="第 {lv} 档 · {label}"
              >
                <span class="tu-rung-n mono">{lv}</span>
                <span class="tu-rung-l">{label}</span>
              </div>
            {/each}
            <span class="tu-sample mono">{sampleNote}</span>
          </div>

          <div class="tu-thread">
            {#each turns as t (t.id)}
              {#if t.role === "tutor"}
                <div class="tu-bubble tu-tutor">
                  <div class="tu-bubble-meta mono">
                    第 {t.level} 档
                    {#if t.verdict}<span style:color={verdictColor(t.verdict)}> · 上轮判 {verdictLabel(t.verdict)}</span>{/if}
                  </div>
                  <div class="tu-bubble-text">{t.text}</div>
                </div>
              {:else}
                <div class="tu-bubble tu-user">
                  <div class="tu-bubble-meta mono">
                    信心 {t.confidence} · {confLabel(t.confidence)}
                    {#if t.verdict}<span style:color={verdictColor(t.verdict)}> · {verdictLabel(t.verdict)}</span>{/if}
                  </div>
                  <div class="tu-bubble-text">{t.text}</div>
                </div>
              {/if}
            {/each}
            {#if busy}<div class="tu-bubble tu-tutor tu-thinking"><div class="tu-bubble-text">想想…</div></div>{/if}
          </div>

          <div class="tu-input">
            <textarea
              class="ipt area"
              placeholder="回答，或者问它问题…"
              bind:value={draft}
              onkeydown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); void send(); }
              }}
              disabled={busy}
            ></textarea>
            <div class="tu-conf">
              <span class="tu-conf-lbl mono">这轮多有底？</span>
              <div class="tu-conf-opts">
                {#each [1, 2, 3, 4, 5] as c (c)}
                  <button
                    class="tu-conf-btn"
                    class:on={confidence === c}
                    onclick={() => (confidence = c)}
                    title={confLabel(c)}
                  >{c}</button>
                {/each}
              </div>
            </div>
            <div class="tu-input-acts">
              <button class="btn primary" disabled={busy || !draft.trim()} onclick={() => void send()}>
                提交
              </button>
              <button class="btn ghost" disabled={busy} onclick={() => void finish()}>结束陪练</button>
            </div>
          </div>
        {/if}
      </main>
    </div>
  {/if}
</div>

<style>
  .tu {
    flex: 1; min-height: 0; overflow: auto;
    padding: 22px 28px 32px;
    display: flex; flex-direction: column; gap: 14px;
  }
  .eyebrow {
    font-family: var(--font-mono);
    font-size: 10.5px; color: var(--fg-faint);
    letter-spacing: 0.05em; text-transform: uppercase;
  }
  .mono { font-family: var(--font-mono); }
  .tu-head {
    display: flex; align-items: flex-start; justify-content: space-between; gap: 16px;
    border-bottom: 1px solid var(--border); padding-bottom: 14px;
  }
  .tu-title {
    margin: 4px 0 0;
    font-family: var(--font-display);
    font-size: 22px; font-weight: 600; letter-spacing: -0.01em;
    color: var(--fg-bright);
  }
  .tu-sub { margin: 6px 0 0; font-size: 11.5px; color: var(--fg-muted); }

  .tu-cols {
    display: grid;
    grid-template-columns: 300px minmax(0, 1fr);
    gap: 16px;
    align-items: start;
  }
  @media (max-width: 900px) {
    .tu-cols { grid-template-columns: 1fr; }
  }
  .tu-side { display: flex; flex-direction: column; gap: 12px; }
  .tu-card {
    border: 1px solid var(--border); border-radius: 10px;
    background: var(--surface);
    padding: 12px 14px;
    display: flex; flex-direction: column; gap: 8px;
  }
  .tu-h2 {
    margin: 0;
    font-family: var(--font-mono);
    font-size: 10.5px; font-weight: 500;
    color: var(--fg-faint);
    letter-spacing: 0.06em; text-transform: uppercase;
  }
  .tu-hint { margin: 0; font-size: 11px; color: var(--fg-faint); line-height: 1.6; }

  .ipt {
    font-family: var(--font-mono); font-size: 12px; color: var(--fg);
    background: var(--surface-2); border: 1px solid var(--border);
    border-radius: 7px; padding: 6px 9px; outline: none; min-width: 0; width: 100%;
  }
  .ipt:focus { border-color: var(--accent-dim); color: var(--fg-bright); }
  .ipt:disabled { opacity: 0.5; }
  .ipt.area { min-height: 64px; max-height: 160px; resize: vertical; line-height: 1.5; }

  .btn {
    display: inline-flex; align-items: center; justify-content: center; gap: 6px;
    font-family: var(--font-mono); font-size: 11.5px;
    padding: 7px 12px; border-radius: 7px; cursor: pointer;
    border: 1px solid var(--border); background: var(--surface-2); color: var(--fg);
  }
  .btn:hover:not(:disabled) { color: var(--fg-bright); border-color: var(--border-strong); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn.primary {
    background: color-mix(in srgb, var(--accent) 22%, transparent);
    border-color: color-mix(in srgb, var(--accent) 45%, transparent);
    color: var(--fg-bright);
  }
  .btn.ghost { background: transparent; color: var(--fg-muted); }
  .btn.wide { width: 100%; }
  .btn.tiny { padding: 3px 9px; font-size: 10.5px; }

  .err {
    padding: 10px 12px; border-radius: 7px;
    background: color-mix(in srgb, var(--err) 10%, var(--surface));
    border: 1px solid color-mix(in srgb, var(--err) 35%, transparent);
    color: var(--err); font-size: 11.5px;
  }

  .tu-sess {
    display: flex; align-items: center; gap: 6px;
    border: 1px solid transparent; border-radius: 7px;
  }
  .tu-sess.on { border-color: var(--accent); background: color-mix(in srgb, var(--accent) 8%, transparent); }
  .tu-sess-main {
    flex: 1; min-width: 0;
    display: flex; align-items: baseline; justify-content: space-between; gap: 8px;
    background: none; border: 0; cursor: pointer;
    font: inherit; color: var(--fg);
    padding: 6px 8px; text-align: left;
  }
  .tu-sess-title { font-size: 12px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .tu-sess-meta { font-size: 10px; color: var(--fg-faint); flex: none; }

  .tu-slot {
    border: 1px solid var(--border); border-radius: 8px;
    padding: 8px 10px;
    display: flex; flex-direction: column; gap: 4px;
  }
  .tu-slot.due { border-color: color-mix(in oklab, var(--danger, #e24b4a) 45%, var(--border)); }
  .tu-slot-head { display: flex; align-items: baseline; justify-content: space-between; gap: 8px; }
  .tu-slot-node { font-size: 12px; font-weight: 500; color: var(--fg-bright); }
  .tu-slot-when { font-size: 10px; color: var(--fg-faint); }
  .tu-slot-q { font-size: 11.5px; color: var(--fg-muted); line-height: 1.5; }
  .tu-slot-a { font-size: 11px; color: var(--fg-faint); line-height: 1.5; }
  .tu-slot-acts { display: flex; gap: 6px; margin-top: 2px; }

  .tu-main { display: flex; flex-direction: column; gap: 12px; min-width: 0; }

  .tu-empty {
    border: 1px dashed var(--border); border-radius: 10px;
    background: var(--surface);
    padding: 48px 24px; text-align: center;
  }
  .tu-empty-big {
    font-family: var(--font-display); font-size: 18px; font-weight: 600;
    color: var(--fg-bright);
  }
  .tu-empty-sub { margin: 8px auto 0; max-width: 460px; font-size: 11.5px; color: var(--fg-muted); line-height: 1.7; }

  .tu-ladder {
    display: flex; align-items: stretch; gap: 6px; flex-wrap: wrap;
  }
  .tu-rung {
    display: flex; align-items: center; gap: 6px;
    border: 1px solid var(--border); border-radius: 999px;
    padding: 4px 10px 4px 6px;
    color: var(--fg-faint);
    background: var(--surface);
  }
  .tu-rung-n {
    display: inline-flex; align-items: center; justify-content: center;
    width: 17px; height: 17px; border-radius: 50%;
    border: 1px solid var(--border);
    font-size: 10px;
  }
  .tu-rung-l { font-size: 11px; }
  .tu-rung.passed { color: var(--fg-muted); }
  .tu-rung.passed .tu-rung-n { border-color: var(--accent-dim); color: var(--accent); }
  .tu-rung.cur {
    border-color: var(--accent);
    color: var(--fg-bright);
    background: color-mix(in srgb, var(--accent) 12%, transparent);
  }
  .tu-rung.cur .tu-rung-n { background: var(--accent); border-color: var(--accent); color: var(--bg, #fff); }
  .tu-sample { font-size: 10.5px; color: var(--fg-faint); align-self: center; }

  .tu-thread {
    display: flex; flex-direction: column; gap: 10px;
    min-height: 200px;
  }
  .tu-bubble {
    max-width: 82%;
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 9px 12px;
    display: flex; flex-direction: column; gap: 5px;
  }
  .tu-bubble-meta { font-size: 10px; color: var(--fg-faint); }
  .tu-bubble-text { font-size: 12.5px; line-height: 1.65; white-space: pre-wrap; word-break: break-word; }
  .tu-tutor { align-self: flex-start; background: var(--surface); }
  .tu-user { align-self: flex-end; background: color-mix(in srgb, var(--accent) 7%, var(--surface)); }
  .tu-thinking { opacity: 0.6; }

  .tu-input {
    border: 1px solid var(--border); border-radius: 10px;
    background: var(--surface);
    padding: 10px 12px;
    display: flex; flex-direction: column; gap: 8px;
    position: sticky; bottom: 0;
  }
  .tu-conf { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .tu-conf-lbl { font-size: 10.5px; color: var(--fg-faint); }
  .tu-conf-opts { display: flex; gap: 4px; }
  .tu-conf-btn {
    width: 26px; height: 22px;
    border: 1px solid var(--border); border-radius: 5px;
    background: var(--surface-2); color: var(--fg-muted);
    font-family: var(--font-mono); font-size: 11px;
    cursor: pointer;
  }
  .tu-conf-btn:hover { color: var(--fg-bright); }
  .tu-conf-btn.on { border-color: var(--accent); color: var(--accent); background: color-mix(in srgb, var(--accent) 10%, transparent); }
  .tu-input-acts { display: flex; gap: 8px; justify-content: flex-end; }
</style>
