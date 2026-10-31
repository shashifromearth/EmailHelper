@echo off
rem Stop EmailHelper (agent, browser worker, job-links, mock sites).
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\stop-local.ps1"
