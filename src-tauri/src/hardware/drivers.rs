//! Menedżer sterowników - "plug & play" dla serwisu.
//!
//! **Windows**
//! - Wykrywanie zainstalowanych pakietów: skan `%SystemRoot%\INF\oem*.inf`
//!   (czytelne bez uprawnień administratora, niezależne od języka systemu).
//! - Wykrywanie urządzeń bez sterownika: enumeracja USB (`nusb`, SetupAPI).
//! - Instalacja: pobranie (HTTPS, SHA-256 w logu, opcjonalna weryfikacja),
//!   potem cicha instalacja z podniesieniem uprawnień (jeden monit UAC):
//!   ZIP/folder/INF -> `pnputil /add-driver ... /subdirs /install`,
//!   MSI -> `msiexec /i ... /qn /norestart`, EXE -> instalator z argumentami.
//! - Źródła: oficjalny Google USB Driver ma stały adres. Samsung / Motorola
//!   zmieniają linki - adres (lub lokalny plik) ustawia się raz w GUI i jest
//!   zapamiętywany w `drivers.json`. Qualcomm nie publikuje sterownika 9008 -
//!   wskazuje się własny instalator/INF (od dostawcy narzędzi serwisowych).
//!
//! **Linux**
//! - Sprawdzenie reguł udev (pokrycie VID kluczowych producentów) i dostępu
//!   do węzłów `/dev/bus/usb` wykrytych urządzeń.
//! - Instalacja `51-android.rules` (M0Rf30/android-udev-rules, walidowane:
//!   bez RUN/PROGRAM; awaryjnie wbudowany zestaw) przez `pkexec` (Polkit),
//!   a gdy brak agenta Polkit - przez `sudo -S` z hasłem podanym w GUI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::adb::events::Emitter;
use crate::adb::manager::{hide_console, run_raw};
use crate::error::{AdbError, Result};
use crate::hardware::usb::{self, UsbDevice, UsbKind, ANDROID_VENDORS};
use crate::utils::{download, CancelToken};

// --------------------------------------------------------------------------- //
// Katalog sterowników (Windows)
// --------------------------------------------------------------------------- //

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum InstallKind {
    /// Wbudowany w Windows (nic do instalowania)
    Builtin,
    /// Paczka INF (ZIP / folder / plik .inf) -> pnputil
    Inf,
    /// Instalator producenta (.exe / .msi)
    Installer,
}

pub struct DriverSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// Wpis wykryty, gdy plik oem*.inf zawiera WSZYSTKIE frazy z którejś grupy.
    markers: &'static [&'static [&'static str]],
    pub default_url: Option<&'static str>,
    pub official_page: Option<&'static str>,
    pub kind: InstallKind,
    /// Domyślne argumenty cichej instalacji dla .exe
    pub default_args: &'static str,
    /// VID-y urządzeń obsługiwanych przez ten sterownik
    vids: &'static [u16],
    kinds: &'static [UsbKind],
}

