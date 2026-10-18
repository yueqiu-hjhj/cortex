#!/usr/bin/env node
// verify.mjs — the one-shot local verification gate.
//
// DEP0190 is fired by Node when we pass args via the shell on Windows; we
// intentionally do so because npm / npx / cargo all need the .cmd shim
// resolver and we control every argument (no user input), so the warning is
// noise rather than a security concern.
process.removeAllListeners("warning");
process.on("warning", (w) => { if (w.name !== "DeprecationWarning" || !String(w.message).includes("DEP0190")) console.warn(w); });
//
// Runs the same checks CI runs on every push and PR:
//   1. svelte-check       (frontend type-check)
//   2. vite build         (frontend production bundle)
//   3. cargo check        (backend compile, all targets)
//   4. cargo test --lib   (backend unit tests, no network required)
//   5. cargo clippy --all-targets -- -D warnings  (strict lint gate)
//      The lib already allows the structural lints that have no good
//      answer on a Tauri command surface: too_many_arguments on every
//      #[tauri::command] entry, type_complexity on public HashMap<...>s,
//      redundant_locals for the `let x = x;` move-into-closure idiom.
//      Tests run under #![cfg_attr(test, allow(clippy::all))] so test
//      code is allowed to be messier.
//
// Designed to be run from the repo root with `npm run verify` or directly
// with `node scripts/verify.mjs`. Exits 0 on full success, 1 on first failure.
// On failure, prints which step failed and the tail of its output (full output
// is also available at the npm script's --verbose flag if you ever need it).

import { spawnSync } from "node:child_process";
import process from "node:process";

const STEPS = [
  { name: "svelte-check (frontend types)",      cmd: "npx", args: ["svelte-check", "--tsconfig", "./tsconfig.json"] },
  { name: "vite build (frontend bundle)",       cmd: "npx", args: ["vite", "build"] },
  { name: "cargo check --all-targets (backend)", cmd: "cargo", args: ["check", "--manifest-path", "src-tauri/Cargo.toml", "--all-targets"] },
  { name: "cargo test  --lib (backend unit tests)", cmd: "cargo", args: ["test", "--manifest-path", "src-tauri/Cargo.toml", "--lib"] },
  { name: "cargo clippy --all-targets -- -D warnings (strict lint)", cmd: "cargo", args: ["clippy", "--manifest-path", "src-tauri/Cargo.toml", "--all-targets", "--", "-D", "warnings"] },
];

function run(step) {
  console.log(`\n\x1b[36m\u2501\u2501 ${step.name}\x1b[0m`);
  console.log(`\x1b[2m  $ ${step.cmd} ${step.args.join(" ")}\x1b[0m`);
  const t0 = Date.now();
  // On Windows, .cmd shims (npm, npx, cargo) need shell: true to actually launch;
  // on POSIX shells we don't need it and turning it on triggers Node's DEP0190
  // deprecation warning. The shell's job is just to resolve the command path
  // — we pass args verbatim, never through a user-controlled string.
  const useShell = process.platform === "win32";
  const r = spawnSync(step.cmd, step.args, { stdio: ["inherit", "pipe", "pipe"], shell: useShell });
  const ms = Date.now() - t0;
  const stdout = r.stdout?.toString() ?? "";
  const stderr = r.stderr?.toString() ?? "";
  if (r.status === 0) {
    // Last useful line is usually the most informative; print all output for transparency.
    process.stdout.write(stdout);
    if (stderr) process.stderr.write(stderr);
    console.log(`\x1b[32m  ok\x1b[0m (${(ms / 1000).toFixed(1)}s)`);
    return true;
  }
  process.stdout.write(stdout);
  process.stderr.write(stderr);
  console.error(`\x1b[31m  FAIL\x1b[0m (${(ms / 1000).toFixed(1)}s, exit ${r.status})`);
  return false;
}

let failed = null;
for (const step of STEPS) {
  if (!run(step)) {
    failed = step;
    break;
  }
}

console.log();
if (failed) {
  console.error(`\x1b[31mverify failed at: ${failed.name}\x1b[0m`);
  process.exit(1);
}
console.log("\x1b[32mverify passed: all 5 steps green\x1b[0m");
