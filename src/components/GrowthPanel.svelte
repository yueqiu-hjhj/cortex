<script lang="ts">
  import * as api from "../lib/api";
  import Icon from "./Icon.svelte";

  const KIND_LABEL: Record<string, string> = {
    goal: "目标",
    misconception: "误区",
    review: "复习",
    strength: "优势",
  };
  const STATUS_LABEL: Record<string, string> = {
    active: "活跃",
    paused: "暂停",
    resolved: "已解决",
    dismissed: "已忽略",
  };
  const KINDS = ["goal", "misconception", "review", "strength"];

  let items = $state<api.GrowthItem[]>([]);
  let proposals = $state<api.GrowthProposal[]>([]);
  let subjects = $state<api.Subject[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let showDerive = $state(false);
  let showManual = $state(false);
  let exportText = $state<string | null>(null);

  // manual-add form
  let mSubject = $state("");
  let mKind = $state("goal");
  let mTitle = $state("");
  let mDetail = $state("");

  async function load() {
    loading = true;
    error = null;
    try {
      items = await api.growthList(null, null);
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  async function loadSubjects() {
    try {
      subjects = await api.listSubjects();
      if (!mSubject && subjects.length) mSubject = subjects[0].id;
    } catch {
      subjects = [];
    }
  }

  $effect(() => {
    void load();
    void loadSubjects();
  });

  async function doDerive() {
    busy = true;
    error = null;
    try {
      proposals = await api.growthDerive(null);
      showDerive = true;
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function accept(p: api.GrowthProposal) {
    busy = true;
    try {
      await api.growthAdd(p.subject_id, p.kind, p.title, p.detail, p.evidence, "derived");
      proposals = proposals.filter(
        (x) => !(x.subject_id === p.subject_id && x.title === p.title && x.kind === p.kind),
      );
      await load();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function addManual() {
    if (!mSubject || !mTitle.trim()) {
      error = "请选择学科并填写标题";
      return;
    }
    busy = true;
    error = null;
    try {
      await api.growthAdd(mSubject, mKind, mTitle.trim(), mDetail.trim(), [], "manual");
      mTitle = "";
      mDetail = "";
      showManual = false;
      await load();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function setStatus(it: api.GrowthItem, status: string) {
    try {
      await api.growthSetStatus(it.id, status);
      await load();
    } catch (e) {
      error = String(e);
    }
  }

  async function del(it: api.GrowthItem) {
    try {
      await api.growthRemove(it.id);
      await load();
    } catch (e) {
      error = String(e);
    }
  }

  async function doExport() {
    try {
      exportText = await api.growthExport(null);
    } catch (e) {
      error = String(e);
    }
  }

  function downloadMd() {
    if (!exportText) return;
    const blob = new Blob([exportText], { type: "text/markdown" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = "成长画像.md";
    a.click();
    URL.revokeObjectURL(url);
  }

  // group live items by kind, preserving the canonical kind order
  let grouped = $derived(
    KINDS.map((k) => ({
      kind: k,
      rows: items.filter((i) => i.kind === k),
    })).filter((g) => g.rows.length > 0),
  );
</script>

<section class="growth">
  <header class="grow-head">
    <h3>成长画像</h3>
    <div class="grow-actions">
      <button class="btn" onclick={doDerive} disabled={busy}>派生建议</button>
      <button class="btn" onclick={() => (showManual = !showManual)}>手动添加</button>
      <button class="btn" onclick={doExport} disabled={loading}>导出</button>
    </div>
  </header>

  {#if error}
    <p class="err">{error}</p>
  {/if}

  {#if showManual}
    <div class="manual">
      <select bind:value={mSubject}>
        {#each subjects as s}
          <option value={s.id}>{s.name}</option>
        {/each}
      </select>
      <select bind:value={mKind}>
        {#each KINDS as k}
          <option value={k}>{KIND_LABEL[k]}</option>
        {/each}
      </select>
      <input placeholder="标题" bind:value={mTitle} />
      <input placeholder="细节（可选）" bind:value={mDetail} />
      <button class="btn btn--ok" onclick={addManual} disabled={busy}>添加</button>
      <button class="btn" onclick={() => (showManual = false)}>取消</button>
    </div>
  {/if}

  {#if showDerive}
    <div class="derive">
      <div class="derive-head">
        <span>基于真实掌握度 / 校准 / 误判数据生成的建议（确认后才入库）</span>
        <button class="btn" onclick={() => (showDerive = false)}>收起</button>
      </div>
      {#if proposals.length === 0}
        <p class="faint">没有可派生的条目——先把薄弱点练出信号（≥2 次练习）再来。</p>
      {:else}
        {#each proposals as p}
          <div class="prop">
            <span class="badge badge--{p.kind}">{KIND_LABEL[p.kind] ?? p.kind}</span>
            <div class="prop-body">
              <div class="prop-title">{p.title}</div>
              <div class="prop-detail">{p.detail}</div>
              <div class="prop-sub faint">{p.subject_name}</div>
            </div>
            <button class="btn btn--ok" onclick={() => accept(p)} disabled={busy}>采纳</button>
          </div>
        {/each}
      {/if}
    </div>
  {/if}

  {#if loading}
    <p class="faint">加载中…</p>
  {:else if items.length === 0}
    <p class="faint">还没有成长条目。点「派生建议」从已有学习数据生成，或手动添加。</p>
  {:else}
    {#each grouped as g}
      <h4 class="kind-h">{KIND_LABEL[g.kind] ?? g.kind}</h4>
      {#each g.rows as it}
        <div class="item item--{it.status}">
          <div class="item-main">
            <span class="badge badge--{it.kind}">{KIND_LABEL[it.kind] ?? it.kind}</span>
            <span class="item-title">{it.title}</span>
            <span class="item-status item-status--{it.status}">{STATUS_LABEL[it.status] ?? it.status}</span>
            {#if it.origin === "derived"}
              <span class="origin faint">派生</span>
            {/if}
          </div>
          <div class="item-detail">{it.detail}</div>
          {#if it.evidence.length}
            <ul class="ev">
              {#each it.evidence as e}
                <li class="faint">{e.excerpt}</li>
              {/each}
            </ul>
          {/if}
          <div class="item-actions">
            {#if it.status === "active"}
              <button class="btn btn--sm" onclick={() => setStatus(it, "paused")}>暂停</button>
              <button class="btn btn--sm" onclick={() => setStatus(it, "resolved")}>解决</button>
              <button class="btn btn--sm" onclick={() => setStatus(it, "dismissed")}>忽略</button>
            {:else if it.status === "paused"}
              <button class="btn btn--sm" onclick={() => setStatus(it, "active")}>恢复</button>
              <button class="btn btn--sm" onclick={() => setStatus(it, "resolved")}>解决</button>
              <button class="btn btn--sm" onclick={() => setStatus(it, "dismissed")}>忽略</button>
            {:else}
              <button class="btn btn--sm" onclick={() => setStatus(it, "active")}>恢复</button>
            {/if}
            <button class="btn btn--sm btn--danger" onclick={() => del(it)}>删除</button>
          </div>
        </div>
      {/each}
    {/each}
  {/if}

  {#if exportText !== null}
    <div class="export-overlay" role="dialog" aria-label="成长画像导出">
      <div class="export-box">
        <header>
          <span>成长画像 · Markdown</span>
          <div>
            <button class="btn" onclick={downloadMd}>下载 .md</button>
            <button class="btn" onclick={() => (exportText = null)}>关闭</button>
          </div>
        </header>
        <pre>{exportText}</pre>
      </div>
    </div>
  {/if}
</section>

<style>
  .growth {
    border: 1px solid var(--border);
    border-radius: 12px;
    background: var(--surface);
    padding: 14px 16px;
  }
  .grow-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 10px;
  }
  .grow-head h3 {
    margin: 0;
    font-size: 15px;
  }
  .grow-actions {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }
  .btn {
    border: 1px solid var(--border);
    background: var(--bg);
    color: var(--fg);
    border-radius: 8px;
    padding: 5px 10px;
    font-size: 12px;
    cursor: pointer;
  }
  .btn:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .btn--ok {
    border-color: var(--ok);
    color: var(--ok);
  }
  .btn--danger {
    border-color: var(--danger);
    color: var(--danger);
  }
  .btn--sm {
    padding: 3px 8px;
    font-size: 11px;
  }
  .manual,
  .derive {
    border: 1px dashed var(--border-strong);
    border-radius: 10px;
    padding: 10px;
    margin-bottom: 12px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .manual input,
  .manual select {
    background: var(--bg);
    color: var(--fg);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 6px 8px;
    font-size: 13px;
  }
  .derive-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    color: var(--fg-muted);
  }
  .prop {
    display: flex;
    gap: 10px;
    align-items: flex-start;
    border-top: 1px solid var(--border);
    padding: 8px 0;
  }
  .prop-body {
    flex: 1;
    min-width: 0;
  }
  .prop-title {
    font-size: 13px;
    font-weight: 600;
  }
  .prop-detail {
    font-size: 12px;
    color: var(--fg-muted);
  }
  .prop-sub {
    font-size: 11px;
  }
  .kind-h {
    font-size: 13px;
    margin: 14px 0 6px;
    color: var(--fg-muted);
  }
  .item {
    border: 1px solid var(--border);
    border-left: 3px solid var(--border-strong);
    border-radius: 10px;
    padding: 10px 12px;
    margin-bottom: 8px;
    background: var(--bg);
  }
  .item--resolved {
    opacity: 0.6;
  }
  .item--dismissed {
    opacity: 0.45;
  }
  .item-main {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .item-title {
    font-weight: 600;
    font-size: 13px;
  }
  .item-status {
    font-size: 11px;
    padding: 1px 7px;
    border-radius: 999px;
    border: 1px solid var(--border-strong);
    color: var(--fg-muted);
  }
  .item-status--active {
    border-color: var(--ok);
    color: var(--ok);
  }
  .item-status--paused {
    border-color: var(--warn);
    color: var(--warn);
  }
  .origin {
    font-size: 11px;
  }
  .item-detail {
    font-size: 12px;
    color: var(--fg-muted);
    margin: 4px 0;
  }
  .ev {
    margin: 2px 0 6px;
    padding-left: 16px;
    font-size: 11px;
  }
  .item-actions {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }
  .badge {
    font-size: 11px;
    padding: 1px 8px;
    border-radius: 999px;
    border: 1px solid var(--border-strong);
    color: var(--fg-muted);
  }
  .badge--goal {
    border-color: var(--accent);
    color: var(--accent);
  }
  .badge--misconception {
    border-color: var(--danger);
    color: var(--danger);
  }
  .badge--review {
    border-color: var(--warn);
    color: var(--warn);
  }
  .badge--strength {
    border-color: var(--ok);
    color: var(--ok);
  }
  .faint {
    color: var(--fg-faint);
    font-size: 12px;
  }
  .err {
    color: var(--danger);
    font-size: 12px;
  }
  .export-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 50;
  }
  .export-box {
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: 12px;
    width: min(680px, 92vw);
    max-height: 80vh;
    display: flex;
    flex-direction: column;
  }
  .export-box header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 10px 14px;
    border-bottom: 1px solid var(--border);
    font-size: 13px;
  }
  .export-box pre {
    padding: 12px 14px;
    overflow: auto;
    white-space: pre-wrap;
    font-size: 12px;
  }
</style>
