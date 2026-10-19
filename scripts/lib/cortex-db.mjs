// Shared Cortex SQLite helpers for the headless migration scripts.
//
// These scripts write into a cortex.db the desktop app already created, mirroring
// the exact insert contracts in src-tauri/src/repo.rs:
//   - ids are UUID v4 strings, timestamps are ms epoch
//   - subjects.position = COALESCE(MAX(position), -1) + 1
//   - flashcard material payload = [{"q": front, "a": back}]
//   - note_links rebuilt on save, target stored as a lowercased title slug
//
// They never create the schema. Launch Cortex once first so migrations land,
// then close the app (SQLite is single-writer) before running a migration.

import { DatabaseSync } from "node:sqlite";
import { randomUUID } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const APP_IDENTIFIER = "app.study.cortex";
const REQUIRED_TABLES = ["subjects", "notes", "note_links", "materials", "srs_cards"];

export function fail(msg) {
  process.stderr.write(`\n✖ ${msg}\n`);
  process.exit(1);
}

export function info(msg) {
  process.stdout.write(`${msg}\n`);
}

export function warn(msg) {
  process.stderr.write(`⚠ ${msg}\n`);
}

export function nowMs() {
  return Date.now();
}

export function newId() {
  return randomUUID();
}

/** Minimal argv parser shared by both importers. */
export function parseArgs(argv) {
  const out = { _: [] };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (a === "--db") out.db = argv[++i];
    else if (a === "--subject") out.subject = argv[++i];
    else if (a === "--deck") out.deck = argv[++i];
    else if (a === "--by-folder") out.byFolder = true;
    else if (a === "--dry-run") out.dryRun = true;
    else if (a === "--help" || a === "-h") out.help = true;
    else if (a.startsWith("--")) fail(`未知参数: ${a}（--help 查看用法）`);
    else out._.push(a);
  }
  return out;
}

/**
 * Resolve the DB path the same way the app does.
 * Priority: --db > $CORTEX_DB > $CORTEX_DATA_DIR/cortex.db > platform app-data dir.
 * Tauri's app_data_dir() is `{data_dir}/{identifier}` on all three platforms.
 */
export function resolveDbPath(dbArg) {
  if (dbArg && dbArg.trim()) return path.resolve(dbArg.trim());
  const envDb = process.env.CORTEX_DB;
  if (envDb && envDb.trim()) return path.resolve(envDb.trim());
  const envDir = process.env.CORTEX_DATA_DIR;
  if (envDir && envDir.trim()) return path.join(envDir.trim(), "cortex.db");
  if (process.platform === "win32") {
    const base = process.env.APPDATA || path.join(os.homedir(), "AppData", "Roaming");
    return path.join(base, APP_IDENTIFIER, "cortex.db");
  }
  if (process.platform === "darwin") {
    return path.join(os.homedir(), "Library", "Application Support", APP_IDENTIFIER, "cortex.db");
  }
  const base = process.env.XDG_DATA_HOME || path.join(os.homedir(), ".local", "share");
  return path.join(base, APP_IDENTIFIER, "cortex.db");
}

export function openDb(dbPath) {
  if (!fs.existsSync(dbPath)) {
    fail(
      `找不到数据库: ${dbPath}\n` +
        `  先启动一次 Cortex 让它创建数据库，或用 --db 指定 cortex.db 的路径。`,
    );
  }
  let db;
  try {
    db = new DatabaseSync(dbPath);
  } catch (e) {
    fail(`无法打开数据库 ${dbPath}: ${e.message}`);
  }
  try {
    db.exec("PRAGMA busy_timeout = 5000;");
    db.exec("PRAGMA foreign_keys = ON;");
  } catch (e) {
    fail(`数据库不可写（Cortex 是否正在运行？请先关闭应用再迁移）: ${e.message}`);
  }
  const have = new Set(
    db.prepare("SELECT name FROM sqlite_master WHERE type='table'").all().map((r) => r.name),
  );
  const missing = REQUIRED_TABLES.filter((t) => !have.has(t));
  if (missing.length) {
    fail(
      `数据库缺少表 ${missing.join(", ")} —— 请先用当前版本 Cortex 启动一次以完成迁移。`,
    );
  }
  return db;
}

/** Run fn inside an IMMEDIATE transaction; rolls back and rethrows on error. */
export function tx(db, fn) {
  db.exec("BEGIN IMMEDIATE");
  try {
    const out = fn();
    db.exec("COMMIT");
    return out;
  } catch (e) {
    try {
      db.exec("ROLLBACK");
    } catch {
      /* ignore rollback failure */
    }
    throw e;
  }
}

export function findSubjectId(db, name) {
  const row = db
    .prepare("SELECT id FROM subjects WHERE name = ? ORDER BY created_at LIMIT 1")
    .get(name);
  return row ? row.id : null;
}

