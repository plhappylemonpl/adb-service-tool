🇬🇧 [English](README.md) | 🇵🇱 Polski

# ADB Service Tool

Desktopowe narzędzie serwisowe do telefonów z Androidem oparte na ADB, Fastboot i MTP: kopia i przywracanie plików, migracja kontaktów/SMS, zarządzanie aplikacjami, diagnostyka i narzędzia producentów (Samsung, Xiaomi, Motorola).
Działa na **Linuksie** i **Windows** (Tauri v2: backend w Rust, frontend w SvelteKit) i jest przeznaczone dla serwisów GSM i techników.
Interfejs jest po polsku i angielsku.

## Funkcje

### Urządzenia i ADB przez Wi-Fi
- Automatyczna detekcja co 2 s: ADB (device / recovery / sideload / unauthorized / offline / brak uprawnień udev), Fastboot (z `getvar product`), MTP (sysfs na Linuksie, WPD na Windows).
- Oznaczenie USB / Wi-Fi przy urządzeniu, **Rozłącz** (`adb disconnect`) dla połączeń bezprzewodowych.
- **Parowanie Android 11+**: `adb pair IP:PORT KOD` → port połączenia (ręcznie, auto-połączenie po mDNS albo `adb mdns services` po GUID z parowania) → `adb connect`; tryb „już sparowane – tylko połącz”.
- **Android ≤ 10 / urządzenie na kablu**: odczyt IP (`ip`, `ifconfig`, `getprop dhcp.*`) → `adb tcpip 5555` → `adb connect` z ponawianiem.
- Informacje z jednego wywołania `adb shell`: model, SoC, Android/SDK, build, poprawki bezpieczeństwa, baseband, bootloader, kernel, RAM, pamięć, bateria, CSC, verified boot, blokada bootloadera, IMEI (best effort). Fastboot: pełne `getvar all`.

### Pobieranie / wysyłanie plików
- Drzewo plików urządzenia, presety (DCIM, WhatsApp, …), karta SD, transfery wielowątkowe z postępem bajtowym, prędkością i ETA.
- **Weryfikacja**: plik jest zaliczony tylko wtedy, gdy `adb` kończy się kodem 0, w wyjściu nie ma znanych błędów, a plik istnieje na dysku z rozmiarem zgłoszonym przez urządzenie. Na końcu audyt sprawdza każdy plik ponownie.
- **Atomowy zapis**: dane trafiają do `<cel>/.adbst-partial/N.part` i są przenoszone na miejsce po weryfikacji; długie ścieżki Windows z prefiksem `\\?\`.
- **Checkpoint**: `<cel>/.adb_pull_progress.jsonl` (append + fsync), więc przerwane pobieranie da się wznowić; pliki już obecne z tym samym rozmiarem są pomijane.
- **Kompletny skan**: `find` czytany strumieniowo (bez błędów `ARG_MAX`), ukryte pliki, puste foldery, dowiązania symboliczne rozwiązywane bez pętli.
- **Nazwy zgodne z NTFS** (domyślnie włączone, na Windows zawsze): usuwanie `: * ? " < > |`, kropek/spacji na końcu i nazw zarezerwowanych.
- **Tryb root**: wykrywa root `adbd`, `su -c` (Magisk/KernelSU) i `su 0`. Pliki chronione (`/data/data`, `Android/data`) są czytane strumieniem `adb exec-out su -c 'cat …'`.
- Wysyłanie: tworzy brakujące foldery, wznawia (pomija pliki o identycznym rozmiarze), postęp bajtowy.

### Kontakty i SMS (aplikacja pomocnicza Android)
- `android-agent/`: mała aplikacja w Kotlinie bez AndroidX (minSdk 21, targetSdk 34), interfejs w **20 językach** (angielski domyślny, polski, niemiecki, hiszpański, francuski, …).
- **Kopia**: narzędzie instaluje agenta, użytkownik akceptuje `READ_CONTACTS` / `READ_SMS` i klika *Wykonaj kopię*. `contacts.vcf` i `sms.xml` (format SMS Backup & Restore) są pobierane i sprawdzane rozmiarem.
- **Przywracanie**: folder kopii trafia do `Download/Restore/<sesja>/`.
  - Kontakty importuje systemowa aplikacja Kontakty (`ACTION_VIEW text/x-vcard`), awaryjnie agent importuje je sam (`WRITE_CONTACTS`).
  - Dla SMS agent na chwilę zostaje domyślną aplikacją SMS (`RoleManager` na Androidzie 10+, `ACTION_CHANGE_DEFAULT` na starszych). Wiadomości są wstawiane przez `ContentResolver`, a duplikaty pomijane.
  - Na koniec przywracana jest poprzednia domyślna aplikacja SMS, przez użytkownika albo przez narzędzie po ADB.
