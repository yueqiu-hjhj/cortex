$ErrorActionPreference = 'Stop'
$f = 'D:\cortex-base\src-tauri\src\lib.rs'
$bytes = [System.IO.File]::ReadAllBytes($f)
$out = New-Object 'System.Collections.Generic.List[byte]'
$i = 0
$replaced = 0
while ($i -lt $bytes.Length) {
    $b = $bytes[$i]
    if ($b -lt 0x80) {
        $out.Add($b)
        $i++
        continue
    }
    # Multi-byte sequence: collect [b, cont1, cont2?] and validate
    $seqLen = 0
    if ($b -ge 0xC2 -and $b -le 0xDF) { $seqLen = 2 }
    elseif ($b -ge 0xE0 -and $b -le 0xEF) { $seqLen = 3 }
    elseif ($b -ge 0xF0 -and $b -le 0xF4) { $seqLen = 4 }
    else {
        # Stray continuation or invalid leading byte -> drop
        $replaced++
        $i++
        continue
    }
    $ok = $true
    for ($j = 1; $j -lt $seqLen; $j++) {
        if ($i + $j -ge $bytes.Length) { $ok = $false; break }
        $cb = $bytes[$i + $j]
        if ($cb -lt 0x80 -or $cb -gt 0xBF) { $ok = $false; break }
    }
    if (-not $ok) {
        # Drop the leading byte (and any stray continuations) until we hit an ASCII or new lead
        $replaced++
        $i++
        continue
    }
    # Whole sequence is valid -> copy as-is
    for ($j = 0; $j -lt $seqLen; $j++) { $out.Add($bytes[$i + $j]) }
    $i += $seqLen
}
[System.IO.File]::WriteAllBytes($f, $out.ToArray())
Write-Output "replaced $replaced invalid bytes"
