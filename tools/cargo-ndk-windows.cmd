@echo off
setlocal

set "SYS=%SystemRoot:~0,2%"
rem DroidBridge command environments may omit standard Windows drive/profile variables.
rem Normalize them before invoking Visual Studio or Cargo so host tooling cannot create
rem relative %%SystemDrive%%\ProgramData trees inside the repository.
set "SystemDrive=%SYS%"
set "ProgramData=%SYS%\ProgramData"
set "ALLUSERSPROFILE=%ProgramData%"

rem Native build helpers may invoke vswhere.exe by name after vcvars setup. Expose the
rem already-installed Visual Studio Installer directory without changing tool selection.
set "VSINSTALLER=%SYS%\Program Files (x86)\Microsoft Visual Studio\Installer"
if exist "%VSINSTALLER%\vswhere.exe" set "PATH=%VSINSTALLER%;%PATH%"

if defined VCToolsInstallDir goto run

set "VCVARS="
for %%R in ("%SYS%\Program Files (x86)" "%SYS%\Program Files") do (
  for /d %%Y in ("%%~R\Microsoft Visual Studio\*") do (
    for /d %%E in ("%%~fY\*") do (
      if exist "%%~fE\VC\Auxiliary\Build\vcvars64.bat" if not defined VCVARS set "VCVARS=%%~fE\VC\Auxiliary\Build\vcvars64.bat"
    )
  )
)

if not defined VCVARS (
  echo DroidBridge build error: MSVC vcvars64.bat not found. 1>&2
  exit /b 2
)

call "%VCVARS%" >nul
if errorlevel 1 exit /b %errorlevel%

:run
cargo %*
exit /b %errorlevel%
