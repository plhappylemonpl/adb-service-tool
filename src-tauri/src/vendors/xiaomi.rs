//! Xiaomi: Anti-Rollback (ARB) z trybu Fastboot + porównanie z firmware
//! oraz detekcja portu Qualcomm EDL 9008.
//!
//! ARB: `fastboot getvar anti` zwraca indeks rollback urządzenia. Wgranie
//! firmware z NIŻSZYM `CURRENT_ANTI_VER` (flash_all.bat/.sh) = trwałe
//! zablokowanie (cegła, ratunek tylko przez EDL z autoryzowanym kontem).

use std::path::Path;

use serde::Serialize;

use crate::adb::Adb;
use crate::error::{AdbError, Result};
use crate::hardware::usb::{self, UsbDevice, UsbKind};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArbInfo {
    /// Indeks ARB urządzenia; None = bootloader nie zgłasza (brak ARB lub nie-Xiaomi)
    pub anti: Option<u32>,
    pub supported: bool,
    pub raw: String,
    pub product: Option<String>,
    pub unlocked: Option<bool>,
    pub slot: Option<String>,
}

fn fb_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let l = l.trim().trim_start_matches("(bootloader)").trim();
        l.strip_prefix(&format!("{key}:"))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}

pub fn parse_anti(text: &str) -> Option<u32> {
    fb_value(text, "anti").and_then(|v| v.parse().ok())
}

async fn getvar(adb: &Adb, serial: &str, var: &str) -> Result<String> {
    let o = adb.fastboot(Some(serial), &["getvar", var], 10).await?;
    // fastboot pisze wynik getvar na stderr
    Ok(format!("{}\n{}", o.stderr, o.stdout))
}

pub async fn arb(adb: &Adb, serial: &str) -> Result<ArbInfo> {
    let raw = getvar(adb, serial, "anti").await?;
    let anti = parse_anti(&raw);
    let product = getvar(adb, serial, "product").await.ok().and_then(|t| fb_value(&t, "product"));
    let unlocked = getvar(adb, serial, "unlocked")
        .await
        .ok()
        .and_then(|t| fb_value(&t, "unlocked"))
        .map(|v| v == "yes");
    let slot = getvar(adb, serial, "current-slot")
        .await
        .ok()
        .and_then(|t| fb_value(&t, "current-slot"));
    Ok(ArbInfo {
        supported: anti.is_some(),
        anti,
        raw: raw
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("Finished") && !l.starts_with("Total time"))
            .collect::<Vec<_>>()
            .join("\n"),
        product,
        unlocked,
        slot,
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirmwareArb {
    pub folder: String,
    /// CURRENT_ANTI_VER z flash_all.* - None gdy skrypt nie sprawdza ARB
    pub anti: Option<u32>,
    pub script: Option<String>,
    pub product: Option<String>,
}

/// Wyciąga `CURRENT_ANTI_VER=N` ze skryptów firmware (fastboot ROM).
pub fn parse_script_anti(text: &str) -> Option<u32> {
    text.lines().find_map(|l| {
        let l = l.trim();
        let l = l.strip_prefix("set ").unwrap_or(l);
        l.strip_prefix("CURRENT_ANTI_VER=")
            .and_then(|v| v.trim().trim_matches('"').parse().ok())
    })
}

pub fn firmware_arb(folder: &Path) -> Result<FirmwareArb> {
    if !folder.is_dir() {
        return Err(AdbError::Io(format!("nie jest folderem / not a folder: {}", folder.display())));
    }
    let candidates = ["flash_all.bat", "flash_all.sh", "flash_all_except_storage.bat", "flash_all_except_data_storage.bat", "flash_all_lock.bat"];
    let mut found: Option<(u32, String)> = None;
    let mut product = None;
    let mut first_script = None;
    for c in candidates {
        let Ok(t) = std::fs::read_to_string(folder.join(c)) else { continue };
        first_script.get_or_insert_with(|| c.to_string());
        if product.is_none() {
            // np.: fastboot %* getvar product 2>&1 | findstr /r /c:"^product: *XXX"
            product = t
                .lines()
                .find_map(|l| l.split("product: *").nth(1).map(|r| r.split(['"', ' ', '\\', '$']).next().unwrap_or("").to_string()))
                .filter(|s| !s.is_empty());
        }
        if let Some(a) = parse_script_anti(&t) {
            found = Some((a, c.to_string()));
            break;
        }
    }
    if first_script.is_none() {
        return Err(AdbError::Other(
            "Brak flash_all.bat / flash_all.sh - wskaż rozpakowany Fastboot ROM / not a fastboot ROM".into(),
        ));
    }
    Ok(FirmwareArb {
        folder: folder.to_string_lossy().to_string(),
        anti: found.as_ref().map(|f| f.0),
        script: found.map(|f| f.1).or(first_script),
        product,
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdlStatus {
    pub present: bool,
    pub devices: Vec<UsbDevice>,
    /// Gotowe do Firehose: port widoczny + sterownik (Windows) / dostęp (Linux)
    pub ready: bool,
    pub detail: String,
    /// Inne tryby Qualcomm/MediaTek (Diag 900E, BROM...)
    pub other_modes: Vec<UsbDevice>,
}

pub async fn edl() -> Result<EdlStatus> {
    let all = tokio::task::spawn_blocking(usb::scan).await?;
    let devices: Vec<UsbDevice> = all.iter().filter(|d| d.kind == UsbKind::QualcommEdl).cloned().collect();
    let other_modes: Vec<UsbDevice> = all
        .into_iter()
        .filter(|d| matches!(d.kind, UsbKind::QualcommDiag | UsbKind::MediatekBrom | UsbKind::MediatekPreloader))
        .collect();
    let (ready, detail) = match devices.first() {
        None => (false, "Brak urządzenia 05C6:9008 / no EDL device".to_string()),
        Some(d) if cfg!(windows) => {
            if d.has_driver {
                (true, format!("Sterownik: {} - gotowe do Firehose (QFIL / Mi Flash)", d.drivers.join(", ")))
            } else {
                (false, "Port 9008 bez sterownika - zainstaluj Qualcomm HS-USB QDLoader (zakładka Sterowniki)".to_string())
            }
        }
        Some(d) => {
            let serial_port = d.drivers.iter().any(|x| x == "qcserial");
            match d.accessible {
                Some(false) => (false, "Brak uprawnień USB - zainstaluj reguły udev (zakładka Sterowniki)".to_string()),
                _ if serial_port => (true, "qcserial (/dev/ttyUSB*) - gotowe (QFIL-like / edl przez port szeregowy)".to_string()),
                _ => (true, "Dostęp przez libusb - gotowe dla bkerler/edl (Sahara/Firehose)".to_string()),
            }
        }
    };
    Ok(EdlStatus {
        present: !devices.is_empty(),
        devices,
        ready,
        detail,
        other_modes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anti_parsing() {
        assert_eq!(parse_anti("anti: 3\nFinished. Total time: 0.001s"), Some(3));
        assert_eq!(parse_anti("(bootloader) anti: 4\nOKAY"), Some(4));
        assert_eq!(parse_anti("getvar:anti FAILED (remote: 'GetVar Variable Not found')"), None);
    }

    #[test]
    fn script_anti() {
        assert_eq!(parse_script_anti("@echo off\nset CURRENT_ANTI_VER=3\nfor /f ..."), Some(3));
        assert_eq!(parse_script_anti("#!/bin/sh\nCURRENT_ANTI_VER=5\n"), Some(5));
        assert_eq!(parse_script_anti("fastboot flash boot boot.img"), None);
    }
}