- **Awaryjna instalacja**: gdy `adb install` jest zablokowany (np. Xiaomi bez „Instalacji przez USB”), `Agent.apk` trafia do `Download/` do ręcznej instalacji, a narzędzie na nią czeka.
- Sprzątanie wykonuje się zawsze, także po błędzie lub anulowaniu: pliki są usuwane z urządzenia, a agent odinstalowywany.

### Aplikacje
- Lista pakietów użytkownika, systemowych, zamrożonych i usuniętych, z oryginalnymi ikonami i nazwami czytanymi z APK zdalnie (tylko potrzebne fragmenty, przez `adb exec-out dd`).
- Ekstrakcja APK (także split), instalacja (`install` / `install-multiple`).
- Akcje dla pojedynczej aplikacji i zbiorcze: usuń dla użytkownika 0, odinstaluj, zamroź, odmroź, przywróć (`install-existing`), wymuś zatrzymanie, wyczyść dane.

### Narzędzia producentów
- **Samsung Flasher (Heimdall)**:
  - wykrycie trybu Download (04E8:685D) i natywny odczyt PIT (ODIN/LOKE, tylko odczyt),
  - `.tar` / `.tar.md5` indeksowane i weryfikowane MD5 strumieniowo,
  - PIT z pliku użytkownika, z firmware (`CSC_*.tar.md5`) albo z urządzenia (`download-pit`, potem `flash --resume`),
  - obrazy `.lz4` dopasowane do wpisów PIT i rozpakowywane w locie (`lz4_flex`) przed Heimdallem,
  - postęp na żywo ważony rozmiarem partycji.
  - Zapis wykonuje Heimdall.
- **Samsung**: Knox warranty bit, KnoxGuard, binary z wersji bootloadera, AP/CP/CSC, verified boot, OEM unlock.
- **Xiaomi**: indeks Anti-Rollback (`fastboot getvar anti`) porównany z `CURRENT_ANTI_VER` z `flash_all.bat/.sh`, wykrywanie Qualcomm EDL 9008.
- **Motorola**: flasher sterowany `flashfile.xml` / `servicefile.xml`: sprawdza obecność plików, opcjonalnie MD5, wykonuje `fastboot` krok po kroku, anulowanie tylko między krokami.
- **Enterprise / Provisioning**: pominięcie kreatora, kody serwisowe, URL MDM, komendy Fastboot OEM z listy dozwolonych.

### APK DevTools
- Łańcuch: pobranie APK (base + split albo lokalny `.apk/.apks/.xapk`) → `apktool d` → edycja → `apktool b` → podpis (uber-apk-signer, lokalny klucz debug) → instalacja.
- Edycja manifestu: wymuszenie debuggable, nowa etykieta, klon pod nową nazwą pakietu. Eksplorator zasobów z podmianą grafik.
- apktool, uber-apk-signer i (gdy brak) Temurin JRE są pobierane z przypiętych wydań GitHub z kontrolą SHA-256.

### Diagnostyka, Logcat, mirroring
- **Diagnostyka HW**: bateria (pojemność, liczba cykli, napięcie, prąd, temperatura, kondycja z `charge_full/design`), źródła zasilania, strefy termiczne; wykresy na żywo.
- **Live Logcat**: `adb logcat -v threadtime`, filtr regex i poziomu liczony w Rust bez restartu, presety (błędy, crashe, kernel panic, bateria, ekran, modem), bufor 50 000 linii do eksportu.
- **Zrzut / Mirroring**: scrcpy (wykrywanie, automatyczne pobranie na Windows, presety jakości), zrzuty ekranu (`screencap -p`).
- **Zasilanie**: restart do systemu / recovery / download / bootloadera / fastbootd / sideload / EDL.
- **Zaawansowane**: konsola `adb shell`, `adb sideload`.

### Sterowniki / udev
- **Windows**: wykrywanie pakietów sterowników (Google ADB/Fastboot, MTP, Samsung, Motorola, Qualcomm 9008) i urządzeń USB bez sterownika. Instalacja jednym monitem UAC (`pnputil` / `msiexec`), opcjonalna kontrola SHA-256.
- **Linux**: sprawdzenie reguł udev i dostępu do `/dev/bus/usb`. Instalacja `51-android.rules` przez `pkexec` / `sudo` (hasło nie jest zapisywane), dodanie użytkownika do `adbusers`, przeładowanie udev.

## Wymagania

Do działania:
- **Android platform-tools** (`adb`, `fastboot`): szukane w `PATH`, typowych lokalizacjach SDK i w `./platform-tools/` obok pliku programu; można je wskazać ręcznie.
- **Linux**: `webkit2gtk-4.1`, `gtk3`, `librsvg`, `libsoup3`, reguły udev dla urządzeń Android.
  Arch: `sudo pacman -S --needed android-tools android-udev webkit2gtk-4.1 gtk3 librsvg libsoup3`
