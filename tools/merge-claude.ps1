$llm = "D:\cortex-base\src-tauri\src\llm.rs"
$addonPath = "D:\cortex-base\tools\claude-cards.txt"
$c = Get-Content -LiteralPath $llm -Raw
$addon = Get-Content -LiteralPath $addonPath -Raw
$lf = [Environment]::NewLine
# Anchor on the Claude-specific closing `format!("claude:{}", self.model)`.
$marker = '    fn name(&self) -> String {' + $lf + '        format!("claude:{}", self.model)' + $lf + '    }'
if (-not $c.Contains($marker)) { Write-Error 'claude name marker missing'; exit 1 }
$c = $c.Replace($marker, $addon + $marker)
Set-Content -LiteralPath $llm -Value $c -NoNewline
Remove-Item $addonPath -Force
"wrote claude override"