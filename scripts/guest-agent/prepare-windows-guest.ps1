<#
.SYNOPSIS
  Prepares a Windows Server space to host several agents at once.

.DESCRIPTION
  Run this INSIDE a Windows Server guest, in an elevated PowerShell, after
  Windows is installed and the virtio drivers are in.

  WHAT IT IS FOR. One Windows guest can host several concurrent interactive
  sessions, and each session has its own desktop, its own input queue and its
  own foreground window. That isolation is the whole reason several agents can
  drive the same machine in parallel without fighting: Windows has exactly one
  foreground window per session, and an application throwing a modal dialog
  grabs it. Separate sessions make the contention structurally impossible
  rather than something to work around.

  WHY SERVER. Client Windows (10, 11) permits one interactive session at a
  time, and the tools that patch that out violate the licence. Windows Server
  with the Remote Desktop Session Host role is the licensed path, and it needs
  RDS CALs on top of the Server licence. This script will tell you what is
  missing rather than guessing.

  WHAT IT DOES NOT DO. It does not install an agent. It creates the accounts
  and the session capacity an agent would run in, and reports what remains.

.PARAMETER AgentCount
  How many agent accounts to create. Each concurrent agent needs its own
  session, and each session needs its own account.

.PARAMETER Prefix
  Account name prefix. Accounts are <prefix>-01, <prefix>-02, …

.EXAMPLE
  .\prepare-windows-guest.ps1 -AgentCount 3
#>

[CmdletBinding()]
param(
  [ValidateRange(1, 32)]
  [int]$AgentCount = 2,

  [ValidatePattern('^[a-z][a-z0-9-]{1,12}$')]
  [string]$Prefix = 'leo-agent'
)

$ErrorActionPreference = 'Stop'

function Say  { param($m) Write-Host "  $m" }
function Head { param($m) Write-Host "`n$m" -ForegroundColor Cyan }
function Warn { param($m) Write-Host "  ! $m" -ForegroundColor Yellow }
function Good { param($m) Write-Host "  + $m" -ForegroundColor Green }

