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
      system_prompt: "You are a Cortex agent. Cite source paths when relevant.",
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
    if (!newInput.trim()) return;
    busy = true;
    try {
      const run = await api.agentsStartRun(agent.id, newInput);
      newInput = "";
      selectedRun = run;
      messages = [];
      app.pushToast({ kind: "success", title: "已开始运行", body: agent.name });
      await load();
    } catch (e) { app.pushToast({ kind: "error", title: "运行失败", body: String(e) }); }
    finally { busy = false; }
  }

  async function showRun(run: api.AgentRun) {
    selectedRun = run;
    try { messages = await api.agentsListMessages(run.id, 200); }
    catch (e) { app.pushToast({ kind: "error", title: String(e) }); }
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
      <button class="btn primary" onclick={() => (editing = blankAgent())}>
        <Icon name="plus" size={12} /> New agent
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
      <button class="btn primary" disabled={busy || !newInput.trim() || !runAgentId} onclick={() => {
        const a = agents.find((x) => x.id === runAgentId); if (a) void startRun(a);
      }}>开始</button>
    </div>
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
        <h4>对话记录</h4>
        {#if !selectedRun}<div class="mut">选一条记录查看完整过程。</div>
        {:else}
          <ol class="msgs">
            {#each messages as m}
              <li class={"msg " + m.role}>
                <div class="mut mono">{m.role}{m.tool_name ? ` · ${m.tool_name}` : ""}</div>
                <div>{m.content}</div>
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
</style>