// Conventions shared by the popup and the options page.
//
// The Cortex desktop app exposes a loopback-only HTTP server on 127.0.0.1:7878
// while it runs (see `src-tauri/src/clip_server.rs`). These helpers are the
// only place that knows the URL shape, so a port change lives in one spot.

export const DEFAULT_PORT = 7878;
export const DEFAULT_TITLE = "Clip to Cortex";

/** {port, subjectId, selectionOnly} from chrome.storage.sync, validated. */
export async function readSettings() {
  const stored = await chrome.storage.sync.get({
    port: DEFAULT_PORT,
    subjectId: "",
    selectionOnly: false,
  });
  const n = Number.parseInt(String(stored.port), 10);
  return {
    port: Number.isFinite(n) && n > 0 && n < 65536 ? n : DEFAULT_PORT,
    subjectId: String(stored.subjectId || ""),
    selectionOnly: !!stored.selectionOnly,
  };
}

export const writeSettings = (patch) => chrome.storage.sync.set(patch);

export const apiBase = (port) => `http://127.0.0.1:${port}`;

/** GET a JSON endpoint on the clip server. */
export async function getJson(base, path) {
  let res;
  try {
    res = await fetch(base + path, { cache: "no-store" });
  } catch {
    throw new Error(`连不上 ${base} —— Cortex 桌面端好像没在运行`);
  }
  if (!res.ok) throw new Error(`${path} 返回 HTTP ${res.status}`);
  return res.json();
}

/**
 * POST /clip. Cortex answers as soon as the row is written (the chunk + embed
 * step happens in the background there), so a generous timeout only matters
 * for the page fetch itself.
 */
export async function saveClip(base, payload) {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), 30000);
  try {
    let res;
    try {
      res = await fetch(base + "/clip", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
        signal: ctrl.signal,
      });
    } catch {
      throw new Error(`连不上 ${base} —— Cortex 桌面端好像没在运行`);
    }
    const raw = await res.text();
    let data;
    try {
      data = JSON.parse(raw);
    } catch {
      data = { ok: res.ok, error: raw.slice(0, 200) };
    }
    if (!res.ok || data.ok === false) throw new Error(data.error || `HTTP ${res.status}`);
    return data;
  } finally {
    clearTimeout(timer);
  }
}

/** The text the user has highlighted in a tab ("" when there is no selection). */
export async function readSelection(tabId) {
  try {
    const [{ result }] = await chrome.scripting.executeScript({
      target: { tabId },
      func: () => String(window.getSelection() || ""),
    });
    return String(result || "").trim();
  } catch {
    // Restricted pages (chrome://, the Web Store, PDF viewers) refuse injection.
    return "";
  }
}

export const isClipable = (url) => /^https?:/i.test(String(url || ""));

export function hostOf(url) {
  try {
    return new URL(url).host;
  } catch {
    return String(url || "");
  }
}
