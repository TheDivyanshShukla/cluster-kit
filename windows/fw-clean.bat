@echo off
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0fw-clean.ps1" %*
if errorlevel 1 pause
