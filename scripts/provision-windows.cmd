@echo off
setlocal
rem Windows 10/11 launcher. Run this saved file from the downloaded repository.
rem Uses the existing PowerShell execution policy and normal UAC approval.
powershell.exe -NoProfile -Command "if (([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { exit 0 } else { exit 1 }"
if errorlevel 1 (
  echo Administrator rights are required. Right-click this file and choose Run as administrator.
  pause
  exit /b 1
)
if not exist "%~dp0provision.ps1" (
  echo Missing provision.ps1. Keep this launcher in the repository scripts folder.
  pause
  exit /b 1
)
powershell.exe -NoLogo -NoProfile -File "%~dp0provision.ps1"
if errorlevel 1 (
  echo Setup failed or the PC policy blocked the script. Review the message above.
  echo For managed PCs, use your administrator-approved signing or deployment process.
)
pause
