<script lang="ts">
  // 研究对话 — ask the literature a question, in words.
  //
  // The search box next door is one string in, one ranked list out. This panel
  // is a thread: the model picks tools (find_authors / search_literature /
  // my_library / read_paper), reads what they return, and answers. Tool rows
  // stay in the transcript on purpose — an answer you cannot trace is a rumor.
  //
  // Agent answers carry `data-clip-scope`, so L3 still works here: select any
  // sentence, file it into a note, and the passage lands under `## 摘录`.
  import * as api from "../lib/api";
  import { app } from "../lib/store.svelte";

  // `panel` is the inline card on the 科研 page; `page` is the first-class
  // sidebar entry, where there is nothing to collapse into.
  let {
    open = $bindable(false),
    mode = "panel",
  }: { open?: boolean; mode?: "panel" | "page" } = $props();

  const shown = $derived(mode === "page" || open);

  let threads = $state<api.ResearchThread[]>([]);
  let messages = $state<api.ResearchMessage[]>([]);
  let activeId = $state<string | null>(null);
  let input = $state("");
  let busy = $state(false);
  let error = $state<string | null>(null);
  let booted = $state(false);
  let scroller = $state<HTMLDivElement | null>(null);

  const SUGGESTIONS = [
    "谁在做检索增强生成的评测？列几个团队和他们的近作",
    "我库里扩散模型相关的论文有哪些？各讲了什么",
    "Transformer 之后，长上下文这条线现在是谁在推",
  ];

  const TOOL_LABEL: Record<string, string> = {
    search_literature: "检索文献",
    find_authors: "查作者 / 团队",
    my_library: "查我的论文库",
    read_paper: "读一篇论文",
  };

  // 知源's 回答依据 panel, in Cortex terms: every tool call shows WHAT was
  // asked (args) next to WHAT came back, colour-coded by what kind of
  // evidence it is — 检索外部 / 读本地库 / 读单篇.
  const TOOL_KIND: Record<string, { label: string; color: string }> = {
    search_literature: { label: "外部检索", color: "var(--info, #378ADD)" },
    find_authors: { label: "外部检索", color: "var(--info, #378ADD)" },
    my_library: { label: "本地库", color: "var(--ok, #639922)" },
    read_paper: { label: "读论文", color: "var(--accent)" },
  };
  function toolKind(name: string | null): { label: string; color: string } {
    return TOOL_KIND[name ?? ""] ?? { label: "工具", color: "var(--fg-faint)" };
  }
  function prettyArgs(args: string | null): string {
    if (!args) return "";
    try {
      return JSON.stringify(JSON.parse(args), null, 0);
    } catch {
      return args;
    }
  }
  /** How many tool calls fed the answer at index i — the 知源-style
   *  "查证比例" in its simplest honest form: a count, per answer. */
  function toolRunSince(i: number): number {
    let n = 0;
    for (let j = i - 1; j >= 0; j--) {
      if (messages[j].role === "tool") n += 1;
      else break;
    }
    return n;
  }

  $effect(() => {
    if (!shown || booted) return;
    booted = true;
    void boot();
  });

  // Keep the newest turn in view. Reading `messages.length` + `busy` is enough
  // to re-run; scrolling itself touches no state.
  $effect(() => {
    const n = messages.length + (busy ? 1 : 0);
    if (!n) return;
    queueMicrotask(() => scroller?.scrollTo({ top: scroller.scrollHeight, behavior: "smooth" }));
  });

  async function boot() {
    try {
      threads = await api.researchThreads();
      if (threads.length) await openThread(threads[0].id);
    } catch (e) {
      error = String(e);
    }
  }

  async function refreshThreads() {
    try {
      threads = await api.researchThreads();
    } catch {
      /* the thread list is a convenience, never a blocker */
    }
  }

  async function openThread(id: string | null) {
    activeId = id;
    error = null;
    if (!id) {
      messages = [];
      return;
    }
    try {
      messages = await api.researchThreadMessages(id);
    } catch (e) {
      error = String(e);
    }
  }

  function newThread() {
    activeId = null;
    messages = [];
    error = null;
    input = "";
  }

  async function removeThread() {
    if (!activeId) return;
    const t = threads.find((x) => x.id === activeId);
    const ok = await app.confirm({
      title: "删除这个对话？",
      body: t?.title ?? "",
      danger: true,
    });
    if (!ok) return;
    try {
      await api.researchThreadDelete(activeId);
      newThread();
      await refreshThreads();
    } catch (e) {
      error = String(e);
    }
  }

  async function send(text?: string) {
    const question = (text ?? input).trim();
    if (!question || busy) return;
    input = "";
    error = null;
    busy = true;
    const threadId = activeId;
    messages = [
      ...messages,
      {
        id: "pending",
        thread_id: threadId ?? "",
        role: "user",
        text: question,
        tool: null,
        args: null,
        created_at: Date.now(),
      },
    ];
    try {
      const out = await api.researchAsk(question, threadId);
      activeId = out.thread_id;
      messages = await api.researchThreadMessages(out.thread_id);
      await refreshThreads();
    } catch (e) {
      error = String(e);
      // Give the question back — losing a typed paragraph to a 429 is rude.
      messages = messages.filter((m) => m.id !== "pending");
      input = question;
    } finally {
      busy = false;
    }
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      void send();
    }
  }

  function toolLabel(name: string | null): string {
    return TOOL_LABEL[name ?? ""] ?? name ?? "工具";
  }

  function toolHint(m: api.ResearchMessage): string {
    const first = m.text.split("\n").find((l) => l.trim().length > 0) ?? "";
    return first.length > 64 ? `${first.slice(0, 64)}…` : first;
  }
