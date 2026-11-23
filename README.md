🇬🇧 English | 🇵🇱 [Polski](README.pl.md)

# ADB Service Tool

A desktop service tool for Android phones built on ADB, Fastboot and MTP: file backup and restore, contacts/SMS migration, app management, diagnostics and vendor tools (Samsung, Xiaomi, Motorola).
It runs on **Linux** and **Windows** (Tauri v2: Rust backend, SvelteKit frontend) and is made for phone repair shops and technicians.
The UI is available in Polish and English.

## Features

### Devices and ADB over Wi-Fi
- Automatic detection every 2 s: ADB (device / recovery / sideload / unauthorized / offline / missing udev permissions), Fastboot (with `getvar product`), MTP (sysfs on Linux, WPD on Windows).
- USB / Wi-Fi badge per device, **Disconnect** (`adb disconnect`) for wireless connections.
- **Android 11+ pairing**: `adb pair IP:PORT CODE` → connection port (manual, mDNS auto-connect or `adb mdns services` matched by the pairing GUID) → `adb connect`; "already paired – connect only" mode.
- **Android ≤ 10 / device on a cable**: reads the device IP (`ip`, `ifconfig`, `getprop dhcp.*`) → `adb tcpip 5555` → `adb connect` with retries.
- Device info from one `adb shell` call: model, SoC, Android/SDK, build, security patch, baseband, bootloader, kernel, RAM, storage, battery, CSC, verified boot, bootloader lock, IMEI (best effort). Fastboot: full `getvar all`.

### Pull / Push files
- Device file tree, presets (DCIM, WhatsApp, …), SD card, multi-threaded transfers with byte progress, speed and ETA.
- **Verification**: a file only counts as copied when `adb` exits with 0, its output has no known errors, and the file exists on disk with the size reported by the device. A final audit re-checks every file.
- **Atomic writes**: data goes to `<dest>/.adbst-partial/N.part` and is moved into place after verification; long Windows paths use the `\\?\` prefix.
- **Checkpoint**: `<dest>/.adb_pull_progress.jsonl` (append + fsync), so an interrupted pull resumes; files already present with the same size are skipped.
- **Complete scan**: `find` streamed line by line (no `ARG_MAX` failures), hidden files, empty folders, symlinks resolved without loops.
- **NTFS-friendly names** (on by default, always on Windows): strips `: * ? " < > |`, trailing dots/spaces and reserved names.
- **Root mode**: detects `adbd` root, `su -c` (Magisk/KernelSU) and `su 0`. Protected files (`/data/data`, `Android/data`) are streamed with `adb exec-out su -c 'cat …'`.
- Push: creates missing folders, resumes (skips files with identical size), byte progress.

### Contacts and SMS (helper Android app)
- `android-agent/`: a small Kotlin app without AndroidX (minSdk 21, targetSdk 34), UI in **20 languages** (English default, Polish, German, Spanish, French, …).
- **Backup**: the tool installs the agent, the user accepts `READ_CONTACTS` / `READ_SMS` and taps *Make backup*. `contacts.vcf` and `sms.xml` (SMS Backup & Restore format) are pulled and size-checked.
- **Restore**: the backup folder is pushed to `Download/Restore/<session>/`.
  - Contacts are imported through the system Contacts app (`ACTION_VIEW text/x-vcard`), with a direct `WRITE_CONTACTS` import as fallback.
  - For SMS the agent temporarily becomes the default SMS app (`RoleManager` on Android 10+, `ACTION_CHANGE_DEFAULT` on older versions). Messages are inserted via `ContentResolver` and duplicates are skipped.
  - At the end the previous default SMS app is restored, by the user or by the tool over ADB.
- **Fallback install**: if `adb install` is blocked (e.g. Xiaomi "Install via USB" disabled), `Agent.apk` is pushed to `Download/` for manual installation and the tool waits for it.
- Cleanup always runs, also after an error or cancel: files are removed from the device and the agent is uninstalled.

### Apps
- List of user, system, disabled and uninstalled packages, with real icons and labels read from the APK remotely (only the needed parts, via `adb exec-out dd`).
- APK extraction (including splits), install (`install` / `install-multiple`).
- Per-app and bulk actions: uninstall for user 0, uninstall, disable, enable, restore (`install-existing`), force-stop, clear data.

