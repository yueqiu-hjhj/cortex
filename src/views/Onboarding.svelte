<script lang="ts">
  import { app, THEMES } from "../lib/store.svelte";
  import type { Theme } from "../lib/store.svelte";
  import Icon from "../components/Icon.svelte";
  import logo from "../assets/cortex-logo.png";

  let { onFinish }: { onFinish?: () => void } = $props();

  let step = $state(0);
  let apiKey = $state("");
  let theme = $state<Theme>("osaka-jade");
  let homelab = $state(false);
  let homelabEndpoint = $state("");
  let subjName = $state("");

  const steps = ["Welcome", "Your key", "Theme", "Homelab", "First subject"];

  function next() {
    step = Math.min(steps.length - 1, step + 1);
  }

  function pickTheme(t: Theme) {
    theme = t;
    app.setTheme(t);
  }

  function finish() {
    onFinish?.();
  }

  const THEME_OPTS: { id: Theme; n: string; c: string; b: string }[] = [
    { id: "osaka-jade",  n: "Osaka Jade",  c: "#2dd5b7", b: "#111c18" },
    { id: "tokyo-night", n: "Tokyo Night",  c: "#7aa2f7", b: "#1a1b26" },
    { id: "catppuccin",  n: "Catppuccin",   c: "#94e2d5", b: "#1e1e2e" },
  ];

  const SUGGESTIONS = ["Algorithms", "Operating Systems", "Statistical Inference"];
</script>

