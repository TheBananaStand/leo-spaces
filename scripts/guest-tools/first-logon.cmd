@echo off
rem Runs once, at the first logon of the account Windows Setup made, from the
rem Leo guest tools CD (%1 is this CD's drive, e.g. E:). Three things, in order:
rem the virtio drivers and guest agent, the edition the key names (Server
rem evaluation media only), and the Leo satellite.
set CD=%1
set LOG=%ProgramData%\Leo\first-logon.log
mkdir "%ProgramData%\Leo" 2>nul
echo [%date% %time%] first logon from %CD% >> "%LOG%"

rem The virtio-win CD is beside this one; find it by the installer it carries.
for %%d in (D E F G H I J) do if exist %%d:\virtio-win-guest-tools.exe (
  echo [%date% %time%] virtio guest tools from %%d: >> "%LOG%"
  "%%d:\virtio-win-guest-tools.exe" /install /quiet /norestart >> "%LOG%" 2>&1
)

if exist %CD%\leo-set-edition.cmd (
  echo [%date% %time%] setting the edition >> "%LOG%"
  call %CD%\leo-set-edition.cmd >> "%LOG%" 2>&1
)

echo [%date% %time%] Leo satellite >> "%LOG%"
powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File %CD%\install-leo-satellite.ps1 >> "%LOG%" 2>&1
echo [%date% %time%] done >> "%LOG%"
