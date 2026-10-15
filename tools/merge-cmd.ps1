$cmd = "D:\cortex-base\src-tauri\src\commands.rs"
$addonPath = "D:\cortex-base\tools\ocr-cmd.txt"
$c = Get-Content -LiteralPath $cmd -Raw
$addon = Get-Content -LiteralPath $addonPath -Raw
$lf = [Environment]::NewLine
# Insert after the `ocr_image_bytes` function (right after the `model.ocr(...)` line).
$marker = '    model.ocr(&[(mime, base64)])' + $lf + '}'
if (-not $c.Contains($marker)) { Write-Error 'ocr_image_bytes marker missing'; exit 1 }
$c = $c.Replace($marker, $marker + $lf + $addon)
Set-Content -LiteralPath $cmd -Value $c -NoNewline
Remove-Item $addonPath -Force
"inserted 2 commands"