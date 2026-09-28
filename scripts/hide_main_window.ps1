# 把主界面窗口隐藏回托盘：按 进程 PID + 窗口类名 找到主窗口后发 WM_CLOSE
# （程序自身会拦截 WM_CLOSE 改为「隐藏到托盘」）
# 用法: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\hide_main_window.ps1 -TargetPid 1234
param([Parameter(Mandatory = $true)][int]$TargetPid)

Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class WinHide {
  public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr lParam);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr hWnd, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr hWnd, uint msg, IntPtr w, IntPtr l);

  public static int CloseMainWindow(uint targetPid) {
    int sent = 0;
    EnumWindows(delegate(IntPtr hWnd, IntPtr lParam) {
      uint owner;
      GetWindowThreadProcessId(hWnd, out owner);
      if (owner == targetPid) {
        var cls = new StringBuilder(256);
        GetClassName(hWnd, cls, 256);
        if (cls.ToString() == "Window Class") {   // winit 的主窗口类名
          PostMessageW(hWnd, 0x0010, IntPtr.Zero, IntPtr.Zero);  // WM_CLOSE
          sent++;
        }
      }
      return true;
    }, IntPtr.Zero);
    return sent;
  }
}
"@

$n = [WinHide]::CloseMainWindow([uint32]$TargetPid)
if ($n -eq 0) {
    Write-Output "pid=$TargetPid 未找到主窗口（可能没在运行）"
} else {
    Write-Output "已向 $n 个主窗口发送 WM_CLOSE（应隐藏到托盘）"
}
