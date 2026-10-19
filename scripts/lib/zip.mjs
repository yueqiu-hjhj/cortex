// Minimal ZIP reader for .apkg / .zip archives — no dependencies.
//
// Supports the two methods Anki actually uses: STORED (0) and DEFLATE (8).
// Sizes are taken from the central directory (authoritative), so entries written
// with a data descriptor still read correctly.

import zlib from "node:zlib";

const EOCD_SIG = 0x06054b50; // End of central directory
const CEN_SIG = 0x02014b50; // Central directory file header
const LOC_SIG = 0x04034b50; // Local file header

/** List central-directory entries. */
export function listZipEntries(buf) {
  if (!Buffer.isBuffer(buf)) throw new Error("listZipEntries expects a Buffer");
  // Find the EOCD record: 22 bytes + up to 64 KiB of trailing comment.
  const minPos = Math.max(0, buf.length - 22 - 0xffff);
  let eocd = -1;
  for (let p = buf.length - 22; p >= minPos; p -= 1) {
    if (buf.readUInt32LE(p) === EOCD_SIG) {
      eocd = p;
      break;
    }
  }
  if (eocd < 0) throw new Error("not a zip file (no end-of-central-directory record)");

  const count = buf.readUInt16LE(eocd + 10);
  const cdOffset = buf.readUInt32LE(eocd + 16);
  if (cdOffset === 0xffffffff) throw new Error("ZIP64 archives are not supported");

  const entries = [];
  let p = cdOffset;
  for (let n = 0; n < count; n += 1) {
    if (p + 46 > buf.length || buf.readUInt32LE(p) !== CEN_SIG) break;
    const method = buf.readUInt16LE(p + 10);
    const compSize = buf.readUInt32LE(p + 20);
    const uncompSize = buf.readUInt32LE(p + 24);
    const nameLen = buf.readUInt16LE(p + 28);
    const extraLen = buf.readUInt16LE(p + 30);
    const commentLen = buf.readUInt16LE(p + 32);
    const localOffset = buf.readUInt32LE(p + 42);
    const name = buf.toString("utf8", p + 46, p + 46 + nameLen);
    if (compSize === 0xffffffff || localOffset === 0xffffffff) {
      throw new Error("ZIP64 entry is not supported");
    }
    entries.push({ name, method, compSize, uncompSize, localOffset });
    p += 46 + nameLen + extraLen + commentLen;
  }
  return entries;
}

function extract(buf, entry) {
  const lo = entry.localOffset;
  if (lo + 30 > buf.length || buf.readUInt32LE(lo) !== LOC_SIG) {
    throw new Error(`corrupt local header for ${entry.name}`);
  }
  const nameLen = buf.readUInt16LE(lo + 26);
  const extraLen = buf.readUInt16LE(lo + 28);
  const start = lo + 30 + nameLen + extraLen;
  const comp = buf.subarray(start, start + entry.compSize);
  if (comp.length !== entry.compSize) {
    throw new Error(`truncated entry ${entry.name}`);
  }
  if (entry.method === 0) return Buffer.from(comp);
  if (entry.method === 8) return zlib.inflateRawSync(comp);
  throw new Error(`unsupported compression method ${entry.method} for ${entry.name}`);
}

/**
 * Read the first entry whose name is in `names` (ordered by preference).
 * Returns { name, data } or null when none match.
 */
export function readZipEntry(buf, names) {
  const entries = listZipEntries(buf);
  for (const wanted of names) {
    const hit = entries.find((e) => e.name === wanted);
    if (hit) return { name: hit.name, data: extract(buf, hit) };
  }
  return null;
}
