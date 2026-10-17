#!/usr/bin/env pwsh
# run-dev.ps1 — local dev loop without `tauri dev`.
#
# Why this exists: `tauri dev` runs vite through `npm run dev` (which serves
# the Svelte app over HTTP on :1420) and then cargo watches + rebuilds. In
# practice that combination occasionally hits odd states where:
#   - vite serves a partial chunk during HMR and the frontend throws before
#     the Tauri backend hands the load over;
#   - the dev server's port forward from Tauri leaks when an HMR reload
#     races the binary's WebView reset;
#   - the watcher tree in src-tauri/target keeps file handles open after
#     cargo exits, breaking quick `cargo build --release` reruns.
#
# This script does the equivalent work without vite at all:
#   1. Build the frontend ONCE with `npm run build` (static files in dist/).
#   2. `cargo run --features custom-protocol` from src-tauri. That feature is
#      what makes Tauri embed `frontendDist = "../dist"` into the binary and
#      serve it over its own custom protocol; no HTTP server, no HMR, no port
#      forwarding. (With the feature off, Tauri instead points the webview at
#      `build.devUrl` = http://localhost:1420 and you get a "refused to
#      connect" page — the whole reason this script exists.) Hot reload of
#      frontend requires a re-run of this script (the same as `cargo run` for
#      any non-webview crate).
#   3. Build mode is dev (fast) by default. Pass -Release for `cargo run
#      --release`.
#
# Usage:
#   pwsh tools\run-dev.ps1               # dev build, ~5s incremental
#   pwsh tools\run-dev.ps1 -Release      # release build, ~30s incremental
#
# Caveats:
#   - This is not a hot-reload loop; iterating on Svelte requires re-running
#     the script. The intended user is making backend (Rust) changes mostly
#     and just needs the frontend to recompile occasionally.
#   - Cargo's incremental cache must exist (one prior `cargo build`).

[CmdletBinding()]
param(
    [switch]$Release
)

$ErrorActionPreference = 'Stop'

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RootDir = Resolve-Path (Join-Path $ScriptDir '..')
$SrcTauri = Join-Path $RootDir 'src-tauri'

Write-Host '== Cortex dev run (no vite) ==' -ForegroundColor Cyan
Write-Host "root: $RootDir"
Write-Host "profile: $(if ($Release) { 'release' } else { 'dev' })"
Write-Host ''

# 1. Static frontend.
Write-Host '[1/2] vite build (static)…' -ForegroundColor Yellow
Push-Location $RootDir
try {
    npm run build
    if ($LASTEXITCODE -ne 0) { throw 'vite build failed' }
} finally {
    Pop-Location
}

# 2. Cargo run. `--features custom-protocol` is what switches the webview from
#    the (absent) dev server to the dist/ assets baked in at compile time.
Write-Host '[2/2] cargo run…' -ForegroundColor Yellow
Push-Location $SrcTauri
try {
    $cargoArgs = @('run', '--features', 'custom-protocol')
    if ($Release) { $cargoArgs += '--release' }
    cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { throw 'cargo run failed' }
} finally {
    Pop-Location
}

Write-Host ''
Write-Host '== exited ==' -ForegroundColor Green
