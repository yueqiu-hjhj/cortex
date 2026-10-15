$llm = "D:\cortex-base\src-tauri\src\llm.rs"
$addonPath = "D:\cortex-base\tools\gemini-cards.txt"
$c = Get-Content -LiteralPath $llm -Raw
$addon = Get-Content -LiteralPath $addonPath -Raw
$lf = [Environment]::NewLine
$marker = '    fn name(&self) -> String {' + $lf + '        format!("gemini:{}", self.model)' + $lf + '    }'
if (-not $c.Contains($marker)) { Write-Error 'gemini name marker missing'; exit 1 }
$c = $c.Replace($marker, $addon + $marker)
Set-Content -LiteralPath $llm -Value $c -NoNewline
Remove-Item $addonPath -Force
"wrote gemini override"