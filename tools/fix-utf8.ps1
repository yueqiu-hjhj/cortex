$ErrorActionPreference = 'Stop'
$f = 'D:\cortex-base\src-tauri\src\lib.rs'
$bytes = [System.IO.File]::ReadAllBytes($f)
Write-Output ("file: " + $bytes.Length + " bytes")
$out = New-Object 'System.Collections.Generic.List[byte]'
$i = 0
while ($i -lt $bytes.Length) {
    $b = $bytes[$i]
    if ($b -eq 0xE2 -and $i + 1 -lt $bytes.Length -and $bytes[$i+1] -eq 0x80 -and ($i + 2 -ge $bytes.Length -or $bytes[$i+2] -lt 0x80 -or $bytes[$i+2] -gt 0xBF)) {
        # Truncated em-dash sequence (E2 80 missing valid 3rd byte) -> replace with --
        $out.Add(0x2D)  # -
        $out.Add(0x2D)  # -
        $i += 2
        Write-Output "replaced truncated em-dash at offset $($i - 2)"
    } else {
        $out.Add($b)
        $i++
    }
}
[System.IO.File]::WriteAllBytes($f, $out.ToArray())
Write-Output "rewrote $f"
