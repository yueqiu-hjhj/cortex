$ErrorActionPreference = 'Continue'
Get-ChildItem -Path 'D:\cortex-base\src-tauri\src' -Filter '*.rs' -Recurse | ForEach-Object {
    $f = $_.FullName
    $bytes = [System.IO.File]::ReadAllBytes($f)
    $out = New-Object 'System.Collections.Generic.List[byte]'
    $i = 0
    $replaced = 0
    while ($i -lt $bytes.Length) {
        $b = $bytes[$i]
        if ($b -lt 0x80) { $out.Add($b); $i++; continue }
        $seqLen = 0
        if ($b -ge 0xC2 -and $b -le 0xDF) { $seqLen = 2 }
        elseif ($b -ge 0xE0 -and $b -le 0xEF) { $seqLen = 3 }
        elseif ($b -ge 0xF0 -and $b -le 0xF4) { $seqLen = 4 }
        else { $replaced++; $i++; continue }
        $ok = $true
        for ($j = 1; $j -lt $seqLen; $j++) {
            if ($i + $j -ge $bytes.Length) { $ok = $false; break }
            $cb = $bytes[$i + $j]
            if ($cb -lt 0x80 -or $cb -gt 0xBF) { $ok = $false; break }
        }
        if (-not $ok) { $replaced++; $i++; continue }
        for ($j = 0; $j -lt $seqLen; $j++) { $out.Add($bytes[$i + $j]) }
        $i += $seqLen
    }
    if ($replaced -gt 0) {
        [System.IO.File]::WriteAllBytes($f, $out.ToArray())
        Write-Output ("$f -> replaced $replaced")
    }
}