pub const CATALOG: &[DriverSpec] = &[
    DriverSpec {
        id: "google",
        name: "Google USB Driver (ADB + Fastboot)",
        description: "Oficjalny sterownik WinUSB dla ADB i Fastboot (Pixel, Xiaomi fastboot 18D1:D00D, większość bootloaderów AOSP).",
        markers: &[&["google, inc", "android"], &["android_winusb"]],
        default_url: Some("https://dl.google.com/android/repository/usb_driver_r13-windows.zip"),
        official_page: Some("https://developer.android.com/studio/run/win-usb"),
        kind: InstallKind::Inf,
        default_args: "",
        vids: &[0x18d1],
        kinds: &[UsbKind::Adb, UsbKind::Fastboot],
    },
    DriverSpec {
        id: "mtp",
        name: "MTP (Windows Portable Devices)",
        description: "Wbudowany w Windows (wpdmtp.inf) - przesyłanie plików bez debugowania USB.",
        markers: &[],
        default_url: None,
        official_page: None,
        kind: InstallKind::Builtin,
        default_args: "",
        vids: &[],
        kinds: &[UsbKind::Mtp],
    },
    DriverSpec {
        id: "samsung",
        name: "Samsung USB Driver for Mobile Phones",
        description: "ADB, MTP i tryb Download (Odin) dla urządzeń Samsung.",
        markers: &[&["samsung", "mobile"], &["ssudadb"], &["ss_conn_usb"]],
        default_url: None,
        official_page: Some("https://developer.samsung.com/android-usb-driver"),
        kind: InstallKind::Installer,
        default_args: "",
        vids: &[0x04e8],
        kinds: &[UsbKind::SamsungDownload],
    },
    DriverSpec {
        id: "motorola",
        name: "Motorola Device Manager / USB Drivers",
        description: "ADB i Fastboot (22B8:2E80) dla urządzeń Motorola / Lenovo.",
        markers: &[&["motorola", "android"], &["motoandroid"], &["mot_usb"]],
        default_url: None,
        official_page: Some("https://en-us.support.motorola.com/app/answers/detail/a_id/88481"),
        kind: InstallKind::Installer,
        default_args: "",
        vids: &[0x22b8],
        kinds: &[],
    },
    DriverSpec {
        id: "qualcomm",
        name: "Qualcomm HS-USB QDLoader 9008",
        description: "Port EDL (05C6:9008) dla flashowania Firehose (QFIL, Mi Flash, edl). Qualcomm nie publikuje sterownika - wskaż instalator lub folder z INF od dostawcy narzędzi serwisowych.",
        markers: &[&["qdloader"], &["qcser"], &["qualcomm", "hs-usb"]],
        default_url: None,
        official_page: None,
        kind: InstallKind::Installer,
        default_args: "",
        vids: &[0x05c6],
        kinds: &[UsbKind::QualcommEdl, UsbKind::QualcommDiag],
    },
];

/// Ustawienia użytkownika dla wpisu (zapamiętywane w drivers.json).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct DriverOverride {
    pub url: Option<String>,
    pub args: Option<String>,
    pub sha256: Option<String>,
}

pub type Overrides = HashMap<String, DriverOverride>;

