$llm = "D:\cortex-base\src-tauri\src\llm.rs"
$addonPath = "D:\cortex-base\tools\openai-cards.txt"
$c = Get-Content -LiteralPath $llm -Raw
$addon = Get-Content -LiteralPath $addonPath -Raw
$lf = [Environment]::NewLine
# The OpenAI-compat impl closes with a `gen_image` fn followed by `fn name`; anchor
# on `gen_image` + opening of `fn name` so we don't match Gemini's name marker.
$marker = '    fn gen_image(&self, prompt: &str) -> Result<String> {' + $lf + '        let key = self.api_key.trim();'
if (-not $c.Contains($marker)) { Write-Error 'openai gen_image marker missing'; exit 1 }
$c = $c.Replace($marker, $addon + $lf + $marker)
Set-Content -LiteralPath $llm -Value $c -NoNewline
Remove-Item $addonPath -Force
"wrote openai override"