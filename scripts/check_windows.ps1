# 列出指定进程的所有顶层窗口及可见性（验证「静默进托盘」是否真的没有可见窗口）
# 用法: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check_windows.ps1 -TargetPid 1234
param([Parameter(Mandatory = $true)][int]$TargetPid)

Add-Type @"
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class WinEnum {
  public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr lParam);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr hWnd, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr hWnd, StringBuilder s, int n);

  public static List<string> Describe(uint targetPid) {
    var rows = new List<string>();
    EnumWindows(delegate(IntPtr hWnd, IntPtr lParam) {
      uint owner;
      GetWindowThreadProcessId(hWnd, out owner);
      if (owner == targetPid) {
        var cls = new StringBuilder(256);
        GetClassName(hWnd, cls, 256);
        var txt = new StringBuilder(256);
        GetWindowText(hWnd, txt, 256);
        rows.Add(String.Format("pid={0} visible={1} class={2} title={3}", owner, IsWindowVisible(hWnd), cls, txt));
      }
      return true;
    }, IntPtr.Zero);
    return rows;
  }
}
"@

$rows = [WinEnum]::Describe([uint32]$TargetPid)
if ($rows.Count -eq 0) {
    Write-Output "pid=$TargetPid 没有任何顶层窗口"
} else {
    $rows | ForEach-Object { Write-Output $_ }
}
