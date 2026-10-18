// Cortex Clipper — background service worker.
//
// The toolbar popup owns the main "pick a subject and save" flow. This worker
// only owns the right-click menu, so a clip is one gesture away:
//   • "Clip to Cortex"            → the whole page
//   • "Clip selection to Cortex"  → just the highlighted text
//
// Feedback is a badge + tooltip on the toolbar icon: context-menu clips never
// open a window, so there is nothing else to talk to.

const MENU_PAGE = "cortex-clip-page";
const MENU_SELECTION = "cortex-clip-selection";
const DEFAULT_PORT = 7878;
const DEFAULT_TITLE = "Clip to Cortex";

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({
      id: MENU_PAGE,
      title: "Clip to Cortex",
      contexts: ["page", "frame", "link", "image"],
    });
    chrome.contextMenus.create({
      id: MENU_SELECTION,
      title: "Clip selection to Cortex",
      contexts: ["selection"],
    });
  });
});

chrome.contextMenus.onClicked.addListener((info, tab) => {
  if (!tab || tab.id == null) return;
  const selectionOnly = info.menuItemId === MENU_SELECTION;
  clip(tab, selectionOnly).then(
    (data) => flash(tab.id, "\u2713", "#3fb950", data.warning ? `已保存（${data.warning}）` : `已保存：${data.title || "页面"}`),
    (err) => flash(tab.id, "!", "#f85149", `失败：${message(err)}`)
  );
});

const message = (e) => String((e && e.message) || e || "unknown error");

async function settings() {
  const stored = await chrome.storage.sync.get({ port: DEFAULT_PORT, subjectId: "" });
  const n = Number.parseInt(String(stored.port), 10);
  return {
    base: `http://127.0.0.1:${Number.isFinite(n) && n > 0 && n < 65536 ? n : DEFAULT_PORT}`,
    subjectId: String(stored.subjectId || ""),
  };
}

async function selectionText(tabId) {
  try {
    const [{ result }] = await chrome.scripting.executeScript({
      target: { tabId },
      func: () => String(window.getSelection() || ""),
    });
    return String(result || "").trim();
  } catch {
    return "";
  }
}

async function clip(tab, selectionOnly) {
  if (!/^https?:/i.test(String(tab.url || ""))) {
    throw new Error("只支持 http/https 页面");
  }
  const { base, subjectId } = await settings();
  const selection = selectionOnly ? await selectionText(tab.id) : "";
  if (selectionOnly && !selection) throw new Error("这个页面上没有选中的文字");

  let res;
  try {
    res = await fetch(`${base}/clip`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        url: tab.url,
        title: tab.title || null,
        selection: selection || null,
        selection_only: selectionOnly,
        subject_id: subjectId || null,
      }),
    });
  } catch {
    throw new Error("连不上 Cortex 桌面端");
  }
  const data = await res.json().catch(() => ({}));
  if (!res.ok || data.ok === false) throw new Error(data.error || `HTTP ${res.status}`);
  return data;
}

/** Badge + tooltip feedback that clears itself. */
function flash(tabId, text, color, detail) {
  chrome.action.setBadgeBackgroundColor({ tabId, color });
  chrome.action.setBadgeText({ tabId, text });
  chrome.action.setTitle({ tabId, title: `Cortex: ${detail}` });
  setTimeout(() => {
    chrome.action.setBadgeText({ tabId, text: "" });
    chrome.action.setTitle({ tabId, title: DEFAULT_TITLE });
  }, 5000);
}
