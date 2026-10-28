param(
  [string]$OutDir = (Join-Path $PSScriptRoot "..\docs\img")
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class ReadmeWin32 {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int hh, bool repaint);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  public struct RECT { public int Left, Top, Right, Bottom; }
  public struct POINT { public int X, Y; }
}
"@
[ReadmeWin32]::SetProcessDPIAware() | Out-Null
$exe = (Resolve-Path (Join-Path $PSScriptRoot "..\src-tauri\target\release\simpletable.exe")).Path
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

function Get-OwnMainWindow([int]$ProcessId) {
  $script:best = [IntPtr]::Zero
  $script:area = 0
  $callback = [ReadmeWin32+EnumProc]{
    param($hwnd, $unused)
    $owner = 0
    [ReadmeWin32]::GetWindowThreadProcessId($hwnd, [ref]$owner) | Out-Null
    if ($owner -eq $ProcessId -and [ReadmeWin32]::IsWindowVisible($hwnd)) {
      $rect = New-Object ReadmeWin32+RECT
      [ReadmeWin32]::GetWindowRect($hwnd, [ref]$rect) | Out-Null
      $candidate = ($rect.Right - $rect.Left) * ($rect.Bottom - $rect.Top)
      if ($candidate -gt $script:area) { $script:area = $candidate; $script:best = $hwnd }
    }
    return $true
  }
  [ReadmeWin32]::EnumWindows($callback, [IntPtr]::Zero) | Out-Null
  return $script:best
}

function Wait-OwnWindow([System.Diagnostics.Process]$Process) {
  for ($i = 0; $i -lt 120; $i++) {
    $hwnd = Get-OwnMainWindow $Process.Id
    if ($hwnd -ne [IntPtr]::Zero) { return $hwnd }
    $Process.Refresh()
    if ($Process.HasExited) { throw "Screenshot process exited: $($Process.ExitCode)" }
    Start-Sleep -Milliseconds 250
  }
  throw "Timed out waiting for own window"
}

function Get-WindowGeometry([IntPtr]$Hwnd) {
  $window = New-Object ReadmeWin32+RECT
  $client = New-Object ReadmeWin32+RECT
  $origin = New-Object ReadmeWin32+POINT
  [ReadmeWin32]::GetWindowRect($Hwnd, [ref]$window) | Out-Null
  [ReadmeWin32]::GetClientRect($Hwnd, [ref]$client) | Out-Null
  [ReadmeWin32]::ClientToScreen($Hwnd, [ref]$origin) | Out-Null
  return @{
    Window = $window
    Client = $client
    ClientX = $origin.X - $window.Left
    ClientY = $origin.Y - $window.Top
  }
}

