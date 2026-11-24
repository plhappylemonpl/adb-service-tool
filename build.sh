#!/usr/bin/env bash
# ADB Service Tool - skrypt budujący (Linux).
# Użycie:  ./build.sh [dev|build|check|windows|agent] [argumenty tauri build]   (domyślnie: build)
#   dev     - tauri dev (hot reload)
#   build   - tauri build (produkcyjny pakiet dla Linuksa)
#   check   - svelte-check + cargo check + testy jednostkowe Rust
#   windows - cross-compile z Linuksa: instalator NSIS + przenośny .exe dla Windows x64
#             (cargo-xwin + target x86_64-pc-windows-msvc; alias: win)
#   agent   - tylko aplikacja pomocnicza Agent.apk (android-agent/) -> src-tauri/resources/agent/
#
# Przed dev/build/windows skrypt pyta, czy zbudować Agent.apk. Bez terminala (CI)
# odpowiedź bierze ze zmiennej AGENT_BUILD=T|N (domyślnie N).
set -euo pipefail

cd "$(dirname "$0")"

MODE="${1:-build}"
GOLD='\033[38;2;201;162;73m'
RED='\033[0;31m'
YEL='\033[0;33m'
NC='\033[0m'

say()  { echo -e "${GOLD}==>${NC} $*"; }
warn() { echo -e "${YEL}UWAGA:${NC} $*"; }
die()  { echo -e "${RED}BŁĄD:${NC} $*" >&2; exit 1; }

# dalsze argumenty trafiają do `tauri build` (np. ./build.sh build --bundles appimage,deb)
shift || true
USER_ARGS=("$@")
[ "$MODE" = "win" ] && MODE="windows"
case "$MODE" in dev|build|check|windows|agent) ;; *) die "Nieznany tryb '$MODE'. Użyj: dev | build | check | windows | agent" ;; esac

# --------------------------------------------------------------------------- #
# Aplikacja pomocnicza Agent.apk (Kontakty i SMS)
# --------------------------------------------------------------------------- #
AGENT_DIR="android-agent"
AGENT_RES="src-tauri/resources/agent/Agent.apk"
# dodatkowe argumenty `tauri build/dev` (np. build bez agenta)
TAURI_EXTRA=()

# poprawny APK: archiwum ZIP z manifestem i kodem
apk_valid() {
  [ -f "$1" ] || return 1
  [ "$(head -c 2 "$1" 2>/dev/null)" = "PK" ] || return 1
  [ "$(wc -c < "$1" | tr -d ' ')" -gt 10000 ] || return 1
  grep -qa "AndroidManifest.xml" "$1" && grep -qa "classes.dex" "$1"
}

# Zgodność z Androidem 11+ (resources.arsc STORED + wyrównanie 4 B) - android-agent/tools/apkcheck.jar.
# Bez Javy nie da się sprawdzić -> 0 (nie blokujemy buildu PC).
apk_compat() {
  command -v java >/dev/null 2>&1 || return 0
  java -Dfile.encoding=UTF-8 -Dstdout.encoding=UTF-8 -Dstderr.encoding=UTF-8 \
    -cp "$AGENT_DIR/tools/apkcheck.jar" ApkCheck verify "$1" 2>&1 | grep -v '^Picked up '
  return "${PIPESTATUS[0]}"
}

agent_banner_error() {
  echo -e "${RED}┌──────────────────────────────────────────────────────────────────────┐${NC}"
  echo -e "${RED}│${NC} Nie udało się zbudować Agent.apk - $1"
  echo -e "${RED}│${NC} Kontynuuję budowanie aplikacji na PC z dotychczasowym Agent.apk."
  echo -e "${RED}└──────────────────────────────────────────────────────────────────────┘${NC}"
}

ask_agent() {
  local ans="${AGENT_BUILD:-}"
  if [ -z "$ans" ]; then
    if [ -t 0 ]; then
      read -r -p "Czy chcesz najpierw skompilować aplikację pomocniczą Agent.apk z folderu android-agent? [T/N] " ans || ans="N"
    else
      say "Brak terminala - pomijam kompilację Agent.apk (wymuś: AGENT_BUILD=T)."
      ans="N"
    fi
  fi
  case "$ans" in [TtYy]*) return 0 ;; *) return 1 ;; esac
}

