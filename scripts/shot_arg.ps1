# Launch release exe with CSV arg, wait for load, PrintWindow capture
$ErrorActionPreference = "Continue"
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win32f {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@
Get-Process simpletable -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 400
$arg = "C:\Users\aaahzb\AppData\Local\Temp\simpletable_shot_fixture.csv"
$p = Start-Process -FilePath "D:\SVN_Cursor\simpleTable-public\src-tauri\target\release\simpletable.exe" -ArgumentList "`"$arg`"" -PassThru
$sw = [Diagnostics.Stopwatch]::StartNew()
while ($sw.Elapsed.TotalSeconds -lt 15) {
  $p.Refresh()
  if ($p.MainWindowHandle -ne 0 -and $p.MainWindowTitle -ne "") { break }
  Start-Sleep -Milliseconds 50
}
$sw.Stop()
Write-Output ("STARTUP_MS=" + $sw.ElapsedMilliseconds)
Write-Output ("TITLE=" + $p.MainWindowTitle)
Start-Sleep -Milliseconds 1200  # allow CSV load + render
$r = New-Object Win32f+RECT
[Win32f]::GetWindowRect($p.MainWindowHandle, [ref]$r) | Out-Null
$bmp = New-Object System.Drawing.Bitmap ($r.Right - $r.Left), ($r.Bottom - $r.Top)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hd = $g.GetHdc()
[Win32f]::PrintWindow($p.MainWindowHandle, $hd, 2) | Out-Null
$g.ReleaseHdc($hd)
$g.Dispose()
$bmp.Save("D:\SVN_Cursor\simpleTable-public\shot_data.png", [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output "SAVED shot_data.png"
