# Build a debug APK for aarch64 (most common phone arch). The Tauri CLI hits two
# known machine-level blockers on Windows: it tries to symlink the .so into
# jniLibs/, and gradle must download its distribution. This script does the
# same thing tauri android build does, but uses Copy-Item instead of symlink
# and tells you exactly where the APK lands.
param(
  [string]$Target = 'aarch64',
  [string]$TauriDir = 'D:\cortex-base',
  [string]$JavaHome = 'D:\codelabs\DevEco Studio\jbr',
  [string]$AndroidHome = 'D:\android-sdk',
  [string]$NdkVersion = '26.3.11579264'
)
$ErrorActionPreference = 'Stop'
$env:JAVA_HOME = $JavaHome
$env:ANDROID_HOME = $AndroidHome
$env:ANDROID_SDK_ROOT = $AndroidHome
$env:ANDROID_NDK_ROOT = Join-Path $AndroidHome "ndk\$NdkVersion"
$env:PATH = "$JavaHome\bin;$AndroidHome\platform-tools;$AndroidHome\cmdline-tools\latest\bin;$env:PATH"

$profile = 'debug'
$so = Join-Path $TauriDir "src-tauri\target\$Target-linux-android\$profile\libcortex_lib.so"
$jniArch = switch ($Target) {
  'aarch64'           { 'arm64-v8a' }
  'armv7-linux-androideabi' { 'armeabi-v7a' }
  'i686-linux-android' { 'x86' }
  'x86_64-linux-android' { 'x86_64' }
  default { throw "unknown target arch: $Target" }
}
$jniDir = Join-Path $TauriDir "src-tauri\gen\android\app\src\main\jniLibs\$jniArch"
$apk = Join-Path $TauriDir "src-tauri\gen\android\app\build\outputs\apk\$profile\app-$profile.apk"

Write-Host "=== 1) cargo build (tauri android build up to the symlink step) ===" -ForegroundColor Cyan
Push-Location $TauriDir
try {
  npx tauri android build --debug --apk --target $Target 2>&1 | Select-String -Pattern 'Finished|error|symlink|Error' | ForEach-Object { $_.Line }
} catch {
  Write-Host "(tauri android build reported an error, that's fine \u2014 the .so is what we need)" -ForegroundColor Yellow
}
Pop-Location

if (-not (Test-Path $so)) {
  throw "Rust .so not found at $so"
}

Write-Host "=== 2) copy .so into jniLibs (replaces the failed symlink) ===" -ForegroundColor Cyan
if (-not (Test-Path $jniDir)) { New-Item -ItemType Directory -Path $jniDir -Force | Out-Null }
Copy-Item -LiteralPath $so -Destination (Join-Path $jniDir 'libcortex_lib.so') -Force
Get-ChildItem $jniDir | Select-Object Name, Length

Write-Host "=== 3) gradle :app:assembleDebug ===" -ForegroundColor Cyan
Push-Location (Join-Path $TauriDir 'src-tauri\gen\android')
& .\gradlew.bat :app:assembleDebug
Pop-Location

if (Test-Path $apk) {
  Write-Host "=== DONE ===" -ForegroundColor Green
  Get-Item $apk | Select-Object Name, Length, FullName
} else {
  Write-Host "APK not found at $apk \u2014 check gradle output above" -ForegroundColor Red
}