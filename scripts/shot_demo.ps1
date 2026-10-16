# 演示截图：选中「每日行程」多行单元格 → 右侧编辑窗格显示全文。
# 截法用屏幕直采（CopyFromScreen）——PrintWindow/PW_RENDERFULLCONTENT 对 WebView2
# 合成表面会丢编辑窗格/状态栏且布局失真（本轮实测对比确认），屏幕直采是显示真值。
# 用法: powershell -File shot_demo.ps1 <csv路径> <输出png> [右键次数,默认5] [裁剪顶部px,默认33]
param(
  [Parameter(Mandatory = $true)][string]$Csv,
  [Parameter(Mandatory = $true)][string]$Out,
  [int]$Col = 5,
  [int]$CropTop = 33
)
$ErrorActionPreference = "Continue"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win32e {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint x, uint y, uint d, int e);
  public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@
taskkill /IM simpletable.exe /F 2>$null | Out-Null
Start-Sleep -Milliseconds 500
$proc = Start-Process -FilePath "D:\SVN_Cursor\simpleTable-public\src-tauri\target\release\simpletable.exe" `
  -ArgumentList "`"$Csv`"" -PassThru
Start-Sleep -Seconds 4
$p = Get-Process simpletable | Select-Object -First 1
$h = [IntPtr]::Zero
$r = New-Object Win32e+RECT
$w = 0; $hh = 0
for ($i = 0; $i -lt 60; $i++) {
  $p.Refresh()
  $cand = $p.MainWindowHandle
  if ($cand -ne [IntPtr]::Zero -and [Win32e]::GetWindowRect($cand, [ref]$r) -and
      ($r.Right - $r.Left) -gt 100 -and ($r.Bottom - $r.Top) -gt 100) {
    $h = $cand; $w = $r.Right - $r.Left; $hh = $r.Bottom - $r.Top; break
  }
  Start-Sleep -Milliseconds 250
}
if ($h -eq [IntPtr]::Zero) { Write-Output "NO_MAIN_WINDOW"; exit 1 }
[Win32e]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 600
# 点第 1 数据行首列（大致位置即可，行内列用方向键精确走）
$cx = $r.Left + 185
$cy = $r.Top + 255
[Win32e]::SetCursorPos($cx, $cy) | Out-Null
Start-Sleep -Milliseconds 250
[Win32e]::mouse_event(0x0002, 0, 0, 0, 0)  # LDOWN
[Win32e]::mouse_event(0x0004, 0, 0, 0, 0)  # LUP
Start-Sleep -Milliseconds 300
$ws = New-Object -ComObject WScript.Shell
$ws.AppActivate($p.Id) | Out-Null
for ($i = 0; $i -lt $Col; $i++) { $ws.SendKeys("{RIGHT}"); Start-Sleep -Milliseconds 250; }
Start-Sleep -Milliseconds 700
# 屏幕直采窗口矩形（物理像素），再裁掉标题栏
Write-Output ("RECT=" + $w + "x" + $hh)
$bmp = New-Object System.Drawing.Bitmap $w, $hh
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, (New-Object System.Drawing.Size $w, $hh))
$g.Dispose()
$crop = New-Object System.Drawing.Bitmap $w, ($hh - $CropTop)
$cg = [System.Drawing.Graphics]::FromImage($crop)
$cg.DrawImage($bmp, (New-Object System.Drawing.Rectangle 0, 0, $w, ($hh - $CropTop)),
  (New-Object System.Drawing.Rectangle 0, $CropTop, $w, ($hh - $CropTop)), [System.Drawing.GraphicsUnit]::Pixel)
$cg.Dispose()
$crop.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$crop.Dispose(); $bmp.Dispose()
Write-Output ("SAVED=" + $Out)