# Kompilacja: 1) build-sdkless.sh (kotlinc + R8 + apktool), 2) Gradle (gradlew / gradle).
# Ustawia AGENT_BUILT na ścieżkę zbudowanego APK. Zwraca 1 przy porażce (bez przerywania skryptu).
AGENT_BUILT=""
build_agent() {
  [ -d "$AGENT_DIR" ] || { agent_banner_error "brak folderu $AGENT_DIR"; return 1; }
  local rc=0 reason=""
  say "Buduję Agent.apk (kotlinc + R8 + apktool)..."
  bash "$AGENT_DIR/build-sdkless.sh" || rc=$?
  if [ $rc -eq 0 ] && apk_valid "$AGENT_DIR/build/Agent.apk"; then
    AGENT_BUILT="$AGENT_DIR/build/Agent.apk"
    return 0
  fi
  if [ $rc -eq 2 ]; then reason="brak narzędzi (lista wyżej)"; else reason="błąd kompilacji (kod $rc, log wyżej)"; fi

  # Gradle (Android Studio): gradlew w projekcie albo gradle w PATH + Android SDK
  local gradle=""
  if [ -x "$AGENT_DIR/gradlew" ]; then gradle="./gradlew"; elif command -v gradle >/dev/null 2>&1; then gradle="gradle"; fi
  local sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
  [ -z "$sdk" ] && [ -d "$HOME/Android/Sdk" ] && sdk="$HOME/Android/Sdk"
  if [ -n "$gradle" ] && { [ -n "$sdk" ] || [ -f "$AGENT_DIR/local.properties" ]; } && command -v java >/dev/null 2>&1; then
    say "Próbuję Gradle ($gradle assembleRelease)..."
    if (cd "$AGENT_DIR" && ANDROID_HOME="${sdk:-}" $gradle --no-daemon -q assembleRelease); then
      local out="$AGENT_DIR/app/build/outputs/apk/release/app-release.apk"
      if apk_valid "$out" && apk_compat "$out"; then
        AGENT_BUILT="$out"
        return 0
      fi
      reason="Gradle nie utworzył poprawnego $out (resources.arsc / wyrównanie)"
    else
      reason="$reason; Gradle też zakończył się błędem"
    fi
  fi
  agent_banner_error "$reason"
  return 1
}

install_agent_apk() {
  # APK, który Android 11+ odrzuci (Failure [-124]), nie trafia do zasobów
  if ! apk_compat "$1"; then
    warn "$1 jest niezgodny z Androidem 11+ - nie kopiuję go do zasobów."
    return 1
  fi
  mkdir -p "$(dirname "$AGENT_RES")"
  cp -f "$1" "$AGENT_RES"
  say "Agent.apk -> $AGENT_RES ($(wc -c < "$AGENT_RES" | tr -d ' ') B, sha256 $(sha256sum "$AGENT_RES" 2>/dev/null | cut -c1-12))"
}

# Pytanie + kompilacja + kopiowanie do zasobów Tauri. Gdy nie ma żadnego APK,
# aplikacja buduje się bez agenta (build.rs pomija include_bytes!, bundle bez zasobu).
prepare_agent() {
  if ask_agent && build_agent; then
    install_agent_apk "$AGENT_BUILT"
  fi
  if apk_valid "$AGENT_RES"; then
    if apk_compat "$AGENT_RES"; then
      say "Agent.apk w zasobach Tauri: OK (zostanie dołączony do instalatora i wbudowany w binarkę)."
    else
      warn "Agent.apk w zasobach jest NIEZGODNY z Androidem 11+ (instalacja skończy się Failure [-124])."
      warn "Przebuduj go: odpowiedz T na pytanie o kompilację albo uruchom ./build.sh agent."
    fi
  elif apk_valid "$AGENT_DIR/build/Agent.apk" && install_agent_apk "$AGENT_DIR/build/Agent.apk"; then
    :
  else
    warn "brak poprawnego $AGENT_RES - aplikacja zostanie zbudowana BEZ funkcji \"Kontakty i SMS\"."
    rm -f "$AGENT_RES"
    TAURI_EXTRA=(--config "$PWD/src-tauri/tauri.noagent.conf.json")
  fi
}