<div class="onb">
  <!-- Left rail: step list -->
  <div class="onb-rail">
    <div class="onb-brand">
      <img class="ds-logo-glyph" src={logo} alt="Cortex" /> Cortex
    </div>
    <div class="onb-steps">
      {#each steps as s, i}
        <div class={"onb-step" + (i === step ? " on" : "") + (i < step ? " done" : "")}>
          <span class="os-num">
            {#if i < step}
              <Icon name="check" size={11} />
            {:else}
              {i + 1}
            {/if}
          </span>
          {s}
        </div>
      {/each}
    </div>
    <div class="onb-skip">
      <button class="btn btn--ghost btn--sm" onclick={finish}>跳过设置</button>
    </div>
  </div>

  <!-- Main content -->
  <div class="onb-main">
    <div class="onb-card">
      <!-- Step 0: Welcome -->
      {#if step === 0}
        <div class="onb-pane">
          <div class="eyebrow">首次使用</div>
          <h1 class="read onb-h">给难学的科目，一个安静的地方。</h1>
          <p class="onb-p read">
            Cortex 把讲义、PDF、录音和网页变成结构化的学习材料 —
            可信的速查表、闪卡，以及严格限定在你所选范围内的对话。
            五步即可完成，之后随时可以改。
          </p>
          <div class="onb-feats">
            <div class="onb-feat">
              <Icon name="book" size={15} color="var(--accent)" />
              <span>速查表做完整性检查 — 不会悄悄漏掉内容</span>
            </div>
            <div class="onb-feat">
              <Icon name="record" size={15} color="var(--accent)" />
              <span>内置讲座录音与实时转写</span>
            </div>
            <div class="onb-feat">
              <Icon name="cmd" size={15} color="var(--accent)" />
              <span>键盘优先，Helix 风格的模式化导航</span>
            </div>
          </div>
          <button class="btn btn--primary onb-cta" onclick={next}>
            开始使用 <Icon name="arrowR" size={14} />
          </button>
        </div>

      <!-- Step 1: BYOK -->
      {:else if step === 1}
        <div class="onb-pane">
          <div class="eyebrow">自带密钥</div>
          <h1 class="read onb-h">粘贴你的 Gemini API 密钥。</h1>
          <p class="onb-p read">
            Cortex is BYOK — your key stays on this machine, in the OS keychain. Nothing routes
            through our servers. You can add Claude, OpenAI or a local Ollama model later in Settings.
          </p>
          <!-- svelte-ignore a11y_label_has_associated_control -->
          <label class="onb-label mono">GEMINI 密钥</label>
          <input
            class="input"
            bind:value={apiKey}
            placeholder="AIza…"
            style="font-family:var(--font-mono)"
          />
          <div class="onb-row">
            <Icon name="diamond" size={11} color="var(--fg-faint)" />
            <span class="mono faint">存在系统钥匙串 · 永不同步</span>
          </div>
          <div class="onb-actions">
            <button class="btn btn--ghost" onclick={next}>稍后再加</button>
            <button class="btn btn--primary" onclick={next}>继续 <Icon name="arrowR" size={14} /></button>
          </div>
        </div>

      <!-- Step 2: Theme -->
      {:else if step === 2}
        <div class="onb-pane">
          <div class="eyebrow">外观</div>
          <h1 class="read onb-h">检测到你的 Omarchy 主题。</h1>
          <p class="onb-p read">
            Cortex 会跟着桌面主题实时换肤。检测到 <b class="accent">Osaka Jade</b> —
            可以直接用它，也可以另选一个。之后切换 Omarchy 主题时 Cortex 会自动更新。
          </p>
          <div class="onb-themes">
            {#each THEME_OPTS as t}
              <button
                class={"onb-theme" + (theme === t.id ? " on" : "")}
                onclick={() => pickTheme(t.id)}
                style="background:{t.b}"
              >
                <span class="ot-sw" style="background:{t.c}"></span>
                <span class="ot-name" style="color:{theme === t.id ? '#fff' : '#cbd'}">{t.n}</span>
                {#if theme === t.id}
                  <span class="ot-check" style="color:{t.c}">
                    <Icon name="check" size={13} />
                  </span>
                {/if}
              </button>
            {/each}
          </div>
          <div class="onb-actions">
            <span class="mono faint">来自 Omarchy 检测</span>
            <button class="btn btn--primary" onclick={next}>继续 <Icon name="arrowR" size={14} /></button>
          </div>
        </div>

      <!-- Step 3: Homelab -->
      {:else if step === 3}
        <div class="onb-pane">
          <div class="eyebrow">可选</div>
          <h1 class="read onb-h">有自己的 homelab 吗？</h1>
          <p class="onb-p read">
            可以把重活 — Whisper 转写、大模型生成、备份 — 交给局域网里的机器。
            不配置也完全可用，只是大任务会慢一些。
          </p>
          <button
            class={"onb-toggle" + (homelab ? " on" : "")}
            onclick={() => (homelab = !homelab)}
          >
            <span class="ot-knob"></span>
            <span class="mono">{homelab ? "Use homelab for heavy jobs" : "Run everything on this machine"}</span>
          </button>
          {#if homelab}
            <div class="onb-homelab">
              <!-- svelte-ignore a11y_label_has_associated_control -->
              <label class="onb-label mono">服务地址</label>
              <input
                class="input"
                bind:value={homelabEndpoint}
                placeholder="http://homelab.local:11434"
                style="font-family:var(--font-mono)"
              />
              <button class="btn btn--sm" style="margin-top:10px">
                <Icon name="refresh" size={12} /> Test connection
              </button>
            </div>
          {/if}
          <div class="onb-actions">
            <button class="btn btn--ghost" onclick={next}>跳过</button>
            <button class="btn btn--primary" onclick={next}>继续 <Icon name="arrowR" size={14} /></button>
          </div>
        </div>

      <!-- Step 4: First subject -->
      {:else if step === 4}
        <div class="onb-pane">
          <div class="eyebrow">最后一步</div>
          <h1 class="read onb-h">创建第一个科目。</h1>
          <p class="onb-p read">
            A subject holds topics, sources and one living cheatsheet. Name it after a course —
            you'll add lectures next.
          </p>
          <!-- svelte-ignore a11y_label_has_associated_control -->
          <label class="onb-label mono">科目名称</label>
          <!-- svelte-ignore a11y_autofocus -->
          <input
            class="input"
            bind:value={subjName}
            placeholder="例如：算法导论"
            autofocus
          />
          <div class="onb-suggest">
            {#each SUGGESTIONS as x}
              <button class="btn btn--sm btn--ghost" onclick={() => (subjName = x)}>{x}</button>
            {/each}
          </div>
          <button class="btn btn--primary onb-cta" onclick={finish}>
            <Icon name="check" size={14} /> Enter Cortex
          </button>
        </div>
      {/if}
    </div>

    <!-- Progress bar -->
    <div class="onb-progress">
      <div class="onb-progress-bar" style="width:{((step + 1) / steps.length) * 100}%"></div>
    </div>
  </div>
</div>