</script>

{#if shown}
  <div class="rc" class:is-page={mode === "page"}>
    <div class="rc-head">
      <div class="rc-title">
        <span class="dot"></span>
        研究对话
        <span class="rc-sub">问方向、问团队、问具体某个人 —— 它会真的去查，查完再答</span>
      </div>
      <div class="rc-head-actions">
        <select
          class="rc-select"
          value={activeId ?? ""}
          onchange={(e) => void openThread((e.currentTarget as HTMLSelectElement).value || null)}
          title="切换对话"
        >
          <option value="">＋ 新对话</option>
          {#each threads as t (t.id)}
            <option value={t.id}>{t.title}（{t.turns} 轮）</option>
          {/each}
        </select>
        <button class="btn" onclick={newThread} title="从空白开始">新对话</button>
        <button class="btn" disabled={!activeId || busy} onclick={() => void removeThread()} title="删除当前对话">
          删除
        </button>
        {#if mode === "panel"}
          <button class="btn" onclick={() => (open = false)} title="收起">收起</button>
        {/if}
      </div>
    </div>

    {#if error}
      <div class="rc-err">{error}</div>
    {/if}

    <div class="rc-log" bind:this={scroller}>
      {#if messages.length === 0 && !busy}
        <div class="rc-empty">
          <p>别搜关键词了，直接问：</p>
          {#each SUGGESTIONS as s}
            <button class="rc-chip" onclick={() => void send(s)}>{s}</button>
          {/each}
          <p class="rc-faint">
            它会先用 OpenAlex 找人 / 找论文（可按作者、机构、年份过滤），再查你的本地论文库；每一步检索都留在对话里。
          </p>
        </div>
      {/if}

      {#each messages as m, i (m.id)}
        {#if m.role === "user"}
          <div class="rc-row is-user">
            <div class="rc-bubble is-user">{m.text}</div>
          </div>
        {:else if m.role === "tool"}
          {@const kind = toolKind(m.tool)}
          <details class="rc-tool">
            <summary>
              <span class="rc-tool-kind" style:color={kind.color}>{kind.label}</span>
              <span class="rc-tool-name">{toolLabel(m.tool)}</span>
              <span class="rc-tool-hint">{toolHint(m)}</span>
            </summary>
            {#if m.args}
              <div class="rc-tool-args mono" title="这次调用传给工具的参数">{prettyArgs(m.args)}</div>
            {/if}
            <pre>{m.text}</pre>
          </details>
        {:else}
          {@const since = toolRunSince(i)}
          {#if since > 0}
            <div class="rc-turn-summary mono" title="这个回答之前发生的工具调用——上面每条都可以展开核对">
              本回合 {since} 次工具调用 · 参数与返回都在上面
            </div>
          {/if}
          <div class="rc-row is-agent">
            <div class="rc-bubble is-agent" data-clip-scope="agent" data-clip-label="研究对话">
              {m.text}
            </div>
          </div>
        {/if}
      {/each}

      {#if busy}
        <div class="rc-row is-agent">
          <div class="rc-bubble is-agent is-thinking">
            <span class="dots"><i></i><i></i><i></i></span>
            正在查资料…
          </div>
        </div>
      {/if}
    </div>

    <div class="rc-compose">
      <textarea
        bind:value={input}
        onkeydown={onKey}
        rows={2}
        placeholder="例如：哪个团队在做长上下文评测？看看他们 2025 年的工作 / 我库里关于扩散模型的笔记有哪些"
        disabled={busy}
      ></textarea>
      <div class="rc-send-col">
        <button class="btn primary" disabled={busy || !input.trim()} onclick={() => void send()}>
          {busy ? "查询中…" : "发送"}
        </button>
        <span class="rc-faint">Enter 发送 · Shift+Enter 换行</span>
      </div>
    </div>
  </div>
{/if}

<style>
  .rc {
    border: 1px solid var(--border);
    border-radius: 10px;
    background: var(--surface);
    margin: 10px 0 14px;
    display: flex;
    flex-direction: column;
    /* Stable height in two different hosts (科研 flex column / 阅读器 scroller):
       the log is the only part that grows, so the composer never jumps. */
    flex: 0 0 auto;
    height: clamp(360px, 62vh, 640px);
    min-height: 0;
  }
  /* Page mode fills the workspace: the transcript is the page, so the fixed
     panel height (and the border that separated it from 科研) has to go. */
  .rc.is-page {
    height: 100%;
    flex: 1 1 auto;
    margin: 0;
    border: 0;
    border-radius: 0;
    background: transparent;
  }
  .rc.is-page .rc-log { padding: 12px clamp(12px, 4vw, 48px); }
  .rc.is-page .rc-compose { padding: 10px clamp(12px, 4vw, 48px) 14px; }
  .rc-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 10px 12px;
    border-bottom: 1px solid var(--border);
    flex-wrap: wrap;
  }
  .rc-title {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    font-weight: 600;
    color: var(--fg-bright);
  }
  .rc-sub {
    font-weight: 400;
    font-size: 11.5px;
    color: var(--fg-faint);
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-dim);
  }
  .rc-head-actions {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .rc-select {
    max-width: 240px;
    background: var(--surface-2);
    color: var(--fg);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 5px 6px;
    font-size: 12px;
  }
  .rc-err {
    margin: 8px 12px 0;
    border: 1px solid var(--err);
    color: var(--err);
    border-radius: 6px;
    padding: 7px 10px;
    font-size: 12px;
  }
  .rc-log {
    padding: 12px;
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .rc-empty {
    margin: auto;
    max-width: 560px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    align-items: stretch;
    text-align: left;
  }
  .rc-empty p {
    margin: 0;
    color: var(--fg-muted);
    font-size: 12.5px;
  }
  .rc-chip {
    text-align: left;
    background: var(--surface-2);
    border: 1px solid var(--border);
    color: var(--fg);
    border-radius: 7px;
    padding: 9px 11px;
    font-size: 12.5px;
    cursor: pointer;
  }
  .rc-chip:hover {
    border-color: var(--accent);
    color: var(--fg-bright);
  }
  .rc-faint {
    color: var(--fg-faint);
    font-size: 11.5px;
    line-height: 1.6;
  }
  .rc-row {
    display: flex;
  }
  .rc-row.is-user {
    justify-content: flex-end;
  }
  .rc-row.is-agent {
    justify-content: flex-start;
  }
  .rc-bubble {
    max-width: 88%;
    padding: 9px 12px;
    border-radius: 9px;
    font-size: 13px;
    line-height: 1.7;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .rc-bubble.is-user {
    background: var(--accent-dim);
    border: 1px solid color-mix(in oklab, var(--accent) 40%, transparent);
    color: var(--fg-bright);
  }
  .rc-bubble.is-agent {
    background: var(--surface-2);
    border: 1px solid var(--border);
    color: var(--fg);
  }
  .is-thinking {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--fg-muted);
  }
  .dots {
    display: inline-flex;
    gap: 3px;
  }
  .dots i {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--accent);
    animation: rc-blink 1.1s infinite ease-in-out;
  }
  .dots i:nth-child(2) {
    animation-delay: 0.15s;
  }
  .dots i:nth-child(3) {
    animation-delay: 0.3s;
  }
  @keyframes rc-blink {
    0%,
    80%,
    100% {
      opacity: 0.25;
    }
    40% {
      opacity: 1;
    }
  }
  .rc-tool {
    align-self: flex-start;
    max-width: 88%;
    border: 1px dashed var(--border);
    border-radius: 7px;
    background: transparent;
    font-size: 11.5px;
    color: var(--fg-muted);
  }
  .rc-tool summary {
    cursor: pointer;
    padding: 5px 9px;
    display: flex;
    gap: 8px;
    align-items: baseline;
    list-style: none;
  }
  .rc-tool summary::-webkit-details-marker {
    display: none;
  }
  .rc-tool-name {
    color: var(--accent);
    font-weight: 600;
  }
  .rc-tool-kind {
    flex: none;
    font-family: var(--font-mono);
    font-size: 9.5px;
    letter-spacing: 0.04em;
    border: 1px solid currentColor;
    border-radius: 3px;
    padding: 0 4px;
    opacity: 0.9;
  }
  .rc-tool-hint {
    color: var(--fg-faint);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .rc-tool-args {
    margin: 0;
    padding: 0 10px 6px;
    font-size: 10.5px;
    color: var(--fg-faint);
    white-space: pre-wrap;
    word-break: break-all;
  }
  .rc-turn-summary {
    align-self: flex-start;
    font-size: 10.5px;
    color: var(--fg-faint);
    padding: 2px 0 0 2px;
  }
  .rc-tool pre {
    margin: 0;
    padding: 0 10px 8px;
    max-height: 220px;
    overflow: auto;
    white-space: pre-wrap;
    font-size: 11.5px;
    line-height: 1.6;
    color: var(--fg-muted);
  }
  .rc-compose {
    border-top: 1px solid var(--border);
    padding: 10px 12px;
    display: flex;
    gap: 10px;
    align-items: stretch;
  }
  .rc-compose textarea {
    flex: 1;
    resize: vertical;
    min-height: 46px;
    max-height: 160px;
    background: var(--surface-2);
    color: var(--fg);
    border: 1px solid var(--border);
    border-radius: 7px;
    padding: 8px 10px;
    font: inherit;
    font-size: 12.5px;
    line-height: 1.6;
  }
  .rc-compose textarea:focus {
    outline: none;
    border-color: var(--accent);
  }
  .rc-send-col {
    display: flex;
    flex-direction: column;
    justify-content: space-between;
    gap: 4px;
    min-width: 92px;
  }
</style>
