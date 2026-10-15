# Sign the release APK with a debug-or-release keystore and align it. Idempotent:
# re-running overwrites the previous signed APK.
#
# Keystore can come from any of (highest priority first):
#   1. Cmd-line params -KeystorePath/-KeyAlias/-KeystorePass/-KeyPass
#   2. Env vars  CORTEX_KEYSTORE_PATH / CORTEX_KEY_ALIAS / CORTEX_KEYSTORE_PASS / CORTEX_KEY_PASS
#   3. `<repo-root>\local.properties` keys `keystore.path`, `keystore.alias`,
#      `keystore.password`, `keystore.keyPassword` (passwords are cleartext —
#      file MUST stay out of git, already covered by `*.local` in .gitignore)
#   4. `~/.android/debug.keystore` + well-known `androiddebugkey` alias
#
# Usage examples:
#   tools\sign-android.ps1                                                  # debug keystore
#   tools\sign-android.ps1 -KeystorePath D:\keys\cortex.jks -KeyAlias cortex  # release keystore
#   $env:CORTEX_KEYSTORE_PATH='D:\keys\cortex.jks'; tools\sign-android.ps1   # CI

param(
  [string]$KeystorePath,
  [string]$KeyAlias,
  [string]$KeystorePass,
  [string]$KeyPass,
  [string]$BuildTools = 'D:\android-sdk\build-tools\36.0.0',
  [string]$TauriDir   = 'D:\cortex-base'
)

$ErrorActionPreference = 'Stop'

# ── resolve keystore (4-level fallback) ──
$localProps = Join-Path $TauriDir 'local.properties'
if (-not $KeystorePath -and (Test-Path $localProps)) {
  $pairs = @{}
  Get-Content $localProps | ForEach-Object {
    if ($_ -match '^\s*([^=#]+?)\s*=\s*(.*?)\s*$') { $pairs[$Matches[1]] = $Matches[2] }
  }
  if (-not $KeystorePath -and $pairs['keystore.path'])     { $KeystorePath = $pairs['keystore.path'] }
  if (-not $KeyAlias   -and $pairs['keystore.alias'])     { $KeyAlias    = $pairs['keystore.alias'] }
  if (-not $KeystorePass -and $pairs['keystore.password']) { $KeystorePass = $pairs['keystore.password'] }
  if (-not $KeyPass    -and $pairs['keystore.keyPassword']) { $KeyPass    = $pairs['keystore.keyPassword'] }
}
if (-not $KeystorePath) { $KeystorePath = Join-Path $env:USERPROFILE '.android\debug.keystore' }
if (-not $KeyAlias)     { $KeyAlias     = 'androiddebugkey' }
if (-not $KeystorePass -and $KeystorePath -like '*.android\debug.keystore') {
  $KeystorePass = 'android'
  $KeyPass      = 'android'
}
if (-not $KeystorePass) { $KeystorePass = $env:CORTEX_KEYSTORE_PASS }
if (-not $KeyPass)      { $KeyPass      = $env:CORTEX_KEY_PASS }
if (-not $KeyPass)      { $KeyPass      = $KeystorePass }

if (-not (Test-Path $KeystorePath)) {
  throw "Keystore not found: $KeystorePath`nGenerate one with:`n  keytool -genkey -v -keystore $KeystorePath -alias $KeyAlias -keyalg RSA -keysize 2048 -validity 10000"
}


# ── ensure JAVA_HOME for apksigner.bat ──
if (-not $env:JAVA_HOME -or -not (Test-Path (Join-Path $env:JAVA_HOME 'bin\java.exe'))) {
  $candidates = @(
    'D:\codelabs\DevEco Studio\jbr',
    'D:\Program Files\Java\jdk-21',
    'D:\Program Files\Java\jdk-17',
    'D:\Program Files\Android Studio\jbr'
  )
  foreach ($cand in $candidates) {
    if (Test-Path (Join-Path $cand 'bin\java.exe')) { $env:JAVA_HOME = $cand; break }
  }
}
if (-not $env:JAVA_HOME) { throw 'JAVA_HOME not set and no JDK found in common locations' }
# ── locate tools ──
$apksigner = Join-Path $BuildTools 'apksigner.bat'
$zipalign  = Join-Path $BuildTools 'zipalign.exe'
if (-not (Test-Path $apksigner)) { throw "apksigner not found at $apksigner" }
if (-not (Test-Path $zipalign))  { throw "zipalign not found at $zipalign" }

# ── locate unsigned APK ──
$unsigned = Join-Path $TauriDir 'src-tauri\gen\android\app\build\outputs\apk\arm64\release\app-arm64-release-unsigned.apk'
if (-not (Test-Path $unsigned)) {
  throw "Unsigned APK not found at $unsigned`nBuild it first with: tools\build-android.ps1 -Target aarch64 (and use the release profile)"
}
$aligned   = [System.IO.Path]::ChangeExtension($unsigned, '.apk') -replace '-unsigned\.apk$', '-aligned.apk'
$signed    = [System.IO.Path]::ChangeExtension($unsigned, '.apk') -replace '-unsigned\.apk$', '.apk'

Write-Host '=== 1) zipalign 4-byte boundary (required before v2+ signing) ===' -ForegroundColor Cyan
& $zipalign -p -f 4 $unsigned $aligned
if ($LASTEXITCODE -ne 0) { throw "zipalign failed (exit $LASTEXITCODE)" }

Write-Host '=== 2) apksigner sign (v1+v2+v3, RSA-PSS) ===' -ForegroundColor Cyan
& $apksigner sign `
  --ks $KeystorePath `
  --ks-key-alias $KeyAlias `
  --ks-pass "pass:$KeystorePass" `
  --key-pass "pass:$KeyPass" `
  --v1-signing-enabled true `
  --v2-signing-enabled true `
  --v3-signing-enabled true `
  --out $signed $aligned
if ($LASTEXITCODE -ne 0) { throw "apksigner failed (exit $LASTEXITCODE)" }

Write-Host '=== 3) verify ===' -ForegroundColor Cyan
& $apksigner verify --verbose $signed | Select-Object -First 6

# Cleanup the aligned intermediate.
Remove-Item -LiteralPath $aligned -Force -ErrorAction SilentlyContinue

Write-Host ''
Write-Host '=== DONE ===' -ForegroundColor Green
Get-Item $signed | Select-Object Name, Length, FullName | Format-List
