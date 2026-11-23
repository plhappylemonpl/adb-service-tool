#!/usr/bin/env bash
# Buduje Agent.apk BEZ Android Studio / Gradle: kotlinc -> R8 -> apktool (aapt2) -> ApkCheck fix
# (resources.arsc STORED + wyrównanie) -> zipalign -p -f 4 -> uber-apk-signer -> ApkCheck verify.
# Wynik: android-agent/build/Agent.apk (kopiowanie do zasobów Tauri robi ../build.sh).
#
# Narzędzia są wyszukiwane automatycznie (zmienne środowiskowe mają pierwszeństwo):
#   JAVA          - Java 11+: $JAVA_HOME, PATH, albo JRE pobrane przez APK DevTools aplikacji
#   KOTLINC_HOME  - kotlinc z PATH, albo pobierany (przypięta wersja + SHA-256) do .tools/
#   ANDROID_JAR   - <SDK>/platforms/android-NN/android.jar (NN >= 29), najnowszy
#   D8_JAR        - <SDK>/build-tools/<wersja>/lib/d8.jar (zawiera R8), najnowszy
#   APKTOOL_JAR   - z narzędzi APK DevTools aplikacji albo pobierany do .tools/
#   SIGNER_JAR    - uber-apk-signer: j.w.
#   SDK szukany w: $ANDROID_HOME, $ANDROID_SDK_ROOT, ~/Android/Sdk, /opt/android-sdk,
#   ~/Library/Android/sdk, %LOCALAPPDATA%\Android\Sdk (Git Bash).
#
# Kody wyjścia: 0 = OK, 2 = brak narzędzi (nic nie zbudowano), 1 = błąd kompilacji.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="$HERE/build"
TOOLS="$HERE/.tools"
PKG="com.drapco.agent"
VERSION_CODE=2
VERSION_NAME="1.1"
MIN_SDK=21
TARGET_SDK=34
MIN_COMPILE_API=29
APP_ID="com.drapco.adbservicetool"

KOTLIN_URL="https://github.com/JetBrains/kotlin/releases/download/v2.0.21/kotlin-compiler-2.0.21.zip"
KOTLIN_SHA="0352c0a45bd22f80f6b26e485cd04da8047baa5de54865281fb9f89a4a7bcf2a"
APKTOOL_URL="https://github.com/iBotPeaches/Apktool/releases/download/v2.11.1/apktool_2.11.1.jar"
APKTOOL_SHA="56d59c524fc764263ba8d345754d8daf55b1887818b15cd3b594f555d249e2db"
SIGNER_URL="https://github.com/patrickfav/uber-apk-signer/releases/download/v1.3.0/uber-apk-signer-1.3.0.jar"
SIGNER_SHA="e1299fd6fcf4da527dd53735b56127e8ea922a321128123b9c32d619bba1d835"

say()  { echo "  [agent] $*"; }
MISSING=()
missing() { MISSING+=("$1"); }

# ------------------------------------------------------------------ pomocnicze
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1
  else echo "brak-sha256"; fi
}

# fetch URL SHA PLIK - tylko HTTPS + weryfikacja SHA-256
fetch() {
  local url="$1" sha="$2" dest="$3"
  mkdir -p "$(dirname "$dest")"
  say "pobieram $(basename "$dest") ..."
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --proto '=https' -o "$dest.part" "$url" || return 1
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$dest.part" "$url" || return 1
  else
    return 1
  fi
  local got
  got="$(sha256_of "$dest.part")"
  if [ "$got" != "$sha" ]; then
    rm -f "$dest.part"
    say "SHA-256 się nie zgadza dla $(basename "$dest") (oczekiwano $sha, jest $got) - plik usunięty"
    return 1
  fi
  mv -f "$dest.part" "$dest"
}

unzip_to() {
  local zip="$1" dir="$2"
  mkdir -p "$dir"
  if command -v unzip >/dev/null 2>&1; then unzip -q -o "$zip" -d "$dir"
  elif command -v bsdtar >/dev/null 2>&1; then bsdtar -xf "$zip" -C "$dir"
  elif [ -n "${JAR_TOOL:-}" ]; then (cd "$dir" && "$JAR_TOOL" xf "$zip")
  else return 1; fi
}

java_major() {
  local v
  v="$("$1" -version 2>&1 | grep -m1 -Eo 'version "[^"]+"' | cut -d'"' -f2)"
  case "$v" in 1.*) echo "${v#1.}" | cut -d. -f1 ;; *) echo "${v%%[.+-]*}" ;; esac
}

newest() { ls -d "$@" 2>/dev/null | sort -V | tail -n1; }

