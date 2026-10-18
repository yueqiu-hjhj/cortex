// Popup: pick a subject, optionally trim to the selection, save.
//
// Order matters: health first (the whole panel is useless when Cortex is
// closed, and the user must hear that BEFORE clicking Save), then the subject
// list, then the page + selection preview.

import { apiBase, getJson, hostOf, isClipable, readSelection, readSettings, saveClip } from "./shared.js";

const $ = (id) => document.getElementById(id);
const banner = $("banner");
const form = $("form");
const subjectSel = $("subject");
const selBox = $("selection");
const result = $("result");
const savePage = $("save-page");
const saveSelection = $("save-selection");

let base = apiBase(7878);
let tab = null;

const showBanner = (text, kind = "warn") => {
  banner.textContent = text;
  banner.className = `banner ${kind}`;
  banner.hidden = false;
};

const showResult = (text, kind = "ok") => {
  result.textContent = text;
  result.className = `result ${kind}`;
  result.hidden = false;
};

async function init() {
  const cfg = await readSettings();
  base = apiBase(cfg.port);
  const [active] = await chrome.tabs.query({ active: true, currentWindow: true });
  tab = active || null;

  try {
    await getJson(base, "/health");
  } catch (e) {
    showBanner(`${e.message}。启动 Cortex 之后回来即可。`, "bad");
    return;
  }

  if (!tab || !isClipable(tab.url)) {
    showBanner("这个页面不能被剪藏（只支持 http/https 页面）。", "warn");
    return;
  }

  $("page-title").textContent = tab.title || "(无标题)";
  $("page-url").textContent = tab.url;
  $("page-host").textContent = hostOf(tab.url);

  try {
    const data = await getJson(base, "/subjects");
    for (const s of data.subjects || []) {
      const opt = document.createElement("option");
      opt.value = s.id;
      opt.textContent = `${s.glyph || ""} ${s.name}`.trim();
      subjectSel.appendChild(opt);
    }
    const preferred = cfg.subjectId || data.default_subject || "";
    if (preferred) subjectSel.value = preferred;
  } catch (e) {
    showBanner(`读不到学科列表：${e.message}`, "warn");
  }

  const preselected = await readSelection(tab.id);
  if (preselected) {
    selBox.value = preselected;
    selBox.classList.add("has-sel");
  }
  if (cfg.selectionOnly && !preselected) {
    showResult("默认「只存划选」，但这个页面上还没有选中的文字。", "bad");
  }

  form.hidden = false;
  savePage.addEventListener("click", () => doSave(false));
  saveSelection.addEventListener("click", () => doSave(true));
  $("open-options").addEventListener("click", () => chrome.runtime.openOptionsPage());
}

async function doSave(selectionOnly) {
  const selection = selBox.value.trim();
  if (selectionOnly && !selection) {
    showResult("要先在页面上选中一段文字，或者改存整页。", "bad");
    return;
  }

  for (const b of [savePage, saveSelection]) b.disabled = true;
  showResult("正在抓取页面并存入 Cortex…");
  try {
    const data = await saveClip(base, {
      url: tab.url,
      title: tab.title || null,
      selection: selection || null,
      selection_only: selectionOnly,
      subject_id: subjectSel.value || null,
    });
    const name = data.title || "已保存";
    showResult(data.warning ? `已保存：${name}\n（${data.warning}）` : `已保存：${name}`, "ok");
  } catch (e) {
    showResult(`保存失败：${(e && e.message) || e}`, "bad");
  } finally {
    for (const b of [savePage, saveSelection]) b.disabled = false;
  }
}

init();
