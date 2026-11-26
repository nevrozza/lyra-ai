@echo off
start "Lyra AI startup" "%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -STA -ExecutionPolicy Bypass -File "%~dp0Start-Lyra.ps1" -HideStartupConsole %*
