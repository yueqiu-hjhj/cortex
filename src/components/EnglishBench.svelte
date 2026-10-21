<script lang="ts">
  import * as api from "../lib/api";
  import { jobs } from "../lib/jobs.svelte";
  import { app } from "../lib/store.svelte";
  import Icon from "./Icon.svelte";

  let {
    sourceId,
    kind,
    onclose,
  }: { sourceId: string; kind: "close" | "translate"; onclose: () => void } = $props();

  let result = $state<api.BenchResult | null>(null);
  let err = $state<string | null>(null);
  let busy = $state(false);
  // 初始页签跟随 prop；之后由面板内的页签自己切（父级永远传 "close"，
  // 但 prop 变的时候页签应该跟着变，而不是停留在旧值）。
  let tab = $state<"close" | "translate">(kind);
  $effect(() => {
    tab = kind;
  });
  // 选择题作答：dimension -> 已选选项 index（点了才揭晓，和真实考试一样）
  let picked = $state<Record<string, number>>({});
  // 已存入词汇本的替换对（sourceId:index）
  let savedSyn = $state<Set<string>>(new Set());

  const DIM_LABELS: Record<string, string> = {
    translation: "全文精译",
    gist: "主旨与态度",
    syntax: "长难句拆解",
    synonyms: "同义替换",
    familiar: "熟词生义",
    traps: "命题陷阱",
    culture_words: "文化负载词",
    phrases: "高分短语",
    checklist: "评分自查",
  };
  const ATTITUDE_LABELS: Record<string, string> = {
    support: "支持",
    neutral: "中立",
    critical: "批评",
    mixed: "复杂/矛盾",
  };

  async function load() {
    try {
      result = await api.benchRunGet(tab, sourceId);
      err = null;
    } catch (e) {
      err = String(e);
    }
  }

  $effect(() => {
    void tab;
    void load();
  });

  const running = $derived(result?.run.status === "running");
  $effect(() => {
    if (!result || result.run.status !== "running") return;
    const iv = setInterval(() => void load(), 2500);
    return () => clearInterval(iv);
  });

  async function start() {
    busy = true;
    err = null;
    try {
      const runId = await api.benchRunCreate(tab, sourceId);
      // get_latest is newest-first, so the run we just made is what comes
      // back; re-fetch by polling would be equivalent but this is one call.
      result = await api.benchRunGet(tab, sourceId);
      if (!result || result.run.id !== runId) {
        result = { run: { ...result!.run, id: runId }, items: [] };
      }
      jobs.start({
        kind: "extract",
        label: tab === "close" ? "应试精读" : "翻译训练",
        subjectId: "",
        run: () => api.benchRunExecute(runId),
        onDone: () => void load(),
      });
    } catch (e) {
      err = String(e);
    } finally {
      busy = false;
    }
  }

  async function regenerate() {
    // A fresh run every time: the previous one stays in history.
    await start();
  }

  async function retry(dimension: string) {
    if (!result) return;
    try {
      await api.benchItemRetry(result.run.id, dimension);
      await load();
    } catch (e) {
      err = String(e);
    }
  }

  async function exportMd() {
    if (!result) return;
    const { save } = await import("@tauri-apps/plugin-dialog");
    const dest = await save({
      defaultPath: `${tab === "close" ? "应试精读" : "翻译训练"}-${result.run.source_name}.md`,
      filters: [{ name: "Markdown", extensions: ["md"] }],
    });
    if (!dest) return;
    try {
      await api.benchMdFile(result.run.id, dest);
      app.pushToast({ kind: "success", title: "已导出 Markdown" });
    } catch (e) {
      app.pushToast({ kind: "error", title: "导出失败", body: String(e) });
    }
  }

  async function saveSynonym(p: { original: string; upgrade: string; context?: string }, i: number) {
    try {
      await api.benchSynonymSave(sourceId, p.original, p.upgrade, p.context ?? "");
      app.pushToast({ kind: "success", title: "已存入词汇本，进入复习队列" });
      savedSyn = new Set([...savedSyn, `${sourceId}:${i}`]);
    } catch (e) {
      app.pushToast({ kind: "error", title: "存入失败", body: String(e) });
    }
  }

  function item(dimension: string): api.BenchItem | undefined {
    return result?.items.find((i) => i.dimension === dimension);
  }
  function pct(): number {
    if (!result) return 0;
    const done = result.items.filter((i) => i.status === "done").length;
    return Math.round((done / Math.max(result.items.length, 1)) * 100);
  }
