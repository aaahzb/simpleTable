# Retry: full-screen capture + bring app to front first
$ErrorActionPreference = "Continue"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win32b {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@
$p = Get-Process simpletable -ErrorAction SilentlyContinue
if ($null -eq $p) {
  $p = Start-Process -FilePath "D:\SVN_Cursor\simpleTable-public\src-tauri\target\release\simpletable.exe" -PassThru
  Start-Sleep -Seconds 2
  $p = Get-Process simpletable -ErrorAction SilentlyContinue
}
$h = $p.MainWindowHandle
[Win32b]::ShowWindow($h, 9) | Out-Null   # SW_RESTORE
[Win32b]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 800
$b = [System.Windows.Forms.SystemInformation]::VirtualScreen
Write-Output ("SCREEN=" + $b.Width + "x" + $b.Height + " @ " + $b.Left + "," + $b.Top)
$bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($b.Left, $b.Top, 0, 0, $bmp.Size)
$bmp.Save("D:\SVN_Cursor\simpleTable-public\shot_screen.png", [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Output "SAVED shot_screen.png"
