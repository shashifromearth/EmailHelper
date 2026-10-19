@echo off
rem Start (or restart) EmailHelper with one command. Options: -Rebuild, -NoBuild, -WithMockSites
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\start.ps1" %*