$names = @{ main = "main.png"; split = "split-view.png"; text = "text-lines.png" }
foreach ($scene in @("main", "split", "text")) {
  $token = [Guid]::NewGuid().ToString("N")
  $prefix = Join-Path $env:TEMP "simpletable-readme-$token"
  $fixtureDir = $prefix
  $ready = "$prefix-ready.txt"
  $errorMarker = "$prefix-error.txt"
  $temporaryFiles = @(
    $ready,
    $errorMarker,
    (Join-Path $fixtureDir "release-plan.csv"),
    (Join-Path $fixtureDir "release-checklist.csv"),
    (Join-Path $fixtureDir "server.log"),
    (Join-Path $fixtureDir "notes.txt")
  )
  $proc = $null
  $ownsFixtureDir = $false
  try {
    if (Test-Path -LiteralPath $fixtureDir) { throw "Fixture directory already exists: $fixtureDir" }
    foreach ($temporary in $temporaryFiles) {
      if (Test-Path -LiteralPath $temporary) { throw "Temporary path already exists: $temporary" }
    }
    New-Item -ItemType Directory -Force -Path $fixtureDir | Out-Null
    $ownsFixtureDir = $true
    $proc = Start-Process -FilePath $exe -ArgumentList @("--park", "readme-$scene-$token") -PassThru
    $deadline = (Get-Date).AddSeconds(90)
    while (-not (Test-Path -LiteralPath $ready)) {
      if (Test-Path -LiteralPath $errorMarker) { throw (Get-Content -LiteralPath $errorMarker -Raw) }
      $proc.Refresh()
      if ($proc.HasExited) { throw "Scene $scene exited: $($proc.ExitCode)" }
      if ((Get-Date) -gt $deadline) { throw "Scene $scene ready timeout" }
      Start-Sleep -Milliseconds 250
    }
    $hwnd = Wait-OwnWindow $proc
    $dpi = [ReadmeWin32]::GetDpiForWindow($hwnd)
    if ($dpi -lt 96) { throw "Invalid window DPI: $dpi" }
    $captureWidth = [int][Math]::Round(1440 * $dpi / 96)
    $captureHeight = [int][Math]::Round(900 * $dpi / 96)
    [ReadmeWin32]::MoveWindow($hwnd, 40, 40, $captureWidth, $captureHeight, $true) | Out-Null
    [ReadmeWin32]::SetForegroundWindow($hwnd) | Out-Null
    Start-Sleep -Milliseconds 500
    $geometry = Get-WindowGeometry $hwnd
    $frameWidth = ($geometry.Window.Right - $geometry.Window.Left) - $geometry.Client.Right
    $frameHeight = ($geometry.Window.Bottom - $geometry.Window.Top) - $geometry.Client.Bottom
    [ReadmeWin32]::MoveWindow($hwnd, 40, 40, $captureWidth + $frameWidth, $captureHeight + $frameHeight, $true) | Out-Null
    Start-Sleep -Milliseconds 1000
    $geometry = Get-WindowGeometry $hwnd
    $width = $geometry.Window.Right - $geometry.Window.Left
    $height = $geometry.Window.Bottom - $geometry.Window.Top
    if ($geometry.Client.Right -ne $captureWidth -or $geometry.Client.Bottom -ne $captureHeight) {
      throw "Unexpected client size: $($geometry.Client.Right)x$($geometry.Client.Bottom)"
    }
    $fullBitmap = New-Object System.Drawing.Bitmap $width, $height
    $fullGraphics = [System.Drawing.Graphics]::FromImage($fullBitmap)
    $hdc = $fullGraphics.GetHdc()
    $printed = [ReadmeWin32]::PrintWindow($hwnd, $hdc, 2)
    $fullGraphics.ReleaseHdc($hdc)
    $fullGraphics.Dispose()
    if (-not $printed) { throw "PrintWindow failed for scene $scene" }
    $bitmap = New-Object System.Drawing.Bitmap 1440, 900
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $graphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $destination = New-Object System.Drawing.Rectangle 0, 0, 1440, 900
    $source = New-Object System.Drawing.Rectangle $geometry.ClientX, $geometry.ClientY, $captureWidth, $captureHeight
    $graphics.DrawImage($fullBitmap, $destination, $source, [System.Drawing.GraphicsUnit]::Pixel)
    $graphics.Dispose()
    $fullBitmap.Dispose()
    $pixels = @($bitmap.GetPixel(20, 20).ToArgb(), $bitmap.GetPixel(500, 300).ToArgb(), $bitmap.GetPixel(1000, 700).ToArgb())
    if (($pixels | Select-Object -Unique).Count -eq 1) { throw "Blank screenshot detected" }
    $out = Join-Path $OutDir $names[$scene]
    $bitmap.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
    $bitmap.Dispose()
    Write-Output "CAPTURE=$scene DPI=$dpi CLIENT=${captureWidth}x${captureHeight}"
    Write-Output "SAVED=$out"
  } finally {
    if ($null -ne $proc -and -not $proc.HasExited) {
      $proc.CloseMainWindow() | Out-Null
      if (-not $proc.WaitForExit(5000)) { $proc.Kill(); $proc.WaitForExit() }
    }
    if ($ownsFixtureDir) {
      foreach ($temporary in $temporaryFiles) {
        if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Force }
      }
      if (Test-Path -LiteralPath $fixtureDir) { Remove-Item -LiteralPath $fixtureDir -Force }
    }
  }
}
