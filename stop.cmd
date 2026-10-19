@echo off
rem Stop EmailHelper (agent, browser worker, mock sites).
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\stop-local.ps1"
