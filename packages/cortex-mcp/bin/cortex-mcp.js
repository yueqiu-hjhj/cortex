#!/usr/bin/env node
// cortex-mcp launcher — the real server is the Rust binary `cortex-mcp`
// (src-tauri/src/bin/cortex-mcp.rs). This wrapper resolves a binary in this
// order and exec's it with the same argv:
//
//   1. $CORTEX_MCP_BIN            explicit path to a binary
//   2. `cortex-mcp` on $PATH      a previously downloaded / installed one
//   3. download from GitHub Releases (cached under ~/.cortex-mcp-bin)
//
// MCP hosts spawn us over stdio, so the wrapper must be silent on stdout —
// all diagnostics go to stderr, and the download step happens once, lazily.

const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const https = require("node:https");

const REPO = process.env.CORTEX_MCP_RELEASE_REPO || "yueqiu-hjhj/cortex";

function fail(msg) {
  process.stderr.write(`cortex-mcp: ${msg}\n`);
  process.exit(1);
}

function platformAsset() {
  const p = os.platform();
  const a = os.arch();
  if (p === "win32") return a === "arm64" ? "cortex-mcp-aarch64-pc-windows-msvc.exe" : "cortex-mcp-x86_64-pc-windows-msvc.exe";
  if (p === "darwin") return a === "arm64" ? "cortex-mcp-aarch64-apple-darwin" : "cortex-mcp-x86_64-apple-darwin";
  if (p === "linux") return a === "arm64" ? "cortex-mcp-aarch64-unknown-linux-gnu" : "cortex-mcp-x86_64-unknown-linux-gnu";
  return null;
}

function findOnPath() {
  const which = process.platform === "win32" ? "where" : "which";
  const r = spawnSync(which, ["cortex-mcp"], { encoding: "utf8" });
  if (r.status === 0 && r.stdout) {
    const first = r.stdout.split(/\r?\n/).find((l) => l.trim());
    if (first && fs.existsSync(first.trim())) return first.trim();
  }
  return null;
}

function download(asset, dest) {
  return new Promise((resolve, reject) => {
    const url = `https://github.com/${REPO}/releases/latest/download/${asset}`;
    const get = (u, hops) => {
      if (hops > 5) return reject(new Error("too many redirects"));
      https
        .get(u, (res) => {
          if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
            res.resume();
            return get(res.headers.location, hops + 1);
          }
          if (res.statusCode !== 200) {
            res.resume();
            return reject(new Error(`HTTP ${res.statusCode} for ${u}`));
          }
          const out = fs.createWriteStream(dest);
          res.pipe(out);
          out.on("finish", () => out.close(() => resolve(dest)));
        })
        .on("error", reject);
    };
    get(url, 0);
  });
}

async function main() {
  // 1. explicit
  const explicit = process.env.CORTEX_MCP_BIN;
  if (explicit && fs.existsSync(explicit)) return exec(explicit);
  // 2. PATH
  const onPath = findOnPath();
  if (onPath) return exec(onPath);
  // 3. download (cached)
  const asset = platformAsset();
  if (!asset) fail(`unsupported platform ${os.platform()}-${os.arch()}; set CORTEX_MCP_BIN to a cortex-mcp binary`);
  const cacheDir = path.join(os.homedir(), ".cortex-mcp-bin");
  fs.mkdirSync(cacheDir, { recursive: true });
  const dest = path.join(cacheDir, asset);
  if (!fs.existsSync(dest)) {
    process.stderr.write(`cortex-mcp: fetching ${asset} from ${REPO} releases…\n`);
    try {
      await download(asset, dest);
      fs.chmodSync(dest, 0o755);
    } catch (e) {
      fail(`download failed: ${e.message}. Install the Cortex desktop app (which ships cortex-mcp) or set CORTEX_MCP_BIN.`);
    }
  }
  exec(dest);
}

function exec(bin) {
  const r = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });
  if (r.error) fail(String(r.error));
  process.exit(r.status == null ? 1 : r.status);
}

main();
