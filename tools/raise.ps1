# Force-show the hidden Cortex Tauri window and park it at a known spot.
# Kept separate from win.ps1's "show" because that one calls SetForegroundWindow,
# which blocks for 30s+ on this machine.
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class RW {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int cx, int cy, uint f);
  public static IntPtr Find(uint target) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h,l) => {
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (pid == target) {
        var c = new StringBuilder(256); GetClassName(h, c, 256);
        if (c.ToString().Contains("Tauri")) { found = h; return false; }
      }
      return true;
    }, IntPtr.Zero);
    return found;
  }
}
"@

$proc = Get-Process cortex -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $proc) { Write-Output "no cortex.exe running"; exit 1 }

$h = [RW]::Find([uint32]$proc.Id)
if ($h -eq [IntPtr]::Zero) { Write-Output "no Tauri window"; exit 1 }

Write-Output "handle=$($h.ToInt64())"
[RW]::ShowWindow($h, 5) | Out-Null   # SW_SHOW
Write-Output "show -> ok"
[RW]::SetWindowPos($h, [IntPtr]0, 40, 30, 1440, 920, 0x40) | Out-Null  # HWND_TOP, no activate
Write-Output "moved -> ok"

# WebView2 throttles its compositor while the window is hidden, so a window that
# was force-shown after boot can stay unpainted (flat white). A one-pixel resize
# is enough to make it re-layout and paint.
for ($i = 0; $i -lt 3; $i++) {
  [RW]::SetWindowPos($h, [IntPtr]0, 40, 30, 1439 + $i, 919 + $i, 0x40) | Out-Null
  Start-Sleep -Milliseconds 250
}
[RW]::SetWindowPos($h, [IntPtr]0, 40, 30, 1440, 920, 0x40) | Out-Null
Write-Output "nudged -> ok"
