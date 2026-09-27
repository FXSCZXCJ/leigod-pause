Add-Type @"
using System;
using System.Runtime.InteropServices;
public class WMsg {
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowExW(IntPtr p, IntPtr c, string cls, IntPtr name);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
}
"@
$h = [WMsg]::FindWindowExW([IntPtr](-3), [IntPtr]::Zero, "LegodPauseShutdownWnd", [IntPtr]::Zero)
if ($h -eq [IntPtr]::Zero) { Write-Output "NOT_FOUND"; exit 1 }
Write-Output ("FOUND: 0x" + $h.ToString('X'))
$ok1 = [WMsg]::PostMessage($h, 0x0011, [IntPtr]::Zero, [IntPtr]::Zero)   # WM_QUERYENDSESSION
Start-Sleep -Milliseconds 500
$ok2 = [WMsg]::PostMessage($h, 0x0016, [IntPtr]::One,  [IntPtr]::One)    # WM_ENDSESSION wParam=TRUE
Write-Output ("POSTED_QES=$ok1 POSTED_ES=$ok2")
