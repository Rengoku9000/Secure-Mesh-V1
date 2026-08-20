@echo off
setlocal
cd /d "%~dp0\.."

set "BIN_PATH=%CD%\dist-app\securemesh.exe"

if not exist "%BIN_PATH%" (
    echo Binary not found at %BIN_PATH%.
    echo Please run 'npm run app:stage' first.
    pause
    exit /b 1
)

echo Launching Node A...
start "Node A" cmd /c "set SECUREMESH_DATA_DIR=%TEMP%\smA && "%BIN_PATH%""

timeout /t 2 /nobreak >nul

echo Launching Node B...
start "Node B" cmd /c "set SECUREMESH_DATA_DIR=%TEMP%\smB && "%BIN_PATH%""

echo Both nodes launched successfully.
