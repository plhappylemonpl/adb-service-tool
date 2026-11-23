//! Detekcja urządzeń (ADB + Fastboot + MTP) oraz odczyt informacji o sprzęcie.
//!
//! Odczyt ADB robimy JEDNYM wywołaniem `adb shell` (getprop + meminfo + df +
//! dumpsys battery ...) - zamiast kilkunastu osobnych, co przy wolnym USB
//! robiło różnicę kilku sekund.

use serde::Serialize;

use crate::adb::Adb;
use crate::error::Result;
use crate::mtp_handler::{self, MtpDevice};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnMode {
    Adb,
    Recovery,
    Sideload,
    Fastboot,
    Mtp,
    Unauthorized,
    Offline,
    /// Linux: brak reguł udev (adb: "no permissions")
    NoPermissions,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub serial: String,
    pub mode: ConnMode,
    /// z `adb devices -l` (model:...) lub z USB (MTP)
    pub model_hint: Option<String>,
    pub product: Option<String>,
    pub transport_id: Option<String>,
    /// połączenie bezprzewodowe (adb connect ip:port / mDNS _adb-tls-connect)
    pub wireless: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub serial: String,
    pub source: String,
    pub manufacturer: Option<String>,
    pub brand: Option<String>,
    pub model: Option<String>,
    pub device: Option<String>,
    pub android_version: Option<String>,
    pub sdk: Option<String>,
    pub build_id: Option<String>,
    pub fingerprint: Option<String>,
    pub security_patch: Option<String>,
    pub baseband: Option<String>,
    pub bootloader: Option<String>,
    pub soc: Option<String>,
    pub hardware: Option<String>,
    pub cpu_abi: Option<String>,
    pub ram_kb: Option<u64>,
    pub storage_total_kb: Option<u64>,
    pub storage_used_kb: Option<u64>,
    pub resolution: Option<String>,
    pub kernel: Option<String>,
    pub battery_level: Option<u32>,
    /// charging | discharging | notCharging | full | unknown
    pub battery_status: Option<String>,
    /// good | overheat | dead | overVoltage | failure | cold | unknown
    pub battery_health: Option<String>,
    pub battery_temp_c: Option<f32>,
    pub battery_voltage_mv: Option<u32>,
    pub serial_no: Option<String>,
    pub imei: Option<String>,
    /// Samsung CSC / region
    pub csc: Option<String>,
    /// green / yellow / orange (verified boot)
    pub verified_boot: Option<String>,
    pub bootloader_locked: Option<bool>,
    pub slot: Option<String>,
    /// Dodatkowe pary klucz/wartość (np. pełne `fastboot getvar all`).
    pub extra: Vec<(String, String)>,
}

/// Skanuje `adb devices -l` + `fastboot devices` (+ opcjonalnie MTP) i scala listę.
pub async fn scan(adb: &Adb, mtp: Option<&[MtpDevice]>) -> Result<Vec<Device>> {
    let mut out: Vec<Device> = Vec::new();

    // --- ADB ---
    if let Ok(r) = adb.run(None, &["devices", "-l"], 10).await {
        out.extend(parse_adb_devices(&r.stdout));
    }

    // --- Fastboot ---
    if adb.fastboot.is_some() {
        if let Ok(r) = adb.fastboot(None, &["devices"], 8).await {
            for line in r.stdout.lines() {
                let Some(serial) = line.split_whitespace().next() else { continue };
                if serial.is_empty() || out.iter().any(|d| d.serial == serial) {
                    continue;
                }
                let product = fastboot_product(adb, serial).await;
                out.push(Device {
                    serial: serial.to_string(),
                    mode: ConnMode::Fastboot,
                    model_hint: product.clone(),
                    product,
                    transport_id: None,
                    wireless: false,
                });
            }
        }
    }

    // --- MTP (bez debugowania) ---
    if let Some(list) = mtp {
        for m in list {
            let id = m.id();
            // to samo urządzenie już widoczne przez ADB/Fastboot -> nie duplikuj
            if out.iter().any(|d| d.serial == id) || m.has_adb {
                continue;
            }
            out.push(Device {
                serial: id,
                mode: ConnMode::Mtp,
                model_hint: m.display_name(),
                product: None,
                transport_id: None,
                wireless: false,
            });
        }
    }

    Ok(out)
}