if (-not ([Security.Principal.WindowsPrincipal] [Security.Principal.WindowsIdentity]::GetCurrent()
      ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  throw 'Run this in an elevated PowerShell.'
}

# ── What this guest is ────────────────────────────────────────────────────────
Head 'This guest'
$os = Get-CimInstance Win32_OperatingSystem
Say $os.Caption
Say "$([math]::Round($os.TotalVisibleMemorySize / 1MB, 1)) GB RAM, $((Get-CimInstance Win32_ComputerSystem).NumberOfLogicalProcessors) logical processors"

# ProductType 1 is a workstation; 2 is a domain controller; 3 is a server.
# The distinction decides whether anything below is even possible, so it is
# checked before anything is created rather than failing halfway.
$isServer = (Get-CimInstance Win32_OperatingSystem).ProductType -ne 1
if (-not $isServer) {
  Warn 'This is client Windows, which allows ONE interactive session at a time.'
  Warn 'One agent can drive this guest. Running several in parallel needs Windows'
  Warn 'Server with the RDS role — the tools that patch client Windows to allow'
  Warn 'more sessions violate the licence, so this script will not do it.'
  Warn 'Accounts will still be created; only one can be signed in at a time.'
}

# ── The Remote Desktop Session Host role ──────────────────────────────────────
Head 'Multi-session capacity'
if ($isServer) {
  $rds = Get-WindowsFeature -Name RDS-RD-Server -ErrorAction SilentlyContinue
  if ($rds -and $rds.Installed) {
    Good 'RD Session Host role is installed.'
  } else {
    Warn 'RD Session Host is NOT installed. Without it this guest allows two'
    Warn 'administrative sessions only, which is enough to try things and not'
    Warn 'enough to run a pool of agents. Install with:'
    Say  '    Install-WindowsFeature -Name RDS-RD-Server -IncludeManagementTools -Restart'
  }

  # Licensing is the ceiling people hit months later, so it is reported up
  # front: without CALs the grace period expires and sessions stop being
  # granted, with an error that mentions licensing and nothing else.
  $lic = Get-CimInstance -Namespace root\cimv2\TerminalServices -ClassName Win32_TerminalServiceSetting -ErrorAction SilentlyContinue
  if ($lic) {
    Say "Licensing mode: $($lic.LicensingName)"
    if ($lic.LicensingName -eq 'Remote Desktop for Administration') {
      Warn 'Administration mode: two sessions maximum, 120-day grace period.'
      Warn 'A pool of agents needs RDS CALs and a licence server.'
    }
  }
}

# ── Let the sessions in ───────────────────────────────────────────────────────
Head 'Remote Desktop'
$ts = 'HKLM:\System\CurrentControlSet\Control\Terminal Server'
Set-ItemProperty -Path $ts -Name 'fDenyTSConnections' -Value 0
Good 'Remote Desktop enabled.'

# Each agent needs its own session, so the per-user single-session restriction
# has to go. This is the setting that otherwise silently reuses one session for
# every connection by the same account — which looks like agents stealing each
# other's windows.
Set-ItemProperty -Path $ts -Name 'fSingleSessionPerUser' -Value 0
Good 'One session per connection (not one per user).'

Enable-NetFirewallRule -DisplayGroup 'Remote Desktop' -ErrorAction SilentlyContinue
Good 'Firewall rule enabled.'

# A disconnected session must keep running — that is what makes an agent's work
# survive the human closing the viewer. The default on some builds is to end
# the session on disconnect, which kills the workload mid-run.
$policy = 'HKLM:\SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services'
if (-not (Test-Path $policy)) { New-Item -Path $policy -Force | Out-Null }
# 0 means never — a disconnected session is kept indefinitely.
Set-ItemProperty -Path $policy -Name 'MaxDisconnectionTime' -Value 0 -Type DWord
Set-ItemProperty -Path $policy -Name 'MaxIdleTime' -Value 0 -Type DWord
Good 'Disconnected sessions keep running.'

# ── Hardware graphics for every session ───────────────────────────────────────
#
# Without this, RDS sessions get the software rasteriser and a passed-through
# GPU sits idle while the CAD application reports no hardware acceleration —
# which reads as a broken passthrough rather than a policy default.
Head 'Graphics'
Set-ItemProperty -Path $policy -Name 'bEnumerateHWBeforeSW' -Value 1 -Type DWord
Good 'Sessions will use the hardware adapter when there is one.'
$gpu = Get-CimInstance Win32_VideoController | Select-Object -First 1
if ($gpu) { Say "Adapter: $($gpu.Name)" }

# ── The agent accounts ────────────────────────────────────────────────────────
Head "Agent accounts ($AgentCount)"
$created = @()
for ($i = 1; $i -le $AgentCount; $i++) {
  $name = '{0}-{1:d2}' -f $Prefix, $i
  if (Get-LocalUser -Name $name -ErrorAction SilentlyContinue) {
    Say "$name already exists."
  } else {
    # A random password that is never printed and never stored here. The
    # account is reached through the hub's own credential store; a password
    # echoed to a transcript is a credential in a log file.
    Add-Type -AssemblyName System.Web
    $pw = [System.Web.Security.Membership]::GeneratePassword(24, 6)
    $secure = ConvertTo-SecureString $pw -AsPlainText -Force
    New-LocalUser -Name $name -Password $secure -FullName "Leo agent $i" `
      -Description 'Runs one Leo agent session' -PasswordNeverExpires | Out-Null
    Good "$name created."
    $created += $name
  }
  # Remote Desktop Users is what grants a session; without it the account
  # authenticates and is then refused a desktop.
  Add-LocalGroupMember -Group 'Remote Desktop Users' -Member $name -ErrorAction SilentlyContinue
}
if ($created.Count -gt 0) {
  Warn 'Passwords were generated and not shown. Set one you hold with:'
  Say  "    Set-LocalUser -Name $($created[0]) -Password (Read-Host -AsSecureString)"
}

# ── What is left ──────────────────────────────────────────────────────────────
Head 'What is left'
Say '1. Install the applications the agents will drive (SOLIDWORKS, simulation tools).'
Say '   Every concurrent instance consumes a licence seat, and SOLIDWORKS in'
Say '   particular needs a network licence (SNL) — standalone seats do not'
Say '   activate on a session host. That is usually the real ceiling on how'
Say '   many agents can run at once, well before RAM or the GPU.'
Say '2. Install the in-guest agent in each session.'
Say '3. Prefer application APIs over clicking: SOLIDWORKS has a COM API, and'
Say '   UI Automation exposes a semantic element tree. Both survive a version'
Say '   bump; pixel coordinates do not.'

if ($isServer) {
  Head 'Session capacity'
  Say "This guest can host $AgentCount agent sessions once the RDS role and CALs are in place."
} else {
  Head 'Session capacity'
  Warn 'One at a time, on client Windows.'
}
