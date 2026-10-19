<script lang="ts">
  import Icon from "./Icon.svelte";
  import { convertFileSrc } from "@tauri-apps/api/core";
  import * as api from "../lib/api";
  import { app } from "../lib/store.svelte";

  export interface Slide {
    title: string;
    bullets: string[];
    notes?: string;
  }

  let {
    slides,
    title = "Slideshow",
    materialId,
    onExit,
  }: { slides: Slide[]; title?: string; materialId?: string; onExit?: () => void } = $props();

  let idx = $state(0);
  let showNotes = $state(false);

  // ── Video overview (D1) ────────────────────────────────────────────────
  // The slides get rasterized HERE on a canvas (the webview has the CJK fonts
  // and the theme palette; a Rust text rasterizer would need a font stack for
  // one feature), then the backend does TTS + ffmpeg muxing. First run only
  // downloads ffmpeg; afterwards it is cached in <data>/bin/.
  let videoBusy = $state(false);
  let videoSrc = $state<string | null>(null);
  let videoErr = $state<string | null>(null);
  let videoNote = $state<string | null>(null);

  function cssVar(name: string, fallback: string): string {
    const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
    return v || fallback;
  }

  function wrapText(
    ctx: CanvasRenderingContext2D,
    text: string,
    x: number,
    y: number,
    maxW: number,
    lh: number,
    maxLines: number,
  ): number {
    // Greedy wrap honoring CJK: break anywhere when a word would overshoot.
    let line = "";
    let cy = y;
    let lines = 0;
    for (const ch of text) {
      if (ctx.measureText(line + ch).width > maxW && line) {
        ctx.fillText(line, x, cy);
        cy += lh;
        lines += 1;
        line = ch;
        if (lines >= maxLines - 1) {
          ctx.fillText(line + "…", x, cy);
          return cy + lh;
        }
      } else {
        line += ch;
      }
    }
    if (line) {
      ctx.fillText(line, x, cy);
      cy += lh;
    }
    return cy;
  }

  async function renderSlidePng(s: Slide, i: number): Promise<string> {
    const W = 1280;
    const H = 720;
    const cv = document.createElement("canvas");
    cv.width = W;
    cv.height = H;
    const ctx = cv.getContext("2d");
    if (!ctx) throw new Error("canvas 2d 不可用");
    const surface = cssVar("--surface", "#171d1a");
    ctx.fillStyle = surface;
    ctx.fillRect(0, 0, W, H);
    // Accent top rule + footer band, matching the in-app slide chrome.
    ctx.fillStyle = cssVar("--accent", "#2dd5b7");
    ctx.fillRect(0, 0, W, 8);
    ctx.fillStyle = cssVar("--border", "#2a3530");
    ctx.fillRect(0, H - 4, W, 4);
    // Slide number (top right)
    ctx.fillStyle = cssVar("--fg-faint", "#5f6f68");
    ctx.font = "500 26px ui-monospace, 'Cascadia Code', Menlo, monospace";
    ctx.textAlign = "right";
    ctx.fillText(`${i + 1} / ${slides.length}`, W - 44, 64);
    ctx.textAlign = "left";
    // Title (wrapped, max 3 lines)
    ctx.fillStyle = cssVar("--fg-bright", "#e8f1ec");
    ctx.font = "600 58px -apple-system, 'Segoe UI', 'Microsoft YaHei', 'PingFang SC', sans-serif";
    const titleEnd = wrapText(ctx, s.title || "", 90, 168, W - 180, 74, 3);
    // Bullets
    ctx.font = "36px -apple-system, 'Segoe UI', 'Microsoft YaHei', 'PingFang SC', sans-serif";
    ctx.fillStyle = cssVar("--fg", "#d7e0db");
    let y = Math.max(titleEnd + 36, 300);
    for (const b of s.bullets.slice(0, 7)) {
      ctx.fillStyle = cssVar("--accent", "#2dd5b7");
      ctx.beginPath();
      ctx.arc(104, y - 12, 7, 0, Math.PI * 2);
      ctx.fill();
      ctx.fillStyle = cssVar("--fg", "#d7e0db");
      y = wrapText(ctx, b, 130, y, W - 230, 50, 2) + 18;
      if (y > H - 110) break;
    }
    // Footer: material title
    ctx.fillStyle = cssVar("--fg-faint", "#5f6f68");
    ctx.font = "500 24px ui-monospace, 'Cascadia Code', Menlo, monospace";
    ctx.fillText(title.slice(0, 60), 90, H - 44);
    return cv.toDataURL("image/png");
  }

  async function generateVideo() {
    if (!materialId || videoBusy || slides.length === 0) return;
    videoBusy = true;
    videoErr = null;
    videoNote = null;
    try {
      const inputs: api.VideoSlideInput[] = [];
      for (let i = 0; i < slides.length; i++) {
        const png = await renderSlidePng(slides[i], i);
        inputs.push({ png, title: slides[i].title, bullets: slides[i].bullets, notes: slides[i].notes ?? "" });
      }
      const res = await api.synthesizeVideoOverview(materialId, inputs, false);
      videoSrc = convertFileSrc(res.path);
      videoNote = res.ffmpeg_fetched
        ? "首次运行下载了 ffmpeg（约 80MB，已缓存，之后不再下载）。"
        : null;
    } catch (e) {
      videoErr = String(e instanceof Error ? e.message : e);
    } finally {
      videoBusy = false;
    }
  }

  const current = $derived(slides[idx] ?? null);
  const hasPrev = $derived(idx > 0);
  const hasNext = $derived(idx < slides.length - 1);
  const hasNotes = $derived(!!current?.notes);

  // ── Single-slide regeneration (2.7) ──────────────────────────────────
  // Deck-aware rewrite of the CURRENT slide, patched into the material
  // payload in place; the local array is mutated so the stage updates live.
  let regenBusy = $state(false);
  async function regenerateCurrent() {
    if (!materialId || regenBusy || !current) return;
    const instruction = await app.prompt({
      title: `重写第 ${idx + 1} 页`,
      label: "指令（可选）",
      placeholder: "留空 = 自动润色；或写「加一个例子」「更精炼」…",
    });
    if (instruction === null) return; // cancelled
    regenBusy = true;
    try {
      const payload = await api.regenerateSlide(materialId, idx, instruction.trim() || null);
      const arr = (payload as { slides?: Slide[] }).slides;
      if (Array.isArray(arr) && arr[idx]) slides[idx] = arr[idx];
      app.pushToast({ kind: "success", title: `第 ${idx + 1} 页已重写`, body: "其余页面保持不变。" });
    } catch (e) {
      app.pushToast({ kind: "error", title: "重写失败", body: String(e) });
    } finally {
      regenBusy = false;
    }
  }

  function prev() { if (hasPrev) { idx -= 1; } }
  function next() { if (hasNext) { idx += 1; } }

  $effect(() => {
    function onKey(e: KeyboardEvent) {
      const el = document.activeElement as HTMLElement | null;
      if (el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA")) return;
      if (e.key === "ArrowRight" || e.key === "ArrowDown") { e.preventDefault(); next(); }
      else if (e.key === "ArrowLeft" || e.key === "ArrowUp") { e.preventDefault(); prev(); }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });
</script>

<div class="ss-wrap">
  <!-- ── Header bar ── -->
  <div class="ss-bar">
    {#if onExit}
      <button class="btn btn--icon btn--sm btn--ghost" onclick={onExit} title="Back to materials">
        <span style="display:inline-flex;transform:rotate(180deg)"><Icon name="chevron" size={14} /></span>
      </button>
    {/if}
    <span class="eyebrow" style="flex:1">{title}</span>
    {#if materialId && slides.length > 0}
      <button
        class="btn btn--sm btn--ghost"
        disabled={regenBusy}
        onclick={regenerateCurrent}
        title="AI 重写当前页（可给指令；会考虑前后页衔接，其余页不动）"
      >
        <Icon name="refresh" size={12} /> {regenBusy ? "重写中…" : "重写本页"}
      </button>
    {/if}
    {#if materialId && slides.length > 0}
      <button
        class="btn btn--sm btn--primary"
        disabled={videoBusy}
        onclick={generateVideo}
        title="渲染幻灯片 + 旁白合成为视频（首次运行会下载 ffmpeg）"
      >
        <Icon name="play" size={12} /> {videoBusy ? "视频合成中…" : "生成视频"}
      </button>
    {/if}
    {#if hasNotes}
      <button
        class="btn btn--sm btn--ghost{showNotes ? ' on' : ''}"
        onclick={() => (showNotes = !showNotes)}
        title="Toggle speaker notes"
      >
        <Icon name="grid" size={12} /> Notes
      </button>
    {/if}
    <span class="mono faint" style="font-size: var(--t-xs);">{idx + 1} / {slides.length}</span>
  </div>

  {#if videoErr}
    <div class="ss-notes" style="border-color:var(--err);margin:12px 16px 0;">
      <div class="ss-notes-h mono" style="color:var(--err)">视频生成失败</div>
      <p class="ss-notes-body">{videoErr}</p>
    </div>
  {/if}

  {#if slides.length === 0}
    <div class="ss-empty">
      <Icon name="play" size={26} color="var(--fg-faint)" />
      <p class="mono faint" style="margin-top: 12px;">没有可用的幻灯片。</p>
    </div>
  {:else if videoSrc}
    <!-- Generated video replaces the stage until the user backs out -->
    <div class="ss-stage">
      <!-- svelte-ignore a11y_media_has_caption -->
      <video src={videoSrc} controls style="width:100%;max-width:1100px;border-radius:var(--rad-4);border:1px solid var(--border-strong);background:#000;margin:0 auto;"></video>
      {#if videoNote}
        <p class="mono faint" style="font-size: var(--t-xs); margin: 0;">{videoNote}</p>
      {/if}
      <button class="btn btn--ghost" style="align-self:center" onclick={() => (videoSrc = null)}>
        返回幻灯片
      </button>
    </div>
  {:else if current}
    <!-- ── Slide content ── -->
    <div class="ss-stage">
      <div class="ss-slide">
        <div class="ss-slide-n mono">{idx + 1}</div>
        <h2 class="ss-slide-title read">{current.title}</h2>
        {#if current.bullets.length > 0}
          <ul class="ss-bullets">
            {#each current.bullets as bullet (bullet)}
              <li class="ss-bullet read">{bullet}</li>
            {/each}
          </ul>
        {/if}
      </div>

      {#if showNotes && current.notes}
        <div class="ss-notes">
          <div class="ss-notes-h mono">演讲者备注</div>
          <p class="ss-notes-body read">{current.notes}</p>
        </div>
      {/if}
    </div>

    <!-- ── Navigation ── -->
    <div class="ss-nav">
      <button
        class="btn btn--ghost"
        onclick={prev}
        disabled={!hasPrev}
      >
        <span style="display:inline-flex;transform:rotate(180deg)"><Icon name="chevron" size={13} /></span>
        Prev
      </button>

      <!-- Slide pips -->
      <div class="ss-pips">
        {#each slides as _, n (n)}
          <button
            class="ss-pip{n === idx ? ' on' : ''}"
            onclick={() => (idx = n)}
            title="Slide {n + 1}"
          ></button>
        {/each}
      </div>

      <button
        class="btn btn--ghost"
        onclick={next}
        disabled={!hasNext}
      >
        Next
        <Icon name="chevron" size={13} />
      </button>
    </div>
  {/if}
</div>

<style>
  .ss-wrap {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow: hidden;
  }

  .ss-bar {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 48px;
    padding: 0 16px;
    border-bottom: 1px solid var(--border);
    flex: none;
  }

  .ss-bar .btn.on {
    background: color-mix(in oklab, var(--accent) 10%, var(--surface));
    border-color: var(--accent);
    color: var(--accent);
  }

  .ss-empty {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
  }

  .ss-stage {
    flex: 1;
    display: flex;
    flex-direction: column;
    overflow-y: auto;
    min-height: 0;
    padding: var(--sp-8) var(--sp-10);
    gap: var(--sp-6);
  }

  .ss-slide {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--sp-6);
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--rad-4);
    padding: var(--sp-10);
    min-height: 340px;
    position: relative;
    box-shadow: var(--shadow-2);
  }

  .ss-slide-n {
    position: absolute;
    top: 14px;
    right: 16px;
    font-size: var(--t-2xs);
    color: var(--fg-faint);
  }

  .ss-slide-title {
    font-size: var(--r-xl);
    font-weight: 600;
    color: var(--fg-bright);
    text-align: center;
    max-width: 640px;
    margin: 0;
  }

  .ss-bullets {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    max-width: 600px;
    width: 100%;
  }

  .ss-bullet {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    font-size: var(--r-md);
    color: var(--fg);
    line-height: 1.45;
  }

  .ss-bullet::before {
    content: "·";
    color: var(--accent);
    font-weight: 700;
    flex: none;
    margin-top: 1px;
  }

  .ss-notes {
    background: var(--bg-sunken);
    border: 1px solid var(--border);
    border-radius: var(--rad-3);
    padding: var(--sp-4) var(--sp-5);
  }

  .ss-notes-h {
    font-size: var(--t-2xs);
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--fg-faint);
    margin-bottom: var(--sp-2);
  }

  .ss-notes-body {
    font-size: var(--r-sm);
    color: var(--fg-muted);
    line-height: 1.55;
    margin: 0;
  }

  .ss-nav {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 20px;
    border-top: 1px solid var(--border);
    flex: none;
    background: var(--bg-sunken);
  }

  .ss-pips {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
    justify-content: center;
    max-width: 400px;
  }

  .ss-pip {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    border: none;
    background: var(--surface-3);
    cursor: pointer;
    padding: 0;
    transition: background var(--dur-fast), transform var(--dur-fast);
  }

  .ss-pip:hover { background: var(--fg-faint); transform: scale(1.2); }
  .ss-pip.on { background: var(--accent); transform: scale(1.25); }
</style>
