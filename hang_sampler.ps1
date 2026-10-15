# hang_sampler.ps1 - v7: per-thread diagnosis of the "Not Responding" ghost.
# v1-v5 changelog: TimeSpan summed manually (Measure-Object -Sum throws);
# GetWindowThreadProcessId RETURN VALUE is the thread id (out param = pid);
# $pid is a PS read-only automatic variable (renamed $targetPid); thread
# snapshot via .NET Get-Process .Threads (CIM Win32_Thread unreliable);
# mouse wiggle per sample so IsHungAppWindow sees pending input like a real
# user session; prog marker "latest" sorted numerically (prog_5 vs prog_50).
# Trigger markers: park delcol mode writes delcol_applying / delcol_applied.
# Output: %TEMP%\hang_samples.csv  (summary per sample)
#         %TEMP%\hang_threads.csv  (per-thread detail per sample)
# ASCII-only string literals on purpose: PS 5.1 reads BOM-less UTF-8 as ANSI.
param(
  [string]$procName = "simpletable",
  [int]$intervalMs = 1200
)

$sig = @"
using System;
using System.Runtime.InteropServices;
public class U32v3 {
  [DllImport("user32.dll")] public static extern bool IsHungAppWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
  public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@
Add-Type -TypeDefinition $sig

$dir = $env:TEMP
$outS = Join-Path $dir "hang_samples.csv"
$outT = Join-Path $dir "hang_threads.csv"
"t,hung,mainState,mainWait,mainCpu,stCpu,webCpu,engCpu,werCpu,prog" | Out-File $outS -Encoding ascii
"t,tid,state,wait,cpuPct" | Out-File $outT -Encoding ascii

# ---- wait reason decode (KTIMER wait reasons) ----
$waitNames = @{
  0="Executive"; 1="FreePage"; 2="PageIn"; 3="PoolAlloc"; 4="DelayExec"; 5="Suspended";
  6="UserRequest"; 7="WrExecutive"; 8="WrMutex"; 9="WrDispatchInt"; 10="WrPageIn";
  11="WrPoolAlloc"; 12="WrDelayExec"; 13="WrSuspended"; 14="WrUserRequest";
  15="WrEventPair"; 16="WrQueue"; 17="WrLpcReceive"; 18="WrLpcReply";
  19="WrVirtualMem"; 20="WrPageOut"; 21="WrRendezvous"; 22="WrKeyedEvent";
  23="WrTerminated"; 27="WrKernel"; 28="WrResource"; 29="WrPushLock";
  37="WrIOCompletion"; 38="WrDeferredPreempt"
}
$stateNames = @{ 0="Init"; 1="Ready"; 2="Running"; 3="Standby"; 4="Terminated"; 5="Wait"; 6="Transition"; 7="?" }
function WName($w) { if ($waitNames.ContainsKey([int]$w)) { return $waitNames[[int]$w] }; return "wr$w" }
function SName($s) { if ($stateNames.ContainsKey([int]$s)) { return $stateNames[[int]$s] }; return "st$s" }

# ---- process CPU (Get-Process; TimeSpan summed manually) ----
function CpuMsOf($nameFilter) {
  $ms = 0.0
  Get-Process | Where-Object { $_.ProcessName -like $nameFilter } | ForEach-Object {
    try { $ms += $_.TotalProcessorTime.TotalMilliseconds } catch {}
  }
  return $ms
}

# ---- find process + its largest visible window ----
$proc = $null
for ($t = 0; $t -lt 240; $t += 1) {
  $proc = Get-Process $procName -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($proc) { break }
  Start-Sleep -Milliseconds 500
}
if (-not $proc) { Write-Host "no $procName process"; exit 1 }

$cb = [U32v3+EnumProc]{
  param($h, $l)
  if (-not [U32v3]::IsWindowVisible($h)) { return $true }
  $pid2 = 0
  [U32v3]::GetWindowThreadProcessId($h, [ref]$pid2) | Out-Null
  if ($pid2 -eq $proc.Id) { $script:found = $h; return $false }
  return $true
}
$script:found = [IntPtr]::Zero
[U32v3]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
$hwnd = $script:found
if ($hwnd -eq [IntPtr]::Zero) { Write-Host "no window"; exit 1 }
# NB: GetWindowThreadProcessId RETURNS the thread id; the [ref] out param
# receives the process id. v1-v3 passed guiTid as the out var and threw the
# return value away, so the "main thread" lookup chased a nonexistent id.
$ownerPid = 0
$guiTid = [U32v3]::GetWindowThreadProcessId($hwnd, [ref]$ownerPid)
$ncpu = [Environment]::ProcessorCount
Write-Host "hwnd=$hwnd guiTid=$guiTid pid=$($proc.Id) ProcessorCount=$ncpu"

# ---- per-thread snapshot via CIM ----
# Per-thread snapshot via .NET ProcessThread (CIM Win32_Thread proved
# unreliable here: it returned only one thread of the target process).
# v1-v2's guiTid bug (out-param vs return value) is fixed at the call site.
function ThreadSnap($targetPid) {
  $map = @{}
  try {
    $p = Get-Process -Id $targetPid -ErrorAction Stop
    foreach ($th in $p.Threads) {
      $tid = [uint32]$th.Id
      $st = 7; $wr = 0
      try { $st = [int]$th.ThreadState; $wr = [int]$th.WaitReason } catch {}
      $cpu = 0.0
      try { $cpu = $th.TotalProcessorTime.TotalMilliseconds } catch {}
      $map[$tid] = @{ State = $st; Wait = $wr; CpuMs = $cpu }
    }
  } catch {
    Write-Host "thread snapshot failed: $($_.Exception.Message)"
  }
  return $map
}

# ---- wait for applying marker (index build on 2.2GB can take minutes) ----
$deadline = (Get-Date).AddSeconds(600)
while (-not (Test-Path (Join-Path $dir "delcol_applying.txt"))) {
  if ((Get-Date) -gt $deadline) { Write-Host "timeout waiting applying"; exit 1 }
  Start-Sleep -Milliseconds 1000
}
Write-Host "applying detected, sampling..."

$prevSnap = ThreadSnap $proc.Id
$prevSt = CpuMsOf $procName
$prevWeb = CpuMsOf "msedgewebview2*"
$prevEng = CpuMsOf "MsMpEng"
$prevWer = CpuMsOf "WerFault*"
$t0 = Get-Date
$maxWait = 900
while (-not (Test-Path (Join-Path $dir "delcol_applied.txt"))) {
  if (((Get-Date) - $t0).TotalSeconds -gt $maxWait) { Write-Host "sample watchdog timeout"; break }
  Start-Sleep -Milliseconds $intervalMs
  $dt = [double]$intervalMs
  $snap = ThreadSnap $proc.Id

  # summary row
  $mainState = "?"; $mainWait = "?"; $mainCpu = 0.0
  $guiKey = [uint32]$guiTid
  if ($snap.ContainsKey($guiKey) -and $prevSnap.ContainsKey($guiKey)) {
    $mainState = SName $snap[$guiKey].State
    $mainWait = WName $snap[$guiKey].Wait
    $mainCpu = [math]::Round(($snap[$guiKey].CpuMs - $prevSnap[$guiKey].CpuMs) / $dt * 100.0 / $ncpu, 1)
  }
  $nowSt = CpuMsOf $procName
  $nowWeb = CpuMsOf "msedgewebview2*"
  $nowEng = CpuMsOf "MsMpEng"
  $nowWer = CpuMsOf "WerFault*"
  $stCpu = [math]::Round(($nowSt - $prevSt) / $dt * 100.0 / $ncpu, 1)
  $webCpu = [math]::Round(($nowWeb - $prevWeb) / $dt * 100.0 / $ncpu, 1)
  $engCpu = [math]::Round(($nowEng - $prevEng) / $dt * 100.0 / $ncpu, 1)
  $werCpu = [math]::Round(($nowWer - $prevWer) / $dt * 100.0 / $ncpu, 1)
  $prevSt = $nowSt; $prevWeb = $nowWeb; $prevEng = $nowEng; $prevWer = $nowWer

  # reproduce the user's interactive condition: a little real mouse traffic
  # over the window each sample. IsHungAppWindow only flags a hung queue when
  # messages are PENDING - an idle occluded window with an empty queue reads
  # as "not hung" even if its thread never wakes.
  $r = New-Object U32v3+RECT
  [U32v3]::GetWindowRect($hwnd, [ref]$r) | Out-Null
  [U32v3]::SetCursorPos($r.Left + 80, $r.Top + 120) | Out-Null
  Start-Sleep -Milliseconds 60
  [U32v3]::mouse_event(0x0001, 2, 2, 0, [UIntPtr]::Zero)  # MOUSEEVENTF_MOVE
  Start-Sleep -Milliseconds 60

  $hung = 0
  if ([U32v3]::IsHungAppWindow($hwnd)) { $hung = 1 }
  $prog = ""
  $latest = Get-ChildItem -Path $dir -Filter "prog_*.txt" -ErrorAction SilentlyContinue |
    Sort-Object { [int]($_.BaseName -replace '^prog_') } | Select-Object -Last 1
  if ($latest) { $prog = $latest.BaseName -replace '^prog_' }
  $el = [math]::Round(((Get-Date) - $t0).TotalSeconds, 0)
  "$el,$hung,$mainState,$mainWait,$mainCpu,$stCpu,$webCpu,$engCpu,$werCpu,$prog" | Out-File $outS -Encoding ascii -Append

  # per-thread detail: every thread with measurable CPU + the GUI thread
  foreach ($k in $snap.Keys) {
    $cpu = 0.0
    if ($prevSnap.ContainsKey($k)) {
      $cpu = [math]::Round(($snap[$k].CpuMs - $prevSnap[$k].CpuMs) / $dt * 100.0 / $ncpu, 1)
    }
    if ($cpu -gt 0.1 -or $k -eq $guiKey) {
      $tag = ""
      if ($k -eq $guiKey) { $tag = "MAIN" }
      "$el,$k$tag,$(SName $snap[$k].State),$(WName $snap[$k].Wait),$cpu" | Out-File $outT -Encoding ascii -Append
    }
  }
  $prevSnap = $snap
}
Write-Host "applied detected"
