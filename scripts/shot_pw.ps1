# PrintWindow with PW_RENDERFULLCONTENT (DWM composites WebView2 content)
$ErrorActionPreference = "Continue"
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win32c {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@
$p = Get-Process simpletable -ErrorAction SilentlyContinue
if ($null -eq $p) { Write-Output "NO_PROCESS"; exit 1 }
$h = $p.MainWindowHandle
$r = New-Object Win32c+RECT
[Win32c]::GetWindowRect($h, [ref]$r) | Out-Null
$w = $r.Right - $r.Left; $hh = $r.Bottom - $r.Top
Write-Output ("RECT=" + $w + "x" + $hh)
$bmp = New-Object System.Drawing.Bitmap $w, $hh
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hd = $g.GetHdc()
$ok = [Win32c]::PrintWindow($h, $hd, 2)  # PW_RENDERFULLCONTENT
$g.ReleaseHdc($hd)
$g.Dispose()
$bmp.Save("D:\SVN_Cursor\simpleTable-public\shot_pw.png", [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output ("PRINTWINDOW_OK=" + $ok)
