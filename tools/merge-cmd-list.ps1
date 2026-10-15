$lib = "D:\cortex-base\src-tauri\src\lib.rs"
$c = Get-Content -LiteralPath $lib -Raw
$lf = [Environment]::NewLine
$marker = 'commands::ocr_image_bytes,'
if (-not $c.Contains($marker)) { Write-Error 'ocr_image_bytes list entry missing'; exit 1 }
# PowerShell .NET Replace takes (old, new) only; do first-occurrence replacement manually.
$idx = $c.IndexOf($marker)
if ($idx -lt 0) { Write-Error 'index not found'; exit 1 }
$end = $idx + $marker.Length
$addition = $lf + '    commands::extract_flashcards_from_image,' + $lf + '    commands::save_flashcards_to_subject,'
$c2 = $c.Substring(0, $end) + $addition + $c.Substring($end)
Set-Content -LiteralPath $lib -Value $c2 -NoNewline
"registered 2 commands"