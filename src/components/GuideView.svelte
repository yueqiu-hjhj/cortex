<script lang="ts">
  import Icon from "./Icon.svelte";

  interface GuideSection { heading?: string; points?: string[] }
  interface SelfTest { q?: string; a?: string }
  interface GuideData {
    overview?: string;
    sections?: GuideSection[];
    pitfalls?: string[];
    selftest?: SelfTest[];
  }

  let { data, title, onExit }: { data?: GuideData; title?: string; onExit?: () => void } = $props();

  const overview = $derived(typeof data?.overview === "string" ? data!.overview!.trim() : "");
  const sections = $derived(
    (Array.isArray(data?.sections) ? data!.sections! : []).filter(
      (s) => s && (s.heading || (Array.isArray(s.points) && s.points.length))
    )
  );
  const pitfalls = $derived(
    (Array.isArray(data?.pitfalls) ? data!.pitfalls! : []).filter((p) => typeof p === "string" && p.trim())
  );
  const selftest = $derived(
    (Array.isArray(data?.selftest) ? data!.selftest! : []).filter((t) => t && (t.q || t.a))
  );
  const hasGuide = $derived(!!overview || sections.length > 0 || pitfalls.length > 0 || selftest.length > 0);
</script>

<div class="workspace-scroll">
  <div class="guide-page">
    {#if onExit}
      <div class="guide-toolbar">
        <button class="btn btn--icon btn--sm btn--ghost" onclick={onExit} title="返回学习材料">
          <span style="display:inline-flex;transform:rotate(180deg)"><Icon name="chevron" size={14} /></span>
        </button>
        <span class="guide-title">{title ?? "学习指南"}</span>
        {#if selftest.length}
          <span class="guide-dim">{selftest.length} 道自测</span>
        {/if}
      </div>
    {/if}

    {#if hasGuide}
      <article class="guide-body">
        {#if overview}
          <p class="guide-overview">{overview}</p>
        {/if}

        {#each sections as sec, si (si)}
          <section class="guide-section">
            {#if sec.heading}<h2>{sec.heading}</h2>{/if}
            {#if Array.isArray(sec.points) && sec.points.length}
              <ul>
                {#each sec.points as pt, pi (pi)}
                  <li>{pt}</li>
                {/each}
              </ul>
            {/if}
          </section>
        {/each}

        {#if pitfalls.length}
          <section class="guide-section">
            <h2>常见易错点</h2>
            <ul class="guide-pitfalls">
              {#each pitfalls as pit, pi (pi)}
                <li>{pit}</li>
              {/each}
            </ul>
          </section>
        {/if}

        {#if selftest.length}
          <section class="guide-section">
            <h2>自测</h2>
            <div class="guide-selftest">
              {#each selftest as t, ti (ti)}
                <details class="guide-quiz">
                  <summary><span class="guide-num">{ti + 1}</span>{t.q}</summary>
                  {#if t.a}<p class="guide-answer">{t.a}</p>{/if}
                </details>
              {/each}
            </div>
          </section>
        {/if}
      </article>
    {:else}
      <div class="guide-empty">
        <p>这份学习指南是空的。</p>
        <p class="guide-dim">重新生成这份材料即可填充。</p>
      </div>
    {/if}
  </div>
</div>

<style>
  .guide-page {
    max-width: 760px;
    margin: 0 auto;
    padding: 20px 24px 48px;
  }
  .guide-toolbar {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 16px;
  }
  .guide-title {
    font-weight: 600;
    font-size: 15px;
  }
  .guide-dim {
    color: var(--fg-muted);
    font-size: 12px;
  }
  .guide-body {
    font-size: 14px;
    line-height: 1.65;
  }
  .guide-overview {
    font-size: 15px;
    color: var(--fg);
    border-left: 3px solid var(--accent);
    padding: 4px 0 4px 14px;
    margin: 0 0 24px;
  }
  .guide-section {
    margin-bottom: 24px;
  }
  .guide-section h2 {
    font-size: 13px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--fg-muted);
    margin: 0 0 8px;
  }
  .guide-section ul {
    margin: 0;
    padding-left: 20px;
  }
  .guide-section li {
    margin: 4px 0;
  }
  .guide-pitfalls li {
    color: var(--warn, #d97706);
  }
  .guide-selftest {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .guide-quiz {
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 10px 14px;
  }
  .guide-quiz summary {
    cursor: pointer;
    font-weight: 500;
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .guide-num {
    color: var(--accent);
    font-size: 12px;
    font-weight: 600;
    min-width: 18px;
  }
  .guide-answer {
    margin: 8px 0 0 26px;
    color: var(--fg-muted);
    border-top: 1px dashed var(--border);
    padding-top: 8px;
  }
  .guide-empty {
    padding: 40px 0;
    text-align: center;
  }
  .guide-empty p {
    margin: 4px 0;
  }
</style>
