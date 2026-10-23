<script lang="ts">
  // Prompt lab — the one place where "this prompt got better" can be checked
  // instead of asserted.
  //
  // 实验台: edit the text a real call uses, then run the draft and the built-in
  // default against the same input and read both answers side by side.
  // 评测: fixtures (input + deterministic rules) scored for both versions. No
  // LLM grading its own homework — a rubric that can be talked into a pass is
  // not a rubric.
  // 检索: what RAG actually did with the question — tokenization, both channels,
  // rank fusion, the final cut — plus the 4-mode retrieval evaluation. Same
  // page because "why was this answer wrong" has one answer chain, not four.
  // 回答: the honest signals every answer already records — terminal state,
  // grounding, stripped fabrications, rollover lineage, frozen-quote drift —
  // aggregated, because separately they are trivia and together they say
  // whether the failure is the model, the question, or the library.
  import { app } from "../lib/store.svelte";
  import Icon from "../components/Icon.svelte";
  import * as api from "../lib/api";

  type Tab = "bench" | "eval" | "retrieval" | "health";
  let tab = $state<Tab>("bench");

  // Answer health: one read-only aggregate per (window, subject). Refetched
  // when either changes — the Lab is not a live dashboard, it is a snapshot
  // you take on purpose.
  let healthDays = $state(30);
  const HEALTH_WINDOWS = [
    { d: 7, label: "7d" },
    { d: 30, label: "30d" },
    { d: 90, label: "90d" },
    { d: 0, label: "全部" },
  ];
  let health = $state<api.AnswerHealth | null>(null);
  let healthErr = $state<string | null>(null);
  let healthLoading = $state(false);
  // Trend is its own request on a fixed 8-week window: "is this getting worse"
  // is a different question from "what does the last 30 days look like".
  let trend = $state<api.WeekBucket[] | null>(null);
  function fmtDay(ms: number) {
    const d = new Date(ms);
    return `${d.getMonth() + 1}/${d.getDate()}`;
  }
  async function loadHealth() {
    healthLoading = true;
    healthErr = null;
    try {
      health = await api.answerHealth(app.activeSubjectId ?? null, healthDays);
    } catch (e) {
      healthErr = String(e);
    } finally {
      healthLoading = false;
    }
    // A failed trend must not take the health report down with it: they answer
    // different questions and the chart is the optional one.
    try {
      trend = await api.answerHealthTrend(app.activeSubjectId ?? null, 8);
    } catch {
      trend = null;
    }
  }
  $effect(() => {
    if (tab !== "health") return;
    void healthDays;
    void app.activeSubjectId;
    void loadHealth();
  });
  // Citation ledger: the rows behind the drift rate above. On demand — a full
  // audit re-checks every frozen quote against its source, which is fine to ask
  // for and wrong to run on every tab switch.
  let audit = $state<api.CitationAudit | null>(null);
  let auditErr = $state<string | null>(null);
  let auditLoading = $state(false);
  let repairing = $state(false);
  async function runAudit() {
    auditLoading = true;
    auditErr = null;
    try {
      audit = await api.auditCitations(app.activeSubjectId ?? null, healthDays);
    } catch (e) {
      auditErr = String(e);
    } finally {
      auditLoading = false;
    }
  }
  async function repairAll() {
    repairing = true;
    auditErr = null;
    try {
      const r = await api.repairDriftedCitations(app.activeSubjectId ?? null, healthDays);
      await runAudit();
      await loadHealth();
      app.pushToast({
        kind: r.repaired > 0 ? "info" : "error",
        title: r.repaired > 0 ? `重锚 ${r.repaired} 条引用` : "没有可重锚的引用",
        body:
          r.refused > 0
            ? `${r.refused} 条没有诚实的锚点，保持原样（不猜相似段落）`
            : "引文已按现文重新冻结，旧引文留痕可查",
      });
    } catch (e) {
      auditErr = String(e);
    } finally {
      repairing = false;
    }
  }
  async function repairOne(row: api.AuditedCitation) {
    auditErr = null;
    try {
      const out = await api.refreezeMessageCitation(row.message_id, row.citation.chunk_id);
      if (!out.citation) {
        const why =
          out.reanchor.kind === "unanchorable" ? out.reanchor.reason : out.reanchor.kind;
        app.pushToast({ kind: "error", title: "无法重锚", body: why });
        return;
      }
      // Re-audit instead of patching optimistically: a "fixed" badge that isn't
      // fixed is worse than the drift it replaced.
      await runAudit();
      await loadHealth();
    } catch (e) {
      auditErr = String(e);
    }
  }
  const AUDIT_GROUPS = [
    { key: "drifted", label: "与现文不符", tone: "warn" },
    { key: "source_gone", label: "来源已删", tone: "warn" },
    { key: "unchecked", label: "不可核验（无快照）", tone: "mute" },
    { key: "moved", label: "已移位（引文仍成立）", tone: "ok" },
    { key: "exact", label: "与现文一致", tone: "ok" },
  ] as const;
  function rowsOf(a: api.CitationAudit, key: string) {
    return a.rows.filter((r) => (key === "unchecked" ? r.check === null : r.check?.kind === key));
  }
  const STATE_LABEL: Record<string, string> = {
    completed: "正常结束",
    truncated: "长度截断",
    blocked: "安全拦截",
    aborted: "手动中止",
    failed: "失败",
    unrecorded: "未记录",
  };
  const HEALTH_TONE: Record<string, string> = {
    completed: "ok",
    truncated: "warn",
    blocked: "warn",
    aborted: "info",
    failed: "warn",
    unrecorded: "info",
  };
  function share(n: number, d: number) {
    return d > 0 ? Math.round((n / d) * 100) : 0;
  }
  /** Share of checkable snapshots that no longer hold — same definition as
   *  `SnapshotHealth::drift_pct` on the backend, so the badge and the verdict
   *  line can never disagree. */
  function driftPct(s: api.SnapshotHealth) {
    return share(s.drifted + s.gone, s.checked);
  }

  let infos = $state<api.PromptInfo[]>([]);
  let loading = $state(true);
  let loadErr = $state<string | null>(null);
  let selectedKey = $state("");

  const sel = $derived(infos.find((p) => p.key === selectedKey) ?? null);
  const groups = $derived.by(() => {
    const out: { group: string; items: api.PromptInfo[] }[] = [];
    for (const p of infos) {
      const hit = out.find((g) => g.group === p.group);
      if (hit) hit.items.push(p);
      else out.push({ group: p.group, items: [p] });
    }
    return out;
  });

  // The editor buffer. Kept separate from `sel.body` so "unsaved" is a fact we
  // can show rather than a guess.
  let body = $state("");
  let input = $state("");
  let dirty = $derived(sel !== null && body !== sel.body);
  let bodyChars = $derived([...body].length);
  let delta = $derived(sel ? bodyChars - [...sel.default_body].length : 0);

  let busy = $state<string | null>(null); // which action is running
  let runA = $state<api.PromptRun | null>(null);
  let runB = $state<api.PromptRun | null>(null);
  let runErr = $state<string | null>(null);
  const panes = $derived<{ label: string; res: api.PromptRun | null }[]>([
    { label: "A · 草稿", res: runA },
    { label: "B · 内置默认", res: runB },
  ]);

  // ---- transparency: retrieval trace + 4-mode eval ----
  let traceQuery = $state("");
  let trace = $state<api.RetrievalTrace | null>(null);
  let traceErr = $state<string | null>(null);
  let traceBusy = $state(false);
  let modes = $state<api.EvalOutcome | null>(null);
  let modesKind = $state<"synthetic" | "real" | null>(null);
  let modesErr = $state<string | null>(null);
  // The shipping config the adoption rule judges (must match the Rust const).
  const VERDICT_MODE = "hybrid+rerank";
  const verdictMode = $derived(modes?.reports.find((r) => r.mode === VERDICT_MODE) ?? null);
  const verdict = $derived(modes?.verdict ?? null);

  /** 语言表把机器标签翻成人话（zh / en / mix 直接摆出来太生硬）。 */
  const LANG_LABEL: Record<string, string> = { zh: "中文", en: "英文", mix: "中英混" };
  const langLabel = (l: string) => LANG_LABEL[l] ?? l;

  /** 「关键词腿失效会掉多少」的实时答案：中文在纯向量与出厂配置之间的落差。
   *  没有落差就没有故事，返回 null 让那一行提示消失，而不是印一个 0。 */
  const zhGap = $derived.by(() => {
    const zh = (mode: string) =>
      modes?.reports.find((r) => r.mode === mode)?.langs.find((l) => l.lang === "zh") ?? null;
    const base = zh("vector");
    const best = zh(VERDICT_MODE);
    if (!base || !best || best.recall_at_k <= base.recall_at_k) return null;
    return { base: base.recall_at_k, best: best.recall_at_k, gap: best.recall_at_k - base.recall_at_k };
  });

  const channels = $derived<{ name: string; hits: api.TraceHit[] }[]>(
    trace
      ? [
          { name: "向量", hits: trace.vector },
          { name: "关键词", hits: trace.keyword },
          { name: "融合", hits: trace.fused },
          { name: "最终", hits: trace.final_hits },
        ]
      : [],
  );

  async function runTrace() {
    if (!traceQuery.trim() || traceBusy) return;
    traceBusy = true;
    traceErr = null;
    try {
      trace = await api.retrievalTrace(traceQuery.trim(), app.activeSubject?.id ?? null, 6);
    } catch (e) {
      traceErr = String(e);
      trace = null;
    } finally {
      traceBusy = false;
    }
  }

  async function runModes(kind: "synthetic" | "real") {
    if (busy === "modes") return;
    busy = "modes";
    modesErr = null;
    modesKind = kind;
    try {
      modes =
        kind === "synthetic"
          ? await api.retrievalEval(5)
          : await api.retrievalEvalReal(app.activeSubject?.id ? [app.activeSubject.id] : null, 5);
    } catch (e) {
      modesErr = String(e);
      modes = null;
    } finally {
      busy = null;
    }
  }

  let evals = $state<api.PromptEval[]>([]);
  let historyRows = $state<api.PromptEvalRunSummary[]>([]);
  let outcome = $state<api.PromptEvalOutcome | null>(null);
  let evalErr = $state<string | null>(null);
  let adding = $state(false);
  let newName = $state("");
  let newInput = $state("");
  let newRules = $state("contains: \nmaxchars: 1200");

  async function load() {
    loading = true;
    loadErr = null;
    try {
      infos = await api.promptList();
      const remembered = (() => {
        try { return localStorage.getItem("cortex.lab.prompt"); } catch { return null; }
      })();
      const keep = infos.find((p) => p.key === selectedKey)?.key
        ?? infos.find((p) => p.key === remembered)?.key
        ?? infos[0]?.key
        ?? "";
      if (keep) select(keep, false);
    } catch (e) {
      loadErr = String(e);
    } finally {
      loading = false;
    }
  }

  function select(key: string, resetInput = true) {
    const p = infos.find((x) => x.key === key);
    if (!p) return;
    selectedKey = key;
    body = p.body;
    if (resetInput) input = p.sample;
    runA = null;
    runB = null;
    runErr = null;
    outcome = null;
    evalErr = null;
    try { localStorage.setItem("cortex.lab.prompt", key); } catch { /* private mode */ }
    void loadEvals(key);
  }

  async function loadEvals(key = selectedKey) {
    if (!key) return;
    try { evals = await api.promptEvals(key); } catch { evals = []; }
    try { historyRows = await api.promptEvalHistory(key, 6); } catch { historyRows = []; }
  }

  function when(ts: number) {
    const d = new Date(ts);
    const p = (n: number) => String(n).padStart(2, "0");
    return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
  }

  async function save() {
    if (!sel || busy) return;
    busy = "save";
    try {
      await api.promptSave(sel.key, body);
      await load();
      select(selectedKey, false);
      app.pushToast({ kind: "success", title: "已保存", body: "下一次调用就用这一版" });
    } catch (e) {
      app.pushToast({ kind: "error", title: "保存失败", body: String(e) });
    } finally {
      busy = null;
    }
  }

  async function resetToDefault() {
    if (!sel || busy) return;
    busy = "reset";
    try {
      await api.promptReset(sel.key);
      await load();
      select(selectedKey, false);
      app.pushToast({ kind: "success", title: "已还原内置默认" });
    } catch (e) {
      app.pushToast({ kind: "error", title: "还原失败", body: String(e) });
    } finally {
      busy = null;
    }
  }

  async function run(which: "a" | "b") {
    if (!sel || busy) return;
    runErr = null;
    busy = which;
    try {
      // A is the draft in the editor; B is always the built-in default, so the
      // comparison survives a save.
      const res = which === "a"
        ? await api.promptRun(sel.key, input, body)
        : await api.promptRun(sel.key, input, sel.default_body);
      if (which === "a") runA = res;
      else runB = res;
    } catch (e) {
      runErr = String(e);
    } finally {
      busy = null;
    }
  }

  async function addEval() {
    if (!sel || busy) return;
    busy = "add";
    evalErr = null;
    try {
      await api.promptEvalAdd(sel.key, newName, newInput, newRules);
      adding = false;
      newName = "";
      newInput = "";
      newRules = "contains: \nmaxchars: 1200";
      await loadEvals();
    } catch (e) {
      evalErr = String(e);
    } finally {
      busy = null;
    }
  }

  async function delEval(id: string) {
    try {
      await api.promptEvalDelete(id);
      await loadEvals();
      outcome = null;
    } catch (e) {
      app.pushToast({ kind: "error", title: "删除失败", body: String(e) });
    }
  }

  async function scoreAll() {
    if (!sel || busy) return;
    busy = "score";
    evalErr = null;
    outcome = null;
    try {
      // Deliberately no reload: judging must not discard an unsaved draft —
      // the draft is exactly what is being judged.
      outcome = await api.promptEvalRun(sel.key, body, sel.default_body);
      await loadEvals();
    } catch (e) {
      evalErr = String(e);
    } finally {
      busy = null;
    }
  }

  $effect(() => { void load(); });
