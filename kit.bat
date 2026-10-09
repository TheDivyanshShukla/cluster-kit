@echo off
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0kit.ps1" %*
if errorlevel 1 pause
