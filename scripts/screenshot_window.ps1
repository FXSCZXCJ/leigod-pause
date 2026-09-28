# Capture the main window (winit class name "Window Class") of a process, to verify
# UI colors / fonts. Uses PrintWindow(PW_RENDERFULLCONTENT) so only the window renders
# into a memory DC -- the desktop behind it is never captured, and occlusion is fine.
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\screenshot_window.ps1 -TargetPid 1234 -Out shot.png
param(
    [Parameter(Mandatory = $true)][int]$TargetPid,
    [string]$Out = "$env:TEMP\legod_window.png"
)

Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class WinShot {
  public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr lParam);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr hWnd, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdcBlt, uint flags);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr hWnd, int attr, out RECT rect, int size);

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

$hwnd = [WinShot]::MainWindow([uint32]$TargetPid)
if ($hwnd -eq [IntPtr]::Zero) {
    Write-Output "pid=$TargetPid : main window not found"
    exit 1
}
if (-not [WinShot]::IsWindowVisible($hwnd)) {
    Write-Output "NOTE: main window is hidden; the capture may be blank (open it with --show first)"
}

$r = New-Object WinShot+RECT
[void][WinShot]::GetWindowRect($hwnd, [ref]$r)
$w = $r.Right - $r.Left
$h = $r.Bottom - $r.Top
if ($w -le 0 -or $h -le 0) { Write-Output "bad window size: ${w}x${h}"; exit 1 }

Add-Type -AssemblyName System.Drawing
$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
$ok = [WinShot]::PrintWindow($hwnd, $hdc, 2)   # 2 = PW_RENDERFULLCONTENT
$g.ReleaseHdc($hdc)
$g.Dispose()

# GetWindowRect 含 Win11 的"不可见调整边框"（左右各约 8px、底部约 8px），
# 那部分不会被绘制，直接保存会出现黑边 -> 按 DWM 可见边界裁剪
$fr = New-Object WinShot+RECT
$dwm = [WinShot]::DwmGetWindowAttribute($hwnd, 9, [ref]$fr, [System.Runtime.InteropServices.Marshal]::SizeOf($fr))
$final = $bmp
if ($dwm -eq 0) {
    $cx = $fr.Left - $r.Left
    $cy = $fr.Top - $r.Top
    $cw = $fr.Right - $fr.Left
    $ch = $fr.Bottom - $fr.Top
    if ($cw -gt 0 -and $ch -gt 0 -and $cx -ge 0 -and $cy -ge 0 -and ($cx + $cw) -le $w -and ($cy + $ch) -le $h) {
        $crop = New-Object System.Drawing.Rectangle $cx, $cy, $cw, $ch
        $final = $bmp.Clone($crop, $bmp.PixelFormat)
        Write-Output "cropped to visible frame: offset=($cx,$cy) size=${cw}x${ch}"
    }
}

$finalW = $final.Width
$finalH = $final.Height
$final.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$final.Dispose()
$bmp.Dispose()
if ($ok) {
    Write-Output "saved: $Out (${finalW}x${finalH})"
} else {
    Write-Output "PrintWindow failed; saved (possibly blank): $Out"
}
