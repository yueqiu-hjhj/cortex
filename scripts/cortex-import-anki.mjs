#!/usr/bin/env node
// Headless Anki `.apkg` → Cortex migration (roadmap 1.1).
//
// Reads an Anki package without opening the desktop app, then writes it into an
// existing cortex.db:
//   Anki deck      → Cortex subject (one per deck, or --subject to merge)
//   deck cards     → a `flashcards` material with payload [{q,a}]
//   each card      → an srs_cards row (due now), so it enters the review queue
//
// The card text is cleaned exactly like src-tauri/src/anki.rs (strip HTML,
// decode entities, collapse whitespace), so a script migration and an in-app
// import produce the same fronts/backs.
//
// Usage:
//   node scripts/cortex-import-anki.mjs deck.apkg [options]
//     --db <path>      cortex.db to write (default: the app's data dir)
//     --subject <name> put every deck into one subject with this name
//     --deck <name>    only import this deck (exact name)
//     --dry-run        parse and report, write nothing
//     --help
//
// Requires Node >= 22 (uses the built-in node:sqlite). Close Cortex first.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { DatabaseSync } from "node:sqlite";
import { readZipEntry } from "./lib/zip.mjs";
import { cleanField } from "./lib/anki-text.mjs";
import {
  resolveDbPath,
  openDb,
  tx,
  ensureSubject,
  saveFlashcardMaterial,
  materialExists,
  insertSrsCard,
  parseArgs,
  fail,
  info,
  warn,
} from "./lib/cortex-db.mjs";

const USAGE = `Anki .apkg → Cortex

用法:
  node scripts/cortex-import-anki.mjs <deck.apkg> [选项]

选项:
  --db <path>       指定 cortex.db（默认用应用数据目录里的那个）
  --subject <name>  把所有牌组并入这一个学科（默认：每个牌组一个学科）
  --deck <name>     只导入指定牌组
  --dry-run         只解析并报告，不写入数据库
  --help            显示本帮助

提示:
  - 先启动一次 Cortex 生成数据库，迁移时请关闭应用。
  - 若提示 zstd 压缩，请在 Anki 导出时勾选「支持旧版 Anki」。`;

const ZSTD_MAGIC = 0xfd2fb528;

function readCollection(buf) {
  let entry;
  try {
    entry = readZipEntry(buf, ["collection.anki21", "collection.anki2"]);
  } catch (e) {
    fail(`不是有效的 .apkg（无法解压 ZIP）: ${e.message}`);
  }
  if (!entry) fail("不是有效的 .apkg（ZIP 内没有 collection.anki21 / collection.anki2）。");
  if (entry.data.length >= 4 && entry.data.readUInt32LE(0) === ZSTD_MAGIC) {
    fail(
      "这个 .apkg 的 collection 是 zstd 压缩的新版格式，无法直接读取。\n" +
        "  请在 Anki 导出时勾选「支持旧版 Anki / Support older Anki versions」后重试。",
    );
  }
  const tmp = path.join(
    os.tmpdir(),
    `cortex-anki-${process.pid}-${Date.now()}-${Math.random().toString(16).slice(2)}.anki2`,
  );
  fs.writeFileSync(tmp, entry.data);
  try {
    const c = new DatabaseSync(tmp);
    try {
      const col = c.prepare("SELECT decks FROM col LIMIT 1").get();
      if (!col) throw new Error("collection 缺少 col 表（不是 Anki 集合）");
      const decksJson = JSON.parse(col.decks);
      const decks = new Map();
      for (const [id, d] of Object.entries(decksJson)) {
        decks.set(String(id), (d && d.name) || `Deck ${id}`);
      }
      // note id -> deck id (a note with cards in several decks uses its first card)
      const noteDeck = new Map();
      for (const row of c.prepare("SELECT nid, did FROM cards").all()) {
        const nid = String(row.nid);
        if (!noteDeck.has(nid)) noteDeck.set(nid, String(row.did));
      }
      const notes = c.prepare("SELECT id, flds FROM notes").all();
      return { decks, noteDeck, notes };
    } finally {
      c.close();
    }
  } catch (e) {
    fail(`无法读取 .apkg 内的数据库: ${e.message}`);
  } finally {
    try {
      fs.unlinkSync(tmp);
    } catch {
      /* temp file already gone */
    }
  }
}

