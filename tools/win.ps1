# Small helper for driving / capturing the Cortex desktop window while
# developing the UI. Run with: powershell -ExecutionPolicy Bypass -File tools\win.ps1 <cmd>
param(
  [string]$Cmd = "list",
  [int]$X = 60, [int]$Y = 40, [int]$W = 1440, [int]$H = 920,
  [string]$Out = "D:\cortex-base\shot.png"
)

Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class WG {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RC r);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int cx, int cy, uint f);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsZoomed(IntPtr h);
  [StructLayout(LayoutKind.Sequential)] public struct RC { public int L,T,R,B; }

  public static List<string> Rows(uint target) {
    var res = new List<string>();
    EnumWindows((h,l) => {
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (pid == target) {
        var t = new StringBuilder(256); GetWindowText(h, t, 256);
        var c = new StringBuilder(256); GetClassName(h, c, 256);
        RC r; GetWindowRect(h, out r);
        res.Add(h.ToInt64()+"|vis="+IsWindowVisible(h)+"|zoom="+IsZoomed(h)+"|"+c+"|"+t+"|"+r.L+","+r.T+","+r.R+","+r.B);
      }
      return true;
    }, IntPtr.Zero);
    return res;
  }
  public static IntPtr MainFor(uint target) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h,l) => {
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (pid == target && IsWindowVisible(h)) {
        var c = new StringBuilder(256); GetClassName(h, c, 256);
        if (c.ToString().Contains("Tauri")) { found = h; return false; }
      }
      return true;
    }, IntPtr.Zero);
    return found;
  }
}
"@

Add-Type -AssemblyName System.Windows.Forms, System.Drawing

$proc = Get-Process cortex -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $proc) { Write-Output "no cortex.exe running"; exit 1 }

function Shot([string]$path) {
  $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
  $bmp = New-Object System.Drawing.Bitmap($b.Width, $b.Height)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($b.X, $b.Y, 0, 0, $bmp.Size)
  $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $bmp.Dispose()
  Write-Output "shot -> $path"
}

switch ($Cmd) {
  "list" {
    [WG]::Rows([uint32]$proc.Id) | ForEach-Object { $_ }
  }
  "topshot" {
    # Move the window on-screen, raise it to the top without activating it
    # (SetForegroundWindow can hang), then capture the full screen.
    $h = [WG]::MainFor([uint32]$proc.Id)
    if ($h -eq [IntPtr]::Zero) { Write-Output "no Tauri window"; exit 1 }
    [WG]::ShowWindow($h, 9) | Out-Null
    [WG]::SetWindowPos($h, [IntPtr](-1), $X, $Y, $W, $H, 0x40) | Out-Null
    Start-Sleep -Milliseconds 1200
    Shot $Out
    $r = New-Object WG+RC
    [WG]::GetWindowRect($h, [ref]$r) | Out-Null
    Write-Output "handle=$($h.ToInt64()) rect=$($r.L),$($r.T),$($r.R),$($r.B)"
  }
  "show" {
    $h = [WG]::MainFor([uint32]$proc.Id)
    if ($h -eq [IntPtr]::Zero) { Write-Output "no Tauri window"; exit 1 }
    [WG]::ShowWindow($h, 3) | Out-Null          # SW_MAXIMIZE
    [WG]::ShowWindow($h, 9) | Out-Null          # SW_RESTORE
    [WG]::SetWindowPos($h, [IntPtr](-1), $X, $Y, $W, $H, 0x40) | Out-Null
    Start-Sleep -Milliseconds 600
    [WG]::SetForegroundWindow($h) | Out-Null
    Start-Sleep -Milliseconds 800
    $r = New-Object WG+RC
    [WG]::GetWindowRect($h, [ref]$r) | Out-Null
    Write-Output "handle=$($h.ToInt64()) rect=$($r.L),$($r.T),$($r.R),$($r.B)"
  }
  "shot" { Shot $Out }
  "focus" {
    $h = [WG]::MainFor([uint32]$proc.Id)
    [WG]::SetForegroundWindow($h) | Out-Null
    Write-Output "focused $($h.ToInt64())"
  }
  default { Write-Output "usage: win.ps1 [list|show|shot|focus]" }
}
