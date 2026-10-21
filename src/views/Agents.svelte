<script lang="ts">
  import { app } from "../lib/store.svelte";
  import * as api from "../lib/api";
  import Icon from "../components/Icon.svelte";

  let agents = $state<api.Agent[]>([]);
  let skills = $state<api.Skill[]>([]);
  let tools = $state<api.Tool[]>([]);
  let runs = $state<api.AgentRun[]>([]);
  let loaded = $state(false);
  let busy = $state(false);
  let editing = $state<api.Agent | null>(null);
  let selectedRun = $state<api.AgentRun | null>(null);
  let messages = $state<api.AgentMessage[]>([]);
  let newInput = $state("");
  let runAgentId = $state("");
  /** Run currently executing on the backend — the transcript polls live. */
  let runningRunId = $state<string | null>(null);

  $effect(() => { void load(); });
  async function load() {
    loaded = false;
    try {
      [agents, skills, tools, runs] = await Promise.all([
        api.agentsList(), api.agentsListSkills(), api.agentsListTools(), api.agentsListRuns(50),
      ]);
      if (!runAgentId && agents.length) runAgentId = agents[0].id;
    } catch (e) { app.pushToast({ kind: "error", title: "智能体加载失败", body: String(e) }); }
    finally { loaded = true; }
  }

  function blankAgent(): api.Agent {
    return {
      id: "", user_id: "u-default", name: "", description: null,
      system_prompt: "你是 Cortex 智能体。回答时引用来源；知识库没有的内容要明说。",
      skills: [], tools: [], model: null, temperature: 0.3,
      max_steps: 8, enabled: true, created_at: 0, updated_at: 0,
    };
  }

  async function save() {
    if (!editing) return;
    if (!editing.name.trim()) {
      app.pushToast({ kind: "warning", title: "智能体名称不能为空" });
      return;
    }
    busy = true;
    try {
      await api.agentsUpsert(
        editing.id || null, editing.name, editing.description,
        editing.system_prompt, editing.skills, editing.tools,
        editing.model, editing.temperature, editing.max_steps, editing.enabled,
      );
      app.pushToast({ kind: "success", title: editing.id ? "智能体已更新" : "智能体已创建" });
      editing = null;
      await load();
    } catch (e) { app.pushToast({ kind: "error", title: "保存失败", body: String(e) }); }
    finally { busy = false; }
  }

  async function remove(a: api.Agent) {
    if (!await app.confirm({ title: "删除这个智能体？", body: a.name, danger: true })) return;
    try {
      await api.agentsDelete(a.id);
      app.pushToast({ kind: "success", title: "智能体已删除", body: a.name });
      await load();
    } catch (e) { app.pushToast({ kind: "error", title: String(e) }); }
  }

  async function startRun(agent: api.Agent) {
    if (!newInput.trim() || runningRunId) return;
    busy = true;
    try {
      // Create the run first so the transcript is visible while the loop
      // works, then execute it and poll messages as steps land.
      const run = await api.agentsStartRun(agent.id, newInput, continueThread ? (activeThread ?? undefined) : undefined);
      newInput = "";
      selectedRun = run;
      messages = run.thread_id
        ? await api.agentsThreadMessages(run.thread_id, 300)
        : await api.agentsListMessages(run.id, 200);
      runningRunId = run.id;
      // Surface an execute-side failure even though the polling loop below
      // never awaits the promise while the run row stays "pending".
      let finErr: string | null = null;
      const finished = api.agentsExecuteRun(agent.id, run.input, run.id).catch((e) => { finErr = String(e); });
      while (runningRunId === run.id) {
        await new Promise((r) => setTimeout(r, 1500));
        try { messages = activeThread ? await api.agentsThreadMessages(activeThread, 300) : await api.agentsListMessages(run.id, 200); } catch { /* mid-run */ }
        const rs = await api.agentsListRuns(50).catch(() => []);
        const now = rs.find((x) => x.id === run.id);
        if (finErr || (now && now.status !== "pending")) {
          if (now) selectedRun = now;
          break;
        }
      }
      await finished;
      messages = activeThread ? await api.agentsThreadMessages(activeThread, 300) : await api.agentsListMessages(run.id, 200);
      await load();
      if (finErr) app.pushToast({ kind: "error", title: "运行失败", body: finErr });
      else app.pushToast({ kind: "success", title: "运行完成", body: agent.name });
    } catch (e) {
      app.pushToast({ kind: "error", title: "运行失败", body: String(e) });
      await load().catch(() => {});
    } finally {
      runningRunId = null;
      busy = false;
    }
  }

  /** One-click demo: a research agent wired to every read-only built-in tool —
   *  the "agent auto-research" story, runnable the moment this view opens. */
  async function createDemoAgent() {
    if (agents.some((a) => a.name === "自动研究员")) {
      app.pushToast({ kind: "info", title: "「自动研究员」已存在" });
      return;
    }
    busy = true;
    try {
      await api.agentsUpsert(
        null,
        "自动研究员",
        "在知识库里检索、读来源、给出带引用的研究摘要",
        "你是一个扎根于用户知识库的研究员。回答任何问题前先用 search_notebook / search_notes 检索，必要时用 get_source 精读来源、用 list_notebooks / get_knowledge_tree 了解结构。最终回答必须用「来源名 · 位置」标注每一条关键论断的出处；知识库没有的内容要明说。回答使用用户的语言（默认中文）。",
        ["引用优先"].filter(() => skills.some((s) => s.enabled)),
        tools.filter((t) => t.enabled && t.kind === "read").map((t) => t.name),
        null, 0.3, 12, true,
      );
      app.pushToast({ kind: "success", title: "已创建「自动研究员」", body: "在下方输入一个研究问题试试。" });
      await load();
    } catch (e) {
      app.pushToast({ kind: "error", title: "创建失败", body: String(e) });
    } finally { busy = false; }
  }

  async function showRun(run: api.AgentRun) {
    selectedRun = run;
    try {
      // Shared thread? Then show the whole conversation (all agents) — the
      // multi-agent shared-context transcript (0047).
      messages = run.thread_id
        ? await api.agentsThreadMessages(run.thread_id, 300)
        : await api.agentsListMessages(run.id, 200);
    } catch (e) { app.pushToast({ kind: "error", title: String(e) }); }
  }

  /** The open conversation's thread (null = none selected / legacy run). */
  const activeThread = $derived(selectedRun?.thread_id || null);

  // "继续此线程": when on, the next run from the composer joins the open
  // thread (same conversation, any agent), keeping all earlier context.
  let continueThread = $state(false);

  /** Switch agent mid-conversation: same thread, new agent, context kept. */
  async function switchAgent(agent: api.Agent) {
    if (!activeThread || !newInput.trim() || runningRunId) return;
    busy = true;
    try {
      const run = await api.agentsSwitchAgent(agent.id, activeThread, newInput.trim());
      newInput = "";
      selectedRun = run;
      messages = await api.agentsThreadMessages(activeThread, 300);
      runningRunId = run.id;
      let swErr: string | null = null;
      const finished = api.agentsExecuteRun(agent.id, run.input, run.id).catch((e) => { swErr = String(e); });
      while (runningRunId === run.id) {
        await new Promise((r) => setTimeout(r, 1500));
        try { messages = await api.agentsThreadMessages(activeThread, 300); } catch { /* mid-run */ }
        const rs = await api.agentsListRuns(50).catch(() => []);
        const now = rs.find((x) => x.id === run.id);
        if (swErr || (now && now.status !== "pending")) { if (now) selectedRun = now; break; }
      }
      await finished;
      messages = await api.agentsThreadMessages(activeThread, 300);
      await load();
      if (swErr) app.pushToast({ kind: "error", title: "运行失败", body: swErr });
      else app.pushToast({ kind: "success", title: "运行完成", body: `${agent.name} · 上下文已保留` });
    } catch (e) {
      app.pushToast({ kind: "error", title: "切换运行失败", body: String(e) });
    } finally { runningRunId = null; busy = false; }
  }
