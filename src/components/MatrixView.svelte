<script lang="ts">
  import Icon from "./Icon.svelte";

  interface MatrixRow { label?: string; cells?: string[] }
  interface MatrixData { columns?: string[]; rows?: MatrixRow[] }

  let { data, title, onExit }: { data?: MatrixData; title?: string; onExit?: () => void } = $props();

  const columns = $derived(Array.isArray(data?.columns) ? data!.columns!.filter(Boolean) : []);
  const rows = $derived(
    (Array.isArray(data?.rows) ? data!.rows! : []).filter(
      (r) => r && (r.label || (Array.isArray(r.cells) && r.cells.length))
    )
  );
  const hasMatrix = $derived(columns.length > 0 && rows.length > 0);
</script>

<div class="workspace-scroll">
  <div class="matrix-page">
    {#if onExit}
      <div class="matrix-toolbar">
        <button class="btn btn--icon btn--sm btn--ghost" onclick={onExit} title="返回学习材料">
          <span style="display:inline-flex;transform:rotate(180deg)"><Icon name="chevron" size={14} /></span>
        </button>
        <span class="matrix-title">{title ?? "对比矩阵"}</span>
        <span class="matrix-dim">{rows.length} 行 × {columns.length} 列</span>
      </div>
    {/if}

    {#if hasMatrix}
      <div class="matrix-scroll">
        <table class="matrix-table">
          <thead>
            <tr>
              <th class="matrix-corner"></th>
              {#each columns as col}
                <th scope="col">{col}</th>
              {/each}
            </tr>
          </thead>
          <tbody>
            {#each rows as row, ri (ri)}
              <tr>
                <th scope="row">{row.label}</th>
                {#each columns as col, ci (ci)}
                  {@const cell = row.cells?.[ci]?.trim()}
                  {@const missing = !cell || cell === "—"}
                  <td class:matrix-missing={missing}>{cell || "—"}</td>
                {/each}
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {:else}
      <div class="matrix-empty">
        <p>这个矩阵是空的。</p>
        <p class="matrix-dim">重新生成这份材料即可填充。</p>
      </div>
    {/if}
  </div>
</div>

<style>
  .matrix-page {
    max-width: 1100px;
    margin: 0 auto;
    padding: 20px 24px 48px;
  }
  .matrix-toolbar {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 16px;
  }
  .matrix-title {
    font-weight: 600;
    font-size: 15px;
  }
  .matrix-dim {
    color: var(--fg-muted);
    font-size: 12px;
  }
  .matrix-scroll {
    overflow-x: auto;
    border: 1px solid var(--border);
    border-radius: 10px;
    background: var(--surface);
  }
  .matrix-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
    line-height: 1.5;
  }
  .matrix-table th,
  .matrix-table td {
    padding: 10px 14px;
    text-align: left;
    border-bottom: 1px solid var(--border);
    vertical-align: top;
  }
  .matrix-table thead th {
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--fg-muted);
    background: color-mix(in srgb, var(--border) 35%, transparent);
    position: sticky;
    top: 0;
  }
  .matrix-table tbody tr:last-child th,
  .matrix-table tbody tr:last-child td {
    border-bottom: none;
  }
  .matrix-table tbody th[scope="row"] {
    font-weight: 600;
    white-space: nowrap;
    min-width: 140px;
    background: color-mix(in srgb, var(--border) 18%, transparent);
  }
  .matrix-missing {
    color: var(--fg-muted);
    font-style: italic;
  }
  .matrix-empty {
    padding: 40px 0;
    text-align: center;
  }
  .matrix-empty p {
    margin: 4px 0;
  }
</style>