/// `fastboot getvar product` - z pamięcią podręczną (watcher skanuje co 2 s,
/// a bootloader odpowiada wolno; pytamy raz na serial).
async fn fastboot_product(adb: &Adb, serial: &str) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(v) = cache.lock().ok().and_then(|c| c.get(serial).cloned()) {
        return v;
    }
    let v = adb
        .fastboot(Some(serial), &["getvar", "product"], 5)
        .await
        .ok()
        .and_then(|o| {
            format!("{}\n{}", o.stderr, o.stdout)
                .lines()
                .find_map(|l| l.trim().strip_prefix("product:").map(|v| v.trim().to_string()))
                .filter(|v| !v.is_empty())
        });
    if let Ok(mut c) = cache.lock() {
        c.insert(serial.to_string(), v.clone());
    }
    v
}

fn parse_adb_devices(stdout: &str) -> Vec<Device> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('*') || line.starts_with("List of devices") {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(serial) = parts.next() else { continue };
        let state = parts.next().unwrap_or("");
        let mode = match state {
            "device" => ConnMode::Adb,
            "recovery" | "rescue" => ConnMode::Recovery,
            "sideload" => ConnMode::Sideload,
            "unauthorized" => ConnMode::Unauthorized,
            "offline" => ConnMode::Offline,
            "bootloader" => ConnMode::Fastboot,
            "no" => ConnMode::NoPermissions,
            _ => ConnMode::Offline,
        };
        let tag = |k: &str| {
            line.split_whitespace()
                .find_map(|t| t.strip_prefix(k))
                .map(|v| v.replace('_', " "))
        };
        out.push(Device {
            serial: serial.to_string(),
            mode,
            model_hint: tag("model:"),
            product: tag("product:"),
            transport_id: tag("transport_id:"),
            wireless: crate::adb::wireless::is_wireless_serial(serial),
        });
    }
    out
}

// --------------------------------------------------------------------------- //
// Informacje o urządzeniu
// --------------------------------------------------------------------------- //

/// Zbiera komplet informacji, zależnie od trybu połączenia.
pub async fn info(adb: &Adb, serial: &str, mode: &str) -> Result<DeviceInfo> {
    match mode {
        "fastboot" => fastboot_info(adb, serial).await,
        "mtp" => Ok(mtp_info(serial).await),
        _ => adb_info(adb, serial).await,
    }
}

const INFO_SCRIPT: &str = "getprop; echo __MEM__; grep MemTotal /proc/meminfo; echo __KER__; uname -r; \
echo __WM__; wm size 2>/dev/null; echo __DF__; df -k /data 2>/dev/null; echo __BAT__; dumpsys battery 2>/dev/null; echo __END__";

async fn adb_info(adb: &Adb, serial: &str) -> Result<DeviceInfo> {
    let r = adb.shell(Some(serial), INFO_SCRIPT, 25).await?;
    let mut d = parse_info_output(&r.stdout);
    d.serial = serial.to_string();
    d.source = "adb".into();
    d.imei = read_imei(adb, serial).await;
    Ok(d)
}