### Vendor tools
- **Samsung Flasher (Heimdall)**:
  - Download Mode detection (04E8:685D) and native PIT read (ODIN/LOKE, read-only).
  - `.tar` / `.tar.md5` indexed and MD5-verified as a stream.
  - PIT taken from the user's file, from the firmware (`CSC_*.tar.md5`) or from the device (`download-pit`, then `flash --resume`).
  - `.lz4` images are matched to PIT entries and decompressed on the fly (`lz4_flex`) before Heimdall.
  - Live progress weighted by partition size.
  - Writing is done by Heimdall.
- **Samsung**: Knox warranty bit, KnoxGuard, binary from the bootloader version, AP/CP/CSC, verified boot, OEM unlock.
- **Xiaomi**: Anti-Rollback index (`fastboot getvar anti`) compared with `CURRENT_ANTI_VER` from `flash_all.bat/.sh`, Qualcomm EDL 9008 detection.
- **Motorola**: flasher driven by `flashfile.xml` / `servicefile.xml`: checks that all files exist, optional MD5, runs `fastboot` step by step, cancels only between steps.
- **Enterprise / Provisioning**: setup wizard skip, secret codes, MDM URL, allow-listed Fastboot OEM commands.

### APK DevTools
- Chain: Pull APK (base + splits, or a local `.apk/.apks/.xapk`) → `apktool d` → edit → `apktool b` → sign (uber-apk-signer, local debug key) → install.
- Manifest edits: force debuggable, new label, clone under a new package name. Resource explorer with image replacement.
- apktool, uber-apk-signer and (if missing) a Temurin JRE are downloaded from pinned GitHub releases with SHA-256 checks.

### Diagnostics, Logcat, mirroring
- **HW diagnostics**: battery (capacity, cycle count, voltage, current, temperature, health from `charge_full/design`), power sources, thermal zones; live charts.
- **Live Logcat**: `adb logcat -v threadtime`, regex and level filter applied in Rust without restarting, presets (errors, crashes, kernel panic, battery, display, modem), 50 000-line buffer for export.
- **Mirror**: scrcpy (detection, automatic download on Windows, quality presets), screenshots (`screencap -p`).
- **Power**: reboot to system / recovery / download / bootloader / fastbootd / sideload / EDL.
- **Advanced**: `adb shell` console, `adb sideload`.

### Updates
- Built-in updater (no `tauri-plugin-updater`, no tokens): reads `latest.json` from the latest GitHub release (`releases/latest/download/latest.json`).
  If the release has no `latest.json`, it only shows the version from the public releases API.
- Background check a few seconds after start (can be turned off in *Advanced*), manual *Check for updates*, skip a version or remind later.
- Every download is checked against the SHA-256 and size from `latest.json` before anything is run or replaced; a mismatch deletes the file and stops the update.
- Supported install types:
  - Windows installer: runs the new `*-setup.exe`.
  - Windows portable: replaces the `.exe` (old file renamed to `.exe.old`, removed on next start).
  - Linux AppImage: atomic replace of `$APPIMAGE`.
  - Linux portable `tar.gz`: atomic replace of the binary.
  - Linux system package: downloads the `.deb` and shows the install command, or runs `pkexec` after confirmation.

### Drivers / udev
- **Windows**: detects driver packages (Google ADB/Fastboot, MTP, Samsung, Motorola, Qualcomm 9008) and USB devices without a driver. Installs with one UAC prompt (`pnputil` / `msiexec`), optional SHA-256 check.
- **Linux**: checks udev rules and `/dev/bus/usb` access. Installs `51-android.rules` via `pkexec` / `sudo` (the password is never stored), adds the user to `adbusers`, reloads udev.

## Requirements

Runtime:
- **Android platform-tools** (`adb`, `fastboot`): looked up in `PATH`, common SDK locations and `./platform-tools/` next to the executable; can be selected manually.
- **Linux**: `webkit2gtk-4.1`, `gtk3`, `librsvg`, `libsoup3`, udev rules for Android devices.
  Arch: `sudo pacman -S --needed android-tools android-udev webkit2gtk-4.1 gtk3 librsvg libsoup3`
