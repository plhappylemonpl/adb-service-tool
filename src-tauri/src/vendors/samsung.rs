//! Samsung: Knox / status bootloadera (ADB getprop) + detekcja trybu Download.
//!
//! Tryb Download (Odin) wykrywamy po USB (`nusb`, bez libusb): VID 04E8,
//! PID 685D. To urządzenie CDC-ACM, przez które Odin/Heimdall rozmawiają
//! protokołem LOKE ("ODIN" -> "LOKE"). Celowo NIE wysyłamy handshake'u -
//! otwarcie portu zajęłoby go i Odin/Heimdall nie mógłby się połączyć.

use std::collections::HashMap;

use serde::Serialize;

use crate::adb::Adb;
use crate::error::Result;
use crate::hardware::usb::{self, UsbDevice, UsbKind};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusRow {
    pub key: String,
    pub label: String,
    pub value: String,
    /// ok | warn | bad | info
    pub level: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SamsungStatus {
    pub is_samsung: bool,
    pub model: Option<String>,
    /// "0x0" (nietknięty) / "0x1" (przekroczony) / None (niedostępne)
    pub knox: Option<String>,
    pub knox_guard: Option<String>,
    pub binary: Option<String>,
    pub rows: Vec<StatusRow>,
}

/// Rozbiór wersji firmware Samsunga, np. `A525FXXU8GXE1`:
/// A525F = model, XX = region, U/S = typ (U = zwykła, S = łatka bezpieczeństwa),
/// 8 = binary (rewizja bootloadera, "bit"), G = wersja główna OS, X = rok (2024),
/// E = miesiąc (maj), 1 = build.
pub fn parse_fw_version(v: &str) -> Option<(char, String, u32, u32)> {
    let c: Vec<char> = v.trim().chars().collect();
    if c.len() < 9 || !c.iter().all(|x| x.is_ascii_alphanumeric()) {
        return None;
    }
    let n = c.len();
    let kind = c[n - 6];
    let bin = c[n - 5];
    let year = c[n - 3];
    let month = c[n - 2];
    if !matches!(kind, 'U' | 'S') || !year.is_ascii_uppercase() || !month.is_ascii_uppercase() {
        return None;
    }
    let bin_num = if bin.is_ascii_digit() {
        bin as u32 - '0' as u32
    } else if bin.is_ascii_uppercase() {
        10 + (bin as u32 - 'A' as u32)
    } else {
        return None;
    };
    let y = 2001 + (year as u32 - 'A' as u32);
    let m = 1 + (month as u32 - 'A' as u32);
    Some((kind, bin.to_string(), bin_num, y * 100 + m))
}

fn debug_level(v: &str) -> String {
    match v.to_lowercase().as_str() {
        "0x4f4c" => "LOW (0x4f4c)".into(),
        "0x494d" => "MID (0x494d)".into(),
        "0x4948" => "HIGH (0x4948)".into(),
        o => o.to_string(),
    }
}

pub async fn status(adb: &Adb, serial: &str) -> Result<SamsungStatus> {
    let out = adb.shell(Some(serial), "getprop", 15).await?;
    let mut p: HashMap<String, String> = HashMap::new();
    for line in out.stdout.lines() {
        if let Some(rest) = line.trim().strip_prefix('[') {
            if let Some((k, v)) = rest.split_once("]: [") {
                let v = v.trim_end_matches(']').trim();
                if !v.is_empty() {
                    p.insert(k.to_string(), v.to_string());
                }
            }
        }
    }
    let get = |keys: &[&str]| keys.iter().find_map(|k| p.get(*k).cloned());
    let manufacturer = get(&["ro.product.manufacturer", "ro.product.vendor.manufacturer"]).unwrap_or_default();
    let is_samsung = manufacturer.to_lowercase().contains("samsung");

    let mut rows: Vec<StatusRow> = Vec::new();
    let mut push = |key: &str, label: &str, value: Option<String>, level: &str| {
        if let Some(v) = value {
            rows.push(StatusRow {
                key: key.into(),
                label: label.into(),
                value: v,
                level: level.into(),
            });
        }
    };

    // --- Knox / warranty ---
    let knox_raw = get(&["ro.boot.warranty_bit", "ro.warranty_bit"]);
    let knox = knox_raw.as_deref().map(|v| match v {
        "0" => "0x0".to_string(),
        "1" => "0x1".to_string(),
        o => o.to_string(),
    });
    let kg = get(&["ro.boot.kg", "ro.boot.kg.bit", "knox.kg.state"]);

    let bootloader = get(&["ro.boot.bootloader", "ro.bootloader"]);
    let binary = bootloader.as_deref().and_then(parse_fw_version);

    push("model", "Model", get(&["ro.product.model"]), "info");
    push("device", "Device / codename", get(&["ro.product.device"]), "info");
    push("ap", "AP (PDA)", get(&["ro.build.PDA", "ro.build.display.id"]), "info");
    push("bl", "BL (bootloader)", bootloader.clone(), "info");
    push("cp", "CP (baseband)", get(&["gsm.version.baseband", "ril.sw_ver"]), "info");
    push("csc", "CSC (sales code)", get(&["ro.csc.sales_code", "ril.sales_code", "persist.sys.omc_salescode"]), "info");
    push("cscver", "CSC version", get(&["ril.official_cscver", "ro.omc.build.version"]), "info");
    push("carrier", "Carrier ID", get(&["ro.boot.carrierid", "persist.sys.omc_etcpath"]), "info");
    if let Some((kind, ch, num, ym)) = &binary {
        push(
            "binary",
            "Binary (rewizja BL / bit)",
            Some(format!("{ch} ({num}) • typ {kind} • {}-{:02}", ym / 100, ym % 100)),
            "info",
        );
    }
    push(
        "knox",
        "Knox warranty bit",
        knox.clone().map(|k| {
            if k == "0x0" {
                "0x0 - nietknięty / intact".into()
            } else if k == "0x1" {
                "0x1 - PRZEKROCZONY / TRIPPED".into()
            } else {
                k
            }
        }),
        match knox.as_deref() {
            Some("0x0") => "ok",
            Some("0x1") => "bad",
            _ => "info",
        },
    );
    push(
        "kg",
        "KnoxGuard (KG)",
        kg.clone(),
        match kg.as_deref().map(str::to_lowercase).as_deref() {
            Some("locked") | Some("active") | Some("prenormal") => "bad",
            Some("checking") => "warn",
            Some("completed") | Some("00") | Some("0") => "ok",
            _ => "info",
        },
    );
    let locked = get(&["ro.boot.flash.locked"]);
    push(
        "bl_lock",
        "Bootloader zablokowany / locked",
        locked.as_deref().map(|v| if v == "1" { "TAK / YES".to_string() } else { "NIE / NO (odblokowany)".to_string() }),
        if locked.as_deref() == Some("1") { "ok" } else { "warn" },
    );
    let vb = get(&["ro.boot.verifiedbootstate"]);
    push(
        "vb",
        "Verified Boot",
        vb.clone(),
        match vb.as_deref() {
            Some("green") => "ok",
            Some("yellow") => "warn",
            Some("orange") | Some("red") => "bad",
            _ => "info",
        },
    );
    push("verity", "dm-verity", get(&["ro.boot.veritymode"]), "info");
    let oem = get(&["sys.oem_unlock_allowed"]);
    push(
        "oem_unlock",
        "OEM unlock (Opcje programisty)",
        oem.as_deref().map(|v| if v == "1" { "włączony / allowed".into() } else { "wyłączony / disabled".into() }),
        "info",
    );
    push("oem_supported", "OEM unlock supported", get(&["ro.oem_unlock_supported"]), "info");
    push(
        "frp",
        "FRP (partycja)",
        get(&["ro.frp.pst"]).map(|v| format!("{v} (stan FRP sprawdź w Ustawieniach / Odin)")),
        "info",
    );
    push("debug", "Debug level", get(&["ro.boot.debug_level"]).map(|v| debug_level(&v)), "info");
    push("rp", "Rollback protection (RP SWREV)", get(&["ro.boot.rp", "ro.boot.rpswrev"]), "info");
    push("knoxver", "Knox version", get(&["net.knoxscep.version", "ro.config.knox", "ro.knox.enhance.zygote.aslr"]), "info");
    push("selinux", "SELinux", get(&["ro.boot.selinux"]), "info");
    push("serial", "Serial (ril)", get(&["ril.serialnumber", "ro.serialno"]), "info");
    push("hw_rev", "HW revision", get(&["ro.boot.hw_rev", "ril.hw_ver", "ro.revision"]), "info");
    push("android", "Android / One UI", get(&["ro.build.version.release"]).map(|a| {
        match p.get("ro.build.version.oneui") {
            Some(o) => format!("{a} / One UI {o}"),
            None => a,
        }
    }), "info");
    push("patch", "Security patch", get(&["ro.build.version.security_patch"]), "info");

    Ok(SamsungStatus {
        is_samsung,
        model: get(&["ro.product.model"]),
        knox,
        knox_guard: kg,
        binary: binary.map(|(_, c, n, _)| format!("{c} ({n})")),
        rows,
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadModeStatus {
    pub present: bool,
    pub devices: Vec<UsbDevice>,
    /// Windows: czy port ma sterownik (Samsung USB Driver). Linux: dostęp do węzła USB.
    pub driver_ok: Option<bool>,
}

/// Skan USB pod kątem Samsung Download Mode (04E8:685D).
pub async fn download_mode() -> Result<DownloadModeStatus> {
    let devs: Vec<UsbDevice> = tokio::task::spawn_blocking(usb::scan)
        .await?
        .into_iter()
        .filter(|d| d.kind == UsbKind::SamsungDownload)
        .collect();
    let driver_ok = devs.first().map(|d| {
        if cfg!(windows) {
            d.has_driver
        } else {
            d.accessible.unwrap_or(true)
        }
    });
    Ok(DownloadModeStatus {
        present: !devs.is_empty(),
        devices: devs,
        driver_ok,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fw_version() {
        let (kind, ch, num, ym) = parse_fw_version("A525FXXU8GXE1").unwrap();
        assert_eq!(kind, 'U');
        assert_eq!(ch, "8");
        assert_eq!(num, 8);
        assert_eq!(ym, 2024 * 100 + 5);
        let (_, ch2, num2, _) = parse_fw_version("G998BXXSBGXA1").unwrap();
        assert_eq!((ch2.as_str(), num2), ("B", 11));
        assert!(parse_fw_version("XYZ").is_none());
    }
}
