$llm = "D:\cortex-base\src-tauri\src\llm.rs"
$addonPath = "D:\cortex-base\tools\ocr-llm-addon.txt"
$c = Get-Content -LiteralPath $llm -Raw
$addon = Get-Content -LiteralPath $addonPath -Raw
$marker = "use crate::error::{Error, Result};"
$replacement = $marker + "`n" + $addon.TrimStart("`r`n")
$c = $c.Replace($marker, $replacement)
Set-Content -LiteralPath $llm -Value $c -NoNewline
Remove-Item $addonPath -Force
"wrote $llm"