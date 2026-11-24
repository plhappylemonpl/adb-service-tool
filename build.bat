@echo off
REM ADB Service Tool - skrypt budujacy (Windows).
REM Uzycie:  build.bat [dev^|build^|check^|agent]      (domyslnie: build)
REM   agent - tylko aplikacja pomocnicza Agent.apk (android-agent\) -^> src-tauri\resources\agent\
REM Przed dev/build skrypt pyta, czy zbudowac Agent.apk. Bez pytania: set AGENT_BUILD=T (lub N).
setlocal enabledelayedexpansion
cd /d "%~dp0"

set "MODE=%~1"
if "%MODE%"=="" set "MODE=build"
if /i not "%MODE%"=="dev" if /i not "%MODE%"=="build" if /i not "%MODE%"=="check" if /i not "%MODE%"=="agent" (
  echo BLAD: Nieznany tryb "%MODE%". Uzyj: dev ^| build ^| check ^| agent
  exit /b 1
)

set "AGENT_DIR=android-agent"
set "AGENT_RES=src-tauri\resources\agent\Agent.apk"
set "AGENT_BUILT="
set "TAURI_EXTRA="

REM --- tryb: tylko agent (nie wymaga node/cargo) ---
if /i "%MODE%"=="agent" (
  call :build_agent
  if not defined AGENT_BUILT exit /b 1
  call :install_agent "!AGENT_BUILT!"
  exit /b !errorlevel!
)

REM --- wymagane narzedzia ---
where node  >nul 2>&1 || (echo BLAD: Brak 'node'. Zainstaluj Node.js 18+ z https://nodejs.org & exit /b 1)
where npm   >nul 2>&1 || (echo BLAD: Brak 'npm'. & exit /b 1)
where cargo >nul 2>&1 || (echo BLAD: Brak 'cargo' ^(Rust^). Zainstaluj: https://rustup.rs & exit /b 1)

REM --- wersja Node (>=18) ---
for /f "delims=" %%v in ('node -p "process.versions.node.split('.')[0]"') do set "NODE_MAJOR=%%v"
if !NODE_MAJOR! LSS 18 (echo BLAD: Wymagany Node ^>= 18. & exit /b 1)
echo ==^> node major !NODE_MAJOR!

REM --- WebView2 (wbudowany w Windows 10/11; ostrzezenie gdy brak) ---
reg query "HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" >nul 2>&1 || (
  reg query "HKCU\Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" >nul 2>&1 || echo UWAGA: nie wykryto WebView2 Runtime - pobierz z https://developer.microsoft.com/microsoft-edge/webview2/
)

where adb >nul 2>&1 || echo UWAGA: 'adb' nie jest w PATH - aplikacja poprosi o wskazanie adb.exe ^(platform-tools^).

REM --- zaleznosci npm ---
if not exist "node_modules" (
  echo ==^> Instaluje zaleznosci npm...
  call npm install || exit /b 1
)

REM --- ikony (Tauri NIE zbuduje sie bez nich) ---
set "NEED_ICONS="
if not exist "src-tauri\icons\icon.ico" set "NEED_ICONS=1"
if not exist "src-tauri\icons\icon.png" set "NEED_ICONS=1"
if not exist "src-tauri\icons\32x32.png" set "NEED_ICONS=1"
if defined NEED_ICONS (
  echo ==^> Brak ikon - generuje ^(zlote D na czarnym tle^)...
  if not exist "app-icon.png" node scripts\gen-icons.mjs app-icon.png || exit /b 1
  call npx tauri icon app-icon.png || exit /b 1
)

if /i "%MODE%"=="check" (
  echo ==^> svelte-check...
  call npm run check || exit /b 1
  echo ==^> tlumaczenia...
  call npm run --silent check:i18n || exit /b 1
  if not exist "build\index.html" call npm run build || exit /b 1
  echo ==^> cargo check + testy...
  pushd src-tauri
  cargo check || (popd & exit /b 1)
  cargo test --lib || (popd & exit /b 1)
  popd
  echo ==^> OK
  exit /b 0
)

REM --- Agent.apk: pytanie + kompilacja + kopiowanie do zasobow Tauri ---
call :prepare_agent

if /i "%MODE%"=="dev" (
  echo ==^> Tryb developerski ^(hot reload^)...
  call npm run tauri dev -- !TAURI_EXTRA!
  exit /b !errorlevel!
)

echo ==^> Build produkcyjny...
call npm run tauri build -- !TAURI_EXTRA! || exit /b 1
echo ==^> Gotowe. Plik exe: src-tauri\target\release\adb-service-tool.exe
echo ==^> Instalatory ^(msi/nsis^): src-tauri\target\release\bundle\
exit /b 0


REM =========================================================================
REM  Podprogramy - Agent.apk
REM =========================================================================

:prepare_agent
set "ANS=%AGENT_BUILD%"
if not defined ANS set /p "ANS=Czy chcesz najpierw skompilowac aplikacje pomocnicza Agent.apk z folderu android-agent? [T/N] "
if not defined ANS set "ANS=N"
set "ANS=!ANS:~0,1!"
if /i "!ANS!"=="T" call :build_agent
if /i "!ANS!"=="Y" call :build_agent
if defined AGENT_BUILT call :install_agent "!AGENT_BUILT!"

call :apk_valid "%AGENT_RES%"
if !errorlevel! EQU 0 (
  call :apk_compat "%AGENT_RES%"
  if !errorlevel! EQU 0 (
    echo ==^> Agent.apk w zasobach Tauri: OK ^(dolaczony do instalatora i wbudowany w binarke^).
  ) else (
    echo UWAGA: Agent.apk w zasobach jest NIEZGODNY z Androidem 11+ ^(instalacja: Failure [-124]^).
    echo UWAGA: Przebuduj go: odpowiedz T na pytanie o kompilacje albo uruchom build.bat agent.
  )
  exit /b 0
)
call :apk_valid "%AGENT_DIR%\build\Agent.apk"
if !errorlevel! EQU 0 (
  call :install_agent "%AGENT_DIR%\build\Agent.apk"
  if !errorlevel! EQU 0 exit /b 0
)
echo UWAGA: brak poprawnego %AGENT_RES% - aplikacja zostanie zbudowana BEZ funkcji "Kontakty i SMS".
if exist "%AGENT_RES%" del /q "%AGENT_RES%"
set "TAURI_EXTRA=--config "%CD%\src-tauri\tauri.noagent.conf.json""
exit /b 0


REM Kompilacja: 1) Gradle (gradlew.bat / gradle) + Android SDK, 2) build-sdkless.sh przez Git Bash.
REM Ustawia AGENT_BUILT. Nigdy nie przerywa calego skryptu.
:build_agent
set "AGENT_BUILT="
set "REASON="
if not exist "%AGENT_DIR%\" (
  call :agent_error "brak folderu %AGENT_DIR%"
  exit /b 1
)

