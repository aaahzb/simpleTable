# Fill grid via clipboard paste using SendKeys, then PrintWindow capture
$ErrorActionPreference = "Continue"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win32d {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@
$ws = New-Object -ComObject WScript.Shell
$p = Get-Process simpletable -ErrorAction SilentlyContinue
if ($null -eq $p) {
  $p = Start-Process -FilePath "D:\SVN_Cursor\simpleTable-public\src-tauri\target\release\simpletable.exe" -PassThru
  Start-Sleep -Seconds 2
  $p = Get-Process simpletable
}
$h = $p.MainWindowHandle
$ws.AppActivate($p.Id) | Out-Null
Start-Sleep -Milliseconds 400

# TSV: 目的地/评分/备注-like sample (目的,评分,备注 header comes from newFile? no - paste into empty grid does nothing)
# Grid is EMPTY (0x0). Paste into empty grid: paste expands? Model has 0 columns -> region paste needs cols.
# Instead: Ctrl+N? dialog needs interaction. Use Alt hotkeys? 新建 button has no alt key.
# Simplest: type into empty grid won't work. So: use clipboard text WITHOUT tab -> goes nowhere (no current cell).
# => We must create a table first. SendKeys to drive 新建 dialog: click 新建 via mouse_event, then Enter on dialog.
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win32e {
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint x, uint y, uint d, UIntPtr e);
}
"@
$r = New-Object Win32d+RECT
[Win32d]::GetWindowRect($h, [ref]$r) | Out-Null
# click 新建 button (~x=37 offset, y=48 client-ish)
$bx = $r.Left + 37; $by = $r.Top + 48
[Win32e]::SetCursorPos($bx, $by) | Out-Null
Start-Sleep -Milliseconds 200
[Win32e]::mouse_event(0x02, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 60
[Win32e]::mouse_event(0x04, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 600
# dialog: type column names into first input, then Enter (primary = OK)
$ws.AppActivate($p.Id) | Out-Null
Start-Sleep -Milliseconds 200
$colName = [char]0x76EE + [char]0x7684 + [char]0x5730  # mu di de
[System.Windows.Forms.SendKeys]::SendWait($colName)
Start-Sleep -Milliseconds 150
[System.Windows.Forms.SendKeys]::SendWait("{ENTER}")
Start-Sleep -Milliseconds 800
Write-Output "AFTER_NEW"

# now paste TSV into A1
$tab = [char]9; $nl = [char]13 + [char]10
$tsv = ([char]0x5927 + [char]0x7406 + $tab + "3" + $tab + [char]0x53E4 + [char]0x57CE + $nl +
        [char]0x4E3D + [char]0x6C5F + $tab + "10" + $tab + [char]0x96EA + [char]0x5C71 + $nl +
        [char]0x6CD5 + [char]0x6D77 + $tab + "5" + $tab + [char]0x6D77 + [char]0x9E3F + $nl)
[System.Windows.Forms.Clipboard]::SetText($tsv)
$ws.AppActivate($p.Id) | Out-Null
Start-Sleep -Milliseconds 200
# click first cell
$cx = $r.Left + 130; $cy = $r.Top + 130
[Win32e]::SetCursorPos($cx, $cy) | Out-Null
Start-Sleep -Milliseconds 200
[Win32e]::mouse_event(0x02, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 60
[Win32e]::mouse_event(0x04, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 300
[System.Windows.Forms.SendKeys]::SendWait("^v")
Start-Sleep -Milliseconds 800
Write-Output "AFTER_PASTE"

# capture
$bmp = New-Object System.Drawing.Bitmap ($r.Right - $r.Left), ($r.Bottom - $r.Top)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hd = $g.GetHdc()
[Win32d]::PrintWindow($h, $hd, 2) | Out-Null
$g.ReleaseHdc($hd)
$g.Dispose()
$bmp.Save("D:\SVN_Cursor\simpleTable-public\shot_data.png", [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output "SAVED shot_data.png"
