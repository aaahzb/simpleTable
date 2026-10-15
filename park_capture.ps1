# park_capture.ps1 - capture screenshots of a parked simpletable window (--park mode).
# Usage: powershell -ExecutionPolicy Bypass -File park_capture.ps1 -mode wall4m
# ASCII-only string literals on purpose: PS 5.1 reads BOM-less UTF-8 as ANSI.
#
# Capture reliability notes (hard-won):
# - Background/occluded WebView2 windows do NOT recomposite after programmatic
#   scroll; PrintWindow then serves a stale pre-scroll frame. Every capture must
#   nudge REAL input (cursor + wheel) so Chromium commits a fresh frame.
# - The process has a tiny helper window; pick the largest visible top-level one.
param(
  [Parameter(Mandatory = $true)][string]$mode,
  [string]$outPrefix = "park"
)

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms

$sig = @"
using System;
using System.Runtime.InteropServices;
public class Win32 {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int hh, bool repaint);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@
Add-Type -TypeDefinition $sig

function Get-MainHwnd([int]$procId) {
  $best = [IntPtr]::Zero; $bestArea = 0
  $cb = [Win32+EnumProc]{
    param($h, $l)
    $wp = 0
    [Win32]::GetWindowThreadProcessId($h, [ref]$wp) | Out-Null
    if ($wp -eq $procId -and [Win32]::IsWindowVisible($h)) {
      $r = New-Object Win32+RECT
      [Win32]::GetWindowRect($h, [ref]$r) | Out-Null
      $a = [Math]::Max(0, $r.Right - $r.Left) * [Math]::Max(0, $r.Bottom - $r.Top)
      if ($a -gt $bestArea) { $script:bestArea = $a; $script:bestHwnd = $h }
    }
    return $true
  }
  $script:bestHwnd = [IntPtr]::Zero; $script:bestArea = 0
  [Win32]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
  return $script:bestHwnd
}

function Save-Capture($hwnd, $path) {
  $rect = New-Object Win32+RECT
  [Win32]::GetWindowRect($hwnd, [ref]$rect) | Out-Null
  $w = $rect.Right - $rect.Left
  $h = $rect.Bottom - $rect.Top
  $bmp = New-Object System.Drawing.Bitmap $w, $h
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  # PW_RENDERFULLCONTENT: force offscreen render even if occluded
  $ok = [Win32]::PrintWindow($hwnd, $g.GetHdc(), 2)
  $g.ReleaseHdc()
  $g.Dispose()
  # blank detection: sample variance
  $var = 0
  try {
    $c1 = $bmp.GetPixel(20, 20).ToArgb(); $c2 = $bmp.GetPixel([Math]::Min(100, $w - 1), [Math]::Min(60, $h - 1)).ToArgb()
    $c3 = $bmp.GetPixel([Math]::Min(400, $w - 1), [Math]::Min(300, $h - 1)).ToArgb()
    if ($c1 -ne $c2 -or $c2 -ne $c3) { $var = 1 }
  } catch {}
  $src = "print"
  if (-not $ok -or $var -eq 0) {
    $bmp.Dispose()
    $bmp = New-Object System.Drawing.Bitmap $w, $h
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bmp.Size)
    $g.Dispose()
    $src = "screen"
  }
  $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
  $bmp.Dispose()
  Write-Host "saved $path (${w}x${h}) via $src"
}

# Real-input nudge: move cursor into the grid area and wheel down+up.
# Forces Chromium to commit a genuine compositor frame at the current scroll
# position (programmatic scrollTop alone leaves stale frames while background).
function Nudge-Repaint($hwnd) {
  $rect = New-Object Win32+RECT
  [Win32]::GetWindowRect($hwnd, [ref]$rect) | Out-Null
  $cx = $rect.Left + [Math]::Max(60, [int](($rect.Right - $rect.Left) / 2))
  $cy = $rect.Top + [Math]::Max(200, [int](($rect.Bottom - $rect.Top) / 2))
  [Win32]::SetCursorPos($cx, $cy) | Out-Null
  Start-Sleep -Milliseconds 120
  [Win32]::mouse_event(0x0800, 0, 0, 120, [UIntPtr]::Zero)  # WHEEL down 1 notch
  Start-Sleep -Milliseconds 250
  [Win32]::mouse_event(0x0800, 0, 0, ([uint32]0xFFFFFF38), [UIntPtr]::Zero)  # WHEEL up 1 notch (-120)
  Start-Sleep -Milliseconds 350
}

$dir = $env:TEMP

$proc = $null
for ($t = 0; $t -lt 120; $t += 1) {
  $proc = Get-Process simpletable -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($proc) { break }
  Start-Sleep -Milliseconds 500
}
if (-not $proc) { Write-Host "simpletable process not found"; exit 1 }
# clean leftovers from PREVIOUS rounds only: markers older than this process.
# (Deleting fresh markers would deadlock the handshake: the app parks in waitGo.)
foreach ($pat in @("ready_*.txt","go_*.txt","done.txt","error.txt","diag_*.txt","hello.txt","step_*.txt","t_*.txt")) {
  Get-ChildItem -Path $dir -Filter $pat -ErrorAction SilentlyContinue |
    Where-Object { $_.LastWriteTime -lt $proc.StartTime } |
    Remove-Item -Force
}
$hwnd = Get-MainHwnd $proc.Id
if ($hwnd -eq [IntPtr]::Zero) { Write-Host "no visible window"; exit 1 }
Start-Sleep -Milliseconds 800
[Win32]::MoveWindow($hwnd, 40, 40, 1240, 820, $true) | Out-Null
[Win32]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 800

function Wait-Marker($name, $secs) {
  $deadline = (Get-Date).AddSeconds($secs)
  while ((Get-Date) -lt $deadline) {
    $fp = Join-Path $dir "$name.txt"
    if (Test-Path $fp) { return (Get-Content $fp -Raw -ErrorAction SilentlyContinue) }
    Start-Sleep -Milliseconds 200
  }
  return $null
}

$shotCounts = @{ "scan" = 8; "fixh" = 2; "wall4m" = 4; "verify" = 4; "filterscroll" = 4 }
if ($shotCounts.ContainsKey($mode)) {
  for ($i = 0; $i -lt $shotCounts[$mode]; $i += 1) {
    $tag = Wait-Marker "ready_$i" 120
    if ($null -eq $tag) { Write-Host "timeout waiting ready_$i"; exit 1 }
    Nudge-Repaint $hwnd
    $label = ($tag.Trim() -replace '[^0-9a-zA-Z_.]', '_')
    Save-Capture $hwnd "$(pwd)/${outPrefix}_${mode}_${i}_${label}.png"
    New-Item -ItemType File -Path (Join-Path $dir "go_$i.txt") -Force | Out-Null
  }
  Wait-Marker "done" 30 | Out-Null
  Write-Host "$mode complete"
} else {
  $scroll = Wait-Marker "ready_0" 120
  if ($null -eq $scroll) { Write-Host "timeout waiting ready_0"; exit 1 }
  Nudge-Repaint $hwnd
  Start-Sleep -Milliseconds 300
  Save-Capture $hwnd "$(pwd)/${outPrefix}_${mode}.png"
  Write-Host "single capture complete"
}
