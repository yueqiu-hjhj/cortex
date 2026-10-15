$bytes = [System.IO.File]::ReadAllBytes('D:\cortex-base\src-tauri\src\lib.rs')
try {
    $head = $bytes[0..199] -as [byte[]]
    [void][System.Text.Encoding]::UTF8.GetString($head)
    Write-Output "first 200 bytes: valid UTF-8"
} catch {
    Write-Output ("first 200 bytes: FAILED -> " + $_.Exception.Message)
}
# Find first invalid byte position by stepping through.
$pos = 0
while ($pos -lt $bytes.Length) {
    $b = $bytes[$pos]
    if ($b -lt 0x80) { $pos++; continue }
    $seqLen = 0
    if ($b -ge 0xC2 -and $b -le 0xDF) { $seqLen = 2 }
    elseif ($b -ge 0xE0 -and $b -le 0xEF) { $seqLen = 3 }
    elseif ($b -ge 0xF0 -and $b -le 0xF4) { $seqLen = 4 }
    if ($seqLen -eq 0) { Write-Output "INVALID LEAD at offset $pos byte=0x$(('{0:X2}' -f $b))"; break }
    $bad = $false
    for ($j = 1; $j -lt $seqLen; $j++) {
        if ($pos + $j -ge $bytes.Length) { $bad = $true; break }
        $cb = $bytes[$pos + $j]
        if ($cb -lt 0x80 -or $cb -gt 0xBF) { $bad = $true; break }
    }
    if ($bad) {
        $hex = ''
        for ($j = 0; $j -lt [Math]::Min($seqLen, $bytes.Length - $pos); $j++) { $hex += ('{0:X2} ' -f $bytes[$pos + $j]) }
        Write-Output "TRUNCATED at offset $pos (lead 0x$(('{0:X2}' -f $b)) expected $seqLen bytes got: $hex)"
        break
    }
    $pos += $seqLen
}
