#!/usr/bin/env pwsh
# build-personal.ps1 — one-key personal-use build of Cortex.
#
# Differences from `npm run tauri build`:
#   - Skips code signing (TAURI_SIGNING_PRIVATE_KEY is unused).
#   - Bundles only the NSIS installer (no MSI/WiX/.deb/.AppImage noise).
#   - Skips the tauri-cli wrapper; calls cargo build --release directly.
#   - Doesn't write to target/release/bundle for the unsigned case unless
#     NSIS is requested via -Bundle switch.
#
# Usage:
#   pwsh tools\build-personal.ps1            # exe only (fast, ~10 min)
#   pwsh tools\build-personal.ps1 -Bundle    # exe + NSIS installer (~15 min)
#   pwsh tools\build-personal.ps1 -Run       # build then launch the produced exe
#
# Output:
#   - exe:        src-tauri\target\release\cortex.exe
#   - installer:  src-tauri\target\release\bundle\nsis\Cortex_1.0.35_x64-setup.exe
#                  (only with -Bundle)
#
# Notes:
#   - The build runs in src-tauri so cargo's incremental cache stays warm.
#   - RUST_LOG is left unset; tauri prints its own startup info.
#   - On Windows the exe is unsigned — SmartScreen will warn on first run.
#     Personal use: "more info" -> "run anyway".

[CmdletBinding()]
param(
    [switch]$Bundle,
    [switch]$Run
)

$ErrorActionPreference = 'Stop'

# Resolve the cortex-base root (this script lives in <root>\tools).
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RootDir = Resolve-Path (Join-Path $ScriptDir '..')
$SrcTauri = Join-Path $RootDir 'src-tauri'

Write-Host '== Cortex personal build ==' -ForegroundColor Cyan
Write-Host "root:    $RootDir"
Write-Host "src:     $SrcTauri"
Write-Host "bundle:  $(if ($Bundle) { 'YES (NSIS)' } else { 'no — exe only (pass -Bundle for installer)' })"
Write-Host ''

# 1. Frontend bundle. The SvelteKit app must be in dist/ before cargo can link
#    it into the tauri::Context. `npm run build` invokes vite with the
#    production config from package.json.
Write-Host '[1/3] vite build (frontend)…' -ForegroundColor Yellow
Push-Location $RootDir
try {
    npm run build
    if ($LASTEXITCODE -ne 0) { throw 'vite build failed' }
} finally {
    Pop-Location
}

# 2. Rust release build. Tauri 2 with our feature set is ~10 min from cold,
# ~30 s incremental after the first compile; runs in the background terminal
# output stream so long compile times aren't silent.
Write-Host '[2/3] cargo build --release (rust)…' -ForegroundColor Yellow
Push-Location $SrcTauri
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
} finally {
    Pop-Location
}

$ExePath = Join-Path $SrcTauri 'target\release\cortex.exe'
if (-not (Test-Path $ExePath)) {
    throw "expected binary not found at $ExePath"
}
$ExeSize = (Get-Item $ExePath).Length
Write-Host ("  cortex.exe: {0:N1} MB" -f ($ExeSize / 1MB)) -ForegroundColor Green

# 3. Optional NSIS bundle. Tauri-cli's tauri.conf.json ships `bundle.targets =
# "nsis"` (Windows-only); we re-run the bundler in isolation rather than
# `npx tauri build` to skip the vite re-run and code-signing plumbing.
if ($Bundle) {
    Write-Host '[3/3] bundling NSIS installer…' -ForegroundColor Yellow
    Push-Location $RootDir
    try {
        $env:TAURI_SIGNING_PRIVATE_KEY = $null
        $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = $null
        # tauri-cli reads tauri.conf.json's bundle.targets, which is `nsis` on
        # Windows. --no-bundle is intentionally omitted.
        npx --no-install tauri build --bundles nsis 2>&1 | Tee-Object -FilePath (Join-Path $RootDir '.shots\build-personal.log')
        if ($LASTEXITCODE -ne 0) { throw 'tauri build (NSIS) failed — see .shots\build-personal.log' }
    } finally {
        Pop-Location
    }

    $NsisPath = Join-Path $SrcTauri 'target\release\bundle\nsis\Cortex_1.0.35_x64-setup.exe'
    if (Test-Path $NsisPath) {
        $NsisSize = (Get-Item $NsisPath).Length
        Write-Host ("  installer: {0:N1} MB  ({1})" -f ($NsisSize / 1MB), $NsisPath) -ForegroundColor Green
        Write-Host ''
        Write-Host 'NOTE: installer is unsigned — SmartScreen will warn.' -ForegroundColor Yellow
        Write-Host '      Personal use: click "more info" -> "run anyway".' -ForegroundColor Yellow
    }
}

# 4. Optional launch.
if ($Run) {
    Write-Host '== launching cortex.exe ==' -ForegroundColor Cyan
    Start-Process -FilePath $ExePath
}

Write-Host ''
Write-Host '== done ==' -ForegroundColor Green
Write-Host "exe:       $ExePath"
if ($Bundle -and (Test-Path $NsisPath)) {
    Write-Host "installer: $NsisPath"
}