# ------------------------------------------------------------------ Java
app_data_dirs() {
  echo "${XDG_DATA_HOME:-$HOME/.local/share}/$APP_ID"
  echo "$HOME/Library/Application Support/$APP_ID"
  if [ -n "${APPDATA:-}" ]; then
    if command -v cygpath >/dev/null 2>&1; then echo "$(cygpath -u "$APPDATA")/$APP_ID"; else echo "$APPDATA/$APP_ID"; fi
  fi
}

find_java() {
  local c=()
  [ -n "${JAVA:-}" ] && c+=("$JAVA")
  [ -n "${JAVA_HOME:-}" ] && c+=("$JAVA_HOME/bin/java")
  command -v java >/dev/null 2>&1 && c+=("$(command -v java)")
  while IFS= read -r d; do
    for j in "$d"/apk-devtools/tools/jre/*/bin/java "$d"/apk-devtools/tools/jre/*/bin/java.exe; do
      [ -x "$j" ] && c+=("$j")
    done
  done < <(app_data_dirs)
  for j in "${c[@]}"; do
    if [ -x "$j" ] || [ -f "$j" ]; then
      local m
      m="$(java_major "$j")"
      if [ -n "$m" ] && [ "$m" -ge 11 ] 2>/dev/null; then echo "$j"; return 0; fi
    fi
  done
  return 1
}

# ------------------------------------------------------------------ SDK
find_sdk_file() { # $1 = wzorzec względem SDK (np. platforms/android-*/android.jar)
  local roots=()
  [ -n "${ANDROID_HOME:-}" ] && roots+=("$ANDROID_HOME")
  [ -n "${ANDROID_SDK_ROOT:-}" ] && roots+=("$ANDROID_SDK_ROOT")
  roots+=("$HOME/Android/Sdk" "$HOME/Android/sdk" "/opt/android-sdk" "/usr/lib/android-sdk" "$HOME/Library/Android/sdk")
  if [ -n "${LOCALAPPDATA:-}" ] && command -v cygpath >/dev/null 2>&1; then
    roots+=("$(cygpath -u "$LOCALAPPDATA")/Android/Sdk")
  fi
  if [ -f "$HERE/local.properties" ]; then
    local p
    p="$(grep -E '^sdk\.dir=' "$HERE/local.properties" | head -n1 | cut -d= -f2- | sed 's#\\:#:#g; s#\\\\#/#g')"
    [ -n "$p" ] && roots+=("$p")
  fi
  local best="" r f
  for r in "${roots[@]}"; do
    # wzorzec (z *) poza cudzysłowem, katalog SDK w cudzysłowie - spacje w ścieżce OK
    # shellcheck disable=SC2086
    f="$(newest "$r"/$1)"
    if [ -n "$f" ] && { [ -z "$best" ] || [ "$(printf '%s\n%s\n' "$best" "$f" | sort -V | tail -n1)" = "$f" ]; }; then
      best="$f"
    fi
  done
  [ -n "$best" ] && echo "$best"
}

find_tool_jar() { # $1 = prefiks pliku w narzędziach APK DevTools
  local d j
  while IFS= read -r d; do
    j="$(newest "$d"/apk-devtools/tools/"$1"*.jar)"
    [ -n "$j" ] && { echo "$j"; return 0; }
  done < <(app_data_dirs)
  j="$(newest "$TOOLS"/"$1"*.jar)"
  [ -n "$j" ] && echo "$j"
}

# ------------------------------------------------------------------ wyszukiwanie
say "Szukam narzędzi..."

JAVA_BIN="$(find_java || true)"
if [ -z "$JAVA_BIN" ]; then
  missing "Java 11+ (JDK/JRE) - np. Arch: sudo pacman -S jdk17-openjdk; Windows: https://adoptium.net"
else
  export JAVA_HOME="$(cd "$(dirname "$JAVA_BIN")/.." && pwd)"
  export PATH="$JAVA_HOME/bin:$PATH"
  [ -x "$JAVA_HOME/bin/jar" ] && JAR_TOOL="$JAVA_HOME/bin/jar"
  say "Java: $JAVA_BIN (wersja $(java_major "$JAVA_BIN"))"
fi

if [ -z "${ANDROID_JAR:-}" ]; then
  ANDROID_JAR="$(find_sdk_file 'platforms/android-*/android.jar' || true)"
fi
if [ -n "${ANDROID_JAR:-}" ]; then
  api="$(basename "$(dirname "$ANDROID_JAR")" | sed 's/[^0-9]//g')"
  if [ -n "$api" ] && [ "$api" -lt "$MIN_COMPILE_API" ]; then
    missing "Android SDK Platform $MIN_COMPILE_API+ (znaleziono tylko API $api: $ANDROID_JAR)"
    ANDROID_JAR=""
  fi
