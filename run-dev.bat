@echo off
setlocal EnableExtensions
cd /d "%~dp0"

echo ========================================
echo  YunX DEV run (ASCII, no Chinese)
echo ========================================
echo.

if exist "%ProgramFiles%\nodejs" set "PATH=%ProgramFiles%\nodejs;%PATH%"
if exist "%APPDATA%\npm" set "PATH=%APPDATA%\npm;%PATH%"
if exist "%LOCALAPPDATA%\pnpm" set "PATH=%LOCALAPPDATA%\pnpm;%PATH%"
if exist "%USERPROFILE%\.cargo\bin" set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

where npm >nul 2>&1
if errorlevel 1 (
  echo [ERROR] npm not found. Install Node.js.
  pause
  exit /b 1
)

where pnpm >nul 2>&1
if errorlevel 1 (
  echo [INFO] installing pnpm ...
  call npm i -g pnpm
  if errorlevel 1 (
    echo [ERROR] cannot install pnpm
    pause
    exit /b 1
  )
)

where cargo >nul 2>&1
if errorlevel 1 (
  echo [ERROR] cargo not found
  pause
  exit /b 1
)

if not exist "node_modules\" (
  echo [1/2] pnpm install ...
  call pnpm install
  if errorlevel 1 (
    echo [ERROR] pnpm install failed
    pause
    exit /b 1
  )
) else (
  echo [1/2] node_modules ok
)

echo [2/2] pnpm tauri dev ...
call pnpm tauri dev
set ERR=%ERRORLEVEL%
if not "%ERR%"=="0" (
  echo [exit] %ERR%
  echo If build fails with dlltool: switch to Rust MSVC
  echo   rustup default stable-x86_64-pc-windows-msvc
  pause
)
exit /b %ERR%