# tryb: tylko agent (nie wymaga node/cargo)
if [ "$MODE" = "agent" ]; then
  build_agent || exit 1
  install_agent_apk "$AGENT_BUILT" || exit 1
  exit 0
fi

# --- wymagane narzędzia ---
command -v node  >/dev/null 2>&1 || die "Brak 'node'. Zainstaluj Node.js 18+ (Arch: sudo pacman -S nodejs npm)."
command -v npm   >/dev/null 2>&1 || die "Brak 'npm'."
command -v cargo >/dev/null 2>&1 || die "Brak 'cargo' (Rust). Zainstaluj: https://rustup.rs"

NODE_MAJOR="$(node -p 'process.versions.node.split(".")[0]')"
[ "$NODE_MAJOR" -ge 18 ] || die "Wymagany Node >= 18 (masz $(node -v))."
say "node $(node -v), $(cargo --version)"

# --- zależności systemowe Tauri v2 (tylko ostrzeżenie; przy cross-compile niepotrzebne) ---
if [ "$MODE" = "windows" ]; then
  :
elif command -v pkg-config >/dev/null 2>&1; then
  MISSING=()
  for pkg in webkit2gtk-4.1 gtk+-3.0 librsvg-2.0 libsoup-3.0; do
    pkg-config --exists "$pkg" 2>/dev/null || MISSING+=("$pkg")
  done
  if [ ${#MISSING[@]} -gt 0 ]; then
    warn "brak bibliotek systemowych: ${MISSING[*]}"
    echo "    Arch:          sudo pacman -S --needed webkit2gtk-4.1 gtk3 librsvg libsoup3 base-devel openssl libayatana-appindicator"
    echo "    Debian/Ubuntu: sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libsoup-3.0-dev build-essential libssl-dev libayatana-appindicator3-dev"
    echo "    Fedora:        sudo dnf install webkit2gtk4.1-devel gtk3-devel librsvg2-devel libsoup3-devel openssl-devel"
  fi
else
  warn "brak pkg-config - nie sprawdzę zależności systemowych Tauri."
fi

command -v adb >/dev/null 2>&1 || warn "'adb' nie jest w PATH - aplikacja poprosi o wskazanie pliku (Arch: sudo pacman -S android-tools)."

# --- zależności npm ---
if [ ! -d node_modules ]; then
  say "Instaluję zależności npm..."
  npm install
fi

# --- ikony (Tauri NIE zbuduje się bez nich) ---
if [ ! -f src-tauri/icons/icon.png ] || [ ! -f src-tauri/icons/icon.ico ] || [ ! -f src-tauri/icons/32x32.png ]; then
  say "Brak ikon - generuję (złote D na czarnym tle)..."
  [ -f app-icon.png ] || node scripts/gen-icons.mjs app-icon.png
  npx tauri icon app-icon.png
fi

# --- cross-compile na Windows (x64) ---
WIN_TARGET="x86_64-pc-windows-msvc"

# --- NSIS (instalator Windows) przy budowaniu z Linuksa ---
# Szablon src-tauri/windows/installer.nsi nie używa wtyczki nsis_tauri_utils.dll,
# ale nadal potrzebuje standardowych wtyczek NSIS (System, nsDialogs, NSISdl,
# StartMenu, LangDLL). Część linuksowych paczek NSIS ich nie ma albo ma w innym
# miejscu - wtedy dociągamy je z paczki Ubuntu nsis-common (przypięta suma SHA-256)
# i podajemy makensis przez NSISDIR (nakładka: Stubs/Include z systemu + Plugins).
NSIS_PLUGINS_DEB_URL="https://archive.ubuntu.com/ubuntu/pool/universe/n/nsis/nsis-common_3.09-4ubuntu1_all.deb"
NSIS_PLUGINS_DEB_SHA="49471b9debdee4751ebb4caf6b705cc4c678eebfaecefbe56362258893f85859"
NSIS_WRAP_DIR="$PWD/src-tauri/target/.nsis-wrap"

nsis_system_dir() {
  local d
  d="$(makensis -HDRINFO 2>/dev/null | tr ',' '\n' | sed -n 's/^NSISDIR=//p' | head -n1)"
  if [ -z "$d" ] || [ ! -d "$d/Stubs" ]; then
    for d in "$(dirname "$(readlink -f "$(command -v makensis)")")/../share/nsis" /usr/share/nsis /usr/local/share/nsis /usr/lib/nsis; do
      [ -d "$d/Stubs" ] && break
    done
  fi
  [ -d "$d/Stubs" ] || return 1
  (cd "$d" && pwd)
}

# Kompiluje mały skrypt używający wszystkich wtyczek z szablonu. Zwraca kod makensis.
nsis_preflight() {
  local dir="$NSIS_WRAP_DIR/preflight" log="$NSIS_WRAP_DIR/preflight.log"
  mkdir -p "$dir"
  cat > "$dir/preflight.nsi" <<'NSI'
Unicode true
OutFile "preflight.exe"
RequestExecutionLevel user
ReserveFile /plugin LangDLL.dll
ReserveFile /plugin StartMenu.dll
Section
  System::Call 'kernel32::GetTickCount() i .r0'
  NSISdl::download "https://localhost/x" "$TEMP\x"
  nsDialogs::Create 1018
SectionEnd
NSI
  (cd "$dir" && "$NSIS_WRAP_DIR/makensis" -V2 preflight.nsi) > "$log" 2>&1
}

# Wtyczki z paczki nsis-common (Ubuntu 24.04, NSIS 3.09) do ~/.cache.
nsis_fetch_plugins() {
  local cache="${XDG_CACHE_HOME:-$HOME/.cache}/adb-service-tool/nsis-3.09"
  if [ -f "$cache/Plugins/x86-unicode/System.dll" ]; then echo "$cache"; return 0; fi
  command -v curl >/dev/null 2>&1 || { warn "brak curl - nie pobiorę wtyczek NSIS" >&2; return 1; }
  command -v ar   >/dev/null 2>&1 || { warn "brak 'ar' (binutils) - nie rozpakuję paczki z wtyczkami NSIS" >&2; return 1; }
  local tmp; tmp="$(mktemp -d)"
  say "Pobieram standardowe wtyczki NSIS (nsis-common 3.09, SHA-256 przypięte)..." >&2
  if ! curl -fsSL --proto '=https' -o "$tmp/nsis-common.deb" "$NSIS_PLUGINS_DEB_URL"; then
    rm -rf "$tmp"; warn "nie udało się pobrać $NSIS_PLUGINS_DEB_URL" >&2; return 1
  fi
  if [ "$(sha256sum "$tmp/nsis-common.deb" | cut -d' ' -f1)" != "$NSIS_PLUGINS_DEB_SHA" ]; then
    rm -rf "$tmp"; warn "suma SHA-256 paczki nsis-common nie zgadza się - przerywam" >&2; return 1
  fi
  (cd "$tmp" && ar x nsis-common.deb) || { rm -rf "$tmp"; return 1; }
  local data; data="$(ls "$tmp"/data.tar.* | head -n1)"
  mkdir -p "$tmp/x"
  if ! tar -xf "$data" -C "$tmp/x" ./usr/share/nsis/Plugins 2>/dev/null; then
    rm -rf "$tmp"; warn "nie rozpakuję $data (Arch: sudo pacman -S zstd)" >&2; return 1
  fi
  mkdir -p "$cache"
  rm -rf "$cache/Plugins"
  cp -r "$tmp/x/usr/share/nsis/Plugins" "$cache/Plugins"
  rm -rf "$tmp"
  echo "$cache"
}

# Nakładka $NSIS_WRAP_DIR/makensis:
#  - usuwa "-OUTPUTCHARSET UTF8" (Tauri dodaje go zawsze; linuksowy makensis
#    odpowiada ostrzeżeniem 5202, bo ta opcja działa tylko na Windows),
#  - opcjonalnie ustawia NSISDIR na nakładkę z kompletem wtyczek.
nsis_write_wrapper() {
  local real="$1" nsisdir="${2:-}"
  mkdir -p "$NSIS_WRAP_DIR"
  {
    echo '#!/bin/sh'
    echo '# Wygenerowane przez build.sh (tryb windows) - nie edytuj.'
    [ -n "$nsisdir" ] && echo "export NSISDIR='$nsisdir'"
    echo 'skip=0'
    echo 'for a in "$@"; do'
    echo '  shift'
    echo '  if [ "$skip" = 1 ]; then skip=0; continue; fi'
    echo '  case "$a" in -OUTPUTCHARSET|/OUTPUTCHARSET) skip=1; continue ;; esac'
    echo '  set -- "$@" "$a"'
    echo 'done'
    echo "exec '$real' \"\$@\""
  } > "$NSIS_WRAP_DIR/makensis"
  chmod +x "$NSIS_WRAP_DIR/makensis"
}

prepare_nsis() {
  local real sysdir plugins overlay
  real="$(command -v makensis)"
  [ "$real" = "$NSIS_WRAP_DIR/makensis" ] && real="$(PATH="${PATH//$NSIS_WRAP_DIR:/}" command -v makensis)"
  say "NSIS: $real ($("$real" -VERSION 2>/dev/null || echo '?'))"

  # grafiki instalatora (w repo są gotowe - generujemy tylko, gdy ich brak)
  if [ ! -f src-tauri/windows/installer-sidebar.bmp ] || [ ! -f src-tauri/windows/installer-header.bmp ]; then
    say "Generuję grafiki instalatora (czerń + złoto)..."
    node scripts/gen-installer-art.mjs src-tauri/windows
  fi

  nsis_write_wrapper "$real"
  if ! nsis_preflight; then
    warn "makensis nie widzi standardowych wtyczek NSIS:"
    grep -iE "plugin|error" "$NSIS_WRAP_DIR/preflight.log" | head -n 5 | sed 's/^/    /'
    sysdir="$(nsis_system_dir)" || die "nie znalazłem katalogu danych NSIS (Stubs/). Przeinstaluj NSIS (Arch: yay -S nsis)."
    plugins="$(nsis_fetch_plugins)" || die "brak wtyczek NSIS. Przeinstaluj NSIS z pełnym zestawem wtyczek (Arch: yay -S nsis, Debian/Ubuntu: sudo apt install nsis)."
    overlay="$NSIS_WRAP_DIR/nsisdir"
    rm -rf "$overlay"; mkdir -p "$overlay"
    local e
    for e in "$sysdir"/*; do
      [ "$(basename "$e")" = "Plugins" ] || ln -s "$e" "$overlay/"
    done
    ln -s "$plugins/Plugins" "$overlay/Plugins"
    nsis_write_wrapper "$real" "$overlay"
    nsis_preflight || { sed 's/^/    /' "$NSIS_WRAP_DIR/preflight.log" | tail -n 15; die "makensis nadal nie ładuje wtyczek (log: $NSIS_WRAP_DIR/preflight.log)"; }
    say "NSIS: wtyczki z nakładki $overlay"
  fi
  export PATH="$NSIS_WRAP_DIR:$PATH"
}

build_windows() {
  # 1. narzędzia systemowe: NSIS (instalator), LLVM (llvm-rc, llvm-lib), lld (lld-link), clang (clang-cl dla kodu C, np. ring)
  local missing=()
  command -v makensis  >/dev/null 2>&1 || missing+=("nsis (makensis)")
  command -v lld-link  >/dev/null 2>&1 || missing+=("lld (lld-link)")
  command -v llvm-rc   >/dev/null 2>&1 || missing+=("llvm (llvm-rc)")
  command -v clang-cl  >/dev/null 2>&1 || missing+=("clang (clang-cl)")
  if [ ${#missing[@]} -gt 0 ]; then
    warn "brak narzędzi do cross-compile: ${missing[*]}"
    echo "    Arch:          sudo pacman -S --needed lld llvm clang && yay -S nsis"
    echo "    Debian/Ubuntu: sudo apt install nsis lld llvm clang"
    echo "    Fedora:        sudo dnf install mingw32-nsis lld llvm clang"
    die "doinstaluj powyższe i uruchom ponownie: ./build.sh windows"
  fi

  # 2. target Rust + cargo-xwin (instalowane automatycznie przy pierwszym użyciu)
  if command -v rustup >/dev/null 2>&1; then
    if ! rustup target list --installed | grep -qx "$WIN_TARGET"; then
      say "Dodaję target Rust $WIN_TARGET..."
      rustup target add "$WIN_TARGET"
    fi
  else
    warn "brak rustup - zakładam, że target $WIN_TARGET jest już zainstalowany."
  fi
  if ! command -v cargo-xwin >/dev/null 2>&1; then
    say "Instaluję cargo-xwin (jednorazowo, kilka minut)..."
    cargo install --locked cargo-xwin
  fi

  # 3. NSIS: wtyczki + nakładka makensis (bez ostrzeżenia 5202)
  prepare_nsis

  # 4. build - przy pierwszym uruchomieniu cargo-xwin pobiera CRT/SDK Windows od Microsoftu
  #    (kilkaset MB, akceptacja licencji Microsoft; cache w ~/.cache/cargo-xwin)
  #    /IGNORE:4099 - biblioteki MSVC z xwin nie mają plików .pdb; lld-link zgłaszał
  #    to jako LNK4099 przy każdym linkowaniu (bez wpływu na działanie programu).
  local rf_var="CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS"
  case " ${!rf_var:-} " in
    *"/IGNORE:4099"*) ;;
    *) export "$rf_var=${!rf_var:+${!rf_var} }-C link-arg=/IGNORE:4099" ;;
  esac
  say "Cross-compile dla Windows ($WIN_TARGET)..."
  npm run tauri build -- --runner cargo-xwin --target "$WIN_TARGET" ${TAURI_EXTRA[@]+"${TAURI_EXTRA[@]}"} ${USER_ARGS[@]+"${USER_ARGS[@]}"}

  # 5. zebranie wyników do dist/windows
  local rel="src-tauri/target/$WIN_TARGET/release"
  local out="dist/windows"
  mkdir -p "$out"
  local exe="$rel/adb-service-tool.exe"
  [ -f "$exe" ] || die "nie powstał $exe"
  cp -f "$exe" "$out/ADB-Service-Tool-portable.exe"
  local setup
  setup="$(ls -t "$rel"/bundle/nsis/*-setup.exe 2>/dev/null | head -n1 || true)"
  if [ -n "$setup" ]; then
    cp -f "$setup" "$out/"
  else
    warn "nie znaleziono instalatora NSIS w $rel/bundle/nsis/"
  fi
  say "Gotowe. Pliki dla Windows w: $out/"
  ls -lh "$out"
  echo "    Uwaga: exe nie jest podpisany - SmartScreen pokaże \"nieznany wydawca\" (Więcej informacji -> Uruchom mimo to)."
  echo "    Wersja portable wymaga WebView2 (wbudowany w Windows 10/11); instalator doinstaluje go sam."
}

case "$MODE" in
  windows)
    prepare_agent
    build_windows
    ;;
  dev)
    prepare_agent
    say "Tryb developerski (hot reload)..."
    npm run tauri dev -- ${TAURI_EXTRA[@]+"${TAURI_EXTRA[@]}"}
    ;;
  check)
    say "svelte-check..."
    npm run check
    say "tłumaczenia (klucze, placeholdery, dopasowanie locale)..."
    npm run --silent check:i18n
    say "cargo check + testy..."
    [ -f build/index.html ] || npm run build
    (cd src-tauri && cargo check && cargo test --lib)
    say "OK"
    ;;
  build)
    prepare_agent
    say "Build produkcyjny..."
    npm run tauri build -- ${TAURI_EXTRA[@]+"${TAURI_EXTRA[@]}"} ${USER_ARGS[@]+"${USER_ARGS[@]}"}
    say "Gotowe. Plik wykonywalny: src-tauri/target/release/adb-service-tool"
    say "Pakiety (deb/rpm/AppImage): src-tauri/target/release/bundle/"
    ;;
esac