REM --- Java (JDK 17+ dla Gradle, 11+ dla wersji bez Gradle) ---
set "JAVA_OK="
if defined JAVA_HOME if exist "%JAVA_HOME%\bin\java.exe" set "JAVA_OK=1"
where java >nul 2>&1 && set "JAVA_OK=1"
if not defined JAVA_OK (
  call :agent_error "brak Java/JDK 17+ - zainstaluj np. https://adoptium.net i ustaw JAVA_HOME"
  exit /b 1
)

REM --- Android SDK ---
if not defined ANDROID_HOME if defined ANDROID_SDK_ROOT set "ANDROID_HOME=%ANDROID_SDK_ROOT%"
if not defined ANDROID_HOME if exist "%LOCALAPPDATA%\Android\Sdk\platforms\" set "ANDROID_HOME=%LOCALAPPDATA%\Android\Sdk"
set "HAVE_SDK="
if defined ANDROID_HOME if exist "%ANDROID_HOME%\platforms\" set "HAVE_SDK=1"
if exist "%AGENT_DIR%\local.properties" set "HAVE_SDK=1"

REM --- 1. Gradle ---
set "GRADLE="
if exist "%AGENT_DIR%\gradlew.bat" set "GRADLE=gradlew.bat"
if not defined GRADLE where gradle >nul 2>&1 && set "GRADLE=gradle"
if defined GRADLE if defined HAVE_SDK (
  echo ==^> Buduje Agent.apk przez Gradle ^(!GRADLE! assembleRelease^)...
  pushd "%AGENT_DIR%"
  call !GRADLE! --no-daemon -q assembleRelease
  set "GRADLE_RC=!errorlevel!"
  popd
  if "!GRADLE_RC!"=="0" (
    call :apk_valid "%AGENT_DIR%\app\build\outputs\apk\release\app-release.apk"
    if !errorlevel! EQU 0 (
      call :apk_compat "%AGENT_DIR%\app\build\outputs\apk\release\app-release.apk"
      if !errorlevel! EQU 0 (
        set "AGENT_BUILT=%AGENT_DIR%\app\build\outputs\apk\release\app-release.apk"
        exit /b 0
      )
    )
  )
  set "REASON=Gradle zakonczyl sie bledem ^(kod !GRADLE_RC!^)"
)

