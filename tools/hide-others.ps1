# Minimize every top-level window whose class is a Chromium widget (the Codex
# app, Chrome, Edge, ...) so the Cortex window is not covered when we capture.
# SetForegroundWindow is deliberately avoided: it can block for 30s+ here.
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class HW {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  public static List<string> Hide(StringBuilder sb) {
    var res = new List<string>();
    EnumWindows((h,l) => {
      if (!IsWindowVisible(h)) return true;
      var c = new StringBuilder(256); GetClassName(h, c, 256);
      var t = new StringBuilder(256); GetWindowText(h, t, 256);
      if (c.ToString().Contains("Chrome_WidgetWin_1") && !c.ToString().Contains("Tauri")) {
        ShowWindow(h, 6); // SW_MINIMIZE
        res.Add(c + " | " + t);
      }
      return true;
    }, IntPtr.Zero);
    return res;
  }
}
"@
$rows = [HW]::Hide((New-Object System.Text.StringBuilder))
if ($rows.Count -eq 0) { Write-Output "nothing to hide" } else { $rows | ForEach-Object { "minimized: $_" } }
