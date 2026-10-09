@echo off
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0worker.ps1" %*
if errorlevel 1 pause