pub fn load_overrides(config_dir: &Path) -> Overrides {
    std::fs::read_to_string(config_dir.join("drivers.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_overrides(config_dir: &Path, o: &Overrides) -> Result<()> {
    std::fs::create_dir_all(config_dir)?;
    let s = serde_json::to_string_pretty(o).map_err(|e| AdbError::Other(e.to_string()))?;
    std::fs::write(config_dir.join("drivers.json"), s)?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriverStatus {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: InstallKind,
    /// Some(true/false) - wykryto / brak; None - nie dotyczy tego systemu
    pub installed: Option<bool>,
    /// Pliki INF, w których wykryto sterownik
    pub found_in: Vec<String>,
    pub url: Option<String>,
    pub args: String,
    pub sha256: Option<String>,
    pub official_page: Option<String>,
    /// Podłączone urządzenia tego producenta / trybu bez sterownika
    pub devices_missing_driver: usize,
}

// --------------------------------------------------------------------------- //
// Raport (oba systemy)
// --------------------------------------------------------------------------- //

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UdevStatus {
    pub rules_files: Vec<String>,
    pub covered: Vec<String>,
    pub missing: Vec<String>,
    pub pkexec: bool,
    pub sudo: bool,
    pub user: String,
    pub in_adbusers: bool,
    pub package_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriversReport {
    pub os: String,
    pub drivers: Vec<DriverStatus>,
    pub devices: Vec<UsbDevice>,
    pub udev: Option<UdevStatus>,
    /// Czytelne problemy do pokazania na górze zakładki
    pub problems: Vec<String>,
}

pub async fn report(config_dir: &Path) -> Result<DriversReport> {
    let devices = tokio::task::spawn_blocking(usb::scan).await?;
    let overrides = load_overrides(config_dir);
    let mut problems = Vec::new();

    let inf_texts = if cfg!(windows) {
        tokio::task::spawn_blocking(read_oem_infs).await?
    } else {
        Vec::new()
    };

    let mut drivers = Vec::new();
    for spec in CATALOG {
        let ov = overrides.get(spec.id).cloned().unwrap_or_default();
        let (installed, found_in) = if !cfg!(windows) {
            (None, Vec::new())
        } else if spec.kind == InstallKind::Builtin {
            (Some(windows_inf_dir().join("wpdmtp.inf").is_file()), vec!["wpdmtp.inf".into()])
        } else {
            let found = match_infs(&inf_texts, spec.markers);
            (Some(!found.is_empty()), found)
        };
        let missing = devices
            .iter()
            .filter(|d| !d.has_driver)
            .filter(|d| {
                let vid = u16::from_str_radix(&d.vid, 16).unwrap_or(0);
                spec.vids.contains(&vid) || spec.kinds.contains(&d.kind)
            })
            .count();
        if cfg!(windows) && missing > 0 {
            problems.push(format!(
                "{}: {missing} urządzeń bez sterownika / devices without driver",
                spec.name
            ));
        }
        drivers.push(DriverStatus {
            id: spec.id.into(),
            name: spec.name.into(),
            description: spec.description.into(),
            kind: spec.kind,
            installed,
            found_in,
            url: ov.url.clone().or(spec.default_url.map(str::to_string)),
            args: ov.args.clone().unwrap_or_else(|| spec.default_args.to_string()),
            sha256: ov.sha256.clone(),
            official_page: spec.official_page.map(str::to_string),
            devices_missing_driver: missing,
        });
    }

    let udev = if cfg!(target_os = "linux") {
        let st = tokio::task::spawn_blocking(udev_status).await?;
        if !st.missing.is_empty() {
            problems.push(format!(
                "Brak reguł udev dla / missing udev rules for: {}",
                st.missing.join(", ")
            ));
        }
        Some(st)
    } else {
        None
    };
    if cfg!(target_os = "linux") {
        let blocked: Vec<String> = devices
            .iter()
            .filter(|d| d.accessible == Some(false))
            .map(|d| format!("{}:{} {}", d.vid, d.pid, d.product.clone().unwrap_or_default()))
            .collect();
        if !blocked.is_empty() {
            problems.push(format!(
                "Brak uprawnień USB (udev) do / no USB permission for: {}",
                blocked.join(", ")
            ));
        }
    }

    Ok(DriversReport {
        os: std::env::consts::OS.into(),
        drivers,
        devices,
        udev,
        problems,
    })
}

// --------------------------------------------------------------------------- //
// Windows: wykrywanie po plikach INF
// --------------------------------------------------------------------------- //

fn windows_inf_dir() -> PathBuf {
    let root = std::env::var("SystemRoot")
        .or_else(|_| std::env::var("windir"))
        .unwrap_or_else(|_| "C:\\Windows".into());
    PathBuf::from(root).join("INF")
}

/// Dekoduje INF (często UTF-16 LE z BOM).
fn decode_inf(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] == 0xfe {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// (nazwa_pliku, treść_małymi_literami) wszystkich oem*.inf
fn read_oem_infs() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(windows_inf_dir()) else {
        return out;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_lowercase();
        if name.starts_with("oem") && name.ends_with(".inf") {
            if let Ok(b) = std::fs::read(e.path()) {
                out.push((name, decode_inf(&b).to_lowercase()));
            }
        }
    }
    out
}

fn match_infs(infs: &[(String, String)], markers: &[&[&str]]) -> Vec<String> {
    infs.iter()
        .filter(|(_, text)| markers.iter().any(|group| group.iter().all(|m| text.contains(m))))
        .map(|(n, _)| n.clone())
        .collect()
}

// --------------------------------------------------------------------------- //
// Windows: instalacja
// --------------------------------------------------------------------------- //

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallRequest {
    pub id: String,
    /// Lokalny plik (.zip/.inf/.exe/.msi) lub folder z .inf; gdy brak - pobieramy z URL
    pub local_path: Option<String>,
}

/// Cytowanie stringu dla PowerShell (apostrofy podwajane).
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Skrypt PowerShell uruchamiający program z podniesieniem uprawnień (UAC)
/// i zwracający jego kod wyjścia.
fn elevated_script(program: &str, args: &[String]) -> String {
    let arglist = if args.is_empty() {
        String::new()
    } else {
        // Start-Process łączy elementy spacją - elementy ze spacjami muszą mieć własne cudzysłowy
        let items: Vec<String> = args
            .iter()
            .map(|a| {
                if a.contains(' ') && !a.starts_with('"') {
                    ps_quote(&format!("\"{a}\""))
                } else {
                    ps_quote(a)
                }
            })
            .collect();
        format!(" -ArgumentList @({})", items.join(","))
    };
    format!(
        "$ErrorActionPreference='Stop'; try {{ $p = Start-Process -FilePath {}{} -Verb RunAs -Wait -PassThru; exit $p.ExitCode }} catch {{ Write-Error $_.Exception.Message; exit 1223 }}",
        ps_quote(program),
        arglist
    )
}

/// Uruchamia program jako administrator (monit UAC). Zwraca kod wyjścia.
async fn run_elevated(program: &str, args: &[String], timeout_s: u64, cancel: &CancelToken) -> Result<i32> {
    let mut cmd = Command::new("powershell");
    hide_console(&mut cmd);
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command"])
        .arg(elevated_script(program, args));
    let out = run_raw(cmd, timeout_s, Some(cancel)).await?;
    if out.status == 1223 {
        return Err(AdbError::Other(
            "Anulowano monit UAC lub brak uprawnień administratora / UAC cancelled".into(),
        ));
    }
    Ok(out.status)
}

/// Znajduje katalog z plikami .inf (do pnputil /subdirs).
fn has_inf(dir: &Path, depth: u32) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else { return false };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_file() && p.extension().map(|x| x.eq_ignore_ascii_case("inf")).unwrap_or(false) {
            return true;
        }
        if p.is_dir() && depth > 0 && has_inf(&p, depth - 1) {
            return true;
        }
    }
    false
}

fn extract_zip(zip_path: &Path, dest: &Path) -> Result<()> {
    let f = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(f).map_err(|e| AdbError::Parse(format!("zip: {e}")))?;
    std::fs::create_dir_all(dest)?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| AdbError::Parse(format!("zip: {e}")))?;
        let Some(rel) = entry.enclosed_name() else { continue };
        let out = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut w = std::fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut w)?;
    }
    Ok(())
}

