<script lang="ts">
  // A2 出口工具箱：把 AI 结果写回真实 Office 文件。
  // - Word 审阅：选一个 .docx → AI 产出修订/批注建议 → 预览 → 另存为新文件，
  //   修订以真正的 Word tracked changes 呈现，在 Word 里接受/拒绝。
  // - PDF 工具：合并 / 拆分 / 水印 / AcroForm 表单读取与填写。
  // 原文件一律不被修改：所有写操作都落到新文件。
  import * as api from "../lib/api";
  import { app } from "../lib/store.svelte";
  import Icon from "./Icon.svelte";

  let open = $state(false);
  let tab = $state<"docx" | "xlsx" | "pdf">("docx");

  // ---- Word 审阅 ----
  let docxPath = $state("");
  let reviewNote = $state("");
  let reviewing = $state(false);
  let reviewErr = $state("");
  let edits = $state<api.DocxEdit[]>([]);
  let saving = $state(false);
  let saveMsg = $state("");

  async function pickDocx() {
    reviewErr = "";
    saveMsg = "";
    try {
      const { open: pick } = await import("@tauri-apps/plugin-dialog");
      const p = await pick({
        multiple: false,
        filters: [{ name: "Word 文档", extensions: ["docx"] }],
      });
      const path = Array.isArray(p) ? p[0] : p;
      if (path) {
        docxPath = path;
        edits = [];
      }
    } catch (e) {
      reviewErr = String(e);
    }
  }

  async function runReview() {
    if (!docxPath || reviewing) return;
    reviewing = true;
    reviewErr = "";
    edits = [];
    saveMsg = "";
    try {
      edits = await api.docxReviewEdits(docxPath, reviewNote.trim() || undefined);
      if (!edits.length) saveMsg = "AI 没有发现需要修订或批注的问题。";
    } catch (e) {
      reviewErr = String(e);
    } finally {
      reviewing = false;
    }
  }

  async function saveReviewed() {
    if (!docxPath || !edits.length || saving) return;
    saving = true;
    reviewErr = "";
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const stem = docxPath.replace(/\.docx$/i, "");
      const dest = await save({
        defaultPath: `${stem}.reviewed.docx`,
        filters: [{ name: "Word 文档", extensions: ["docx"] }],
      });
      if (!dest) return;
      const author = "AI 审阅";
      const r = await api.docxApplyEdits(docxPath, dest, author, edits);
      saveMsg = `已写入 ${r.applied} 处修订/批注${r.skipped.length ? `，${r.skipped.length} 处未匹配` : ""} → ${dest}`;
      app.pushToast({ kind: "success", title: "修订已写入", body: dest });
    } catch (e) {
      reviewErr = String(e);
    } finally {
      saving = false;
    }
  }

  // ---- PDF 工具 ----
  let pdfPath = $state("");
  let wmText = $state("");
  let pdfFields = $state<api.PdfField[]>([]);
  let pdfValues = $state<Record<string, string>>({});
  let pdfBusy = $state(false);
  let pdfMsg = $state("");
  let pdfErr = $state("");

  async function pickPdf() {
    pdfErr = "";
    pdfMsg = "";
    try {
      const { open: pick } = await import("@tauri-apps/plugin-dialog");
      const p = await pick({
        multiple: false,
        filters: [{ name: "PDF", extensions: ["pdf"] }],
      });
      const path = Array.isArray(p) ? p[0] : p;
      if (path) {
        pdfPath = path;
        pdfFields = [];
        pdfValues = {};
      }
    } catch (e) {
      pdfErr = String(e);
    }
  }

  async function doMerge() {
    pdfBusy = true;
    pdfErr = "";
    pdfMsg = "";
    try {
      const { open: pick, save } = await import("@tauri-apps/plugin-dialog");
      const picked = await pick({
        multiple: true,
        filters: [{ name: "PDF", extensions: ["pdf"] }],
      });
      const paths = Array.isArray(picked) ? picked : picked ? [picked] : [];
      if (paths.length < 2) {
        pdfErr = "请选择至少两个要合并的 PDF。";
        return;
      }
      const dest = await save({
        defaultPath: "merged.pdf",
        filters: [{ name: "PDF", extensions: ["pdf"] }],
      });
      if (!dest) return;
      await api.pdfMerge(paths, dest);
      pdfMsg = `已合并 ${paths.length} 个文件 → ${dest}`;
    } catch (e) {
      pdfErr = String(e);
    } finally {
      pdfBusy = false;
    }
  }

  async function doSplit() {
    if (!pdfPath) {
      pdfErr = "先在上方选择一个 PDF。";
      return;
    }
    pdfBusy = true;
    pdfErr = "";
    pdfMsg = "";
    try {
      const stem = pdfPath.replace(/\.pdf$/i, "");
      const out = await api.pdfSplit(pdfPath, stem, stem.split(/[\\/]/).pop() || "page");
      pdfMsg = `已拆分为 ${out.length} 个单页文件，输出在 ${stem}_001.pdf …`;
    } catch (e) {
      pdfErr = String(e);
    } finally {
      pdfBusy = false;
    }
  }

  async function doWatermark() {
    if (!pdfPath || !wmText.trim()) return;
    pdfBusy = true;
    pdfErr = "";
    pdfMsg = "";
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const stem = pdfPath.replace(/\.pdf$/i, "");
      const dest = await save({
        defaultPath: `${stem}.wm.pdf`,
        filters: [{ name: "PDF", extensions: ["pdf"] }],
      });
      if (!dest) return;
      await api.pdfWatermark(pdfPath, wmText.trim(), dest);
      pdfMsg = `水印已写入 → ${dest}`;
    } catch (e) {
      pdfErr = String(e);
    } finally {
      pdfBusy = false;
    }
  }

  async function readFields() {
    if (!pdfPath) {
      pdfErr = "先在上方选择一个 PDF。";
      return;
    }
    pdfBusy = true;
    pdfErr = "";
    pdfMsg = "";
    try {
      pdfFields = await api.pdfFormFields(pdfPath);
      pdfValues = {};
      if (!pdfFields.length) pdfMsg = "该 PDF 没有 AcroForm 表单字段。";
    } catch (e) {
      pdfErr = String(e);
    } finally {
      pdfBusy = false;
    }
  }

  async function doFill() {
    if (!pdfPath || !pdfFields.length) return;
    pdfBusy = true;
    pdfErr = "";
    pdfMsg = "";
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const stem = pdfPath.replace(/\.pdf$/i, "");
      const dest = await save({
        defaultPath: `${stem}.filled.pdf`,
        filters: [{ name: "PDF", extensions: ["pdf"] }],
      });
      if (!dest) return;
      const values = Object.entries(pdfValues).filter(([, v]) => v !== "");
      if (!values.length) {
        pdfErr = "没有填写任何字段。";
        return;
      }
      const n = await api.pdfFillForm(pdfPath, values as [string, string][], dest);
      pdfMsg = `已填写 ${n} 个字段 → ${dest}`;
    } catch (e) {
      pdfErr = String(e);
    } finally {
      pdfBusy = false;
    }
  }

  function fieldInput(kind: string, options: string[]) {
    if (kind === "checkbox" || kind === "radio" || kind === "choice") {
      const opts = options.length ? options : ["Yes"];
      return { opts: [...opts, "Off"], type: "select" as const };
    }
    return { opts: [], type: "text" as const };
  }

  // ---- 表格改单元格 ----
  // 已有的表格不能重新生成，所以只在副本上改，原文件一个字节都不动。
  let xlsxPath = $state("");
  let xlsxBusy = $state(false);
  let xlsxErr = $state("");
  let xlsxMsg = $state("");
  let xlsxSheets = $state<string[]>([]);
  let xlsxSheet = $state("");
  let xlsxRows = $state<string[][]>([]);
  let xlsxTruncated = $state(false);
  let xlsxEdits = $state<api.CellEdit[]>([]);
  let xlsxCell = $state("");
  let xlsxValue = $state("");
  let xlsxChanged = $state<api.CellChange[]>([]);

  /** 0 → A, 25 → Z, 26 → AA. */
  function colName(i: number): string {
    let out = "";
    let n = i + 1;
    while (n > 0) {
      const r = (n - 1) % 26;
      out = String.fromCharCode(65 + r) + out;
      n = Math.floor((n - 1) / 26);
    }
    return out;
  }

  async function pickXlsx() {
    xlsxErr = "";
    xlsxMsg = "";
    xlsxChanged = [];
    try {
      const { open: pick } = await import("@tauri-apps/plugin-dialog");
      const p = await pick({ multiple: false, filters: [{ name: "Excel 表格", extensions: ["xlsx"] }] });
      const path = Array.isArray(p) ? p[0] : p;
      if (!path) return;
      xlsxPath = path;
      xlsxEdits = [];
      await loadSheet();
    } catch (e) {
      xlsxErr = String(e);
    }
  }

  async function loadSheet(name?: string) {
    if (!xlsxPath) return;
    xlsxBusy = true;
    xlsxErr = "";
    try {
      const p = await api.xlsxPreview(xlsxPath, name, 40);
      xlsxSheets = p.sheets;
      xlsxSheet = p.sheet;
      xlsxRows = p.rows;
      xlsxTruncated = p.truncated;
    } catch (e) {
      xlsxErr = String(e);
    } finally {
      xlsxBusy = false;
    }
  }

  function addEdit() {
    const cell = xlsxCell.trim().toUpperCase();
    if (!cell) return;
    // 同一个单元格改两次没有意义，后来的覆盖前面的。
    xlsxEdits = [...xlsxEdits.filter((e) => e.cell !== cell), { sheet: xlsxSheet, cell, value: xlsxValue }];
    xlsxCell = "";
    xlsxValue = "";
  }

  async function saveXlsx() {
    if (!xlsxPath || !xlsxEdits.length || xlsxBusy) return;
    xlsxBusy = true;
    xlsxErr = "";
    xlsxMsg = "";
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const stem = xlsxPath.replace(/\.xlsx$/i, "");
      const dest = await save({
        defaultPath: `${stem}.已改.xlsx`,
        filters: [{ name: "Excel 表格", extensions: ["xlsx"] }],
      });
      if (!dest) return;
      const r = await api.xlsxSetCells(xlsxPath, dest, xlsxEdits);
      xlsxChanged = r.changed;
      xlsxMsg = `已改 ${r.changed.length} 个单元格${r.skipped.length ? `，${r.skipped.length} 处没改` : ""} → ${dest}`;
      app.pushToast({ kind: "success", title: "表格已更新", body: dest });
      if (r.changed.length) xlsxEdits = [];
      await loadSheet(xlsxSheet);
    } catch (e) {
      xlsxErr = String(e);
    } finally {
      xlsxBusy = false;
    }
  }
