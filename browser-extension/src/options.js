// Options page: which port the clip server lives on, and which subject the
// popup/right-click menu should pre-select.

import { apiBase, getJson, readSettings, writeSettings, DEFAULT_PORT } from "./shared.js";

const $ = (id) => document.getElementById(id);
const portEl = $("port");
const subjectEl = $("subject");
const selectionOnlyEl = $("selection-only");
const statusEl = $("status");

const say = (text, kind = "") => {
  statusEl.textContent = text;
  statusEl.className = `status ${kind}`;
};

async function loadSubjects() {
  const n = Number.parseInt(String(portEl.value), 10);
  const base = apiBase(Number.isFinite(n) && n > 0 && n < 65536 ? n : DEFAULT_PORT);
  try {
    const data = await getJson(base, "/subjects");
    const { subjectId } = await readSettings();
    subjectEl.innerHTML = "";
    const none = document.createElement("option");
    none.value = "";
    none.textContent = "（第一个学科）";
    subjectEl.appendChild(none);
    for (const s of data.subjects || []) {
      const opt = document.createElement("option");
      opt.value = s.id;
      opt.textContent = `${s.glyph || ""} ${s.name}`.trim();
      subjectEl.appendChild(opt);
    }
    subjectEl.value = subjectId || "";
    say(`已连上 ${base}`, "oktext");
    return true;
  } catch (e) {
    subjectEl.innerHTML = "";
    const opt = document.createElement("option");
    opt.value = "";
    opt.textContent = "（Cortex 未运行）";
    subjectEl.appendChild(opt);
    say(`${e.message}`, "badtext");
    return false;
  }
}

async function init() {
  const cfg = await readSettings();
  portEl.value = String(cfg.port);
  selectionOnlyEl.checked = cfg.selectionOnly;
  await loadSubjects();

  portEl.addEventListener("change", loadSubjects);
  $("test").addEventListener("click", loadSubjects);
  $("save").addEventListener("click", async () => {
    const n = Number.parseInt(String(portEl.value), 10);
    const port = Number.isFinite(n) && n > 0 && n < 65536 ? n : DEFAULT_PORT;
    portEl.value = String(port);
    await writeSettings({
      port,
      subjectId: subjectEl.value || "",
      selectionOnly: selectionOnlyEl.checked,
    });
    say("已保存", "oktext");
  });
}

init();
