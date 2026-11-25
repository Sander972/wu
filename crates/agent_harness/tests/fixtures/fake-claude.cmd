@echo off
rem Bare bash on PATH is WSL bash outside Git Bash, so prefer the Git for Windows one
set "GITBASH=bash"
if exist "%ProgramFiles%\Git\bin\bash.exe" set "GITBASH=%ProgramFiles%\Git\bin\bash.exe"
"%GITBASH%" "%~dp0fake-claude.sh" %*