</script>

<button class="btn" onclick={() => (open = !open)}>文档工具</button>

{#if open}
  <div class="bg" onclick={() => (open = false)} role="presentation"></div>
  <div class="modal" role="dialog" aria-label="文档工具：AI 写回 Office">
    <div class="bd">
      <div class="mhead">
        <div class="mtitle">文档工具</div>
        <button class="x" title="关闭" onclick={() => (open = false)}><Icon name="x" size={12} /></button>
      </div>
      <div class="tabs">
        <button class="tab" class:on={tab === "docx"} onclick={() => (tab = "docx")}>Word 审阅回写</button>
        <button class="tab" class:on={tab === "xlsx"} onclick={() => (tab = "xlsx")}>表格改单元格</button>
        <button class="tab" class:on={tab === "pdf"} onclick={() => (tab = "pdf")}>PDF 工具</button>
      </div>

      {#if tab === "docx"}
        <p class="hint">选一个 .docx，AI 审阅后给出修订与批注；写入的是真正的 Word 修订模式（tracked changes），接受 / 拒绝都在 Word 里完成。原文件不会被改动。</p>
        <div class="frow">
          <button class="btn" onclick={pickDocx}>{docxPath ? "换一个文件" : "选择 .docx"}</button>
          <span class="fpath" title={docxPath}>{docxPath || "未选择文件"}</span>
        </div>
        <textarea bind:value={reviewNote} placeholder="审阅侧重点（可选）：例如「检查论证是否自洽」「挑出缺少数据支撑的句子」"></textarea>
        <div class="arow">
          <button class="btn primary" disabled={!docxPath || reviewing} onclick={runReview}>
            {reviewing ? "审阅中…" : "AI 审阅"}
          </button>
          {#if edits.length}
            <span class="count">{edits.length} 条建议</span>
            <button class="btn" disabled={saving} onclick={() => { edits = []; saveMsg = ""; }}>清空建议</button>
            <button class="btn primary" disabled={saving} onclick={saveReviewed}>
              {saving ? "写入中…" : "写入修订（另存新文件）"}
            </button>
          {/if}
        </div>
        {#if reviewErr}<div class="err">{reviewErr}</div>{/if}
        {#if saveMsg && !edits.length}<div class="msg">{saveMsg}</div>{/if}
        {#if saveMsg && edits.length}<div class="msg">{saveMsg}</div>{/if}
        {#each edits as ed, i (i)}
          <div class="edit">
            <div class="ehead">
              <span class="op">{ed.op === "comment" ? "批注" : "插入"}</span>
              <span class="anchor">锚点：{ed.anchor.slice(0, 60)}{ed.anchor.length > 60 ? "…" : ""}</span>
            </div>
            {#if ed.op === "comment"}
              <div class="etext">{ed.comment || ed.text}</div>
            {:else}
              <div class="etext">{ed.text}</div>
              {#if ed.comment}<div class="enote">附批注：{ed.comment}</div>{/if}
            {/if}
          </div>
        {/each}
      {:else if tab === "xlsx"}
        <p class="hint">点表格里的单元格 → 填新值 → 加入改动；数字按数字写、以 = 开头按公式写、留空即清除。写入的是副本，原表格不会被改动。</p>
        <div class="frow">
          <button class="btn" onclick={pickXlsx}>{xlsxPath ? "换一个文件" : "选择 .xlsx"}</button>
          <span class="fpath" title={xlsxPath}>{xlsxPath || "未选择文件"}</span>
        </div>
        {#if xlsxSheets.length > 1}
          <div class="arow">
            {#each xlsxSheets as sh (sh)}
              <button class="btn" class:on={sh === xlsxSheet} disabled={xlsxBusy} onclick={() => loadSheet(sh)}>{sh}</button>
            {/each}
          </div>
        {/if}
        {#if xlsxRows.length}
          <div class="gridwrap">
            <table class="grid">
              <tbody>
                {#each xlsxRows as row, ri (ri)}
                  <tr>
                    <th>{ri + 1}</th>
                    {#each row as c, ci (ci)}
                      <td
                        class:sel={xlsxCell === `${colName(ci)}${ri + 1}`}
                        onclick={() => (xlsxCell = `${colName(ci)}${ri + 1}`)}
                        title={`${colName(ci)}${ri + 1}`}>{c}</td>
                    {/each}
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
          {#if xlsxTruncated}<div class="count">只显示前 40 行（也可以直接手填地址）</div>{/if}
        {/if}
        <div class="frow">
          <input class="wm" bind:value={xlsxCell} placeholder="单元格，如 B4" />
          <input class="wm" bind:value={xlsxValue} placeholder="新值 / =SUM(B4:B6)" onkeydown={(e) => e.key === "Enter" && addEdit()} />
          <button class="btn" disabled={!xlsxCell.trim()} onclick={addEdit}>加入改动</button>
        </div>
        {#if xlsxEdits.length}
          <div class="arow">
            <span class="count">{xlsxEdits.length} 处待改</span>
            <button class="btn" disabled={xlsxBusy} onclick={() => (xlsxEdits = [])}>清空</button>
            <button class="btn primary" disabled={xlsxBusy} onclick={saveXlsx}>{xlsxBusy ? "写入中…" : "写入副本（另存新文件）"}</button>
          </div>
        {/if}
        {#if xlsxErr}<div class="err">{xlsxErr}</div>{/if}
        {#if xlsxMsg}<div class="msg">{xlsxMsg}</div>{/if}
        {#each xlsxEdits as e, i (e.cell)}
          <div class="edit">
            <div class="ehead">
              <span class="op">{e.sheet || xlsxSheet}</span>
              <span class="anchor">{e.cell}</span>
              <button class="x" title="移除" onclick={() => (xlsxEdits = xlsxEdits.filter((_, j) => j !== i))}><Icon name="x" size={10} /></button>
            </div>
            <div class="etext">{e.value === "" ? "（清除内容）" : e.value}</div>
          </div>
        {/each}
        {#each xlsxChanged as c, i (i)}
          <div class="edit">
            <div class="ehead">
              <span class="op">{c.sheet} · {c.cell}</span>
            </div>
            <div class="etext">{c.before || "（空）"} → {c.after || "（空）"}</div>
          </div>
        {/each}
      {:else}
        <p class="hint">合并 / 拆分 / 水印 / 表单填写都输出到新文件，原 PDF 不被改动。</p>
        <div class="frow">
          <button class="btn" onclick={pickPdf}>{pdfPath ? "换一个文件" : "选择 .pdf"}</button>
          <span class="fpath" title={pdfPath}>{pdfPath || "未选择文件"}</span>
        </div>
        <div class="pgrid">
          <button class="btn" disabled={pdfBusy} onclick={doMerge}>合并多个 PDF</button>
          <button class="btn" disabled={!pdfPath || pdfBusy} onclick={doSplit}>拆分为单页</button>
        </div>
        <div class="frow">
          <input class="wm" bind:value={wmText} placeholder="水印文字" />
          <button class="btn" disabled={!pdfPath || !wmText.trim() || pdfBusy} onclick={doWatermark}>加水印</button>
        </div>
        <div class="arow">
          <button class="btn" disabled={!pdfPath || pdfBusy} onclick={readFields}>
            {pdfBusy && !pdfFields.length ? "读取中…" : "读取表单字段"}
          </button>
          {#if pdfFields.length}
            <span class="count">{pdfFields.length} 个字段</span>
            <button class="btn primary" disabled={pdfBusy} onclick={doFill}>填写并另存</button>
          {/if}
        </div>
        {#if pdfErr}<div class="err">{pdfErr}</div>{/if}
        {#if pdfMsg}<div class="msg">{pdfMsg}</div>{/if}
        {#each pdfFields as f (f.name)}
          <div class="field">
            <span class="fname" title={f.name}>{f.name}<span class="fkind">{f.kind}</span></span>
            {#if fieldInput(f.kind, f.options).type === "select"}
              <select bind:value={pdfValues[f.name]}>
                <option value="">（不填）</option>
                {#each fieldInput(f.kind, f.options).opts as o (o)}
                  <option value={o}>{o}</option>
                {/each}
              </select>
            {:else}
              <input bind:value={pdfValues[f.name]} placeholder={f.value || ""} />
            {/if}
          </div>
        {/each}
      {/if}
    </div>
  </div>
{/if}

<style>
  .gridwrap {
    max-height: 260px;
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .grid {
    border-collapse: collapse;
    font-size: var(--t-xs);
    color: var(--fg);
  }
  .grid th {
    position: sticky;
    left: 0;
    background: var(--surface-2, rgba(255, 255, 255, 0.04));
    color: var(--fg-faint);
    font-weight: 400;
    padding: 2px 6px;
    border-right: 1px solid var(--border);
  }
  .grid td {
    padding: 2px 8px;
    border-right: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
    white-space: nowrap;
    cursor: pointer;
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .grid td.sel {
    outline: 1px solid var(--accent);
    background: rgba(124, 92, 240, 0.14);
  }
  .btn.on {
    border-color: var(--accent);
    color: var(--fg-bright);
  }
  .bg {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    z-index: 90;
  }
  .modal {
    position: fixed;
    inset: 6vh 10vw;
    z-index: 91;
    background: var(--surface-1, rgba(20, 20, 28, 0.96));
    border-radius: var(--rad-4, 10px);
    border: 1px solid var(--line-2, rgba(255, 255, 255, 0.1));
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.5);
    overflow: auto;
  }
  .bd {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 18px 20px 24px;
  }
  .mhead {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .mtitle {
    font-size: var(--t-lg);
    font-weight: 500;
    color: var(--fg-bright);
  }
  .x {
    background: none;
    border: none;
    color: var(--fg-muted);
    cursor: pointer;
    padding: 4px;
  }
  .tabs {
    display: flex;
    gap: 6px;
  }
  .tab {
    background: none;
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--fg-muted);
    font-size: var(--t-xs);
    padding: 5px 10px;
    cursor: pointer;
  }
  .tab.on {
    color: var(--fg-bright);
    border-color: var(--accent);
    background: rgba(124, 92, 240, 0.1);
  }
  .hint {
    margin: 0;
    font-size: var(--t-xs);
    color: var(--fg-muted);
    line-height: 1.6;
  }
  .frow {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .fpath {
    flex: 1;
    min-width: 0;
    font-size: 11px;
    color: var(--fg-faint);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  textarea {
    width: 100%;
    min-height: 44px;
    resize: vertical;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 7px;
    color: var(--fg);
    font-size: var(--t-xs);
    line-height: 1.6;
    padding: 8px;
    outline: none;
    box-sizing: border-box;
  }
  .wm {
    flex: 1;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 7px;
    color: var(--fg);
    font-size: var(--t-xs);
    padding: 6px 9px;
    outline: none;
  }
  .arow {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .pgrid {
    display: flex;
    gap: 8px;
  }
  .count {
    font-size: 11px;
    color: var(--accent);
    white-space: nowrap;
  }
  .err {
    font-size: var(--t-xs);
    color: #fca5a5;
  }
  .msg {
    font-size: var(--t-xs);
    color: var(--ok, #a3be8c);
    word-break: break-all;
  }
  .edit {
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface);
    padding: 9px 12px;
  }
  .ehead {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 5px;
  }
  .op {
    font-size: 10px;
    color: var(--accent);
    background: rgba(124, 92, 240, 0.12);
    border-radius: 4px;
    padding: 1px 6px;
    white-space: nowrap;
  }
  .anchor {
    flex: 1;
    min-width: 0;
    font-size: 11px;
    color: var(--fg-faint);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .etext {
    font-size: var(--t-sm);
    line-height: 1.6;
    color: var(--fg);
  }
  .enote {
    margin-top: 4px;
    font-size: 11px;
    color: var(--fg-muted);
    font-style: italic;
  }
  .field {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .fname {
    flex: 0 0 40%;
    min-width: 0;
    font-size: 11px;
    color: var(--fg);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .fkind {
    margin-left: 6px;
    font-size: 10px;
    color: var(--fg-faint);
  }
  .field input,
  .field select {
    flex: 1;
    min-width: 0;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--fg);
    font-size: var(--t-xs);
    padding: 5px 8px;
    outline: none;
  }
</style>