- **Windows**: WebView2 (wbudowany w Windows 10/11; instalator doinstaluje go w razie braku), sterowniki USB (zakładka Sterowniki).
- Opcjonalnie:
  - `heimdall` dla Samsung Flashera (na Windows sterownik WinUSB przez Zadig),
  - `scrcpy` do mirroringu,
  - **Java 11+** dla APK DevTools (w razie braku JRE jest pobierane).

Do budowania:
- Node.js 18+, Rust (stable, przez rustup).
- Agent (opcjonalnie, gotowy `Agent.apk` jest w repozytorium): Java 11+, Android SDK (`platforms/android-29+`, `build-tools`) albo Gradle.
- Cross-compile na Windows z Linuksa: `nsis`, `lld`, `llvm`, `clang`; `cargo-xwin` instaluje się sam.

## Budowanie ze źródeł

Linux:

```bash
./build.sh dev      # tryb developerski (hot reload)
./build.sh          # build produkcyjny (.deb / .rpm / AppImage zgodnie z celami Tauri)
./build.sh check    # svelte-check + cargo check + testy Rust
./build.sh windows  # cross-compile na Windows x64: instalator NSIS + przenośny .exe w dist/windows/
./build.sh agent    # tylko aplikacja pomocnicza -> src-tauri/resources/agent/Agent.apk
```

Windows:

```bat
build.bat dev
build.bat
build.bat check
build.bat agent
```

- Przed `dev` / `build` / `windows` skrypty pytają, czy przebudować `Agent.apk` (bez terminala: `AGENT_BUILD=T|N`).
- Agenta buduje `android-agent/build-sdkless.sh` (kotlinc + R8 + apktool + uber-apk-signer, bez Android Studio; narzędzia pobierane z przypiętym SHA-256) albo Gradle.
- **Klucz podpisu agenta**: `android-agent/agent.keystore` (alias `agent`, hasło `android`) to **jawny klucz deweloperski**, który podpisuje wyłącznie aplikację pomocniczą.
  - To celowe: przebudowany agent ma ten sam podpis, więc `adb install -r` działa.
  - Agent jest instalowany tylko na czas jednej sesji kopii/przywracania i zaraz potem odinstalowywany.
  - Nie używaj tego klucza do niczego innego. We własnym forku wygeneruj nowy (`keytool -genkeypair -keystore agent.keystore -alias agent …`) i zaktualizuj `app/build.gradle.kts` / `build-sdkless.sh`.
- Instalator Windows: własny szablon NSIS `src-tauri/windows/installer.nsi` (czarno-złoty motyw, polski + angielski, bez wtyczki `nsis_tauri_utils`, więc buduje się także linuksowym `makensis`).

Testy:

```bash
cd src-tauri && cargo test && cargo clippy --all-targets
npx svelte-check
```

## Struktura projektu

```
adb-service-tool/
├─ build.sh / build.bat        # skrypty budujące (dev | build | check | windows | agent)
├─ scripts/                    # generatory ikony i grafik instalatora (czysty Node)
├─ src/                        # GUI SvelteKit (Svelte 5, Tailwind)
│  └─ lib/
│     ├─ api.ts, stores.svelte.ts, i18n.ts, locales/{pl,en}.json
│     ├─ components/           # karta postępu, panel logu, okna dialogowe, powiadomienia
│     ├─ vendors/              # panele Samsung / Xiaomi / Motorola, Samsung Flasher
│     └─ views/                # jeden widok na zakładkę
├─ src-tauri/                  # backend Rust (Tauri v2, tokio)
│  ├─ src/adb/                 # urządzenia, pull/push, skan, root, aplikacje, Wi-Fi, agent kontaktów/SMS
│  ├─ src/vendors/             # Samsung (Heimdall, PIT), Xiaomi, Motorola, enterprise
│  ├─ src/hardware/            # USB (nusb), LOKE/PIT, sterowniki/udev, diagnostyka, logcat
│  ├─ src/devtools/            # APK DevTools (apktool, signer, manifest)
│  ├─ src/apk/, src/parsers/   # ZIP/AXML/ARSC, tar Samsunga, XML Motoroli
│  ├─ resources/agent/Agent.apk
│  └─ windows/                 # szablon NSIS, grafiki instalatora, polskie teksty
└─ android-agent/              # aplikacja pomocnicza Android (Kotlin) — patrz android-agent/README.md
```

## Zastrzeżenie

Flashowanie firmware, repartycjonowanie, dostęp root, usuwanie aplikacji systemowych i modyfikacja APK mogą unieruchomić urządzenie albo skasować dane.
Te operacje wykonujesz na własną odpowiedzialność. Najpierw zrób kopię i czytaj ostrzeżenia w aplikacji.
APK DevTools jest przeznaczone dla deweloperów pracujących na własnych lub dozwolonych aplikacjach; nie omija licencji ani weryfikacji podpisów.
Projekt nie jest powiązany z Google, Samsungiem, Xiaomi ani Motorolą.