</script>

<div class="lab">
  <header class="lab-head">
    <div>
      <div class="eyebrow">Lab</div>
      <h1 class="lab-title">实验室</h1>
      <p class="lab-sub">
        改一条提示词，跑一次，和上一版并排看；再用固定用例判分。判分只看规则，不让模型给自己打分。
      </p>
    </div>
    <div class="seg" role="tablist">
      <button class="seg-btn" class:on={tab === "bench"} onclick={() => (tab = "bench")} role="tab" aria-selected={tab === "bench"}>
        <Icon name="pencil" size={12} /> 实验台
      </button>
      <button class="seg-btn" class:on={tab === "eval"} onclick={() => (tab = "eval")} role="tab" aria-selected={tab === "eval"}>
        <Icon name="check" size={12} /> 评测
        {#if evals.length}<span class="seg-count">{evals.length}</span>{/if}
      </button>
      <button class="seg-btn" class:on={tab === "retrieval"} onclick={() => (tab = "retrieval")} role="tab" aria-selected={tab === "retrieval"}>
        <Icon name="search" size={12} /> 检索
      </button>
      <button class="seg-btn" class:on={tab === "health"} onclick={() => (tab = "health")} role="tab" aria-selected={tab === "health"}>
        <Icon name="check" size={12} /> 回答
      </button>
    </div>
  </header>

  {#if loadErr}
    <div class="lab-err">{loadErr}</div>
  {:else if loading}
    <div class="lab-empty">正在读取提示词…</div>
  {:else if tab === "health"}
    <section class="lab-main lab-wide">
      <div class="lab-sec first">
        <div class="lab-sec-h">
          <h3 class="lab-sec-t">回答健康度</h3>
          <div class="seg" role="tablist" aria-label="统计窗口">
            {#each HEALTH_WINDOWS as w (w.d)}
              <button
                class="seg-opt"
                class:on={healthDays === w.d}
                onclick={() => (healthDays = w.d)}
                role="tab"
                aria-selected={healthDays === w.d}>{w.label}</button
              >
            {/each}
          </div>
        </div>
        <div class="lab-hint">
          每个回答本来就在记五件事：怎么结束的、有没有检索到依据、有没有编造引用、是不是接在上一次对话之后、引用快照还对不对得上。分开看都是琐事，合起来能说清问题出在模型、问题，还是库。
        </div>

        {#if healthErr}
          <div class="lab-err">{healthErr}</div>
        {:else if healthLoading && !health}
          <div class="lab-empty">正在统计…</div>
        {:else if health}
          {#if health.answers === 0}
            <div class="lab-empty">
              这个窗口里没有回答记录{healthDays > 0 ? `（最近 ${healthDays} 天）` : ""}——先问几个问题再回来看。
            </div>
          {:else}
            <div class="ah-stats">
              <div class="ah-stat">
                <span class="ah-v">{health.answers}</span>
                <span class="ah-k mono">回答</span>
              </div>
              <div class="ah-stat">
                <span class="ah-v">{share(health.grounded, health.answers)}%</span>
                <span class="ah-k mono">检索有依据</span>
              </div>
              <div class="ah-stat">
                <span class="ah-v">{share(health.cited, health.answers)}%</span>
                <span class="ah-k mono">行内引用了来源</span>
              </div>
              <div class="ah-stat">
                <span class="ah-v"
                  >{health.snapshots.checked === 0
                    ? "—"
                    : `${100 - driftPct(health.snapshots)}%`}</span
                >
                <span class="ah-k mono">引用快照对得上</span>
              </div>
            </div>

            <div class="ah-states">
              {#each health.states as s (s.state)}
                <div class="ah-row">
                  <span class="ah-row-k">{STATE_LABEL[s.state] ?? s.state}</span>
                  <span class="ah-track">
                    <span
                      class="ah-fill"
                      class:warn={HEALTH_TONE[s.state] === "warn"}
                      class:info={HEALTH_TONE[s.state] === "info"}
                      style:width={`${Math.max(0, share(s.count, health.answers))}%`}
                    ></span>
                  </span>
                  <span class="ah-row-v mono">{s.count}</span>
                </div>
              {/each}
            </div>

            <ul class="ah-signals">
              {#each health.signals as s, i (i)}
                <li class="ah-sig" class:warn={s.level === "warn"} class:ok={s.level === "ok"}>
                  <span class="ah-dot"></span>{s.text}
                </li>
              {/each}
            </ul>

            <div class="ah-facts mono">
              伪造引用剥离 {health.fabricated_total} 条（{health.fabricated_answers} 个回答） · 无依据回答
              {health.abstained} 个 · 接续对话 {health.rolled_threads} 段 · 快照：精确
              {health.snapshots.exact} / 移位 {health.snapshots.moved} / 与现文不符
              {health.snapshots.drifted} / 来源已删 {health.snapshots.gone} / 已重锚
              {health.snapshots.reanchored} / 无快照 {health.snapshots.unchecked}
            </div>

            {#if trend && trend.some((w) => w.answers > 0)}
              <div class="ah-trend-wrap">
                <div class="ah-trend-h">
                  <span>近 8 周失败率</span>
                  <span class="lab-hint">
                    柱高 = 失败+拦截 / 回答数；空槽表示那 7 天里没有提问
                  </span>
                </div>
                <div class="ah-trend">
                  {#each trend as w (w.start)}
                    {@const rate = w.answers > 0 ? (w.failed / w.answers) * 100 : 0}
                    <div
                      class="ah-tc"
                      title={`${fmtDay(w.start)}–${fmtDay(w.end)} · ${w.answers} 个回答 · 失败 ${w.failed} · 截断 ${w.truncated} · 无依据 ${w.abstained}`}
                    >
                      {#if w.answers > 0}
                        <div
                          class="ah-tb"
                          class:zero={rate === 0}
                          style:height={`${Math.max(rate, 4)}%`}
                        ></div>
                      {:else}
                        <div class="ah-tb empty"></div>
                      {/if}
                    </div>
                  {/each}
                </div>
                <div class="ah-trend-x mono">
                  <span>{fmtDay(trend[0].start)}</span>
                  <span>本周</span>
                </div>
              </div>
            {/if}

            <div class="ah-ledger">
              <div class="lab-sec-h">
                <h3 class="lab-sec-t">引用台账</h3>
                <span class="lab-hint">
                  逐条核验库里冻结的引文——上面的漂移率就是这张表算出来的，两者不会打架
                </span>
              </div>
              <div class="ah-ledger-bar">
                <button class="btn btn--sm" onclick={() => void runAudit()} disabled={auditLoading}>
                  <Icon name="search" size={11} />
                  {auditLoading ? "核验中…" : "核验全库引用"}
                </button>
                {#if audit && audit.drifted > 0}
                  <button class="btn btn--sm" onclick={() => void repairAll()} disabled={repairing}>
                    {repairing ? "重锚中…" : `重锚 ${audit.drifted} 条`}
                  </button>
                {/if}
                {#if audit}
                  <span class="lab-hint mono">
                    {new Date(audit.audited_at).toLocaleTimeString()} 核验
                  </span>
                {/if}
              </div>
              {#if auditErr}
                <div class="lab-err">{auditErr}</div>
              {/if}
              {#if audit}
                <div class="ah-ledger-sum mono">
                  {audit.citations} 条引文 / {audit.answers} 个回答 · 一致 {audit.exact} · 移位
                  {audit.moved} · 不符 {audit.drifted} · 来源已删 {audit.source_gone} · 不可核验
                  {audit.unchecked} · 已重锚 {audit.reanchored}
                </div>
                {#if audit.citations === 0}
                  <div class="lab-hint">这个窗口里没有带引文的回答。</div>
                {:else}
                  {#each AUDIT_GROUPS as g (g.key)}
                    {@const rows = rowsOf(audit, g.key)}
                    {#if rows.length}
                      <details
                        class="ah-grp"
                        open={g.key === "drifted" || g.key === "source_gone"}
                      >
                        <summary class="ah-grp-sum">
                          <span
                            class="ah-dot"
                            class:warn={g.tone === "warn"}
                            class:ok={g.tone === "ok"}
                          ></span>
                          {g.label}
                          <span class="faint mono">{rows.length}</span>
                        </summary>
                        <ul class="ah-rows">
                          {#each rows as r, i (r.message_id + "|" + r.citation.chunk_id + "|" + i)}
                            <li class="ah-row">
                              <span class="ah-row-src">{r.citation.source_name}</span>
                              {#if r.citation.loc}<span class="faint mono">{r.citation.loc}</span
                                >{/if}
                              <span class="ah-row-quote">
                                {r.citation.quote || r.citation.snippet || "（无冻结引文）"}
                              </span>
                              {#if r.reanchored}<span class="ah-row-tag">已重锚</span>{/if}
                              {#if g.key === "drifted"}
                                <button
                                  class="btn btn--sm btn--ghost"
                                  onclick={() => void repairOne(r)}>重锚这条</button
                                >
                              {/if}
                            </li>
                          {/each}
                        </ul>
                      </details>
                    {/if}
                  {/each}
                {/if}
              {/if}
            </div>
          {/if}
        {/if}
      </div>
    </section>
  {:else if !infos.length}
    <div class="lab-empty">还没有可编辑的提示词。</div>
  {:else if tab === "retrieval"}
    <section class="lab-main lab-wide">
      <div class="lab-sec first">
        <div class="lab-sec-h">
          <h3 class="lab-sec-t">检索元数据</h3>
          <span class="lab-hint">
            问一句，看它在库里怎么走：切词 → 向量 → 关键词 → 排序融合 → 最终交给模型的那几段
          </span>
        </div>
        <div class="lab-trace-form">
          <input
            class="ipt"
            bind:value={traceQuery}
            placeholder="例如：扩散模型的采样加速 / TCP 三次握手为什么不用四次"
            onkeydown={(e) => e.key === "Enter" && void runTrace()}
          />
          <button class="btn btn--primary btn--sm" disabled={traceBusy || !traceQuery.trim()} onclick={() => void runTrace()}>
            {traceBusy ? "检索中…" : "跑一次"}
          </button>
        </div>
        {#if traceErr}<div class="lab-err">{traceErr}</div>{/if}

        {#if trace}
          <div class="lab-trace-meta mono faint">
            <span>{trace.ms} ms</span>
            <span>{trace.subject_name ?? "整个库"}</span>
            <span>向量 {trace.embed_provider} {trace.embed_ok ? "✓" : "✗"}</span>
            <span>重排 {trace.rerank ? "开" : "关"}</span>
          </div>
          {#if trace.embed_error}
            <div class="lab-trace-warn">{trace.embed_error}</div>
          {/if}
          <div class="lab-chips">
            {#each trace.terms as t (t)}<span class="lab-chip mono">{t}</span>{/each}
          </div>

          <div class="lab-cols">
            {#each channels as ch (ch.name)}
              <div class="lab-col">
                <div class="lab-col-h">
                  {ch.name}<span class="mono faint">{ch.hits.length}</span>
                </div>
                {#if !ch.hits.length}
                  <p class="lab-col-none">没有命中</p>
                {/if}
                {#each ch.hits as h, i (h.chunk_id + i)}
                  <div class="lab-hit">
                    <div class="lab-hit-h">
                      <span class="mono faint">#{i + 1}</span>
                      <span class="lab-hit-n">{h.source_name}</span>
                      <span style="flex:1"></span>
                      <span class="mono faint">{h.score.toFixed(3)}</span>
                    </div>
                    {#if h.loc}<span class="lab-hit-l mono">{h.loc}</span>{/if}
                    <p class="lab-hit-p">{h.preview}</p>
                  </div>
                {/each}
              </div>
            {/each}
          </div>
          <p class="lab-hint">
            「最终」是真正塞进提示词的那几段。前三列没有的，模型也没看到。
          </p>
        {/if}
      </div>

      <div class="lab-sec">
        <div class="lab-sec-h">
          <h3 class="lab-sec-t">4 模式评测</h3>
          <span class="lab-hint">
            vector / rerank / hybrid / hybrid+rerank —— 同一批问题，哪种配置在这台机器上更准
          </span>
        </div>
        <div class="lab-actions">
          <button class="btn btn--sm" disabled={busy === "modes"} onclick={() => void runModes("synthetic")}>
            跑合成语料
          </button>
          <button class="btn btn--sm" disabled={busy === "modes"} onclick={() => void runModes("real")}>
            跑我的库
          </button>
          <span class="lab-hint">合成语料量的是管道本身；我的库量的是你这些文档的真实召回。</span>
        </div>
        {#if modesErr}<div class="lab-err">{modesErr}</div>{/if}
        {#if modes}
          {#if !modes.reports.length}
            <p class="lab-empty-inline">这一档没有可评测的问题——库里还没索引出足够的段落。</p>
          {:else}
            <div class="lab-table">
              <div class="lab-tr lab-th">
                <span>模式</span><span>Recall@5</span><span>MRR</span><span>问题数</span>
              </div>
              {#each modes.reports as m (m.mode)}
                <div class="lab-tr">
                  <span class="mono">{m.mode}</span>
                  <span class="mono">{(m.recall_at_k * 100).toFixed(1)}%</span>
                  <span class="mono">{m.mrr.toFixed(3)}</span>
                  <span class="mono faint">{m.evaluated}</span>
                </div>
              {/each}
            </div>
            <p class="lab-hint">
              {modesKind === "synthetic" ? "合成语料" : "你的库"} · k = {modes.reports[0].k} · 只看召回与名次，不看生成的文字。
            </p>

            <!-- 采纳规则（KnowNote borrow）：与上一次同档运行机械对比。 -->
            {#if verdict}
              <div class="lab-verdict" class:lab-verdict-adopt={verdict.overall === "adopt"} class:lab-verdict-regress={verdict.overall === "regress"}>
                <span class="lab-verdict-tag mono">
                  {verdict.overall === "adopt" ? "✓ ADOPT" : verdict.overall === "regress" ? "✗ REGRESS" : "— NEUTRAL"}
                </span>
                <span class="lab-verdict-mode mono">{verdict.mode} · 对比上次同配置运行</span>
                <div class="lab-verdict-metrics">
                  {#each verdict.metrics as mv (mv.metric)}
                    <span class="lab-vm" class:lab-vm-adopt={mv.verdict === "adopt"} class:lab-vm-regress={mv.verdict === "regress"}>
                      {mv.metric} {mv.baseline.toFixed(3)} → {mv.candidate.toFixed(3)}（{mv.delta >= 0 ? "+" : ""}{mv.delta.toFixed(3)}）
                    </span>
                  {/each}
                </div>
                <p class="lab-hint">{verdict.note}</p>
              </div>
            {/if}

            <!-- 分层明细：只列出厂配置那一档——规则也只审它。 -->
            {#if verdictMode && verdictMode.strata.length}
              <div class="lab-table">
                <div class="lab-tr lab-th lab-tr5">
                  <span>分层 · {verdictMode.mode}</span><span>Recall@5</span><span>MRR</span><span>题数</span><span>top1 均分</span>
                </div>
                {#each verdictMode.strata as s (s.kind)}
                  <div class="lab-tr lab-tr5">
                    <span class="mono">{s.kind}</span>
                    <span class="mono">{(s.recall_at_k * 100).toFixed(1)}%</span>
                    <span class="mono">{s.mrr.toFixed(3)}</span>
                    <span class="mono faint">{s.cases}</span>
                    <span class="mono faint">{s.mean_top1.toFixed(3)}</span>
                  </div>
                {/each}
              </div>
              {#if verdictMode.separation}
                <p class="lab-hint">
                  弃权分离度：可答 {verdictMode.separation.answerable_mean_top1.toFixed(3)} vs 不可答{" "}
                  {verdictMode.separation.unanswerable_mean_top1.toFixed(3)}（差距{" "}
                  {(verdictMode.separation.ratio * 100).toFixed(1)}%）—— 这个差距是「零命中」徽章的全部依据，垮掉它比掉几个召回点更严重。
                </p>
              {/if}
            {/if}

            <!-- 语言维度：中文这条链到底有没有在干活 -->
            {#if verdictMode && verdictMode.langs.length}
              <div class="lab-table">
                <div class="lab-tr lab-th lab-tr4">
                  <span>语言 · {verdictMode.mode}</span><span>Recall@5</span><span>MRR</span><span>关键词腿</span>
                </div>
                {#each verdictMode.langs as l (l.lang)}
                  <div class="lab-tr lab-tr4">
                    <span class="mono">{langLabel(l.lang)}<span class="faint"> · {l.answerable} 题</span></span>
                    <span class="mono">{(l.recall_at_k * 100).toFixed(1)}%</span>
                    <span class="mono">{l.mrr.toFixed(3)}</span>
                    <span class="mono faint">
                      {#if verdictMode.mode.startsWith("hybrid")}{l.keyword_hits}/{l.answerable}{:else}—{/if}
                    </span>
                  </div>
                {/each}
              </div>
              <p class="lab-hint">
                语言分层的召回只按「可答题」算：弃权题没有正确答案，算进去只会让恰好装着它们的那门语言显得更差。
                「关键词腿」是纯关键词通道自己就把正确来源排进前 5 的比例——只有 hybrid 档跑这条腿，其余档显示 —。
              </p>
              {#if zhGap}
                <p class="lab-hint">
                  中文：纯向量 {(zhGap.base * 100).toFixed(1)}% → {VERDICT_MODE} {(zhGap.best * 100).toFixed(1)}%，
                  差的 {(zhGap.gap * 100).toFixed(1)} 个百分点就是关键词腿在中文上的全部贡献。汉字没有空格，
                  这条腿靠二元组切分才匹配得上；换成按空格或整段汉字切分（FTS5 默认就是这样），这一格直接掉到 0。
                </p>
              {/if}
            {/if}
          {/if}
        {/if}
      </div>
    </section>
  {:else}
    <div class="lab-grid">
      <nav class="lab-rail" aria-label="提示词列表">
        {#each groups as g (g.group)}
          <div class="lab-group">{g.group}</div>
          {#each g.items as p (p.key)}
            <button class="lab-item" class:on={p.key === selectedKey} onclick={() => select(p.key)}>
              <span class="lab-item-t">{p.label}</span>
              {#if p.custom}<span class="lab-badge">已改</span>{/if}
            </button>
          {/each}
        {/each}
      </nav>

      <section class="lab-main">
        {#if sel}
          <div class="lab-prompt-head">
            <h2 class="lab-prompt-t">{sel.label}</h2>
            <span class="mono faint">{sel.key}</span>
            {#if sel.custom}<span class="lab-badge on">自定义</span>{/if}
          </div>

          {#if tab === "bench"}
            <textarea
              class="lab-body"
              bind:value={body}
              spellcheck="false"
              aria-label="提示词正文"
            ></textarea>
            <div class="lab-body-foot">
              <span class="mono">
                {bodyChars} 字
                {#if delta !== 0}
                  · <span class:lab-neg={delta < 0}>{delta > 0 ? "+" : ""}{delta}</span> 相对默认
                {/if}
                {#if dirty}<span class="lab-dot">● 未保存</span>{/if}
              </span>
              <span style="flex:1"></span>
              {#if dirty}
                <button class="btn btn--sm" onclick={() => (body = sel.body)}>撤销</button>
              {/if}
              <button class="btn btn--sm" disabled={!!busy || !sel.custom} onclick={() => void resetToDefault()}>
                还原默认
              </button>
              <button class="btn btn--primary btn--sm" disabled={!!busy || !dirty} onclick={() => void save()}>
                {busy === "save" ? "保存中…" : "保存并生效"}
              </button>
            </div>

            <div class="lab-sec">
              <div class="lab-sec-h">
                <h3 class="lab-sec-t">试跑</h3>
                <span class="lab-hint">A = 编辑框里的草稿 · B = 内置默认</span>
              </div>
              <textarea class="lab-input" bind:value={input} spellcheck="false" aria-label="测试输入"></textarea>
              <div class="lab-actions">
                <button class="btn btn--sm" disabled={!!busy || !input.trim()} onclick={() => void run("a")}>
                  {busy === "a" ? "跑 A…" : "跑 A（草稿）"}
                </button>
                <button class="btn btn--sm" disabled={!!busy || !input.trim()} onclick={() => void run("b")}>
                  {busy === "b" ? "跑 B…" : "跑 B（默认）"}
                </button>
                <span style="flex:1"></span>
                <button class="btn btn--sm btn--ghost" disabled={!!busy} onclick={() => { runA = null; runB = null; runErr = null; }}>
                  清空结果
                </button>
              </div>
              {#if runErr}<div class="lab-err">{runErr}</div>{/if}

              {#if runA || runB}
                <div class="lab-runs">
                  {#each panes as pane (pane.label)}
                    <div class="lab-run" class:empty={!pane.res}>
                      <div class="lab-run-h">
                        <span class="lab-run-t">{pane.label}</span>
                        {#if pane.res}
                          <span class="mono faint">{pane.res.ms} ms · {pane.res.chars} 字 · {pane.res.model}</span>
                        {/if}
                      </div>
                      {#if pane.res}
                        <pre class="lab-run-body">{pane.res.text}</pre>
                        <details class="lab-raw">
                          <summary>
                            原始响应
                            {#if pane.res.cleaned}· 含被剥离的推理块{/if}
                            {#if pane.res.finish_reason}· {pane.res.finish_reason}{/if}
                          </summary>
                          <pre class="lab-raw-body">{pane.res.raw || "（空）"}</pre>
                          {#if pane.res.usage}
                            <div class="lab-raw-usage mono">{JSON.stringify(pane.res.usage)}</div>
                          {/if}
                        </details>
                      {:else}
                        <p class="lab-run-none">还没跑。</p>
                      {/if}
                    </div>
                  {/each}
                </div>
              {/if}
            </div>
          {:else}
            <div class="lab-sec first">
              <div class="lab-sec-h">
                <h3 class="lab-sec-t">用例</h3>
                <span class="lab-hint">
                  每条规则一行：contains: 文字 · absent: 文字 · json · minchars: N · maxchars: N · regex: 模式
                </span>
              </div>

              {#if !evals.length}
                <p class="lab-empty-inline">
                  还没有用例。一条用例 = 一个输入 + 几条必须成立的规则，跑的时候 A、B 各答一遍。
                </p>
              {:else}
                <div class="lab-fix-list">
                  {#each evals as e (e.id)}
                    <div class="lab-fix">
                      <div class="lab-fix-h">
                        <span class="lab-fix-n">{e.name}</span>
                        <span style="flex:1"></span>
                        <button class="btn btn--icon btn--sm btn--ghost" title="删除用例" onclick={() => void delEval(e.id)}>
                          <Icon name="x" size={11} />
                        </button>
                      </div>
                      <div class="lab-fix-r mono">{e.rules || "（没有规则——任何输出都算通过）"}</div>
                      <div class="lab-fix-i">{e.input}</div>
                    </div>
                  {/each}
                </div>
              {/if}

              {#if adding}
                <div class="lab-fix-add">
                  <input class="ipt" placeholder="用例名字，例如「必须给出具体年份」" bind:value={newName} />
                  <textarea class="lab-input sm" placeholder="测试输入" bind:value={newInput}></textarea>
                  <textarea class="lab-input sm mono" placeholder="contains: 2021&#10;absent: 抱歉" bind:value={newRules}></textarea>
                  <div class="lab-actions">
                    <button class="btn btn--primary btn--sm" disabled={busy === "add"} onclick={() => void addEval()}>
                      {busy === "add" ? "添加中…" : "添加"}
                    </button>
                    <button class="btn btn--sm btn--ghost" onclick={() => (adding = false)}>取消</button>
                  </div>
                </div>
              {:else}
                <button class="btn btn--sm" onclick={() => { adding = true; newInput = input || sel.sample; }}>
                  <Icon name="plus" size={11} /> 加一条用例
                </button>
              {/if}

              {#if evalErr}<div class="lab-err">{evalErr}</div>{/if}
            </div>

            <div class="lab-sec">
              <div class="lab-sec-h">
                <h3 class="lab-sec-t">判分</h3>
                {#if delta !== 0}
                  <span class="lab-hint">A 用的是编辑框里的草稿（{delta > 0 ? "+" : ""}{delta} 字）</span>
                {:else}
                  <span class="lab-hint">A 和 B 现在是同一版，先改点什么再跑</span>
                {/if}
              </div>
              <div class="lab-actions">
                <button
                  class="btn btn--primary btn--sm"
                  disabled={!!busy || !evals.length}
                  onclick={() => void scoreAll()}
                >
                  {busy === "score" ? `正在跑 ${evals.length} 条用例 × 2…` : `跑评测（${evals.length} 条用例）`}
                </button>
              </div>

              {#if outcome}
                <div class="lab-score">
                  <div class="lab-score-cell" class:win={outcome.a.passed > outcome.b.passed}>
                    <span class="lab-score-l">{outcome.a.label}</span>
                    <span class="lab-score-v">{outcome.a.passed}/{outcome.a.total}</span>
                  </div>
                  <div class="lab-score-cell" class:win={outcome.b.passed > outcome.a.passed}>
                    <span class="lab-score-l">{outcome.b.label}</span>
                    <span class="lab-score-v">{outcome.b.passed}/{outcome.b.total}</span>
                  </div>
                  <div class="lab-score-note">
                    {#if outcome.a.passed > outcome.b.passed}
                      草稿更好。要留下就在「实验台」保存。
                    {:else if outcome.a.passed < outcome.b.passed}
                      草稿更差 —— 别保存，看看下面的失败原因。
                    {:else}
                      打平。规则没覆盖到的差别，只能靠你自己读两次输出。
                    {/if}
                  </div>
                </div>

                {#if historyRows.length > 1}
                  <div class="lab-hist">
                    <span class="lab-hist-h">最近几次</span>
                    {#each historyRows.slice(1) as h (h.run_id)}
                      <span class="lab-hist-row mono">
                        {when(h.created_at)} · {h.label_a} {h.a[0]}/{h.a[1]} · {h.label_b} {h.b[0]}/{h.b[1]}
                      </span>
                    {/each}
                  </div>
                {/if}

                <div class="lab-table">
                  <div class="lab-tr lab-th">
                    <span>用例</span><span>{outcome.a.label}</span><span>{outcome.b.label}</span>
                  </div>
                  {#each outcome.rows as r (r.eval_id)}
                    <div class="lab-tr">
                      <span class="lab-td-name">{r.eval_name}</span>
                      <span class="lab-td-v">
                        <span class="lab-verdict" class:ok={r.a.passed} class:bad={!r.a.passed}>
                          {r.a.passed ? "通过" : "未通过"}
                        </span>
                        <span class="mono faint">{r.a.ms}ms</span>
                      </span>
                      <span class="lab-td-v">
                        <span class="lab-verdict" class:ok={r.b.passed} class:bad={!r.b.passed}>
                          {r.b.passed ? "通过" : "未通过"}
                        </span>
                        <span class="mono faint">{r.b.ms}ms</span>
                      </span>
                    </div>
                    {#if r.a.failures.length || r.b.failures.length}
                      <div class="lab-tr lab-tr-fail">
                        <span></span>
                        <span class="lab-td-f">{r.a.failures.join(" · ")}</span>
                        <span class="lab-td-f">{r.b.failures.join(" · ")}</span>
                      </div>
                    {/if}
                  {/each}
                </div>
              {/if}
            </div>
          {/if}
        {/if}
      </section>
    </div>
  {/if}
</div>

<style>
  .lab {
    padding: 18px 22px 40px;
    max-width: 1240px;
  }
  .eyebrow {
    font-size: var(--t-2xs);
    color: var(--fg-faint);
    text-transform: uppercase;
    letter-spacing: 0.1em;
  }
  .mono { font-family: var(--font-mono); }
  .faint { color: var(--fg-faint); }
  .lab-head {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: 16px;
    flex-wrap: wrap;
    margin: 0 0 18px;
  }
  .lab-title { margin: 4px 0 6px; font-size: var(--t-lg); color: var(--fg-bright); font-weight: 600; }
  .lab-sub { margin: 0; font-size: var(--t-xs); color: var(--fg-muted); max-width: 68ch; line-height: 1.6; }

  .seg {
    display: inline-flex; padding: 3px; gap: 2px;
    background: var(--surface-2); border: 1px solid var(--border); border-radius: 9px;
  }
  .seg-btn {
    display: inline-flex; align-items: center; gap: 6px;
    height: 28px; padding: 0 10px;
    background: transparent; border: 0; border-radius: 7px;
    font-size: var(--t-2xs); color: var(--fg-muted); cursor: pointer;
  }
  .seg-btn:hover { color: var(--fg-bright); }
  .seg-btn.on { background: var(--surface); color: var(--fg-bright); box-shadow: 0 1px 0 var(--border); }
  .seg-count { font-family: var(--font-mono); font-size: 10px; color: var(--fg-faint); }

  .lab-grid {
    display: grid;
    grid-template-columns: 216px minmax(0, 1fr);
    gap: 18px;
    align-items: start;
  }
  .lab-rail {
    display: flex; flex-direction: column; gap: 2px;
    border-right: 1px solid var(--border); padding-right: 12px;
  }
  .lab-group {
    margin: 10px 0 4px; padding: 0 8px;
    font-size: 10px; text-transform: uppercase; letter-spacing: 0.08em;
    color: var(--fg-faint);
  }
  .lab-group:first-child { margin-top: 0; }
  .lab-item {
    display: flex; align-items: center; gap: 6px;
    padding: 6px 8px; border: 0; border-radius: 7px;
    background: transparent; color: var(--fg-muted);
    font-size: var(--t-xs); text-align: left; cursor: pointer;
  }
  .lab-item:hover { background: var(--surface-2); color: var(--fg-bright); }
  .lab-item.on { background: var(--surface-2); color: var(--fg-bright); font-weight: 500; }
  .lab-item-t { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .lab-badge {
    font-size: 9px; padding: 1px 5px; border-radius: 999px;
    background: color-mix(in oklab, var(--accent) 16%, transparent);
    color: var(--accent); border: 1px solid color-mix(in oklab, var(--accent) 30%, transparent);
  }
  .lab-badge.on { color: var(--accent); }

  .lab-main { min-width: 0; }
  .lab-prompt-head { display: flex; align-items: baseline; gap: 8px; flex-wrap: wrap; margin-bottom: 8px; }
  .lab-prompt-t { margin: 0; font-size: var(--t-md); color: var(--fg-bright); font-weight: 600; }

  .lab-body {
    width: 100%; box-sizing: border-box; min-height: 260px; resize: vertical;
    padding: 12px 14px;
    background: var(--surface); color: var(--fg);
    border: 1px solid var(--border); border-radius: 10px;
    font-size: var(--t-xs); line-height: 1.72;
  }
  .lab-body:focus, .lab-input:focus { outline: none; border-color: var(--accent); }
  .lab-body-foot {
    display: flex; align-items: center; gap: 8px; flex-wrap: wrap;
    margin: 8px 0 4px; font-size: var(--t-2xs); color: var(--fg-faint);
  }
  .lab-dot { margin-left: 6px; color: var(--warn); }
  .lab-neg { color: var(--fg-muted); }

  .lab-sec { margin-top: 22px; }
  .lab-sec.first { margin-top: 6px; }
  .lab-sec-h { display: flex; align-items: baseline; gap: 10px; flex-wrap: wrap; margin-bottom: 8px; }
  .lab-sec-t { margin: 0; font-size: var(--t-sm); color: var(--fg-bright); font-weight: 600; }
  .lab-hint { font-size: var(--t-2xs); color: var(--fg-faint); }
  .lab-input {
    width: 100%; box-sizing: border-box; min-height: 84px; resize: vertical;
    padding: 10px 12px;
    background: var(--surface); color: var(--fg);
    border: 1px solid var(--border); border-radius: 9px;
    font-size: var(--t-xs); line-height: 1.6;
  }
  .lab-input.sm { min-height: 64px; margin-top: 8px; }
  .lab-actions { display: flex; align-items: center; gap: 8px; margin-top: 8px; flex-wrap: wrap; }

  .lab-runs {
    display: grid; grid-template-columns: 1fr 1fr; gap: 12px; margin-top: 12px;
  }
  .lab-run {
    border: 1px solid var(--border); border-radius: 10px; background: var(--surface);
    overflow: hidden;
  }
  .lab-run.empty { opacity: 0.6; }
  .lab-run-h {
    display: flex; align-items: baseline; justify-content: space-between; gap: 8px;
    padding: 7px 10px; border-bottom: 1px solid var(--border);
    font-size: var(--t-2xs);
  }
  .lab-run-t { color: var(--fg-bright); font-weight: 600; }
  .lab-run-body {
    margin: 0; padding: 10px 12px; max-height: 360px; overflow: auto;
    font-size: var(--t-xs); line-height: 1.65; white-space: pre-wrap; word-break: break-word;
    color: var(--fg);
  }
  .lab-run-none { margin: 0; padding: 14px 12px; font-size: var(--t-2xs); color: var(--fg-faint); }

  .lab-fix-list { display: flex; flex-direction: column; gap: 8px; }
  .lab-fix { border: 1px solid var(--border); border-radius: 9px; padding: 8px 10px; background: var(--surface); }
  .lab-fix-h { display: flex; align-items: center; gap: 6px; }
  .lab-fix-n { font-size: var(--t-xs); color: var(--fg-bright); font-weight: 500; }
  .lab-fix-r { font-size: var(--t-2xs); color: var(--accent); margin-top: 2px; white-space: pre-wrap; }
  .lab-fix-i {
    font-size: var(--t-2xs); color: var(--fg-muted); margin-top: 4px; line-height: 1.5;
    display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden;
  }
  .lab-fix-add {
    margin-top: 10px; padding: 10px; border: 1px dashed var(--border-strong); border-radius: 10px;
  }
  .lab-fix-add .ipt {
    width: 100%; box-sizing: border-box; padding: 8px 10px;
    background: var(--surface); color: var(--fg);
    border: 1px solid var(--border); border-radius: 8px; font-size: var(--t-xs);
  }

  .lab-score {
    display: flex; align-items: stretch; gap: 10px; flex-wrap: wrap; margin: 12px 0;
  }
  .lab-score-cell {
    display: flex; align-items: baseline; gap: 8px;
    padding: 10px 14px; border: 1px solid var(--border); border-radius: 10px;
    background: var(--surface);
  }
  .lab-score-cell.win { border-color: color-mix(in oklab, var(--ok, var(--accent)) 45%, var(--border)); }
  .lab-score-l { font-size: var(--t-2xs); color: var(--fg-muted); }
  .lab-score-v { font-family: var(--font-mono); font-size: var(--t-md); color: var(--fg-bright); }
  .lab-score-note { align-self: center; font-size: var(--t-2xs); color: var(--fg-muted); }

  .lab-hist { display: flex; align-items: baseline; gap: 10px; flex-wrap: wrap; margin: 0 0 10px; }
  .lab-hist-h { font-size: var(--t-2xs); color: var(--fg-faint); }
  .lab-hist-row { font-size: var(--t-2xs); color: var(--fg-muted); }
  /* ---- retrieval transparency ---- */
  .lab-wide { max-width: 100%; }
  .lab-trace-form { display: flex; gap: 8px; align-items: center; margin: 0 0 10px; }
  .lab-trace-form .ipt { flex: 1; }
  .lab-trace-meta { display: flex; gap: 14px; flex-wrap: wrap; font-size: var(--t-2xs); margin: 0 0 8px; }
  .lab-trace-warn {
    margin: 0 0 10px; padding: 7px 10px; border-radius: 8px;
    background: var(--surface-2); border: 1px solid var(--border);
    font-size: var(--t-2xs); color: var(--fg-muted);
  }
  .lab-chips { display: flex; gap: 5px; flex-wrap: wrap; margin: 0 0 12px; }
  .lab-chip {
    font-size: 10px; padding: 2px 7px; border-radius: 999px;
    background: var(--surface-2); border: 1px solid var(--border); color: var(--fg-muted);
  }
  .lab-cols {
    display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 10px;
  }
  @media (max-width: 1100px) { .lab-cols { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
  .lab-col { border: 1px solid var(--border); border-radius: 10px; padding: 8px; min-width: 0; }
  .lab-col-h {
    display: flex; align-items: baseline; justify-content: space-between;
    font-size: var(--t-2xs); color: var(--fg-bright); font-weight: 600; margin-bottom: 6px;
  }
  .lab-col-none { font-size: var(--t-2xs); color: var(--fg-faint); margin: 0; }
  .lab-hit { padding: 6px 0; border-top: 1px dashed var(--border); }
  .lab-hit:first-of-type { border-top: 0; }
  .lab-hit-h { display: flex; align-items: baseline; gap: 6px; font-size: 10px; }
  .lab-hit-n { color: var(--fg-muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 60%; }
  .lab-hit-l { font-size: 9px; color: var(--fg-faint); }
  .lab-hit-p {
    margin: 3px 0 0; font-size: 10.5px; line-height: 1.45; color: var(--fg-muted);
    display: -webkit-box; -webkit-line-clamp: 3; line-clamp: 3; -webkit-box-orient: vertical; overflow: hidden;
  }

  /* ---- raw provider response ---- */
  .lab-raw { border-top: 1px dashed var(--border); padding-top: 6px; margin-top: 6px; }
  .lab-raw summary {
    cursor: pointer; font-size: var(--t-2xs); color: var(--fg-faint);
  }
  .lab-raw summary:hover { color: var(--fg-muted); }
  .lab-raw-body {
    margin: 6px 0 0; padding: 8px; max-height: 220px; overflow: auto;
    background: var(--surface-2); border: 1px solid var(--border); border-radius: 8px;
    font-family: var(--font-mono); font-size: 10.5px; line-height: 1.5;
    white-space: pre-wrap; word-break: break-word; color: var(--fg-muted);
  }
  .lab-raw-usage { margin-top: 6px; font-size: 10px; color: var(--fg-faint); word-break: break-all; }

  .lab-table { border: 1px solid var(--border); border-radius: 10px; overflow: hidden; }
  .lab-tr {
    display: grid; grid-template-columns: minmax(0, 1.4fr) 110px 110px;
    gap: 8px; align-items: center;
    padding: 7px 12px; border-bottom: 1px solid var(--border);
    font-size: var(--t-2xs);
  }
  .lab-tr:last-child { border-bottom: none; }
  .lab-th { background: var(--surface-2); color: var(--fg-faint); font-weight: 500; }
  /* 分层明细表：5 列（分层/Recall/MRR/题数/top1 均分）。 */
  .lab-tr5 { grid-template-columns: minmax(0, 1.4fr) 92px 92px 56px 96px; }
  /* 语言表：4 列（语言/Recall/MRR/关键词腿）。 */
  .lab-tr4 { grid-template-columns: minmax(0, 1.4fr) 92px 92px 110px; }
  /* 采纳规则横幅（KnowNote borrow）。 */
  .lab-verdict {
    margin-top: 10px; padding: 9px 12px;
    border: 1px solid var(--border); border-radius: 10px;
    display: flex; flex-direction: column; gap: 5px;
  }
  .lab-verdict-adopt { border-color: color-mix(in oklab, var(--ok, #15803d) 45%, transparent); background: color-mix(in oklab, var(--ok, #15803d) 8%, transparent); }
  .lab-verdict-regress { border-color: color-mix(in oklab, var(--err, #b91c1c) 45%, transparent); background: color-mix(in oklab, var(--err, #b91c1c) 8%, transparent); }
  .lab-verdict-tag { font-weight: 700; font-size: 12px; }
  .lab-verdict-adopt .lab-verdict-tag { color: var(--ok, #15803d); }
  .lab-verdict-regress .lab-verdict-tag { color: var(--err, #b91c1c); }
  .lab-verdict-mode { font-size: 10.5px; color: var(--fg-faint); }
  .lab-verdict-metrics { display: flex; flex-wrap: wrap; gap: 6px; }
  .lab-vm { font-size: 10.5px; padding: 1px 7px; border-radius: 999px; color: var(--fg-muted); background: var(--surface-2); border: 1px solid var(--border); }
  .lab-vm-adopt { color: var(--ok, #15803d); border-color: color-mix(in oklab, var(--ok, #15803d) 40%, transparent); }
  .lab-vm-regress { color: var(--err, #b91c1c); border-color: color-mix(in oklab, var(--err, #b91c1c) 40%, transparent); }
  .lab-tr-fail { padding-top: 0; color: var(--err, var(--warn)); }
  .lab-td-name { color: var(--fg-bright); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .lab-td-v { display: flex; align-items: center; gap: 6px; }
  .lab-td-f { font-size: 10px; line-height: 1.4; }
  .lab-verdict { padding: 1px 6px; border-radius: 999px; font-size: 10px; }
  .lab-verdict.ok {
    background: color-mix(in oklab, var(--ok, #3f9) 16%, transparent);
    color: var(--ok, #2a8);
  }
  .lab-verdict.bad {
    background: color-mix(in oklab, var(--err, #e55) 14%, transparent);
    color: var(--err, #c44);
  }

  .lab-empty, .lab-empty-inline {
    padding: 22px; color: var(--fg-faint); font-size: var(--t-xs); text-align: center;
  }
  .lab-empty-inline { text-align: left; padding: 10px 0; line-height: 1.6; }
  .lab-err {
    margin-top: 10px; padding: 8px 10px; border-radius: 8px;
    border: 1px solid color-mix(in oklab, var(--err, #e55) 40%, var(--border));
    color: var(--err, #c44); font-size: var(--t-2xs); line-height: 1.5;
  }

  /* ── 回答健康度 ── */
  .ah-stats {
    display: grid; grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 1px; background: var(--border); border: 1px solid var(--border);
    border-radius: 10px; overflow: hidden; margin: 10px 0;
  }
  .ah-stat { background: var(--surface); padding: 10px 12px; display: flex; flex-direction: column; gap: 2px; }
  .ah-v { font-size: 20px; color: var(--fg-bright); font-variant-numeric: tabular-nums; }
  .ah-k { font-size:var(--t-2xs); color: var(--fg-faint); }
  .ah-states { display: flex; flex-direction: column; gap: 5px; margin: 10px 0 4px; }
  .ah-row { display: grid; grid-template-columns: 74px minmax(0, 1fr) 40px; align-items: center; gap: 8px; }
  .ah-row-k { font-size: var(--t-2xs); color: var(--fg-muted); }
  .ah-track { height: 6px; border-radius: 999px; background: var(--surface-2); overflow: hidden; }
  .ah-fill { display: block; height: 100%; background: var(--accent); border-radius: 999px; }
  .ah-fill.warn { background: var(--warn, #f5a623); }
  .ah-fill.info { background: var(--fg-faint); }
  .ah-row-v { font-size: var(--t-2xs); color: var(--fg-faint); text-align: right; }
  .ah-signals { list-style: none; margin: 12px 0 0; padding: 0; display: flex; flex-direction: column; gap: 6px; }
  .ah-sig {
    display: flex; align-items: flex-start; gap: 7px;
    font-size: var(--t-xs); line-height: 1.55; color: var(--fg-muted);
  }
  .ah-dot { flex: none; width: 6px; height: 6px; border-radius: 50%; margin-top: 6px; background: var(--fg-faint); }
  .ah-sig.warn .ah-dot { background: var(--warn, #f5a623); }
  .ah-sig.warn { color: var(--fg-bright); }
  .ah-sig.ok .ah-dot { background: var(--ok, #3fb950); }
  .ah-facts {
    margin-top: 12px; padding-top: 8px; border-top: 1px dashed var(--border);
    font-size: var(--t-2xs); color: var(--fg-faint); line-height: 1.6;
  }

  /* ── 引用台账 ── */
  .ah-trend-wrap { margin-top: 14px; }
  .ah-trend-h {
    display: flex; align-items: baseline; gap: 8px; flex-wrap: wrap;
    font-size: var(--t-2xs); color: var(--fg-bright);
  }
  .ah-trend { display: flex; align-items: flex-end; gap: 4px; height: 54px; margin-top: 6px; }
  .ah-tc { flex: 1; height: 100%; display: flex; align-items: flex-end; }
  .ah-tb {
    width: 100%; min-height: 3px; border-radius: 3px 3px 0 0;
    background: var(--warn, #f5a623);
  }
  /* A week with answers but no failures is the good case — don't paint it the
     same colour as a bad one. */
  .ah-tb.zero { background: color-mix(in oklab, var(--ok, #3fb950) 70%, transparent); }
  .ah-tb.empty { background: var(--border); height: 3px; }
  .ah-trend-x {
    display: flex; justify-content: space-between;
    font-size: 10px; color: var(--fg-faint); margin-top: 3px;
  }
  .ah-ledger { margin-top: 18px; padding-top: 14px; border-top: 1px solid var(--border); }
  .ah-ledger-bar { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin: 8px 0; }
  .ah-ledger-sum {
    font-size: var(--t-2xs); color: var(--fg-muted); line-height: 1.6;
    padding: 7px 9px; border-radius: 8px; background: var(--surface);
  }
  .ah-grp { margin-top: 8px; }
  .ah-grp-sum {
    display: flex; align-items: center; gap: 7px; cursor: pointer;
    font-size: var(--t-xs); color: var(--fg); padding: 4px 0;
  }
  .ah-grp-sum .ah-dot { margin-top: 0; }
  .ah-grp-sum .ah-dot.warn { background: var(--warn, #f5a623); }
  .ah-grp-sum .ah-dot.ok { background: var(--ok, #3fb950); }
  .ah-rows {
    list-style: none; margin: 4px 0 0; padding: 0 0 0 4px;
    display: flex; flex-direction: column; gap: 4px;
  }
  .ah-row {
    display: flex; align-items: baseline; gap: 8px;
    font-size: var(--t-2xs); line-height: 1.5;
    padding: 5px 8px; border-radius: 7px;
    background: var(--surface); border: 1px solid var(--border);
  }
  .ah-row-src { flex: none; color: var(--fg-bright); }
  .ah-row-quote {
    flex: 1; min-width: 0; color: var(--fg-muted);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .ah-row-tag {
    flex: none; font-size: 10px; padding: 1px 5px; border-radius: 4px;
    color: var(--fg-faint); border: 1px solid var(--border);
  }

  @media (max-width: 860px) {
    .ah-stats { grid-template-columns: repeat(2, minmax(0, 1fr)); }
    .lab-grid { grid-template-columns: 1fr; }
    .lab-rail { flex-direction: row; flex-wrap: wrap; border-right: 0; border-bottom: 1px solid var(--border); padding: 0 0 8px; }
    .lab-group { width: 100%; margin: 4px 0 2px; }
    .lab-runs { grid-template-columns: 1fr; }
    .lab-tr { grid-template-columns: minmax(0, 1fr) 84px 84px; }
  }
</style>