export function ensureSubject(db, name) {
  const found = findSubjectId(db, name);
  if (found) return { id: found, created: false };
  const id = newId();
  const ts = nowMs();
  const pos = db.prepare("SELECT COALESCE(MAX(position), -1) + 1 AS p FROM subjects").get().p;
  db.prepare(
    "INSERT INTO subjects (id, name, code, glyph, color, status, streak, position, created_at, updated_at) " +
      "VALUES (?, ?, NULL, '◆', NULL, 'ready', 0, ?, ?, ?)",
  ).run(id, name, pos, ts, ts);
  return { id, created: true };
}

export function noteExists(db, subjectId, title) {
  return Boolean(
    db.prepare("SELECT 1 AS x FROM notes WHERE subject_id = ? AND title = ? LIMIT 1").get(subjectId, title),
  );
}

export function insertNote(db, subjectId, title, body) {
  const id = newId();
  const ts = nowMs();
  db.prepare(
    "INSERT INTO notes (id, subject_id, topic_id, title, body, created_at, updated_at) VALUES (?, ?, NULL, ?, ?, ?, ?)",
  ).run(id, subjectId, title, body, ts, ts);
  return id;
}

/** True when a flashcard material with this exact title already exists in the subject. */
export function materialExists(db, subjectId, title) {
  return Boolean(
    db
      .prepare("SELECT 1 AS x FROM materials WHERE subject_id = ? AND kind = 'flashcards' AND title = ? LIMIT 1")
      .get(subjectId, title),
  );
}

/** materials.kind='flashcards', payload = [{q,a}]. Returns the material id. */
export function saveFlashcardMaterial(db, subjectId, title, meta, cards) {
  const id = newId();
  const payload = JSON.stringify(cards.map((c) => ({ q: c.q, a: c.a })));
  db.prepare(
    "INSERT INTO materials (id, subject_id, topic_id, kind, title, meta, status, payload, created_at) " +
      "VALUES (?, ?, NULL, 'flashcards', ?, ?, 'ready', ?, ?)",
  ).run(id, subjectId, title, meta, payload, nowMs());
  return id;
}

/** Seed SM-2 state so migrated cards are due immediately. Returns true if inserted. */
export function insertSrsCard(db, subjectId, materialId, itemIndex, itemKey) {
  const ts = nowMs();
  const r = db
    .prepare(
      "INSERT OR IGNORE INTO srs_cards " +
        "(id, subject_id, material_id, kind, item_index, item_key, ease, interval_d, reps, lapses, last_grade, due_at, created_at, updated_at) " +
        "VALUES (?, ?, ?, 'flashcard', ?, ?, 2.5, 0, 0, 0, NULL, ?, ?, ?)",
    )
    .run(newId(), subjectId, materialId, itemIndex, itemKey, ts, ts, ts);
  return r.changes > 0;
}

// ---- [[wiki links]] — mirrors src-tauri/src/note_links.rs -------------------

export function slug(title) {
  return title.trim().toLowerCase();
}

/** Extract [[target]] / [[target|alias]], deduped by (target, alias). */
export function parseLinks(body) {
  const chars = Array.from(body);
  const out = [];
  let i = 0;
  while (i + 3 < chars.length) {
    if (chars[i] === "[" && chars[i + 1] === "[") {
      let j = i + 2;
      let inner = "";
      let closed = false;
      while (j + 1 < chars.length) {
        if (chars[j] === "]" && chars[j + 1] === "]") {
          closed = true;
          break;
        }
        if (chars[j] === "[") break;
        inner += chars[j];
        j += 1;
      }
      if (closed) {
        const trimmed = inner.trim();
        if (trimmed) {
          const bar = trimmed.indexOf("|");
          const target = (bar >= 0 ? trimmed.slice(0, bar) : trimmed).trim();
          let alias = bar >= 0 ? trimmed.slice(bar + 1).trim() : null;
          if (alias === "") alias = null;
          if (target && !out.some((l) => l.target === target && l.alias === alias)) {
            out.push({ target, alias });
          }
        }
        i = j + 2;
        continue;
      }
    }
    i += 1;
  }
  return out;
}

/** Rebuild the outbound-link index for one note (called right after insert). */
export function rebuildLinks(db, noteId, body) {
  db.prepare("DELETE FROM note_links WHERE from_note_id = ?").run(noteId);
  const links = parseLinks(body);
  const ts = nowMs();
  const stmt = db.prepare(
    "INSERT INTO note_links (id, from_note_id, target_slug, alias, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
  );
  for (const link of links) {
    stmt.run(newId(), noteId, slug(link.target), link.alias, ts, ts);
  }
  return links.length;
}