function groupByDeck({ decks, noteDeck, notes }, onlyDeck) {
  const fallbackDeck = decks.size === 1 ? [...decks.values()][0] : "Default";
  const byDeck = new Map();
  for (const n of notes) {
    const fields = String(n.flds ?? "").split("\x1f");
    const q = cleanField(fields[0] ?? "");
    const a = cleanField(fields[1] ?? "");
    if (!q || !a) continue;
    const did = noteDeck.get(String(n.id));
    const deckName = did && decks.has(did) ? decks.get(did) : fallbackDeck;
    if (onlyDeck && deckName !== onlyDeck) continue;
    if (!byDeck.has(deckName)) byDeck.set(deckName, []);
    byDeck.get(deckName).push({ q, a });
  }
  // Dedupe within a deck by lowercased front (mirrors anki.rs dedupe_key).
  for (const [deck, cards] of byDeck) {
    const seen = new Set();
    byDeck.set(
      deck,
      cards.filter((c) => {
        const k = c.q.toLowerCase();
        if (seen.has(k)) return false;
        seen.add(k);
        return true;
      }),
    );
  }
  return byDeck;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    info(USAGE);
    return;
  }
  const apkg = args._[0];
  if (!apkg) fail(`缺少 .apkg 路径。\n\n${USAGE}`);
  if (!fs.existsSync(apkg)) fail(`找不到 .apkg 文件: ${apkg}`);

  let buf;
  try {
    buf = fs.readFileSync(apkg);
  } catch (e) {
    fail(`无法读取 ${apkg}: ${e.message}`);
  }

  const parsed = readCollection(buf);
  const byDeck = groupByDeck(parsed, args.deck);
  if (byDeck.size === 0) {
    fail("没有可导入的卡片（牌组名不匹配，或卡片缺少正面/背面）。");
  }

  const dbPath = resolveDbPath(args.db);
  let total = 0;
  info(`\nAnki 包: ${path.basename(apkg)}`);
  for (const [deck, cards] of byDeck) {
    total += cards.length;
    info(`  ${deck}  →  ${cards.length} 张`);
  }
  info(`  合计: ${total} 张`);
  info(`目标数据库: ${dbPath}`);

  if (args.dryRun) {
    info("\n(dry-run) 未写入任何数据。");
    return;
  }

  const db = openDb(dbPath);
  let subjectsCreated = 0;
  let srsCreated = 0;
  let importedCards = 0;
  let importedDecks = 0;
  let skippedDecks = 0;
  try {
    tx(db, () => {
      for (const [deckName, cards] of byDeck) {
        const subjectName = args.subject || deckName;
        const subj = ensureSubject(db, subjectName);
        if (subj.created) subjectsCreated += 1;
        const title = `${deckName}（Anki 迁移）`;
        // Idempotent: a deck already migrated into this subject is left alone.
        if (materialExists(db, subj.id, title)) {
          skippedDecks += 1;
          continue;
        }
        const meta = `${cards.length} cards · Anki`;
        const materialId = saveFlashcardMaterial(db, subj.id, title, meta, cards);
        cards.forEach((c, i) => {
          if (insertSrsCard(db, subj.id, materialId, i, c.q)) srsCreated += 1;
        });
        importedCards += cards.length;
        importedDecks += 1;
      }
    });
  } finally {
    db.close();
  }

  info(
    `\n✔ 已导入 ${importedCards} 张卡片 → ${importedDecks} 个牌组` +
      `${subjectsCreated ? `，新建 ${subjectsCreated} 个学科` : ""}` +
      `${srsCreated ? `，${srsCreated} 张进入复习队列` : ""}。`,
  );
  if (skippedDecks) warn(`${skippedDecks} 个牌组此前已导入（同名材料存在），本次跳过。`);
  if (srsCreated < importedCards) {
    warn(`${importedCards - srsCreated} 张与已有复习卡重复（同一学科内正面相同），已跳过排期。`);
  }
  info("打开 Cortex 即可在对应学科下看到这些卡片。");
}

main();