/// Instalacja sterownika (zadanie w tle z postępem). Tylko Windows.
pub async fn install_windows(
    req: InstallRequest,
    config_dir: PathBuf,
    cache_dir: PathBuf,
    emitter: Emitter,
    cancel: CancelToken,
) -> Result<()> {
    if !cfg!(windows) {
        return Err(AdbError::Other(
            "Instalacja sterowników dotyczy tylko Windows - na Linuksie użyj reguł udev / Windows only".into(),
        ));
    }
    let spec = CATALOG
        .iter()
        .find(|s| s.id == req.id)
        .ok_or_else(|| AdbError::InvalidArg(req.id.clone()))?;
    if spec.kind == InstallKind::Builtin {
        emitter.ok(format!("{} jest wbudowany w Windows / built-in.", spec.name));
        return Ok(());
    }
    let ov = load_overrides(&config_dir).remove(spec.id).unwrap_or_default();

    // 1. źródło: plik lokalny albo pobranie
    let source: PathBuf = match req.local_path.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(p) => PathBuf::from(p),
        None => {
            let url = ov
                .url
                .clone()
                .or(spec.default_url.map(str::to_string))
                .ok_or_else(|| {
                    AdbError::Other(format!(
                        "Brak adresu pobierania dla „{}” - wklej link do instalatora (strona producenta) albo wskaż plik / no download URL",
                        spec.name
                    ))
                })?;
            let fname = url
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("driver.bin")
                .split('?')
                .next()
                .unwrap_or("driver.bin")
                .to_string();
            let dest = cache_dir.join("drivers").join(spec.id).join(&fname);
            emitter.info(format!("Pobieram / downloading {url}"));
            let sha = download::download(&url, &dest, ov.sha256.as_deref(), &emitter, &cancel).await?;
            emitter.info(format!("SHA-256 {fname}: {sha}"));
            dest
        }
    };
    if !source.exists() {
        return Err(AdbError::Io(format!("nie istnieje / not found: {}", source.display())));
    }

    let ext = source
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    // 2. rozpakowanie ZIP
    let source = if ext == "zip" {
        let dest = cache_dir.join("drivers").join(spec.id).join("unpacked");
        let _ = std::fs::remove_dir_all(&dest);
        emitter.info("Rozpakowuję / extracting...");
        let (z, d) = (source.clone(), dest.clone());
        tokio::task::spawn_blocking(move || extract_zip(&z, &d)).await??;
        dest
    } else {
        source
    };

    // 3. instalacja
    let code = if source.is_dir() || ext == "inf" {
        let target = if source.is_dir() {
            if !has_inf(&source, 4) {
                return Err(AdbError::Other(format!(
                    "W {} nie ma plików .inf / no .inf files",
                    source.display()
                )));
            }
            format!("{}\\*.inf", source.display())
        } else {
            source.display().to_string()
        };
        emitter.info(format!("pnputil /add-driver {target} /subdirs /install (UAC)..."));
        run_elevated(
            "pnputil",
            &["/add-driver".into(), target, "/subdirs".into(), "/install".into()],
            600,
            &cancel,
        )
        .await?
    } else if ext == "msi" {
        emitter.info("msiexec /qn (UAC)...");
        run_elevated(
            "msiexec",
            &["/i".into(), source.display().to_string(), "/qn".into(), "/norestart".into()],
            1800,
            &cancel,
        )
        .await?
    } else if ext == "exe" {
        let args: Vec<String> = ov
            .args
            .clone()
            .unwrap_or_else(|| spec.default_args.to_string())
            .split_whitespace()
            .map(str::to_string)
            .collect();
        emitter.info(format!(
            "Instalator producenta (UAC){} ...",
            if args.is_empty() { " - postępuj wg okna instalatora".to_string() } else { format!(" {}", args.join(" ")) }
        ));
        run_elevated(&source.display().to_string(), &args, 1800, &cancel).await?
    } else {
        return Err(AdbError::InvalidArg(format!(
            "Nieobsługiwany typ pliku .{ext} (obsługiwane: .zip .inf .msi .exe lub folder) / unsupported"
        )));
    };

    // pnputil: 0 OK, 259 brak urządzeń do aktualizacji (pakiet dodany), 3010 wymagany restart
    match code {
        0 | 259 => {
            emitter.ok(format!("{} - zainstalowano / installed.", spec.name));
            Ok(())
        }
        3010 | 1641 => {
            emitter.warn(format!("{} - zainstalowano, wymagany restart Windows / reboot required.", spec.name));
            Ok(())
        }
        c => Err(AdbError::Command(format!("instalator zakończył się kodem / exit code {c}"))),
    }
}

