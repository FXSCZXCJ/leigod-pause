# Send a left click to a window's client area (for GUI verification).
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\click_client.ps1 -TargetPid 1234 -X 127 -Y 14
param(
    [Parameter(Mandatory = $true)][int]$TargetPid,
    [Parameter(Mandatory = $true)][int]$X,
    [Parameter(Mandatory = $true)][int]$Y
)

Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class WinClick {
  public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr lParam);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr hWnd, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr hWnd, uint msg, IntPtr w, IntPtr l);

  public static IntPtr MainWindow(uint targetPid) {
    IntPtr found = IntPtr.Zero;
    EnumWindows(delegate(IntPtr hWnd, IntPtr lParam) {
      uint owner;
      GetWindowThreadProcessId(hWnd, out owner);
      if (owner == targetPid) {
        var cls = new StringBuilder(256);
        GetClassName(hWnd, cls, 256);
        if (cls.ToString() == "Window Class") { found = hWnd; return false; }
      }
      return true;
    }, IntPtr.Zero);
    return found;
  }
}
"@

$hwnd = [WinClick]::MainWindow([uint32]$TargetPid)
if ($hwnd -eq [IntPtr]::Zero) { Write-Output "pid=$TargetPid : main window not found"; exit 1 }

$lp = [IntPtr](($Y -shl 16) -bor ($X -band 0xFFFF))
[void][WinClick]::PostMessageW($hwnd, 0x0200, [IntPtr]::Zero, $lp)     # WM_MOUSEMOVE
[void][WinClick]::PostMessageW($hwnd, 0x0201, [IntPtr]1, $lp)          # WM_LBUTTONDOWN (MK_LBUTTON)
Start-Sleep -Milliseconds 120
[void][WinClick]::PostMessageW($hwnd, 0x0202, [IntPtr]::Zero, $lp)     # WM_LBUTTONUP
Write-Output "clicked client($X,$Y) on pid=$TargetPid"
