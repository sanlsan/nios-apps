@echo off
setlocal EnableExtensions
cd /d "%~dp0"
title Nios Apps
echo.
echo  Сборка Nios Apps для Windows
echo  ----------------------------
echo.
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
where curl.exe >nul 2>nul
if errorlevel 1 goto no_curl
where cargo >nul 2>nul
if not errorlevel 1 goto have_rust
echo [1/3] Устанавливаю Rust...
curl.exe -L --fail --silent --show-error -o "%TEMP%\rustup-init.exe" https://win.rustup.rs/x86_64
if errorlevel 1 goto net_fail
"%TEMP%\rustup-init.exe" -y --profile minimal --default-toolchain stable
if errorlevel 1 goto fail
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
:have_rust
set "VSW=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
set "HAVE_MSVC="
if exist "%VSW%" for /f "usebackq delims=" %%i in (`"%VSW%" -products * -latest -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "HAVE_MSVC=%%i"
if defined HAVE_MSVC goto have_msvc
echo [2/3] Устанавливаю Visual Studio Build Tools (около 3 ГБ, 5-15 минут)...
curl.exe -L --fail --silent --show-error -o "%TEMP%\vs_BuildTools.exe" https://aka.ms/vs/17/release/vs_BuildTools.exe
if errorlevel 1 goto net_fail
"%TEMP%\vs_BuildTools.exe" --quiet --wait --norestart --nocache --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended
set "RC=%errorlevel%"
if "%RC%"=="0" goto have_msvc
if "%RC%"=="3010" goto have_msvc
echo Установщик вернул код %RC%.
goto fail
:have_msvc
echo [3/3] Собираю программу...
cargo build --release --locked
if errorlevel 1 goto fail
if not exist dist mkdir dist
copy /y "target\release\NiosApps.exe" "dist\NiosApps.exe" >nul
echo.
echo  Готово: %~dp0dist\NiosApps.exe
explorer "%~dp0dist"
pause
exit /b 0
:no_curl
echo Не найден curl.exe (нужна Windows 10 1803 или новее).
goto fail
:net_fail
echo Не удалось скачать файл. Проверьте интернет.
:fail
echo.
echo  Сборка не получилась.
pause
exit /b 1