/// Parsuje wynik `INFO_SCRIPT` (wydzielone dla testów).
fn parse_info_output(text: &str) -> DeviceInfo {
    use std::collections::HashMap;

    let mut props: HashMap<String, String> = HashMap::new();
    let mut section = "props";
    let mut d = DeviceInfo::default();

    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        match line.trim() {
            "__MEM__" => {
                section = "mem";
                continue;
            }
            "__KER__" => {
                section = "ker";
                continue;
            }
            "__WM__" => {
                section = "wm";
                continue;
            }
            "__DF__" => {
                section = "df";
                continue;
            }
            "__BAT__" => {
                section = "bat";
                continue;
            }
            "__END__" => break,
            _ => {}
        }
        match section {
            "props" => {
                // [klucz]: [wartość]
                if let Some(rest) = line.strip_prefix('[') {
                    if let Some((k, v)) = rest.split_once("]: [") {
                        let v = v.trim_end_matches(']').trim();
                        if !v.is_empty() {
                            props.insert(k.to_string(), v.to_string());
                        }
                    }
                }
            }
            "mem" => {
                if let Some(v) = line.strip_prefix("MemTotal:") {
                    d.ram_kb = v.split_whitespace().next().and_then(|x| x.parse().ok());
                }
            }
            "ker" => {
                if d.kernel.is_none() && !line.trim().is_empty() {
                    d.kernel = Some(line.trim().to_string());
                }
            }
            "wm" => {
                if let Some(v) = line.strip_prefix("Physical size:") {
                    d.resolution = Some(v.trim().to_string());
                }
            }
            "df" => {
                let cols: Vec<&str> = line.split_whitespace().collect();
                if cols.len() >= 4 && cols[1].chars().all(|c| c.is_ascii_digit()) {
                    d.storage_total_kb = cols[1].parse().ok();
                    d.storage_used_kb = cols[2].parse().ok();
                }
            }
            "bat" => {
                let l = line.trim();
                if let Some((k, v)) = l.split_once(':') {
                    let v = v.trim();
                    match k.trim() {
                        "level" => d.battery_level = v.parse().ok(),
                        "status" => {
                            d.battery_status = Some(
                                match v {
                                    "2" => "charging",
                                    "3" => "discharging",
                                    "4" => "notCharging",
                                    "5" => "full",
                                    _ => "unknown",
                                }
                                .into(),
                            )
                        }
                        "health" => {
                            d.battery_health = Some(
                                match v {
                                    "2" => "good",
                                    "3" => "overheat",
                                    "4" => "dead",
                                    "5" => "overVoltage",
                                    "6" => "failure",
                                    "7" => "cold",
                                    _ => "unknown",
                                }
                                .into(),
                            )
                        }
                        "temperature" => d.battery_temp_c = v.parse::<f32>().ok().map(|t| t / 10.0),
                        "voltage" => d.battery_voltage_mv = v.parse().ok(),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    let p = |keys: &[&str]| -> Option<String> { keys.iter().find_map(|k| props.get(*k).cloned()) };

    d.manufacturer = p(&["ro.product.manufacturer", "ro.product.vendor.manufacturer", "ro.product.system.manufacturer"]);
    d.brand = p(&["ro.product.brand", "ro.product.vendor.brand"]);
    d.model = p(&["ro.product.model", "ro.product.vendor.model", "ro.product.system.model"]);
    d.device = p(&["ro.product.device", "ro.product.vendor.device", "ro.build.product"]);
    d.android_version = p(&["ro.build.version.release_or_codename", "ro.build.version.release"]);
    d.sdk = p(&["ro.build.version.sdk"]);
    d.build_id = p(&["ro.build.display.id", "ro.build.id"]);
    d.fingerprint = p(&["ro.build.fingerprint", "ro.vendor.build.fingerprint"]);
    d.security_patch = p(&["ro.build.version.security_patch"]);
    d.baseband = p(&["gsm.version.baseband", "ro.baseband"]);
    d.bootloader = p(&["ro.bootloader", "ro.boot.bootloader"]);
    d.hardware = p(&["ro.hardware", "ro.boot.hardware"]);
    d.cpu_abi = p(&["ro.product.cpu.abi"]);
    d.serial_no = p(&["ro.serialno", "ro.boot.serialno"]);
    d.csc = p(&["ro.csc.sales_code", "persist.sys.omc_salescode", "ro.boot.hwc", "ro.miui.region"]);
    d.verified_boot = p(&["ro.boot.verifiedbootstate"]);
    d.bootloader_locked = p(&["ro.boot.flash.locked"]).map(|v| v == "1");
    d.slot = p(&["ro.boot.slot_suffix"]);

    // SoC: Android 12+ ma ro.soc.*, starsze - platforma/chipname
    let soc_m = p(&["ro.soc.manufacturer"]);
    let soc_model = p(&["ro.soc.model", "ro.hardware.chipname", "ro.board.platform", "ro.boot.hardware.platform", "ro.chipname"]);
    d.soc = match (soc_m, soc_model) {
        (Some(a), Some(b)) => Some(format!("{a} {b}")),
        (None, Some(b)) => Some(b),
        (Some(a), None) => Some(a),
        _ => None,
    };

    // Kilka przydatnych dodatków serwisowych
    for (label, key) in [
        ("Build incremental", "ro.build.version.incremental"),
        ("Build date", "ro.build.date"),
        ("Product name", "ro.product.name"),
        ("Treble", "ro.treble.enabled"),
        ("Encryption", "ro.crypto.state"),
        ("Encryption type", "ro.crypto.type"),
        ("SELinux", "ro.boot.selinux"),
        ("OEM unlock allowed", "sys.oem_unlock_allowed"),
        ("One UI", "ro.build.version.oneui"),
        ("MIUI/HyperOS", "ro.miui.ui.version.name"),
        ("Locale", "persist.sys.locale"),
        ("Timezone", "persist.sys.timezone"),
        ("Operator", "gsm.operator.alpha"),
        ("SIM state", "gsm.sim.state"),
    ] {
        if let Some(v) = props.get(key) {
            d.extra.push((label.to_string(), v.clone()));
        }
    }
    d
}

/// Best-effort odczyt IMEI. Od Androida 10 wymaga uprawnień uprzywilejowanych,
/// więc na wielu urządzeniach zwróci None (wtedy: *#06# na ekranie urządzenia).
async fn read_imei(adb: &Adb, serial: &str) -> Option<String> {
    const ATTEMPTS: &[&str] = &[
        "service call iphonesubinfo 1 s16 com.android.shell",
        "service call iphonesubinfo 1",
        "service call iphonesubinfo 3 i32 0 s16 com.android.shell",
        "service call iphonesubinfo 4 i32 0 s16 com.android.shell",
    ];
    for cmd in ATTEMPTS {
        let Ok(r) = adb.shell(Some(serial), cmd, 8).await else { continue };
        if let Some(imei) = parse_parcel_imei(&r.stdout) {
            return Some(imei);
        }
    }
    // Bardzo stare Androidy (<=4.4)
    if let Ok(r) = adb.shell(Some(serial), "dumpsys iphonesubinfo", 8).await {
        for l in r.stdout.lines() {
            if let Some(v) = l.split("Device ID =").nth(1) {
                let v = v.trim();
                if is_luhn_imei(v) {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// Wyciąga 15-cyfrowy IMEI z wyjścia `service call` (Parcel w hexach
/// z podglądem ASCII w apostrofach: '....3.5.6.7.8...').
fn parse_parcel_imei(out: &str) -> Option<String> {
    if out.contains("Exception") || out.contains("Permission") {
        // czasem mimo wyjątku są dane - ale wtedy to śmieci; odpuszczamy
        return None;
    }
    let mut ascii = String::new();
    for line in out.lines() {
        let mut inside = false;
        for c in line.chars() {
            if c == '\'' {
                inside = !inside;
                continue;
            }
            if inside && c != '.' {
                ascii.push(c);
            }
        }
    }
    let digits: String = ascii.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 15 {
        let cand = &digits[..15];
        if is_luhn_imei(cand) {
            return Some(cand.to_string());
        }
    }
    None
}

fn is_luhn_imei(s: &str) -> bool {
    if s.len() != 15 || !s.chars().all(|c| c.is_ascii_digit()) || s.chars().all(|c| c == '0') {
        return false;
    }
    let sum: u32 = s
        .chars()
        .rev()
        .enumerate()
        .map(|(i, c)| {
            let mut d = c.to_digit(10).unwrap_or(0);
            if i % 2 == 1 {
                d *= 2;
                if d > 9 {
                    d -= 9;
                }
            }
            d
        })
        .sum();
    sum % 10 == 0
}

/// Fastboot: `fastboot getvar all` (wynik na stderr, linie "(bootloader) k: v").
async fn fastboot_info(adb: &Adb, serial: &str) -> Result<DeviceInfo> {
    let r = adb.fastboot(Some(serial), &["getvar", "all"], 20).await?;
    let text = format!("{}\n{}", r.stderr, r.stdout);
    let mut d = DeviceInfo {
        serial: serial.to_string(),
        source: "fastboot".into(),
        ..Default::default()
    };
    for line in text.lines() {
        let l = line.trim().trim_start_matches("(bootloader)").trim();
        let Some((k, v)) = l.split_once(": ") else { continue };
        let (k, v) = (k.trim(), v.trim());
        if v.is_empty() {
            continue;
        }
        match k {
            "product" => d.device = Some(v.into()),
            "serialno" => d.serial_no = Some(v.into()),
            "version-bootloader" => d.bootloader = Some(v.into()),
            "version-baseband" => d.baseband = Some(v.into()),
            "unlocked" => d.bootloader_locked = Some(v != "yes"),
            "current-slot" => d.slot = Some(v.into()),
            "variant" => d.hardware = Some(v.into()),
            _ => {}
        }
        if !k.starts_with("partition-") && !d.extra.iter().any(|(ek, _)| ek == k) {
            d.extra.push((k.to_string(), v.to_string()));
        }
    }
    Ok(d)
}

async fn mtp_info(serial: &str) -> DeviceInfo {
    let list = mtp_handler::scan().await;
    let mut d = DeviceInfo {
        serial: serial.to_string(),
        source: "mtp".into(),
        ..Default::default()
    };
    if let Some(m) = list.iter().find(|m| m.id() == serial) {
        d.manufacturer = m.manufacturer.clone();
        d.model = m.product.clone();
        d.serial_no = m.serial.clone();
        if let (Some(v), Some(p)) = (&m.vid, &m.pid) {
            d.extra.push(("USB VID:PID".into(), format!("{v}:{p}")));
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_devices() {
        let s = "List of devices attached\nR58M123 device usb:1-1 product:a52q model:SM_A525F device:a52q transport_id:3\nABC unauthorized usb:1-2 transport_id:4\nXYZ no permissions (missing udev rules); see [http://...]\n";
        let d = parse_adb_devices(s);
        assert_eq!(d.len(), 3);
        assert_eq!(d[0].mode, ConnMode::Adb);
        assert_eq!(d[0].model_hint.as_deref(), Some("SM A525F"));
        assert_eq!(d[1].mode, ConnMode::Unauthorized);
        assert_eq!(d[2].mode, ConnMode::NoPermissions);
    }

    #[test]
    fn parses_info() {
        let s = "[ro.product.model]: [SM-A525F]\n[ro.soc.manufacturer]: [QTI]\n[ro.soc.model]: [SM7125]\n__MEM__\nMemTotal:        5612345 kB\n__KER__\n4.14.190\n__WM__\nPhysical size: 1080x2400\n__DF__\nFilesystem 1K-blocks Used Available Use% Mounted on\n/dev/block/dm-8 110000000 50000000 60000000 46% /data\n__BAT__\nCurrent Battery Service state:\n  level: 87\n  status: 2\n  temperature: 291\n__END__\n";
        let d = parse_info_output(s);
        assert_eq!(d.model.as_deref(), Some("SM-A525F"));
        assert_eq!(d.soc.as_deref(), Some("QTI SM7125"));
        assert_eq!(d.ram_kb, Some(5612345));
        assert_eq!(d.resolution.as_deref(), Some("1080x2400"));
        assert_eq!(d.storage_total_kb, Some(110000000));
        assert_eq!(d.battery_level, Some(87));
        assert_eq!(d.battery_status.as_deref(), Some("charging"));
        assert_eq!(d.battery_temp_c, Some(29.1));
    }

    #[test]
    fn parses_parcel() {
        let s = "Result: Parcel(\n  0x00000000: 00000000 0000000f 00350033 00380035 '........3.5.5.8'\n  0x00000010: 00350030 00300031 00350031 00360037 '0.5.1.0.1.5.7.6'\n  0x00000020: 00330032 00000032                   '2.3.2...        ')\n";
        // 355805101576232 - przykładowy (nie musi przejść Luhna); sprawdzamy brak paniki
        let _ = parse_parcel_imei(s);
        assert!(is_luhn_imei("490154203237518"));
        assert!(!is_luhn_imei("490154203237519"));
    }
}