// --------------------------------------------------------------------------- //
// Linux: reguły udev
// --------------------------------------------------------------------------- //

const RULES_URL: &str = "https://raw.githubusercontent.com/M0Rf30/android-udev-rules/main/51-android.rules";
const RULES_TARGET: &str = "/etc/udev/rules.d/51-android.rules";
const RULES_DIRS: &[&str] = &["/etc/udev/rules.d", "/usr/lib/udev/rules.d", "/lib/udev/rules.d", "/run/udev/rules.d"];

/// Producenci, których brak w regułach zgłaszamy jako problem.
const KEY_VENDORS: &[u16] = &[0x18d1, 0x04e8, 0x2717, 0x05c6, 0x22b8, 0x0e8d, 0x12d1, 0x2a70, 0x22d9, 0x1004];

fn current_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_default()
}

fn valid_username(u: &str) -> bool {
    !u.is_empty()
        && u.len() <= 64
        && u.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

fn udev_status() -> UdevStatus {
    let mut files = Vec::new();
    let mut text = String::new();
    for d in RULES_DIRS {
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().map(|x| x == "rules").unwrap_or(false) {
                    if let Ok(t) = std::fs::read_to_string(&p) {
                        let low = t.to_lowercase();
                        if ANDROID_VENDORS.iter().any(|(v, _)| low.contains(&format!("\"{v:04x}\""))) {
                            files.push(p.display().to_string());
                            text.push_str(&low);
                        }
                    }
                }
            }
        }
    }
    let mut covered = Vec::new();
    let mut missing = Vec::new();
    for vid in KEY_VENDORS {
        let name = usb::vendor_name(*vid).unwrap_or("?");
        let label = format!("{name} ({vid:04x})");
        if text.contains(&format!("\"{vid:04x}\"")) {
            covered.push(label);
        } else {
            missing.push(label);
        }
    }
    let user = current_user();
    let in_adbusers = std::fs::read_to_string("/etc/group")
        .map(|g| {
            g.lines().any(|l| {
                let parts: Vec<&str> = l.split(':').collect();
                parts.first() == Some(&"adbusers")
                    && parts.get(3).map(|m| m.split(',').any(|x| x == user)).unwrap_or(false)
            })
        })
        .unwrap_or(false);

    UdevStatus {
        rules_files: files,
        covered,
        missing,
        pkexec: which::which("pkexec").is_ok(),
        sudo: which::which("sudo").is_ok(),
        user,
        in_adbusers,
        package_hint: distro_package_hint(),
    }
}

