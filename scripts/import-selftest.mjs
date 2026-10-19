#!/usr/bin/env node
// End-to-end self-test for the headless importers.
//
// Builds a throwaway cortex.db, a DEFLATE-compressed .apkg, and a small Obsidian
// vault in the OS temp dir, then runs both scripts as child processes and
// asserts the resulting rows — including re-run idempotency.
//
//   node scripts/import-selftest.mjs
//
// Exits 0 and prints "ALL CHECKS PASSED" on success, non-zero on failure.
// Requires Node >= 22 (node:sqlite). No network, no npm install.

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import zlib from "node:zlib";
import { DatabaseSync } from "node:sqlite";

const ROOT = path.resolve(import.meta.dirname, "..");
const ANKI = path.join(ROOT, "scripts", "cortex-import-anki.mjs");
const OBSIDIAN = path.join(ROOT, "scripts", "cortex-import-obsidian.mjs");

const SCHEMA = `
CREATE TABLE subjects (
  id TEXT PRIMARY KEY, owner_id TEXT NOT NULL DEFAULT 'local', name TEXT NOT NULL,
  code TEXT, glyph TEXT NOT NULL DEFAULT '◆', color TEXT,
  status TEXT NOT NULL DEFAULT 'ready', streak INTEGER NOT NULL DEFAULT 0,
  position INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE notes (
  id TEXT PRIMARY KEY, subject_id TEXT, topic_id TEXT,
  title TEXT NOT NULL DEFAULT '', body TEXT NOT NULL DEFAULT '', source_id TEXT,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE note_links (
  id TEXT PRIMARY KEY, from_note_id TEXT NOT NULL, target_slug TEXT NOT NULL,
  alias TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE materials (
  id TEXT PRIMARY KEY, subject_id TEXT NOT NULL, topic_id TEXT, kind TEXT NOT NULL,
  title TEXT NOT NULL, meta TEXT, status TEXT NOT NULL DEFAULT 'ready',
  payload TEXT, created_at INTEGER NOT NULL);
CREATE TABLE srs_cards (
  id TEXT PRIMARY KEY, subject_id TEXT NOT NULL, material_id TEXT, kind TEXT NOT NULL,
  item_index INTEGER NOT NULL, item_key TEXT NOT NULL, ease REAL NOT NULL DEFAULT 2.5,
  interval_d INTEGER NOT NULL DEFAULT 0, reps INTEGER NOT NULL DEFAULT 0,
  lapses INTEGER NOT NULL DEFAULT 0, last_grade INTEGER, due_at INTEGER NOT NULL,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
  UNIQUE(subject_id, kind, item_key));
`;

const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let i = 0; i < 256; i += 1) {
    let c = i;
    for (let k = 0; k < 8; k += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[i] = c >>> 0;
  }
  return t;
})();