</script>

<div class="ag">
  <header class="ag-head">
    <div>
      <div class="eyebrow">智能体</div>
      <h1 class="ag-title">智能体平台</h1>
      <p class="ag-sub mono">自定义智能体 · 技能/工具白名单 · 运行记录</p>
    </div>
    <div class="ag-tools">
      <button class="btn" disabled={busy} onclick={() => void createDemoAgent()}>
        <Icon name="bolt" size={12} /> 示例：自动研究员
      </button>
      <button class="btn primary" onclick={() => (editing = blankAgent())}>
        <Icon name="plus" size={12} /> 新建智能体
      </button>
    </div>
  </header>

  {#if editing}
    <div class="card">
      <h3>{editing.id ? "Edit agent" : "New agent"}</h3>
      <div class="form-row">
        <label>名称<input class="input" bind:value={editing.name} /></label>
        <label>描述<input class="input" bind:value={editing.description} /></label>
        <label class="full">系统提示词<textarea class="input area" rows="3" bind:value={editing.system_prompt}></textarea></label>
        <label>随机性<input class="input" type="number" min="0" max="2" step="0.05" bind:value={editing.temperature} /></label>
        <label>最大步数<input class="input" type="number" min="1" max="64" step="1" bind:value={editing.max_steps} /></label>
        <label class="check"><input type="checkbox" bind:checked={editing.enabled} /> 启用</label>
      </div>
      <div class="form-row">
        <fieldset>
          <legend>技能</legend>
          {#each skills as sk}
            <label class="check">
              <input type="checkbox" checked={editing.skills.includes(sk.name)} onchange={(e) => {
                const c = (e.target as HTMLInputElement).checked;
                editing!.skills = c ? [...editing!.skills, sk.name] : editing!.skills.filter((s) => s !== sk.name);
              }} /> {sk.name}
            </label>
          {/each}
        </fieldset>
        <fieldset>
          <legend>工具</legend>
          {#each tools as t}
            <label class="check">
              <input type="checkbox" checked={editing.tools.includes(t.name)} onchange={(e) => {
                const c = (e.target as HTMLInputElement).checked;
                editing!.tools = c ? [...editing!.tools, t.name] : editing!.tools.filter((s) => s !== t.name);
              }} /> {t.name} <span class="tag">{t.kind}</span>
            </label>
          {/each}
        </fieldset>
      </div>
      <div class="row-end">
        <button class="btn" onclick={() => (editing = null)}>取消</button>
        <button class="btn primary" disabled={busy} onclick={() => void save()}>保存</button>
      </div>
    </div>
  {/if}

  <div class="card">
    <h3>运行</h3>
    <div class="row-inline">
      <input class="input" placeholder="输入给智能体的内容…" bind:value={newInput}
             onkeydown={(e) => e.key === "Enter" && runAgentId && void startRun(agents.find((a) => a.id === runAgentId)!)} />
      <select class="input" value={runAgentId} onchange={(e) => runAgentId = (e.target as HTMLSelectElement).value}>
        {#each agents as a}<option value={a.id}>{a.name}</option>{/each}
      </select>
      <label class="thr-check" title="勾选后，下一次运行加入当前打开的共享线程（任何智能体，此前全部消息都是上下文）">
        <input type="checkbox" checked={continueThread} disabled={!activeThread}
               onchange={(e) => (continueThread = (e.target as HTMLInputElement).checked)} />
        继续此线程
      </label>
      <label class="thr-check" title="勾选后，下一次运行加入当前打开的共享线程（任何智能体，此前全部消息都是上下文）">
        <input type="checkbox" checked={continueThread} disabled={!activeThread}
               onchange={(e) => (continueThread = (e.target as HTMLInputElement).checked)} />
        继续此线程
      </label>
      <button class="btn primary" disabled={busy || runningRunId !== null || !newInput.trim() || !runAgentId} onclick={() => {
        const a = agents.find((x) => x.id === runAgentId); if (a) void startRun(a);
      }}>{runningRunId ? "运行中…" : "开始"}</button>
    </div>
    {#if runningRunId}
      <div class="mut mono">智能体正在执行（每步都会写进记录，下方对话实时刷新）。工具循环期间不会冻结其他页面。</div>
    {/if}
    <div class="split">
      <div>
        <h4>历史</h4>
        {#if !runs.length}<div class="mut">还没有运行记录。</div>
        {:else}
          <ul class="runs">
            {#each runs as r}
              <li>
                <button class={"run" + (selectedRun?.id === r.id ? " on" : "")} onclick={() => void showRun(r)}>
                  <span class={"status " + r.status}>{r.status}</span>
                  <span class="mono run-input">{r.input.slice(0, 40)}{r.input.length > 40 ? "..." : ""}</span>
                  <span class="mut mono">{r.steps} steps</span>
                </button>
              </li>
            {/each}
          </ul>
        {/if}
      </div>
      <div>
        <h4>对话记录{#if activeThread} · 共享线程{/if}</h4>
        {#if !selectedRun}<div class="mut">选一条记录查看完整过程。</div>
        {:else}
          {#if activeThread}
            <!-- Multi-agent shared context (0047): switch agents mid-thread. -->
            <div class="thr-switch">
              <span class="mut mono">切换智能体继续此线程（上下文保留）：</span>
              {#each agents.filter((a) => a.id !== selectedRun?.agent_id) as a}
                <button class="btn btn--sm" disabled={runningRunId !== null || !newInput.trim()}
                        title={`用 ${a.name} 在此线程里回答上方输入`} onclick={() => void switchAgent(a)}>
                  {a.name}
                </button>
              {/each}
            </div>
          {/if}
          <ol class="msgs">
            {#each messages as m}
              <li class={"msg " + m.role}>
                <div class="mut mono">{m.role}{m.agent_name ? ` · ${m.agent_name}` : ""}{m.tool_name ? ` · ${m.tool_name}` : ""}</div>
                <!-- selectable so a useful step can be clipped into a note (L3) -->
                <div data-clip-scope="agent" data-clip-label={`智能体 · ${m.agent_name ?? "agent"}`}>{m.content}</div>
              </li>
            {/each}
          </ol>
        {/if}
      </div>
    </div>
  </div>

  <div class="card">
    <h3>智能体</h3>
    {#if !agents.length}<div class="mut">还没有智能体。</div>
    {:else}
      <ul class="ag-list">
        {#each agents as a}
          <li>
            <div>
              <strong>{a.name}</strong>
              <span class="mut mono">{a.description ?? ""}</span>
              <span class="mut mono">{a.skills.length} skills · {a.tools.length} tools</span>
            </div>
            <div class="row-end">
              <button class="btn" onclick={() => (editing = { ...a })}>编辑</button>
              <button class="btn" onclick={() => void remove(a)}>删除</button>
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
</div>

<style>
  .ag { flex: 1; overflow: auto; padding: 22px 28px 32px; display: flex; flex-direction: column; gap: 14px; }
  .eyebrow { font-family: var(--font-mono); font-size: 10.5px; color: var(--fg-faint); text-transform: uppercase; }
  .ag-head { display: flex; justify-content: space-between; align-items: flex-start; border-bottom: 1px solid var(--border); padding-bottom: 14px; }
  .ag-title { font-family: var(--font-display); font-size: 22px; margin: 4px 0 0; color: var(--fg-bright); }
  .ag-sub { font-size: 11.5px; color: var(--fg-muted); margin: 6px 0 0; }
  .ag-tools { display: flex; gap: 8px; }
  .card { background: var(--surface); border: 1px solid var(--border); border-radius: 8px; padding: 16px; display: flex; flex-direction: column; gap: 10px; }
  .card h3 { font-family: var(--font-display); font-size: 16px; margin: 0; color: var(--fg-bright); }
  .card h4 { font-family: var(--font-display); font-size: 13px; margin: 4px 0; color: var(--fg-bright); }
  .form-row { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
  .form-row label { display: flex; flex-direction: column; gap: 4px; font-size: 11.5px; color: var(--fg-muted); }
  .form-row label.full { grid-column: 1 / -1; }
  .form-row label.check { flex-direction: row; align-items: center; gap: 6px; }
  .form-row fieldset { border: 1px solid var(--border); border-radius: 6px; padding: 8px; }
  .form-row fieldset legend { font-size: 11px; color: var(--fg-muted); }
  .form-row .check { display: flex; align-items: center; gap: 6px; font-size: 12px; }
  .input { background: var(--surface-2); border: 1px solid var(--border); border-radius: 6px; padding: 6px 8px; color: var(--fg); font-family: var(--font-mono); font-size: 12px; }
  .input.area { min-height: 60px; font-family: var(--font-read); }
  .row-inline { display: flex; gap: 6px; align-items: center; flex-wrap: wrap; }
  .row-end { display: flex; justify-content: flex-end; gap: 6px; }
  .btn { background: var(--surface-2); border: 1px solid var(--border); color: var(--fg); padding: 5px 10px; border-radius: 6px; cursor: pointer; font-family: var(--font-mono); font-size: 11.5px; }
  .btn.primary { background: color-mix(in srgb, var(--accent) 22%, transparent); border-color: color-mix(in srgb, var(--accent) 45%, transparent); color: var(--fg-bright); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .mut { color: var(--fg-faint); font-size: 11.5px; }
  .tag { font-family: var(--font-mono); font-size: 10px; padding: 1px 5px; border-radius: 4px; background: var(--surface-2); }
  .split { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; }
  .runs { list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 4px; }
  .run { display: grid; grid-template-columns: 64px 1fr auto; gap: 8px; align-items: center; background: var(--surface-2); border: 1px solid var(--border); border-radius: 6px; padding: 6px 10px; width: 100%; cursor: pointer; color: var(--fg); }
  .run.on { border-color: var(--accent); }
  .status { font-size: 10px; padding: 1px 6px; border-radius: 4px; font-family: var(--font-mono); }
  .status.pending, .status.running { background: color-mix(in srgb, var(--info) 15%, transparent); color: var(--info); }
  .status.success { background: color-mix(in srgb, var(--ok) 15%, transparent); color: var(--ok); }
  .status.error, .status.cancelled { background: color-mix(in srgb, var(--err) 15%, transparent); color: var(--err); }
  .run-input { font-size: 12px; }
  .msgs { list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 6px; max-height: 360px; overflow: auto; }
  .msg { border-radius: 6px; padding: 6px 10px; font-size: 12px; }
  .msg.user { background: var(--surface-2); }
  .msg.assistant { background: color-mix(in srgb, var(--accent) 10%, transparent); }
  .msg.tool { background: color-mix(in srgb, var(--info) 10%, transparent); }
  .ag-list { list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 4px; }
  .ag-list li { display: flex; justify-content: space-between; align-items: center; padding: 6px 10px; border-radius: 6px; background: var(--surface-2); border: 1px solid var(--border); }
  .ag-list strong { color: var(--fg-bright); margin-right: 8px; }
  .ag-list .mut { margin-left: 8px; }
  .thr-check { display: inline-flex; align-items: center; gap: 5px; font-size: 11.5px; color: var(--fg-muted); cursor: pointer; }
  .thr-switch { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; padding: 6px 8px; margin-bottom: 6px;
    background: color-mix(in oklab, var(--accent) 6%, transparent); border: 1px solid color-mix(in oklab, var(--accent) 22%, transparent); border-radius: 6px; }
  .thr-check { display: inline-flex; align-items: center; gap: 5px; font-size: 11.5px; color: var(--fg-muted); cursor: pointer; }
  .thr-switch { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; padding: 6px 8px; margin-bottom: 6px;
    background: color-mix(in oklab, var(--accent) 6%, transparent); border: 1px solid color-mix(in oklab, var(--accent) 22%, transparent); border-radius: 6px; }
</style>