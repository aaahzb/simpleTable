# simpletable release exe: startup timing + interactive fill + screenshots
$ErrorActionPreference = "Continue"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win32 {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint x, uint y, uint d, UIntPtr e);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@
$exe = "D:\SVN_Cursor\simpleTable-public\src-tauri\target\release\simpletable.exe"
$shotEmpty = "D:\SVN_Cursor\simpleTable-public\shot_empty.png"
$shotData = "D:\SVN_Cursor\simpleTable-public\shot_data.png"

# kill stale
Get-Process simpletable -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 300

# launch + measure time-to-window
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath $exe -PassThru
$title = ""
while ($sw.Elapsed.TotalSeconds -lt 15) {
  $p.Refresh()
  if ($p.MainWindowHandle -ne 0 -and $p.MainWindowTitle -ne "") { $title = $p.MainWindowTitle; break }
  Start-Sleep -Milliseconds 50
}
$sw.Stop()
Write-Output ("STARTUP_MS=" + $sw.ElapsedMilliseconds)
Write-Output ("TITLE=" + $title)
Write-Output ("HANDLE=" + $p.MainWindowHandle)

function Shot($path) {
  $r = New-Object Win32+RECT
  [Win32]::GetWindowRect($p.MainWindowHandle, [ref]$r) | Out-Null
  $w = $r.Right - $r.Left; $h = $r.Bottom - $r.Top
  $bmp = New-Object System.Drawing.Bitmap $w, $h
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
  $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $bmp.Dispose()
  Write-Output ("SAVED " + $path + " " + $w + "x" + $h)
}

Start-Sleep -Milliseconds 600
[Win32]::SetForegroundWindow($p.MainWindowHandle) | Out-Null
Start-Sleep -Milliseconds 300
Shot $shotEmpty

# fill via clipboard paste: click grid area, Ctrl+V
$rr = New-Object Win32+RECT
[Win32]::GetWindowRect($p.MainWindowHandle, [ref]$rr) | Out-Null
$cx = $rr.Left + [int](($rr.Right - $rr.Left) * 0.30)
$cy = $rr.Top + [int](($rr.Bottom - $rr.Top) * 0.45)
[Win32]::SetCursorPos($cx, $cy) | Out-Null
Start-Sleep -Milliseconds 200
[Win32]::mouse_event(0x02, 0, 0, 0, [UIntPtr]::Zero)  # L down
Start-Sleep -Milliseconds 80
[Win32]::mouse_event(0x04, 0, 0, 0, [UIntPtr]::Zero)  # L up
Start-Sleep -Milliseconds 300

$tab = [char]9; $nl = [char]13 + [char]10
$tsv = ([char]0x5927 + [char]0x7406 + $tab + "3" + $tab + [char]0x53E4 + [char]0x57CE + $nl +
        [char]0x4E3D + [char]0x6C5F + $tab + "10" + $tab + [char]0x96EA + [char]0x5C71 + $nl +
        [char]0x6CD5 + [char]0x6D77 + $tab + "5" + $tab + [char]0x6D77 + [char]0x9E3F + $nl)
[System.Windows.Forms.Clipboard]::SetText($tsv)
Start-Sleep -Milliseconds 200
[System.Windows.Forms.SendKeys]::SendWait("^v")
Start-Sleep -Milliseconds 800
Shot $shotData

# zoom out view via Ctrl+wheel? skip; leave as is
Write-Output "DONE"