/// Pakiet reguł z repozytorium dystrybucji (alternatywa dla instalacji przez aplikację).
fn distro_package_hint() -> Option<String> {
    let t = std::fs::read_to_string("/etc/os-release").ok()?;
    let get = |k: &str| {
        t.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .map(|v| v.trim_matches('"').to_lowercase())
            .unwrap_or_default()
    };
    let (id, like) = (get("ID"), get("ID_LIKE"));
    let is = |k: &str| id == k || like.split_whitespace().any(|x| x == k);
    Some(
        if is("arch") {
            "sudo pacman -S --needed android-udev"
        } else if is("debian") || is("ubuntu") {
            "sudo apt install android-sdk-platform-tools-common"
        } else if is("fedora") {
            "sudo dnf install android-tools"
        } else if is("opensuse") || id.starts_with("opensuse") {
            "sudo zypper install android-udev-rules"
        } else {
            return None;
        }
        .to_string(),
    )
}

/// Walidacja pobranych reguł: tylko dopasowania/uprawnienia, żadnych poleceń.
fn rules_are_safe(rules: &str) -> bool {
    if !rules.contains("SUBSYSTEM") || rules.len() > 512 * 1024 {
        return false;
    }
    !rules.lines().map(str::trim).filter(|l| !l.starts_with('#')).any(|l| {
        l.contains("RUN") || l.contains("PROGRAM") || l.contains("IMPORT{program") || l.contains("IMPORT{cmdline")
    })
}

/// Wbudowany zestaw reguł (gdy GitHub niedostępny): wszyscy znani producenci
/// + tryby serwisowe (Odin, EDL, BROM). Dostęp dla zalogowanego użytkownika (uaccess).
pub fn builtin_rules() -> String {
    let mut s = String::from(
        "# ADB Service Tool (Drapco) - reguły udev dla urządzeń Android i trybów serwisowych\n\
         # (ADB, Fastboot, MTP, Samsung Download/Odin, Qualcomm EDL 9008, MediaTek BROM)\n\
         ACTION!=\"add|bind|change\", GOTO=\"adbst_end\"\n\
         SUBSYSTEM!=\"usb\", GOTO=\"adbst_end\"\n\
         ENV{DEVTYPE}!=\"usb_device\", GOTO=\"adbst_end\"\n",
    );
    for (vid, name) in ANDROID_VENDORS {
        s.push_str(&format!(
            "# {name}\nATTR{{idVendor}}==\"{vid:04x}\", MODE=\"0660\", GROUP=\"adbusers\", TAG+=\"uaccess\", SYMLINK+=\"android%n\"\n"
        ));
    }
    s.push_str("LABEL=\"adbst_end\"\n");
    s
}