else
  missing "Android SDK: platforms/android-$MIN_COMPILE_API+/android.jar (Android Studio -> SDK Manager, albo sdkmanager \"platforms;android-34\"; ustaw ANDROID_HOME)"
fi

if [ -z "${D8_JAR:-}" ]; then
  D8_JAR="$(find_sdk_file 'build-tools/*/lib/d8.jar' || true)"
fi
[ -n "${D8_JAR:-}" ] || missing "Android SDK Build-Tools (lib/d8.jar) - sdkmanager \"build-tools;34.0.0\""

# narzędzia pobieralne - tylko gdy jest Java (bez niej i tak nic nie zbudujemy)
if [ -n "$JAVA_BIN" ]; then
  if [ -z "${KOTLINC_HOME:-}" ]; then
    if command -v kotlinc >/dev/null 2>&1; then
      KOTLINC_HOME="$(cd "$(dirname "$(readlink -f "$(command -v kotlinc)")")/.." && pwd)"
    elif [ -x "$TOOLS/kotlinc/bin/kotlinc" ]; then
      KOTLINC_HOME="$TOOLS/kotlinc"
    elif fetch "$KOTLIN_URL" "$KOTLIN_SHA" "$TOOLS/kotlin-compiler.zip" && unzip_to "$TOOLS/kotlin-compiler.zip" "$TOOLS"; then
      rm -f "$TOOLS/kotlin-compiler.zip"
      chmod +x "$TOOLS"/kotlinc/bin/* 2>/dev/null || true
      KOTLINC_HOME="$TOOLS/kotlinc"
    fi
  fi
  [ -n "${KOTLINC_HOME:-}" ] && [ -f "$KOTLINC_HOME/lib/kotlin-stdlib.jar" ] \
    || missing "Kotlin compiler (kotlinc) - nie znaleziono i nie udało się pobrać"

  [ -n "${APKTOOL_JAR:-}" ] || APKTOOL_JAR="$(find_tool_jar apktool || true)"
  if [ -z "${APKTOOL_JAR:-}" ] && fetch "$APKTOOL_URL" "$APKTOOL_SHA" "$TOOLS/apktool_2.11.1.jar"; then
    APKTOOL_JAR="$TOOLS/apktool_2.11.1.jar"
  fi
  [ -n "${APKTOOL_JAR:-}" ] || missing "apktool 2.11+ (nie udało się pobrać)"

  [ -n "${SIGNER_JAR:-}" ] || SIGNER_JAR="$(find_tool_jar uber-apk-signer || true)"
  if [ -z "${SIGNER_JAR:-}" ] && fetch "$SIGNER_URL" "$SIGNER_SHA" "$TOOLS/uber-apk-signer-1.3.0.jar"; then
    SIGNER_JAR="$TOOLS/uber-apk-signer-1.3.0.jar"
  fi
  [ -n "${SIGNER_JAR:-}" ] || missing "uber-apk-signer (nie udało się pobrać)"
fi

if [ ${#MISSING[@]} -gt 0 ]; then
  echo "  [agent] BRAK NARZĘDZI do zbudowania Agent.apk:"
  for m in "${MISSING[@]}"; do echo "          - $m"; done
  exit 2
fi
say "android.jar: $ANDROID_JAR"
say "d8/R8:       $D8_JAR"
say "kotlinc:     $KOTLINC_HOME"

# ApkCheck (android-agent/tools/apkcheck.jar, czysta Java 8+, źródło obok)
apkcheck() {
  "$JAVA_BIN" -Dfile.encoding=UTF-8 -Dstdout.encoding=UTF-8 -Dstderr.encoding=UTF-8 \
    -cp "$HERE/tools/apkcheck.jar" ApkCheck "$@" 2>&1 | grep -v '^Picked up '
  return "${PIPESTATUS[0]}"
}

# ------------------------------------------------------------------ budowanie
set -e
rm -rf "$OUT"
mkdir -p "$OUT/dex" "$OUT/apk"

say "[1/6] kotlinc"
"$KOTLINC_HOME/bin/kotlinc" -jvm-target 1.8 -no-reflect -nowarn \
  -classpath "$ANDROID_JAR" -d "$OUT/classes.jar" \
  "$HERE"/app/src/main/java/com/drapco/agent/*.kt

say "[2/6] R8 (dex + odchudzenie kotlin-stdlib)"
"$JAVA_BIN" -cp "$D8_JAR" com.android.tools.r8.R8 --release --min-api "$MIN_SDK" \
  --lib "$ANDROID_JAR" --pg-conf "$HERE/app/proguard-rules.pro" \
  --output "$OUT/dex" "$OUT/classes.jar" "$KOTLINC_HOME/lib/kotlin-stdlib.jar"

say "[3/6] struktura APK (manifest + zasoby + dex)"
cp "$OUT/dex/classes.dex" "$OUT/apk/"
cp -r "$HERE/app/src/main/res" "$OUT/apk/res"
# manifest Gradle (bez package/uses-sdk) -> pełny manifest dla aapt2
sed -e "s#<manifest xmlns:android=\"http://schemas.android.com/apk/res/android\">#<manifest xmlns:android=\"http://schemas.android.com/apk/res/android\" package=\"$PKG\" android:versionCode=\"$VERSION_CODE\" android:versionName=\"$VERSION_NAME\">\n    <uses-sdk android:minSdkVersion=\"$MIN_SDK\" android:targetSdkVersion=\"$TARGET_SDK\" />#" \
  "$HERE/app/src/main/AndroidManifest.xml" > "$OUT/apk/AndroidManifest.xml"
cat > "$OUT/apk/apktool.yml" <<EOF
version: 2.11.1
apkFileName: Agent.apk
isFrameworkApk: false
usesFramework:
  ids:
  - 1
  tag: null
sdkInfo:
  minSdkVersion: $MIN_SDK
  targetSdkVersion: $TARGET_SDK
packageInfo:
  forcedPackageId: 127
  renameManifestPackage: null
versionInfo:
  versionCode: $VERSION_CODE
  versionName: $VERSION_NAME
sharedLibrary: false
sparseResources: false
compactEntries: false
# resources.arsc MUSI być bez kompresji (Android 11+ / targetSdk 30+: inaczej Failure [-124])
doNotCompress:
- arsc
- resources.arsc
- png
EOF

say "[4/6] apktool b (aapt2)"
"$JAVA_BIN" -jar "$APKTOOL_JAR" b -f -p "$OUT/framework" -o "$OUT/Agent-built.apk" "$OUT/apk"

# Wymuszenie układu wymaganego przez Androida 11+ niezależnie od wersji apktool:
# resources.arsc STORED (bez kompresji) + wyrównanie danych nieskompresowanych do 4 B.
say "[5/6] resources.arsc bez kompresji + wyrównanie (ApkCheck fix)"
apkcheck fix "$OUT/Agent-built.apk" "$OUT/Agent-unsigned.apk" || { say "ApkCheck fix nie powiódł się"; exit 1; }

# zipalign -p -f 4 z Android SDK (jeśli jest) - przed podpisem (podpis v2/v3 obejmuje układ archiwum)
SIGN_IN="$OUT/Agent-unsigned.apk"
ZIPALIGN="${ZIPALIGN:-$(find_sdk_file 'build-tools/*/zipalign' || true)}"
[ -z "$ZIPALIGN" ] && ZIPALIGN="$(find_sdk_file 'build-tools/*/zipalign.exe' || true)"
# (zipalign bez argumentów wypisuje "Usage" i kończy się kodem != 0 - stąd "|| true" przy pipefail)
if [ -n "$ZIPALIGN" ] && { "$ZIPALIGN" 2>&1 || true; } | grep -qi "usage"; then
  say "      zipalign -p -f 4 ($ZIPALIGN)"
  "$ZIPALIGN" -p -f 4 "$OUT/Agent-unsigned.apk" "$OUT/Agent-aligned.apk"
  "$ZIPALIGN" -c -p 4 "$OUT/Agent-aligned.apk" || { say "zipalign -c: wyrównanie niepoprawne"; exit 1; }
  SIGN_IN="$OUT/Agent-aligned.apk"