</script>

<div class="bench-bg" onclick={onclose} role="presentation"></div>
<div class="bench" role="dialog" aria-label="应试精读">
  <header class="b-head">
    <div class="b-tabs">
      <button class="b-tab" class:on={tab === "close"} onclick={() => (tab = "close")}>六维精读</button>
      <button class="b-tab" class:on={tab === "translate"} onclick={() => (tab = "translate")}>翻译训练</button>
    </div>
    <span style="flex:1"></span>
    {#if result?.run.status === "done"}
      <button class="btn btn--sm" onclick={exportMd}><Icon name="doc" size={12} /> 导出</button>
    {/if}
    <button class="btn btn--sm" onclick={onclose}>关闭</button>
  </header>

  <div class="b-body">
    {#if err}<p class="b-err">{err}</p>{/if}

    {#if !result}
      <div class="b-intro">
        <h3>{tab === "close" ? "六维精读" : "翻译训练"}</h3>
        <p>
          {#if tab === "close"}
            把这一篇读透：全文精译、主旨与态度、长难句拆解、同义替换（可直通词汇本）、熟词生义、命题陷阱选择题。
            六个维度各自独立调用——坏一个只坏一个，可单独重试。每个声称引用原文的条目都会先验证原文里真有这句话。
          {:else}
            针对这一篇的三个训练：文化负载词（直译误区 → 意译推荐）、高分短语（平庸表达 → 高分表达）、翻译评分自查清单。
          {/if}
        </p>
        <button class="btn primary" disabled={busy} onclick={start}>
          {busy ? "创建中…" : tab === "close" ? "开始六维解析" : "开始训练"}
        </button>
      </div>
    {:else}
      <div class="b-runhead">
        <span class="b-src">{result.run.source_name}</span>
        {#if result.run.truncated}<span class="b-trunc" title="原文过长，只解析了前段">前段</span>{/if}
        {#if running}
          <span class="b-badge run">进行中 {pct()}%</span>
        {:else if result.run.status === "error"}
          <span class="b-badge fail">失败</span>
        {:else}
          <span class="b-badge ok">完成</span>
        {/if}
        <span style="flex:1"></span>
        <button class="btn btn--sm" disabled={running || busy} onclick={regenerate}>
          {busy ? "…" : "重新生成"}
        </button>
      </div>
      {#if result.run.error && result.run.status === "done"}
        <p class="b-note">{result.run.error}</p>
      {/if}
      {#if result.run.status === "error"}
        <p class="b-note err">{result.run.error ?? "所有维度都失败了——检查模型配置后重试"}</p>
      {/if}

      {#each result.items as it (it.id)}
        {@const label = DIM_LABELS[it.dimension] ?? it.dimension}
        <section class="b-dim">
          <h4>
            {label}
            {#if it.status === "pending" && running}<span class="b-badge run">等待中</span>
            {:else if it.status === "error"}<span class="b-badge fail">失败</span>{/if}
            {#if it.status === "error"}
              <button class="b-retry" onclick={() => retry(it.dimension)}>重试</button>
            {/if}
          </h4>

          {#if it.status === "pending"}
            <p class="b-dim-empty">{running ? "排队生成中…" : "尚未生成"}</p>
          {:else if it.status === "error"}
            <p class="b-dim-empty err">{it.error}</p>
          {:else if it.dimension === "translation"}
            <p class="b-trans">{(it.payload as { text?: string })?.text}</p>
          {:else if it.dimension === "gist"}
            {@const p = it.payload as { main_idea?: string; attitude?: string; confidence?: number; points?: string[] }}
            <p class="b-gist-idea"><b>主旨：</b>{p?.main_idea}</p>
            <p class="b-gist-att">
              <b>态度：</b>{ATTITUDE_LABELS[p?.attitude ?? ""] ?? p?.attitude}
              <span class="b-conf" title="模型自评置信度">置信度 {Math.round((p?.confidence ?? 0) * 100)}%</span>
            </p>
            {#if p?.points?.length}
              <ul class="b-points">
                {#each p.points as pt}<li>{pt}</li>{/each}
              </ul>
            {/if}
          {:else if it.dimension === "syntax"}
            {@const ss = (it.payload as { sentences?: { original: string; structure: string; translation: string }[] })?.sentences ?? []}
            {#each ss as s, i (i)}
              <div class="b-syn">
                <div class="b-syn-o">{s.original}</div>
                <div class="b-syn-s"><b>结构：</b>{s.structure}</div>
                <div class="b-syn-t">{s.translation}</div>
              </div>
            {/each}
          {:else if it.dimension === "synonyms"}
            {@const ps = (it.payload as { pairs?: { original: string; upgrade: string; context?: string }[] })?.pairs ?? []}
            {#each ps as pr, i (i)}
              <div class="b-pair">
                <span class="b-pair-o">{pr.original}</span>
                <span class="b-arrow">→</span>
                <span class="b-pair-u">{pr.upgrade}</span>
                <button
                  class="b-save"
                  class:done={savedSyn.has(`${sourceId}:${i}`)}
                  disabled={savedSyn.has(`${sourceId}:${i}`)}
                  onclick={() => saveSynonym(pr, i)}
                  title="存进词汇本，进入 FSRS 复习队列"
                >
                  {savedSyn.has(`${sourceId}:${i}`) ? "已存" : "存词汇本"}
                </button>
              </div>
              {#if pr.context}<div class="b-pair-ctx">「{pr.context}」</div>{/if}
            {/each}
          {:else if it.dimension === "familiar"}
            {@const ws = (it.payload as { words?: { word: string; common?: string; here: string; sentence?: string }[] })?.words ?? []}
            {#each ws as w, i (i)}
              <div class="b-word">
                <b>{w.word}</b>
                <span class="b-word-c">常义「{w.common}」</span>
                <span class="b-arrow">→</span>
                <span class="b-word-h">此处「{w.here}」</span>
                {#if w.sentence}<div class="b-word-s">「{w.sentence}」</div>{/if}
              </div>
            {/each}
          {:else if it.dimension === "traps"}
            {@const qs = (it.payload as { questions?: { question: string; options: string[]; answer: number; trap?: string; explain?: string }[] })?.questions ?? []}
            {#each qs as q, qi (qi)}
              <div class="b-q">
                <div class="b-q-t">{qi + 1}. {q.question}</div>
                <div class="b-opts">
                  {#each q.options as o, oi (oi)}
                    <button
                      class="b-opt"
                      class:right={picked[qi] != null && oi === q.answer}
                      class:wrong={picked[qi] === oi && oi !== q.answer}
                      disabled={picked[qi] != null}
                      onclick={() => (picked = { ...picked, [qi]: oi })}
                    >
                      {"ABCD"[oi]}. {o}
                    </button>
                  {/each}
                </div>
                {#if picked[qi] != null}
                  <div class="b-q-a">
                    {#if picked[qi] === q.answer}<span class="b-ok">答对了</span>{:else}<span class="b-no">答错了</span>{/if}
                    {#if q.trap}<span class="b-trap">陷阱：{q.trap}</span>{/if}
                    {#if q.explain}<div class="b-q-e">{q.explain}</div>{/if}
                  </div>
                {/if}
              </div>
            {/each}
          {:else if it.dimension === "culture_words"}
            {@const ws = (it.payload as { words?: { phrase: string; literal?: string; proper: string; example?: string }[] })?.words ?? []}
            {#each ws as w, i (i)}
              <div class="b-cw">
                <b>{w.phrase}</b>
                <span class="b-cw-l">直译「{w.literal}」</span>
                <span class="b-arrow">→</span>
                <span class="b-cw-p">宜译「{w.proper}」</span>
                {#if w.example}<div class="b-cw-e">{w.example}</div>{/if}
              </div>
            {/each}
          {:else if it.dimension === "phrases"}
            {@const is = (it.payload as { items?: { scene?: string; plain: string; better: string }[] })?.items ?? []}
            {#each is as ph, i (i)}
              <div class="b-ph">
                {#if ph.scene}<span class="b-ph-s">{ph.scene}</span>{/if}
                <span class="b-ph-p">{ph.plain}</span>
                <span class="b-arrow">→</span>
                <span class="b-ph-b">{ph.better}</span>
              </div>
            {/each}
          {:else if it.dimension === "checklist"}
            {@const ds = (it.payload as { dimensions?: { name: string; what?: string }[] })?.dimensions ?? []}
            <ul class="b-check">
              {#each ds as d, i (i)}
                <li><b>{d.name}</b>{#if d.what}：{d.what}{/if}</li>
              {/each}
            </ul>
          {/if}
        </section>
      {/each}
    {/if}
  </div>
</div>

<style>
  .bench-bg {
    position: fixed; inset: 0; z-index: 60;
    background: color-mix(in oklab, var(--bg-sunken) 70%, transparent);
  }
  .bench {
    position: fixed; z-index: 61; inset: 5% 8%;
    background: var(--bg); border: 1px solid var(--border); border-radius: 12px;
    display: flex; flex-direction: column; overflow: hidden;
    box-shadow: 0 18px 60px rgb(0 0 0 / 0.35);
  }
  .b-head { display: flex; align-items: center; gap: 8px; padding: 10px 14px; border-bottom: 1px solid var(--border); }
  .b-tabs { display: inline-flex; gap: 2px; background: var(--surface-2); border-radius: 999px; padding: 2px; }
  .b-tab { border: 0; background: transparent; color: var(--fg-muted); font-size: var(--t-xs); padding: 3px 12px; border-radius: 999px; cursor: pointer; }
  .b-tab.on { background: var(--accent); color: var(--accent-fg, #04120c); }
  .b-body { flex: 1; overflow-y: auto; padding: 14px 18px 24px; }
  .b-err { color: var(--err, var(--warn)); font-size: var(--t-xs); }
  .b-note { font-size: var(--t-xs); color: var(--fg-muted); margin: 4px 0 8px; }
  .b-note.err { color: var(--err, var(--warn)); }
  .b-intro h3 { margin: 18px 0 8px; font-size: var(--t-l, 16px); }
  .b-intro p { font-size: var(--t-s); color: var(--fg-muted); line-height: 1.7; max-width: 560px; }
  .b-intro .btn { margin-top: 12px; }
  .b-runhead { display: flex; align-items: center; gap: 8px; margin-bottom: 10px; }
  .b-src { font-size: var(--t-s); font-weight: 600; color: var(--fg-bright); }
  .b-trunc { font-size: 10px; color: var(--warn); border: 1px solid var(--warn); border-radius: 4px; padding: 0 5px; }
  .b-badge { font-size: 10px; font-family: var(--font-mono); padding: 1px 8px; border-radius: 999px; border: 1px solid var(--border); }
  .b-badge.run { color: var(--accent); border-color: color-mix(in oklab, var(--accent) 45%, transparent); }
  .b-badge.ok { color: var(--ok); border-color: color-mix(in oklab, var(--ok) 45%, transparent); }
  .b-badge.fail { color: var(--err, #e5484d); border-color: color-mix(in oklab, var(--err, #e5484d) 45%, transparent); }
  .b-dim { border: 1px solid var(--border); border-radius: 8px; padding: 10px 14px; margin-bottom: 10px; background: var(--surface); }
  .b-dim h4 { margin: 0 0 8px; font-size: var(--t-s); display: flex; align-items: center; gap: 8px; }
  .b-retry { border: 1px solid var(--border); background: var(--surface-2); color: var(--fg-muted); font-size: 10px; border-radius: 4px; padding: 1px 8px; cursor: pointer; }
  .b-dim-empty { font-size: var(--t-xs); color: var(--fg-muted); margin: 0; }
  .b-dim-empty.err { color: var(--err, var(--warn)); }
  .b-trans { font-size: var(--t-s); line-height: 1.9; margin: 0; white-space: pre-wrap; }
  .b-gist-idea, .b-gist-att { font-size: var(--t-s); margin: 0 0 4px; line-height: 1.7; }
  .b-conf { font-size: 10px; color: var(--fg-muted); margin-left: 8px; }
  .b-points { margin: 4px 0 0; padding-left: 18px; }
  .b-points li { font-size: var(--t-s); color: var(--fg); line-height: 1.7; }
  .b-syn { padding: 6px 0; border-bottom: 1px dashed var(--border); }
  .b-syn:last-child { border-bottom: 0; }
  .b-syn-o { font-size: var(--t-s); color: var(--fg-bright); line-height: 1.6; }
  .b-syn-s { font-size: var(--t-xs); color: var(--accent); margin: 3px 0; }
  .b-syn-t { font-size: var(--t-s); color: var(--fg); }
  .b-pair { display: flex; align-items: baseline; gap: 8px; padding: 4px 0; font-size: var(--t-s); flex-wrap: wrap; }
  .b-pair-o { color: var(--fg-muted); }
  .b-arrow { color: var(--fg-faint); }
  .b-pair-u { color: var(--ok); font-weight: 600; }
  .b-pair-ctx { font-size: var(--t-xs); color: var(--fg-faint); margin: -2px 0 6px; }
  .b-save {
    margin-left: auto; border: 1px solid var(--accent); color: var(--accent); background: transparent;
    font-size: 10px; border-radius: 999px; padding: 1px 10px; cursor: pointer;
  }
  .b-save.done { border-color: var(--fg-faint); color: var(--fg-faint); cursor: default; }
  .b-word { padding: 5px 0; font-size: var(--t-s); border-bottom: 1px dashed var(--border); }
  .b-word:last-child { border-bottom: 0; }
  .b-word-c, .b-word-h { margin: 0 6px; font-size: var(--t-xs); }
  .b-word-h { color: var(--accent); }
  .b-word-s { font-size: var(--t-xs); color: var(--fg-faint); margin-top: 2px; }
  .b-q { padding: 8px 0; border-bottom: 1px dashed var(--border); }
  .b-q:last-child { border-bottom: 0; }
  .b-q-t { font-size: var(--t-s); line-height: 1.6; margin-bottom: 6px; }
  .b-opts { display: grid; grid-template-columns: 1fr 1fr; gap: 4px 10px; }
  .b-opt {
    text-align: left; border: 1px solid var(--border); background: var(--surface-2); color: var(--fg);
    font-size: var(--t-xs); border-radius: 6px; padding: 5px 9px; cursor: pointer;
  }
  .b-opt:disabled { cursor: default; }
  .b-opt.right { border-color: var(--ok); color: var(--ok); }
  .b-opt.wrong { border-color: var(--err, #e5484d); color: var(--err, #e5484d); }
  .b-q-a { margin-top: 6px; font-size: var(--t-xs); }
  .b-ok { color: var(--ok); }
  .b-no { color: var(--err, #e5484d); }
  .b-trap { color: var(--warn); margin-left: 8px; }
  .b-q-e { color: var(--fg-muted); margin-top: 3px; line-height: 1.6; }
  .b-cw, .b-ph { padding: 5px 0; font-size: var(--t-s); border-bottom: 1px dashed var(--border); }
  .b-cw:last-child, .b-ph:last-child { border-bottom: 0; }
  .b-cw-l, .b-cw-p { margin: 0 6px; font-size: var(--t-xs); }
  .b-cw-p { color: var(--ok); }
  .b-cw-e { font-size: var(--t-xs); color: var(--fg-faint); margin-top: 2px; }
  .b-ph-s { font-size: 10px; color: var(--fg-muted); border: 1px solid var(--border); border-radius: 999px; padding: 0 8px; margin-right: 8px; }
  .b-ph-p { color: var(--fg-muted); }
  .b-ph-b { color: var(--ok); font-weight: 600; }
  .b-check { margin: 0; padding-left: 4px; list-style: none; }
  .b-check li { font-size: var(--t-s); line-height: 1.9; }
  .b-check li::before { content: "☐ "; color: var(--fg-muted); }
  @media (max-width: 760px) {
    .bench { inset: 2% 3%; }
    .b-opts { grid-template-columns: 1fr; }
  }
</style>
