<script lang="ts">
  // Prompt lab — the one place where "this prompt got better" can be checked
  // instead of asserted.
  //
  // 实验台: edit the text a real call uses, then run the draft and the built-in
  // default against the same input and read both answers side by side.
  // 评测: fixtures (input + deterministic rules) scored for both versions. No
  // LLM grading its own homework — a rubric that can be talked into a pass is
  // not a rubric.
  import { app } from "../lib/store.svelte";
  import Icon from "../components/Icon.svelte";
  import * as api from "../lib/api";

  type Tab = "bench" | "eval";
  let tab = $state<Tab>("bench");

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
    </div>
  </header>

  {#if loadErr}
    <div class="lab-err">{loadErr}</div>
  {:else if loading}
    <div class="lab-empty">正在读取提示词…</div>
  {:else if !infos.length}
    <div class="lab-empty">还没有可编辑的提示词。</div>
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
    display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden;
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
  .lab-table { border: 1px solid var(--border); border-radius: 10px; overflow: hidden; }
  .lab-tr {
    display: grid; grid-template-columns: minmax(0, 1.4fr) 110px 110px;
    gap: 8px; align-items: center;
    padding: 7px 12px; border-bottom: 1px solid var(--border);
    font-size: var(--t-2xs);
  }
  .lab-tr:last-child { border-bottom: none; }
  .lab-th { background: var(--surface-2); color: var(--fg-faint); font-weight: 500; }
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

  @media (max-width: 860px) {
    .lab-grid { grid-template-columns: 1fr; }
    .lab-rail { flex-direction: row; flex-wrap: wrap; border-right: 0; border-bottom: 1px solid var(--border); padding: 0 0 8px; }
    .lab-group { width: 100%; margin: 4px 0 2px; }
    .lab-runs { grid-template-columns: 1fr; }
    .lab-tr { grid-template-columns: minmax(0, 1fr) 84px 84px; }
  }
</style>
