#!/usr/bin/env node
// Headless Obsidian vault → Cortex migration (roadmap 1.1).
//
// Walks a vault of Markdown files and writes them as Cortex notes, rebuilding
// the [[double-link]] index exactly like src-tauri/src/note_links.rs so the
// backlinks panel works immediately after migration.
//
//   vault folder   → subject (or one per top-level folder with --by-folder)
//   each .md file  → a note (title = frontmatter `title` or file name)
//   [[links]]      → note_links rows (slug = lowercased target)
//
// Usage:
//   node scripts/cortex-import-obsidian.mjs <vault-dir> [options]
//     --db <path>      cortex.db to write (default: the app's data dir)
//     --subject <name> put every note into one subject with this name
//     --by-folder      one subject per top-level folder inside the vault
//     --dry-run        parse and report, write nothing
//     --help
//
// Requires Node >= 22 (uses the built-in node:sqlite). Close Cortex first.

import fs from "node:fs";
import path from "node:path";
import {
  resolveDbPath,
  openDb,
  tx,
  ensureSubject,
  noteExists,
  insertNote,
  rebuildLinks,
  parseArgs,
  fail,
  info,
  warn,
} from "./lib/cortex-db.mjs";

const USAGE = `Obsidian 库 → Cortex

用法:
  node scripts/cortex-import-obsidian.mjs <vault-dir> [选项]

选项:
  --db <path>       指定 cortex.db（默认用应用数据目录里的那个）
  --subject <name>  把所有笔记并入这一个学科（默认：库名一个学科）
  --by-folder       每个顶层文件夹一个学科
  --dry-run         只解析并报告，不写入数据库
  --help            显示本帮助

提示:
  - 先启动一次 Cortex 生成数据库，迁移时请关闭应用。
  - 只读取 .md 文件；.obsidian / .trash 等隐藏目录会跳过。`;

const SKIP_DIRS = new Set([".obsidian", ".trash", ".git", "node_modules", ".stfolder", ".smart-env"]);

function walk(dir, out) {
  for (const ent of fs.readdirSync(dir, { withFileTypes: true })) {
    if (ent.name.startsWith(".")) continue;
    const full = path.join(dir, ent.name);
    if (ent.isDirectory()) {
      if (!SKIP_DIRS.has(ent.name)) walk(full, out);
    } else if (ent.isFile() && ent.name.toLowerCase().endsWith(".md")) {
      out.push(full);
    }
  }
}

/** Minimal YAML frontmatter reader: title/tags/aliases (scalars, inline + list). */
function parseFrontmatter(text) {
  if (!text.startsWith("---")) return { meta: {}, body: text };
  const m = /^---\r?\n([\s\S]*?)\r?\n---\r?\n?/.exec(text);
  if (!m) return { meta: {}, body: text };
  const meta = {};
  let key = null;
  for (const line of m[1].split(/\r?\n/)) {
    const kv = /^([A-Za-z0-9_-]+)\s*:\s*(.*)$/.exec(line);
    if (kv) {
      key = kv[1].toLowerCase();
      const val = kv[2].trim();
      if (val === "") {
        meta[key] = [];
      } else if (val.startsWith("[") && val.endsWith("]")) {
        meta[key] = val
          .slice(1, -1)
          .split(",")
          .map((s) => s.trim().replace(/^["']|["']$/g, ""))
          .filter(Boolean);
      } else {
        meta[key] = val.replace(/^["']|["']$/g, "");
      }
    } else {
      const li = /^\s*-\s+(.*)$/.exec(line);
      if (li && key) {
        if (!Array.isArray(meta[key])) meta[key] = [];
        meta[key].push(li[1].trim().replace(/^["']|["']$/g, ""));
      }
    }
  }
  // Drop the single blank line that separates the frontmatter from the body.
  return { meta, body: text.slice(m[0].length).replace(/^\r?\n/, "") };
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    info(USAGE);
    return;
  }
  const vault = args._[0];
  if (!vault) fail(`缺少 Obsidian 库路径。\n\n${USAGE}`);
  if (!fs.existsSync(vault) || !fs.statSync(vault).isDirectory()) {
    fail(`不是有效的目录: ${vault}`);
  }

  const files = [];
  walk(path.resolve(vault), files);
  if (files.length === 0) fail("这个目录里没有 .md 文件。");

  const vaultName = path.basename(path.resolve(vault));
  const root = path.resolve(vault);

  const planned = [];
  for (const file of files) {
    const rel = path.relative(root, file);
    const seg = rel.split(path.sep);
    const topFolder = seg.length > 1 ? seg[0] : null;
    const subjectName = args.subject || (args.byFolder && topFolder ? topFolder : vaultName);

    let text;
    try {
      text = fs.readFileSync(file, "utf8");
    } catch (e) {
      warn(`跳过无法读取的文件 ${rel}: ${e.message}`);
      continue;
    }
    const { meta, body } = parseFrontmatter(text);
    const stem = path.basename(file, path.extname(file));
    const title = typeof meta.title === "string" && meta.title.trim() ? meta.title.trim() : stem;
    planned.push({ rel, subjectName, title, body });
  }

  const dbPath = resolveDbPath(args.db);
  const perSubject = new Map();
  for (const p of planned) perSubject.set(p.subjectName, (perSubject.get(p.subjectName) || 0) + 1);
  info(`\nObsidian 库: ${vaultName}（${planned.length} 个 Markdown 文件）`);
  for (const [s, n] of perSubject) info(`  ${s}  →  ${n} 篇笔记`);
  info(`目标数据库: ${dbPath}`);

  if (args.dryRun) {
    info("\n(dry-run) 未写入任何数据。");
    return;
  }

  const db = openDb(dbPath);
  let inserted = 0;
  let skipped = 0;
  let links = 0;
  try {
    tx(db, () => {
      for (const p of planned) {
        const subj = ensureSubject(db, p.subjectName);
        if (noteExists(db, subj.id, p.title)) {
          skipped += 1;
          continue;
        }
        const noteId = insertNote(db, subj.id, p.title, p.body);
        links += rebuildLinks(db, noteId, p.body);
        inserted += 1;
      }
    });
  } finally {
    db.close();
  }

  info(`\n✔ 已导入 ${inserted} 篇笔记，建立 ${links} 条 [[双链]]。`);
  if (skipped) warn(`${skipped} 篇同名笔记已存在，已跳过（同名会在双链解析中产生歧义）。`);
  info("打开 Cortex 即可在对应学科下看到这些笔记。");
}

main();
