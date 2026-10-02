@echo off
setlocal EnableExtensions
cd /d "%~dp0"

echo ========================================
echo  YunX make PORTABLE green package
echo ========================================
echo.

if exist "%ProgramFiles%\nodejs\npm.cmd" set "PATH=%ProgramFiles%\nodejs;%PATH%"
if exist "%APPDATA%\npm" set "PATH=%APPDATA%\npm;%PATH%"
if exist "%LOCALAPPDATA%\pnpm" set "PATH=%LOCALAPPDATA%\pnpm;%PATH%"
if exist "%USERPROFILE%\.cargo\bin" set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

where npm >nul 2>&1
if errorlevel 1 (
  echo [ERROR] npm not found. Install Node.js first.
  pause
  exit /b 1
)

where pnpm >nul 2>&1
if errorlevel 1 (
  echo [INFO] installing pnpm ...
  call npm i -g pnpm
  if errorlevel 1 (
    echo [ERROR] pnpm install failed
    pause
    exit /b 1
  )
)

where cargo >nul 2>&1
if errorlevel 1 (
  echo [ERROR] cargo not found. Need Rust MSVC toolchain.
  echo Install: https://rustup.rs
  echo Then: rustup default stable-x86_64-pc-windows-msvc
  echo Also install VS Build Tools with C++ workload.
  pause
  exit /b 1
)

rustc -vV | findstr /C:"host: x86_64-pc-windows-msvc" >nul
if errorlevel 1 (
  echo [WARN] Rust host is NOT windows-msvc.
  echo Tauri on Windows usually needs MSVC, not GNU.
  echo Run: rustup default stable-x86_64-pc-windows-msvc
  echo.
)

if not exist "node_modules\" (
  echo [1/3] pnpm install ...
  call pnpm install
  if errorlevel 1 goto :fail
) else (
  echo [1/3] deps ok
)

echo [2/3] pnpm tauri build --bundles none ...
call pnpm tauri build --bundles none
if errorlevel 1 goto :fail

set "OUT=%~dp0portable\YunX"
if exist "%OUT%" rmdir /s /q "%OUT%"
mkdir "%OUT%" 2>nul

set "REL=%~dp0src-tauri\target\release"
if not exist "%REL%\yunx-desktop.exe" (
  echo [ERROR] release exe missing: %REL%\yunx-desktop.exe
  dir /b "%REL%\*.exe"
  goto :fail
)

copy /Y "%REL%\yunx-desktop.exe" "%OUT%\YunX.exe" >nul

REM sidecars (Tauri externalBin names)
if exist "%REL%\aria2c.exe" copy /Y "%REL%\aria2c.exe" "%OUT%\" >nul
if exist "%REL%\baidupcs.exe" copy /Y "%REL%\baidupcs.exe" "%OUT%\" >nul
if exist "%REL%\goproxy.exe" copy /Y "%REL%\goproxy.exe" "%OUT%\" >nul

REM also copy from binaries folder with target triple stripped names
if not exist "%OUT%\aria2c.exe" if exist "src-tauri\binaries\aria2c-x86_64-pc-windows-msvc.exe" copy /Y "src-tauri\binaries\aria2c-x86_64-pc-windows-msvc.exe" "%OUT%\aria2c.exe" >nul
if not exist "%OUT%\baidupcs.exe" if exist "src-tauri\binaries\baidupcs-x86_64-pc-windows-msvc.exe" copy /Y "src-tauri\binaries\baidupcs-x86_64-pc-windows-msvc.exe" "%OUT%\baidupcs.exe" >nul
if not exist "%OUT%\goproxy.exe" if exist "src-tauri\binaries\goproxy-x86_64-pc-windows-msvc.exe" copy /Y "src-tauri\binaries\goproxy-x86_64-pc-windows-msvc.exe" "%OUT%\goproxy.exe" >nul

echo @echo off> "%OUT%\run.bat"
echo cd /d "%%~dp0">> "%OUT%\run.bat"
echo start "" "YunX.exe">> "%OUT%\run.bat"

echo [3/3] DONE
echo Portable folder:
echo   %OUT%
echo Double-click YunX.exe or run.bat
echo.
explorer "%OUT%"
pause
exit /b 0

:fail
echo.
echo [FAILED] portable build failed.
echo Need: Node.js + pnpm + Rust MSVC + VS Build Tools C++
pause
exit /b 1