REM --- 2. build-sdkless.sh przez Git Bash (kotlinc + R8 + apktool; pobiera brakujace narzedzia) ---
set "GITBASH="
if exist "%ProgramFiles%\Git\bin\bash.exe" set "GITBASH=%ProgramFiles%\Git\bin\bash.exe"
if not defined GITBASH if exist "%ProgramFiles(x86)%\Git\bin\bash.exe" set "GITBASH=%ProgramFiles(x86)%\Git\bin\bash.exe"
if not defined GITBASH if exist "%LOCALAPPDATA%\Programs\Git\bin\bash.exe" set "GITBASH=%LOCALAPPDATA%\Programs\Git\bin\bash.exe"
if defined GITBASH (
  echo ==^> Buduje Agent.apk bez Gradle ^(Git Bash: build-sdkless.sh^)...
  "!GITBASH!" "%AGENT_DIR%/build-sdkless.sh"
  set "SDKLESS_RC=!errorlevel!"
  if "!SDKLESS_RC!"=="0" (
    call :apk_valid "%AGENT_DIR%\build\Agent.apk"
    if !errorlevel! EQU 0 (
      set "AGENT_BUILT=%AGENT_DIR%\build\Agent.apk"
      exit /b 0
    )
  )
  if "!SDKLESS_RC!"=="2" (
    set "REASON=!REASON! brak narzedzi ^(lista wyzej^)"
  ) else (
    set "REASON=!REASON! blad kompilacji ^(kod !SDKLESS_RC!^)"
  )
)

if not defined GRADLE if not defined GITBASH set "REASON=brak Gradle i Git Bash - zainstaluj Android Studio (Gradle + SDK) albo Git for Windows"
if defined GRADLE if not defined HAVE_SDK if not defined GITBASH set "REASON=brak Android SDK - zainstaluj przez Android Studio (SDK Manager) lub ustaw ANDROID_HOME"
if not defined REASON set "REASON=nieznany blad"
call :agent_error "!REASON!"
exit /b 1


:install_agent
REM APK, ktory Android 11+ odrzuci (Failure [-124]), nie trafia do zasobow
call :apk_compat "%~1"
if errorlevel 1 (
  echo UWAGA: %~1 jest niezgodny z Androidem 11+ - nie kopiuje go do zasobow.
  exit /b 1
)
if not exist "src-tauri\resources\agent\" mkdir "src-tauri\resources\agent"
copy /y "%~1" "%AGENT_RES%" >nul || (echo BLAD: nie mozna skopiowac %~1 do %AGENT_RES% & exit /b 1)
for %%A in ("%AGENT_RES%") do echo ==^> Agent.apk -^> %AGENT_RES% ^(%%~zA B^)
exit /b 0


REM Poprawny APK: istnieje, ma ponad 10 KB i zaczyna sie od "PK" (archiwum ZIP).
:apk_valid
if not exist "%~1" exit /b 1
for %%A in ("%~1") do if %%~zA LSS 10000 exit /b 1
set "HDR="
set /p HDR=<"%~1"
if not "!HDR:~0,2!"=="PK" exit /b 1
exit /b 0


:apk_compat
REM Zgodnosc z Androidem 11+ (resources.arsc STORED + wyrownanie 4 B). Bez Javy: nie blokuje.
where java >nul 2>&1 || exit /b 0
java -Dfile.encoding=UTF-8 -cp "%AGENT_DIR%\tools\apkcheck.jar" ApkCheck verify "%~1"
exit /b %errorlevel%


:agent_error
echo.
echo +----------------------------------------------------------------------+
echo ^| Nie udalo sie zbudowac Agent.apk - %~1
echo ^| Kontynuuje budowanie aplikacji na PC z dotychczasowym Agent.apk.
echo +----------------------------------------------------------------------+
echo.
exit /b 0