- **Windows**: WebView2 (built into Windows 10/11; the installer adds it if missing), USB drivers (Drivers tab).
- Optional:
  - `heimdall` for the Samsung Flasher (WinUSB driver via Zadig on Windows),
  - `scrcpy` for mirroring,
  - **Java 11+** for APK DevTools (a JRE is downloaded if missing).

Build:
- Node.js 18+, Rust (stable, via rustup).
- Agent (optional, a prebuilt `Agent.apk` is included): Java 11+, Android SDK (`platforms/android-29+`, `build-tools`), or Gradle.
- Windows cross-compile from Linux: `nsis`, `lld`, `llvm`, `clang`; `cargo-xwin` is installed automatically.

## Building from source

Linux:

```bash
./build.sh dev      # development mode (hot reload)
./build.sh          # release build (.deb / .rpm / AppImage per Tauri targets)
./build.sh check    # svelte-check + cargo check + Rust tests
./build.sh windows  # cross-compile for Windows x64: NSIS installer + portable .exe in dist/windows/
./build.sh agent    # build only the helper app -> src-tauri/resources/agent/Agent.apk
```

Windows:

```bat
build.bat dev
build.bat
build.bat check
build.bat agent
```

- Before `dev` / `build` / `windows` the scripts ask whether to rebuild `Agent.apk` (non-interactive: `AGENT_BUILD=T|N`).
- The agent is built by `android-agent/build-sdkless.sh` (kotlinc + R8 + apktool + uber-apk-signer, no Android Studio; tools fetched with pinned SHA-256) or by Gradle.
- **Agent signing key**: `android-agent/agent.keystore` (alias `agent`, password `android`) is a **public development key** that only signs the helper app.
  - This is intentional: rebuilt agents keep the same signature, so `adb install -r` works.
  - The agent is installed only for one backup/restore session and uninstalled right after.
  - Do not use this key for anything else. For your own fork, generate a new key (`keytool -genkeypair -keystore agent.keystore -alias agent …`) and update `app/build.gradle.kts` / `build-sdkless.sh`.
- Windows installer: custom NSIS template `src-tauri/windows/installer.nsi` (black and gold theme, Polish + English, no `nsis_tauri_utils` plugin, so it also builds with Linux `makensis`).

Tests:

```bash
cd src-tauri && cargo test && cargo clippy --all-targets
npx svelte-check
```

## Project structure

```
adb-service-tool/
├─ build.sh / build.bat        # build scripts (dev | build | check | windows | agent)
├─ scripts/                    # icon and installer artwork generators (plain Node)
├─ src/                        # SvelteKit GUI (Svelte 5, Tailwind)
│  └─ lib/
│     ├─ api.ts, stores.svelte.ts, i18n.ts, locales/{pl,en}.json
│     ├─ components/           # progress card, log panel, dialogs, toasts
│     ├─ vendors/              # Samsung / Xiaomi / Motorola panels, Samsung Flasher
│     └─ views/                # one view per tab
├─ src-tauri/                  # Rust backend (Tauri v2, tokio)
│  ├─ src/adb/                 # devices, pull/push, scan, root, apps, Wi-Fi, contacts/SMS agent
│  ├─ src/vendors/             # Samsung (Heimdall, PIT), Xiaomi, Motorola, enterprise
│  ├─ src/hardware/            # USB (nusb), LOKE/PIT, drivers/udev, diagnostics, logcat
│  ├─ src/devtools/            # APK DevTools (apktool, signer, manifest)
│  ├─ src/apk/, src/parsers/   # ZIP/AXML/ARSC, Samsung tar, Motorola XML
│  ├─ resources/agent/Agent.apk
│  └─ windows/                 # NSIS template, installer artwork, Polish strings
└─ android-agent/              # helper Android app (Kotlin) — see android-agent/README.md
```

## Disclaimer

Flashing firmware, repartitioning, root access, debloating system apps and modifying APKs can make a device unusable or erase data.
You perform these operations at your own risk. Back up first and read the warnings in the app.
APK DevTools is meant for developers working with their own or permitted apps; it does not bypass licensing or signature checks.
This project is not affiliated with Google, Samsung, Xiaomi or Motorola.