function crc32(buf) {
  let c = 0xffffffff;
  for (const b of buf) c = CRC_TABLE[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

/** Build a ZIP using DEFLATE — exercises the importer's inflate path. */
function makeZip(entries) {
  const chunks = [];
  const central = [];
  let offset = 0;
  for (const { name, data } of entries) {
    const nameBuf = Buffer.from(name, "utf8");
    const comp = zlib.deflateRawSync(data);
    const crc = crc32(data);
    const lh = Buffer.alloc(30);
    lh.writeUInt32LE(0x04034b50, 0);
    lh.writeUInt16LE(20, 4);
    lh.writeUInt16LE(8, 8);
    lh.writeUInt32LE(crc, 14);
    lh.writeUInt32LE(comp.length, 18);
    lh.writeUInt32LE(data.length, 22);
    lh.writeUInt16LE(nameBuf.length, 26);
    chunks.push(lh, nameBuf, comp);
    const ch = Buffer.alloc(46);
    ch.writeUInt32LE(0x02014b50, 0);
    ch.writeUInt16LE(20, 4);
    ch.writeUInt16LE(20, 6);
    ch.writeUInt16LE(8, 10);
    ch.writeUInt32LE(crc, 16);
    ch.writeUInt32LE(comp.length, 20);
    ch.writeUInt32LE(data.length, 24);
    ch.writeUInt16LE(nameBuf.length, 28);
    ch.writeUInt32LE(offset, 42);
    central.push(ch, nameBuf);
    offset += lh.length + nameBuf.length + comp.length;
  }
  const cd = Buffer.concat(central);
  const eocd = Buffer.alloc(22);
  eocd.writeUInt32LE(0x06054b50, 0);
  eocd.writeUInt16LE(entries.length, 8);
  eocd.writeUInt16LE(entries.length, 10);
  eocd.writeUInt32LE(cd.length, 12);
  eocd.writeUInt32LE(offset, 16);
  return Buffer.concat([...chunks, cd, eocd]);
}

function run(script, args) {
  const r = spawnSync(process.execPath, [script, ...args], { encoding: "utf8" });
  if (r.status !== 0) {
    throw new Error(`${path.basename(script)} exited ${r.status}\n${r.stdout}\n${r.stderr}`);
  }
  return r.stdout;
}

function count(db, table) {
  return db.prepare(`SELECT COUNT(*) AS n FROM ${table}`).get().n;
}

function main() {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "cortex-selftest-"));
  try {
    const dbPath = path.join(tmp, "cortex.db");
    const apkgPath = path.join(tmp, "deck.apkg");
    const vault = path.join(tmp, "vault");

    // 1. cortex.db
    const db = new DatabaseSync(dbPath);
    db.exec(SCHEMA);

    // 2. Anki collection + deflated .apkg
    const collPath = path.join(tmp, "collection.anki2");
    const coll = new DatabaseSync(collPath);
    coll.exec("CREATE TABLE col (id INTEGER PRIMARY KEY, decks TEXT);");
    coll.exec("CREATE TABLE notes (id INTEGER PRIMARY KEY, flds TEXT);");
    coll.exec("CREATE TABLE cards (id INTEGER PRIMARY KEY, nid INTEGER, did INTEGER);");
    coll.prepare("INSERT INTO col VALUES (1, ?)").run(
      JSON.stringify({ 1: { name: "Biology" }, 2: { name: "Physics" } }),
    );
    const US = "\x1f";
    const notes = [
      [100, `What is <b>osmosis</b>?&nbsp;${US}Movement of <i>water</i> across a membrane`],
      [101, `What is 2+2?${US}4`],
      [102, `What is 2+2?${US}four (duplicate)`],
      [103, `F = ma${US}Newton's second law`],
      [104, `<div>Empty back</div>${US}`],
      [105, `Force&nbsp;unit?${US}Newton (N)`],
    ];
    const insNote = coll.prepare("INSERT INTO notes VALUES (?, ?)");
    for (const [id, flds] of notes) insNote.run(id, flds);
    const insCard = coll.prepare("INSERT INTO cards VALUES (?, ?, ?)");
    [[1, 100, 1], [2, 101, 1], [3, 102, 1], [4, 103, 2], [5, 104, 2], [6, 105, 2]]
      .forEach(([id, nid, did]) => insCard.run(id, nid, did));
    coll.close();
    fs.writeFileSync(apkgPath, makeZip([
      { name: "collection.anki2", data: fs.readFileSync(collPath) },
      { name: "media", data: Buffer.from("{}") },
    ]));

    // 3. Obsidian vault
    fs.mkdirSync(path.join(vault, "Sub"), { recursive: true });
    fs.mkdirSync(path.join(vault, ".obsidian"), { recursive: true });
    fs.writeFileSync(path.join(vault, ".obsidian", "app.json"), "{}");
    fs.writeFileSync(path.join(vault, "Index.md"), "# Index\n\nSee [[Alpha]] and [[Beta|第二条]].\n");
    fs.writeFileSync(path.join(vault, "Alpha.md"), "---\ntitle: 甲\ntags: [bio]\n---\n\n# 甲\n\nBack to [[Index]].\n");
    fs.writeFileSync(path.join(vault, "Sub", "Beta.md"), "# Beta\n\nNo frontmatter.\n");

    // 4. Anki import
    run(ANKI, [apkgPath, "--db", dbPath]);
    assert.equal(count(db, "subjects"), 2, "two deck subjects");
    assert.equal(count(db, "materials"), 2, "one material per deck");
    assert.equal(count(db, "srs_cards"), 4, "4 valid cards (dedupe + empty back dropped)");

    const bio = db
      .prepare("SELECT payload FROM materials WHERE title LIKE 'Biology%'")
      .get();
    const cards = JSON.parse(bio.payload);
    assert.equal(cards.length, 2, "Biology has 2 cards after dedupe");
    assert.deepEqual(cards[0], { q: "What is osmosis?", a: "Movement of water across a membrane" });
    assert.deepEqual(cards[1], { q: "What is 2+2?", a: "4" });

    // 5. Anki re-run is idempotent
    run(ANKI, [apkgPath, "--db", dbPath]);
    assert.equal(count(db, "materials"), 2, "re-run does not duplicate materials");

    // 6. Obsidian import
    run(OBSIDIAN, [vault, "--db", dbPath]);
    assert.equal(count(db, "notes"), 3, "3 notes (hidden .obsidian skipped)");
    assert.equal(count(db, "note_links"), 3, "3 wiki links");
    const alias = db.prepare("SELECT alias FROM note_links WHERE target_slug='beta'").get();
    assert.equal(alias.alias, "第二条", "alias preserved");
    const body = db.prepare("SELECT body FROM notes WHERE title='甲'").get().body;
    assert.ok(body.startsWith("# 甲"), "frontmatter stripped, no leading blank line");

    // 7. Obsidian re-run is idempotent
    run(OBSIDIAN, [vault, "--db", dbPath]);
    assert.equal(count(db, "notes"), 3, "re-run does not duplicate notes");

    db.close();
    console.log("ALL CHECKS PASSED");
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
}

main();
