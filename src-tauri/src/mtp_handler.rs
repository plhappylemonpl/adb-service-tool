//! Detekcja urządzeń MTP (urządzenie podłączone w trybie "Przesyłanie plików",
//! bez debugowania USB) - bez zewnętrznych bibliotek.
//!
//!  * Linux: sysfs (`/sys/bus/usb/devices/*`) - interfejs klasy 06/01 (PTP/MTP)
//!    lub interfejs z nazwą "MTP". Z tych samych danych odczytujemy producenta,
//!    model i numer seryjny USB (zwykle równy serialowi ADB).
//!  * Windows: PowerShell `Get-PnpDevice -Class WPD` (Windows Portable Devices).
//!
//! Wynik służy do pokazania urządzenia na liście i podstawowych informacji
//! (producent / model / SN / VID:PID) oraz podpowiedzi "włącz debugowanie USB".

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MtpDevice {
    pub serial: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub vid: Option<String>,
    pub pid: Option<String>,
    /// Czy urządzenie wystawia też interfejs ADB (debugowanie włączone).
    pub has_adb: bool,
}

impl MtpDevice {
    /// Identyfikator na liście urządzeń (SN, a gdy brak - VID:PID).
    pub fn id(&self) -> String {
        match &self.serial {
            Some(s) if !s.is_empty() => s.clone(),
            _ => format!(
                "mtp:{}:{}",
                self.vid.as_deref().unwrap_or("????"),
                self.pid.as_deref().unwrap_or("????")
            ),
        }
    }

    pub fn display_name(&self) -> Option<String> {
        let s = [self.manufacturer.as_deref(), self.product.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        (!s.trim().is_empty()).then_some(s)
    }
}

/// Skanuje urządzenia MTP dla bieżącej platformy.
pub async fn scan() -> Vec<MtpDevice> {
    #[cfg(target_os = "linux")]
    {
        tokio::task::spawn_blocking(linux::scan).await.unwrap_or_default()
    }
    #[cfg(windows)]
    {
        windows::scan().await
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::MtpDevice;
    use std::fs;
    use std::path::Path;

    fn read(p: &Path) -> Option<String> {
        fs::read_to_string(p)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn scan() -> Vec<MtpDevice> {
        let mut out = Vec::new();
        let Ok(rd) = fs::read_dir("/sys/bus/usb/devices") else {
            return out;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            // pomijamy interfejsy (1-1:1.0) i huby root (usb1)
            if name.contains(':') || name.starts_with("usb") {
                continue;
            }
            let dev = e.path();
            let Some(vid) = read(&dev.join("idVendor")) else { continue };
            let pid = read(&dev.join("idProduct"));

            let mut mtp = false;
            let mut adb = false;
            let prefix = format!("{name}:");
            if let Ok(subs) = fs::read_dir(&dev) {
                for s in subs.flatten() {
                    let sn = s.file_name().to_string_lossy().to_string();
                    if !sn.starts_with(&prefix) {
                        continue;
                    }
                    let ip = s.path();
                    let cls = read(&ip.join("bInterfaceClass")).unwrap_or_default();
                    let sub = read(&ip.join("bInterfaceSubClass")).unwrap_or_default();
                    let proto = read(&ip.join("bInterfaceProtocol")).unwrap_or_default();
                    let iface = read(&ip.join("interface")).unwrap_or_default();
                    if (cls == "06" && sub == "01") || iface.to_uppercase().contains("MTP") {
                        mtp = true;
                    }
                    if cls == "ff" && sub == "42" && proto == "01" {
                        adb = true;
                    }
                }
            }
            if mtp {
                out.push(MtpDevice {
                    serial: read(&dev.join("serial")),
                    manufacturer: read(&dev.join("manufacturer")),
                    product: read(&dev.join("product")),
                    vid: Some(vid),
                    pid,
                    has_adb: adb,
                });
            }
        }
        out
    }
}

#[cfg(windows)]
mod windows {
    use super::MtpDevice;
    use crate::adb::manager::{hide_console, run_raw};
    use tokio::process::Command;

    pub async fn scan() -> Vec<MtpDevice> {
        let script = "Get-PnpDevice -PresentOnly -Class WPD -ErrorAction SilentlyContinue | \
                      Select-Object FriendlyName,InstanceId,Manufacturer | ConvertTo-Json -Compress";
        let mut cmd = Command::new("powershell");
        hide_console(&mut cmd);
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", script]);
        let Ok(out) = run_raw(cmd, 15, None).await else {
            return vec![];
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim()) else {
            return vec![];
        };
        let list = match v {
            serde_json::Value::Array(a) => a,
            o @ serde_json::Value::Object(_) => vec![o],
            _ => vec![],
        };
        let mut res: Vec<MtpDevice> = Vec::new();
        for d in list {
            let inst = d.get("InstanceId").and_then(|x| x.as_str()).unwrap_or("").to_uppercase();
            if !inst.contains("VID_") {
                continue;
            }
            let field = |key: &str| -> Option<String> {
                let i = inst.find(key)? + key.len();
                Some(inst[i..].chars().take(4).collect())
            };
            // SN: segment po VID/PID (USB\VID_x&PID_y\SERIAL lub ...#SERIAL#{guid})
            let serial = inst
                .split(['\\', '#'])
                .filter(|s| !s.is_empty() && !s.contains('&') && !s.starts_with('{') && !s.contains("VID_"))
                .find(|s| s.len() >= 6 && s.chars().all(|c| c.is_ascii_alphanumeric()))
                .map(str::to_string);
            let dev = MtpDevice {
                serial,
                manufacturer: d.get("Manufacturer").and_then(|x| x.as_str()).map(str::to_string),
                product: d.get("FriendlyName").and_then(|x| x.as_str()).map(str::to_string),
                vid: field("VID_"),
                pid: field("PID_"),
                has_adb: false,
            };
            if !res.iter().any(|r| r.id() == dev.id()) {
                res.push(dev);
            }
        }
        res
    }
}
