# check_wer_events.ps1 - recent WER events (hang reports)
Get-WinEvent -FilterHashtable @{LogName='Application'; ProviderName='Windows Error Reporting'} -MaxEvents 15 -ErrorAction SilentlyContinue |
  ForEach-Object {
    $msg = $_.Message
    if ($msg.Length -gt 220) { $msg = $msg.Substring(0, 220) }
    "{0}  id={1}  {2}" -f $_.TimeCreated, $_.Id, ($msg -replace "`r?`n", " | ")
  }
