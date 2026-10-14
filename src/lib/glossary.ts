// Per-subject glossary. Stored in localStorage (key: `glossary:<subjectId>`) —
// intentionally NOT in the SQLite DB: the data is small, per-device, and
// private to the study workflow, so a sync story would only add cost. The shape
// is deliberately flat (term + definition + 2-tier status) so a future
// "AI suggest" can be bolted on without a migration.

export type GlossaryStatus = "new" | "known" | "fuzzy";

export interface GlossaryTerm {
  id: string;
  term: string;
  definition: string;
  status: GlossaryStatus;
  createdAt: number;
  updatedAt: number;
}

const ns = (sid: string) => `glossary:${sid}`;

function readRaw(sid: string): GlossaryTerm[] {
  if (typeof localStorage === "undefined") return [];
  try {
    const raw = localStorage.getItem(ns(sid));
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

function writeRaw(sid: string, terms: GlossaryTerm[]): void {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(ns(sid), JSON.stringify(terms));
  } catch (e) {
    // Quota exceeded or storage disabled — surface to caller; the UI toasts it.
    throw e;
  }
}

function newId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) return crypto.randomUUID();
  return "t_" + Math.random().toString(36).slice(2, 10) + Date.now().toString(36);
}

export function loadGlossary(sid: string): GlossaryTerm[] {
  return readRaw(sid);
}

export function addTerm(sid: string, term: string, definition: string): GlossaryTerm {
  const t = term.trim();
  const d = definition.trim();
  if (!t) throw new Error("term is required");
  const now = Date.now();
  const entry: GlossaryTerm = {
    id: newId(),
    term: t,
    definition: d,
    status: "new",
    createdAt: now,
    updatedAt: now,
  };
  const list = readRaw(sid);
  list.unshift(entry);
  writeRaw(sid, list);
  return entry;
}

export function updateTerm(
  sid: string,
  id: string,
  patch: { term?: string; definition?: string }
): GlossaryTerm | null {
  const list = readRaw(sid);
  const i = list.findIndex((t) => t.id === id);
  if (i < 0) return null;
  const next: GlossaryTerm = {
    ...list[i],
    term: patch.term?.trim() ?? list[i].term,
    definition: patch.definition?.trim() ?? list[i].definition,
    updatedAt: Date.now(),
  };
  if (!next.term) return null;
  list[i] = next;
  writeRaw(sid, list);
  return next;
}

export function setStatus(sid: string, id: string, status: GlossaryStatus): GlossaryTerm | null {
  const list = readRaw(sid);
  const i = list.findIndex((t) => t.id === id);
  if (i < 0) return null;
  list[i] = { ...list[i], status, updatedAt: Date.now() };
  writeRaw(sid, list);
  return list[i];
}

export function deleteTerm(sid: string, id: string): GlossaryTerm[] {
  const list = readRaw(sid).filter((t) => t.id !== id);
  writeRaw(sid, list);
  return list;
}

export function clearGlossary(sid: string): void {
  writeRaw(sid, []);
}

export function countByStatus(sid: string): { total: number; known: number; fuzzy: number; new: number } {
  const list = readRaw(sid);
  const out = { total: list.length, known: 0, fuzzy: 0, new: 0 };
  for (const t of list) {
    if (t.status === "known") out.known++;
    else if (t.status === "fuzzy") out.fuzzy++;
    else out.new++;
  }
  return out;
}
