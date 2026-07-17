@echo off
title Stamp Compare Tool Launcher
cd /d "%~dp0"

echo.
echo ============================================
echo   Stamp Compare Tool - One Click Launcher
echo ============================================
echo.

:: ---- Check Python ----
set PYTHON=

python --version >nul 2>&1
if %errorlevel%==0 (
    set PYTHON=python
    goto :python_ok
)

py -3 --version >nul 2>&1
if %errorlevel%==0 (
    set PYTHON=py -3
    goto :python_ok
)

:: ---- Python not found - offer auto install ----
echo [X] Python 3 not found on this computer.
echo.
echo Python is required to run the OCR service.
echo.
set /p AUTOINSTALL="Auto-download and install Python 3.12? (Y/n): "
if /i "%AUTOINSTALL%"=="n" goto :manual_python

echo.
echo Downloading Python 3.12 installer...
echo (This may take a moment depending on your network)
echo.

:: Download using certutil (available on all Windows 7+)
certutil -urlcache -split -f "https://www.python.org/ftp/python/3.12.9/python-3.12.9-amd64.exe" "%TEMP%\python-installer.exe" >nul 2>&1
if %errorlevel% neq 0 (
    echo [!] Download failed. Trying PowerShell...
    powershell -Command "Invoke-WebRequest -Uri 'https://www.python.org/ftp/python/3.12.9/python-3.12.9-amd64.exe' -OutFile '%TEMP%\python-installer.exe'" 2>nul
    if %errorlevel% neq 0 (
        echo [X] Download failed. Please install Python manually:
        echo     https://www.python.org/downloads/
        echo     IMPORTANT: Check "Add Python to PATH"
        goto :die
    )
)

echo Installing Python (silent mode, adding to PATH)...
echo Please wait...
"%TEMP%\python-installer.exe" /quiet InstallAllUsers=0 PrependPath=1 Include_pip=1 Include_test=0

:: Wait for installer to finish
:wait_installer
tasklist /fi "IMAGENAME eq python-3.12.9-amd64.exe" 2>nul | findstr "python" >nul
if %errorlevel%==0 (
    timeout /t 3 /nobreak >nul
    goto :wait_installer
)

:: Clean up installer
del "%TEMP%\python-installer.exe" >nul 2>&1

echo.
echo [OK] Python installed. Refreshing environment...

:: Refresh PATH for current session
for /f "tokens=2*" %%A in ('reg query "HKCU\Environment" /v Path 2^>nul') do set "USERPATH=%%B"
for /f "tokens=2*" %%A in ('reg query "HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment" /v Path 2^>nul') do set "SYSPATH=%%B"
set "PATH=%USERPATH%;%SYSPATH%"

:: Also check common install locations
if exist "%LOCALAPPDATA%\Programs\Python\Python312\python.exe" (
    set "PYTHON=%LOCALAPPDATA%\Programs\Python\Python312\python.exe"
    goto :python_ok
)

:: Re-check python in refreshed PATH
python --version >nul 2>&1
if %errorlevel%==0 (
    set PYTHON=python
    goto :python_ok
)

py -3 --version >nul 2>&1
if %errorlevel%==0 (
    set PYTHON=py -3
    goto :python_ok
)

echo [X] Python installed but not found in PATH.
echo     Please close this window and reopen launcher.bat.
goto :die

:manual_python
echo.
echo Please install Python 3.8+ manually:
echo   1. Visit: https://www.python.org/downloads/
echo   2. Download and run the installer
echo   3. IMPORTANT: Check "Add Python to PATH"
echo   4. Then re-run this launcher
echo.
goto :die

:python_ok
for /f "tokens=*" %%v in ('%PYTHON% --version 2^>^&1') do echo [OK] %%v

:: ---- Check Dependencies ----
%PYTHON% -c "import paddleocr" >nul 2>&1
if %errorlevel%==0 goto :deps_ok

echo.
echo Need to install Python dependencies:
echo   - PaddlePaddle (~700MB)
echo   - PaddleOCR
echo   - opencv-python
echo.
set /p INSTALL="Install now? (Y/n): "
if /i "%INSTALL%"=="n" goto :die

echo.
echo [1/3] PaddlePaddle...
%PYTHON% -m pip install paddlepaddle
if %errorlevel% neq 0 goto :die

echo [2/3] PaddleOCR...
%PYTHON% -m pip install paddleocr
if %errorlevel% neq 0 goto :die

echo [3/3] OpenCV...
%PYTHON% -m pip install opencv-python
if %errorlevel% neq 0 goto :die

echo.
echo [OK] All dependencies installed.
echo.

:deps_ok

:: ---- Check Core Files ----
if not exist "stamp_app.py" (
    echo [X] stamp_app.py not found!
    echo     Make sure it is in the same folder as this bat file.
    goto :die
)

if not exist "stamp-compare.html" (
    echo [X] stamp-compare.html not found!
    echo     Make sure it is in the same folder as this bat file.
    goto :die
)

:: ---- Kill Existing Process on Port 8765 ----
for /f "tokens=5" %%p in ('netstat -ano 2^>nul ^| findstr ":8765" ^| findstr "LISTENING"') do (
    taskkill /f /pid %%p >nul 2>&1
)

:: ---- Start Server ----
echo.
echo Starting OCR service (first load ~15s)...
echo A minimized window will appear - DO NOT close it.
echo.

start "StampOCRServer" /min cmd /c "%PYTHON% stamp_app.py --port 8765 --no-browser"

:: ---- Wait for Server (goto loop, max 60s) ----
set WAIT=0

:waitloop
if %WAIT% geq 60 goto :timeout

%PYTHON% -c "import urllib.request;urllib.request.urlopen('http://127.0.0.1:8765/health')" >nul 2>&1
if %errorlevel%==0 goto :server_ok

set /a WAIT+=1
timeout /t 1 /nobreak >nul
goto :waitloop

:timeout
echo [X] Server did not start within 60 seconds.
echo     Check the minimized window for errors.
echo.
goto :die

:server_ok
echo [OK] Server ready!

:: ---- Open Browser ----
start "" "http://127.0.0.1:8765/"

echo.
echo ============================================
echo   Tool is running at: http://127.0.0.1:8765/
echo   To stop: close this window
echo ============================================
echo.
echo Press any key to stop the service...
pause >nul

:: ---- Cleanup ----
taskkill /f /fi "WINDOWTITLE eq StampOCRServer" >nul 2>&1
for /f "tokens=5" %%p in ('netstat -ano 2^>nul ^| findstr ":8765" ^| findstr "LISTENING"') do (
    taskkill /f /pid %%p >nul 2>&1
)
echo Service stopped.
timeout /t 2 /nobreak >nul
goto :eof

:die
echo.
echo Press any key to exit...
pause >nul
