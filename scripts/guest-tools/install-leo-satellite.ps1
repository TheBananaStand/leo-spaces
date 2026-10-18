<#
.SYNOPSIS
  Installs the Leo satellite in a Windows space, so agents can drive it.
.DESCRIPTION
  Run inside the guest, elevated, from the "Leo guest tools" CD this space
  was created with. It copies the satellite to a machine-wide place, pairs it
  with the hub this space belongs to (you approve the code in Leo → Settings
  → Devices, once), and registers it to start with Windows as SYSTEM — a
  scheduled task rather than a service, because the satellite is a plain
  console program and a task needs no service plumbing to keep one alive.

  Run it again at any time: a satellite already paired keeps its pairing and
  only the binary and the task are refreshed.
.PARAMETER HubHost
  The Leo hub to pair with. Defaults to what the CD carries in leo-hub.txt.
.PARAMETER Name
  What this machine calls itself on the hub. Defaults to the space's name.
#>
param(
  [string]$HubHost,
  [int]$HubPort = 8420,
  [string]$Name
)
$ErrorActionPreference = "Stop"

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$dataDir = Join-Path $env:ProgramData "Leo"
$binDir  = Join-Path $dataDir "bin"
$exe     = Join-Path $binDir "leo-satellited.exe"
$envFile = Join-Path $dataDir "satellite.env"
$task    = "Leo Satellite"

if (-not $HubHost -and (Test-Path (Join-Path $here "leo-hub.txt"))) {
  $HubHost = (Get-Content (Join-Path $here "leo-hub.txt") -Raw).Trim()
}
if (-not $Name -and (Test-Path (Join-Path $here "leo-name.txt"))) {
  $Name = (Get-Content (Join-Path $here "leo-name.txt") -Raw).Trim()
}
if (-not $Name) { $Name = $env:COMPUTERNAME.ToLower() }
if (-not $HubHost) { throw "No hub named: pass -HubHost, or run this from the Leo guest tools CD." }

Write-Host "`nLeo satellite for $Name -> hub $HubHost`:$HubPort" -ForegroundColor Cyan

# The data dir is for SYSTEM and Administrators only: the pairing token lives
# in it, and there is no mode bit on Windows — the directory's ACL is the
# whole of the file's privacy.
New-Item -ItemType Directory -Force -Path $binDir | Out-Null
$acl = Get-Acl $dataDir
$acl.SetAccessRuleProtection($true, $false)
foreach ($id in @("NT AUTHORITY\SYSTEM", "BUILTIN\Administrators")) {
  $rule = New-Object System.Security.AccessControl.FileSystemAccessRule(
    $id, "FullControl", "ContainerInherit,ObjectInherit", "None", "Allow")
  $acl.AddAccessRule($rule)
}
Set-Acl $dataDir $acl

# Stop whatever is running before the binary is replaced.
Get-Process leo-satellited -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Copy-Item (Join-Path $here "leo-satellited.exe") $exe -Force
Write-Host "  + installed $exe"

$env:LEO_DATA_DIR = $dataDir
if (Test-Path $envFile) {
  Write-Host "  = already paired ($envFile kept)"
} elseif (Test-Path (Join-Path $here "satellite.env")) {
  # Leo paired this space when it made it: the token is on the CD, and the
  # person who clicked Create already approved it.
  Copy-Item (Join-Path $here "satellite.env") $envFile -Force
  Write-Host "  + paired from the CD"
} else {
  Write-Host "`nPairing. Approve the code in Leo -> Settings -> Devices." -ForegroundColor Cyan
  & $exe pair $HubHost --port $HubPort --name $Name
  if ($LASTEXITCODE -ne 0) { throw "pairing failed" }
}

# Start with Windows, as SYSTEM, and keep it up: the task restarts the
# program if it exits, which is how a console program survives a crash
# without being a service.
$action  = New-ScheduledTaskAction -Execute $exe -Argument "run"
$trigger = New-ScheduledTaskTrigger -AtStartup
$settings = New-ScheduledTaskSettingsSet -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) `
  -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
$principal = New-ScheduledTaskPrincipal -UserId "NT AUTHORITY\SYSTEM" -LogonType ServiceAccount -RunLevel Highest
Unregister-ScheduledTask -TaskName $task -Confirm:$false -ErrorAction SilentlyContinue
Register-ScheduledTask -TaskName $task -Action $action -Trigger $trigger -Settings $settings -Principal $principal | Out-Null
Start-ScheduledTask -TaskName $task
Write-Host "  + task '$task' registered and started"
Write-Host "`nDone. Claim $Name on the hub (Settings -> Devices) so people's commands are accepted." -ForegroundColor Green