else
  say "      zipalign z SDK niedostępny - wyrównanie: ApkCheck + wbudowany zipalign uber-apk-signer"
fi

say "[6/6] podpis (v1+v2+v3) + weryfikacja"
mkdir -p "$OUT/sign-in"
cp "$SIGN_IN" "$OUT/sign-in/Agent.apk"
"$JAVA_BIN" -jar "$SIGNER_JAR" -a "$OUT/sign-in/Agent.apk" -o "$OUT/signed" \
  --ks "$HERE/agent.keystore" --ksAlias agent --ksPass android --ksKeyPass android >"$OUT/sign.log" 2>&1 \
  || { cat "$OUT/sign.log"; exit 1; }
grep -q "signature verified" "$OUT/sign.log" || { cat "$OUT/sign.log"; exit 1; }
cp "$OUT"/signed/*.apk "$OUT/Agent.apk"
# końcowa bramka: APK, który odrzuciłby Android 11+, NIE wychodzi z tego skryptu
apkcheck verify "$OUT/Agent.apk" || { rm -f "$OUT/Agent.apk"; say "Wynikowy APK nie spełnia wymagań Androida 11+"; exit 1; }
say "OK -> $OUT/Agent.apk ($(wc -c < "$OUT/Agent.apk" | tr -d ' ') B)"