fn root_script(tmp_rules: &str, user: &str) -> String {
    let add_user = if valid_username(user) {
        format!("usermod -aG adbusers '{user}' 2>/dev/null || true\n")
    } else {
        String::new()
    };
    format!(
        "set -e\n\
         install -m 0644 -o root -g root '{tmp_rules}' {RULES_TARGET}\n\
         getent group adbusers >/dev/null 2>&1 || groupadd -r adbusers 2>/dev/null || true\n\
         {add_user}\
         udevadm control --reload-rules\n\
         udevadm trigger --subsystem-match=usb --action=add 2>/dev/null || udevadm trigger\n"
    )
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "result", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum UdevInstallResult {
    Installed { source: String, path: String },
    /// Brak pkexec / agenta Polkit - GUI ma zapytać o hasło (sudo)
    NeedPassword { reason: String },
    WrongPassword,
    Cancelled,
}

/// Instaluje reguły udev. `password = None` -> pkexec; `Some` -> sudo -S.
pub async fn install_udev(password: Option<String>, emitter: &Emitter) -> Result<UdevInstallResult> {
    if !cfg!(target_os = "linux") {
        return Err(AdbError::Other("Reguły udev dotyczą tylko Linuksa / Linux only".into()));
    }

    // 1. reguły: GitHub (walidowane) albo wbudowane
    let (rules, source) = match download::fetch_text(RULES_URL, 512 * 1024).await {
        Ok(t) if rules_are_safe(&t) => (t, "M0Rf30/android-udev-rules (GitHub)".to_string()),
        Ok(_) => {
            emitter.warn("Pobrane reguły nie przeszły walidacji - używam wbudowanych / failed validation, using built-in");
            (builtin_rules(), "wbudowane / built-in".to_string())
        }
        Err(e) => {
            emitter.warn(format!("GitHub niedostępny ({e}) - używam wbudowanych reguł / using built-in"));
            (builtin_rules(), "wbudowane / built-in".to_string())
        }
    };

    let tmp = std::env::temp_dir().join(format!("adbst-51-android-{}.rules", std::process::id()));
    tokio::fs::write(&tmp, &rules).await?;
    let script = root_script(&tmp.display().to_string(), &current_user());

    let result = match password {
        None => {
            if which::which("pkexec").is_err() {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Ok(UdevInstallResult::NeedPassword {
                    reason: "Brak pkexec (Polkit) / pkexec not installed".into(),
                });
            }
            emitter.info("pkexec: potwierdź w oknie uwierzytelniania / authenticate in the Polkit dialog...");
            let mut cmd = Command::new("pkexec");
            cmd.args(["/bin/sh", "-c", &script]);
            let out = run_raw(cmd, 300, None).await;
            match out {
                Ok(o) if o.status == 0 => UdevInstallResult::Installed {
                    source,
                    path: RULES_TARGET.into(),
                },
                Ok(o) if o.status == 126 => UdevInstallResult::Cancelled,
                Ok(o) if o.status == 127 => UdevInstallResult::NeedPassword {
                    reason: format!(
                        "Brak agenta Polkit / no Polkit agent: {}",
                        String::from_utf8_lossy(&o.stderr).trim()
                    ),
                },
                Ok(o) => {
                    let _ = tokio::fs::remove_file(&tmp).await;
                    return Err(AdbError::Command(String::from_utf8_lossy(&o.stderr).trim().to_string()));
                }
                Err(e) => {
                    let _ = tokio::fs::remove_file(&tmp).await;
                    return Err(e);
                }
            }
        }
        Some(pw) => {
            if which::which("sudo").is_err() {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(AdbError::BinaryNotFound("sudo".into()));
            }
            emitter.info("sudo: instaluję reguły / installing rules...");
            sudo_run(pw, &script).await?
        }
    };
    let _ = tokio::fs::remove_file(&tmp).await;

    if let UdevInstallResult::Installed { source, .. } = &result {
        emitter.ok(format!("Reguły udev zainstalowane ({source}) -> {RULES_TARGET}. Odłącz i podłącz urządzenie."));
    }
    Ok(result)
}

/// `sudo -S` z hasłem na stdin. Hasło nigdy nie trafia do logów ani argv,
/// a bufor jest zerowany po użyciu.
async fn sudo_run(password: String, script: &str) -> Result<UdevInstallResult> {
    let mut pw = password.into_bytes();
    pw.push(b'\n');
    let mut child = Command::new("sudo")
        .args(["-S", "-k", "-p", "", "/bin/sh", "-c", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| AdbError::Io(e.to_string()))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&pw).await;
        let _ = stdin.shutdown().await;
    }
    pw.fill(0);
    let out = tokio::time::timeout(std::time::Duration::from_secs(120), child.wait_with_output())
        .await
        .map_err(|_| AdbError::Timeout(120))?
        .map_err(|e| AdbError::Io(e.to_string()))?;
    if out.status.success() {
        return Ok(UdevInstallResult::Installed {
            source: "sudo".into(),
            path: RULES_TARGET.into(),
        });
    }
    let err = String::from_utf8_lossy(&out.stderr).to_lowercase();
    if err.contains("incorrect password") || err.contains("sorry") || err.contains("no password") || err.contains("authentication") {
        return Ok(UdevInstallResult::WrongPassword);
    }
    if err.contains("not in the sudoers") || err.contains("nie występuje w pliku sudoers") {
        return Err(AdbError::Other(
            "Użytkownik nie ma uprawnień sudo / user is not in sudoers".into(),
        ));
    }
    Err(AdbError::Command(String::from_utf8_lossy(&out.stderr).trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inf_matching() {
        let infs = vec![
            ("oem12.inf".to_string(), "[version] provider=%google% ... google, inc ... android adb interface".to_string()),
            ("oem3.inf".to_string(), "qualcomm hs-usb qdloader 9008".to_string()),
        ];
        let g = CATALOG.iter().find(|s| s.id == "google").unwrap();
        assert_eq!(match_infs(&infs, g.markers), vec!["oem12.inf".to_string()]);
        let q = CATALOG.iter().find(|s| s.id == "qualcomm").unwrap();
        assert_eq!(match_infs(&infs, q.markers), vec!["oem3.inf".to_string()]);
        let s = CATALOG.iter().find(|s| s.id == "samsung").unwrap();
        assert!(match_infs(&infs, s.markers).is_empty());
    }

    #[test]
    fn utf16_inf() {
        let mut b = vec![0xff, 0xfe];
        for u in "Samsung".encode_utf16() {
            b.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(decode_inf(&b), "Samsung");
    }

    #[test]
    fn elevated_quoting() {
        let s = elevated_script("pnputil", &["/add-driver".into(), "C:\\Users\\Jan Kowalski\\drv\\*.inf".into()]);
        assert!(s.contains("-FilePath 'pnputil'"));
        assert!(s.contains("'\"C:\\Users\\Jan Kowalski\\drv\\*.inf\"'"));
        assert!(s.contains("-Verb RunAs -Wait -PassThru"));
        let s2 = elevated_script("C:\\x\\it's.exe", &[]);
        assert!(s2.contains("'C:\\x\\it''s.exe'"));
    }

    #[test]
    fn rules_validation() {
        assert!(rules_are_safe(&builtin_rules()));
        assert!(!rules_are_safe("SUBSYSTEM==\"usb\", RUN+=\"/bin/rm -rf /\""));
        assert!(rules_are_safe("# RUN in comment is fine\nSUBSYSTEM==\"usb\", MODE=\"0660\""));
    }

    #[test]
    fn root_script_rejects_weird_user() {
        assert!(!root_script("/tmp/x", "a'; rm -rf /; '").contains("usermod"));
        assert!(root_script("/tmp/x", "user").contains("usermod -aG adbusers 'user'"));
    }
}
