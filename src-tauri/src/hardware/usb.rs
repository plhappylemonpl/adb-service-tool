//! Enumeracja USB w czystym Rust (`nusb` - bez libusb/WinUSB), wspólna dla
//! menedżera sterowników, trybu Download (Samsung), EDL 9008 (Qualcomm),
//! BROM/Preloader (MediaTek).
//!
//! - Linux: sysfs -> sterownik każdego interfejsu + test dostępu do
//!   `/dev/bus/usb/BBB/DDD` (czy reguły udev dają uprawnienia).
//! - Windows: SetupAPI -> usługa sterownika węzła urządzenia
//!   (brak = urządzenie bez sterownika, "żółty wykrzyknik").

use std::sync::atomic::{AtomicUsize, Ordering};

use serde::Serialize;

/// Licznik aktywnych "wyłączności USB" (np. flashowanie Heimdallem). Gdy > 0,
/// skanowanie w tle nie otwiera węzłów /dev/bus/usb (test dostępu), a watcher
/// urządzeń (adb/fastboot/MTP) i sonda Download Mode wstrzymują się - żadne
/// otwarcie 04E8:685D przez aplikację nie wejdzie w sesję Odin/LOKE Heimdalla.
static POLL_PAUSE: AtomicUsize = AtomicUsize::new(0);

/// RAII: wstrzymuje sondowanie USB do końca życia obiektu (także przy błędzie / panice).
pub struct PollPause(());

impl PollPause {
    pub fn acquire() -> PollPause {
        POLL_PAUSE.fetch_add(1, Ordering::SeqCst);
        PollPause(())
    }
}

impl Drop for PollPause {
    fn drop(&mut self) {
        POLL_PAUSE.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Czy sondowanie USB jest wstrzymane (trwa operacja na wyłączność).
pub fn poll_paused() -> bool {
    POLL_PAUSE.load(Ordering::SeqCst) > 0
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum UsbKind {
    Adb,
    Fastboot,
    Mtp,
    /// Samsung Download Mode (Odin / protokół LOKE)
    SamsungDownload,
    /// Qualcomm HS-USB QDLoader 9008 (EDL / Sahara / Firehose)
    QualcommEdl,
    /// Qualcomm Diagnostics (900E / 901D ...)
    QualcommDiag,
    /// MediaTek BootROM (0E8D:0003)
    MediatekBrom,
    /// MediaTek Preloader (0E8D:2000)
    MediatekPreloader,
    /// Inne urządzenie znanego producenta Androida
    Android,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsbDevice {
    pub vid: String,
    pub pid: String,
    pub vendor_name: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
    pub kind: UsbKind,
    /// Sterownik(i) przypięte do urządzenia/interfejsów (np. "WinUSB", "qcserial")
    pub drivers: Vec<String>,
    /// Windows: czy węzeł ma sterownik. Linux: czy jakiś interfejs ma sterownik
    /// (dla ADB/Fastboot/EDL to normalnie false - obsługa przez libusb/adb).
    pub has_driver: bool,
    /// Linux: czy bieżący użytkownik może otworzyć urządzenie (reguły udev).
    pub accessible: Option<bool>,
    /// Interfejsy jako "klasa/podklasa/protokół" (hex)
    pub interfaces: Vec<String>,
}

/// Znani producenci Androida (VID -> nazwa). Używane też do reguł udev.
pub const ANDROID_VENDORS: &[(u16, &str)] = &[
    (0x0502, "Acer"),
    (0x0b05, "ASUS"),
    (0x413c, "Dell"),
    (0x0489, "Foxconn"),
    (0x04c5, "Fujitsu"),
    (0x091e, "Garmin-Asus"),
    (0x18d1, "Google"),
    (0x109b, "Hisense"),
    (0x0bb4, "HTC"),
    (0x12d1, "Huawei"),
    (0x8087, "Intel"),
    (0x24e3, "K-Touch"),
    (0x2116, "KT Tech"),
    (0x0482, "Kyocera"),
    (0x17ef, "Lenovo"),
    (0x1004, "LG"),
    (0x0e8d, "MediaTek"),
    (0x22b8, "Motorola"),
    (0x0409, "NEC"),
    (0x2080, "Nook"),
    (0x0955, "NVIDIA"),
    (0x2257, "OTGV"),
    (0x2a70, "OnePlus"),
    (0x22d9, "OPPO"),
    (0x10a9, "Pantech"),
    (0x1d4d, "Pegatron"),
    (0x0471, "Philips"),
    (0x04da, "Panasonic"),
    (0x05c6, "Qualcomm"),
    (0x1f53, "SK Telesys"),
    (0x04e8, "Samsung"),
    (0x04dd, "Sharp"),
    (0x054c, "Sony"),
    (0x0fce, "Sony Ericsson"),
    (0x2340, "Teleepoch"),
    (0x0930, "Toshiba"),
    (0x2d95, "vivo"),
    (0x2717, "Xiaomi"),
    (0x19d2, "ZTE"),
    (0x1ebf, "Coolpad"),
    (0x2970, "Wileyfox"),
    (0x1bbb, "TCL/Alcatel"),
    (0x29a9, "Realme"),
    (0x2ae5, "Fairphone"),
    (0x1949, "Amazon"),
    (0x0bda, "Realtek"),
    (0x1782, "Spreadtrum/Unisoc"),
    (0x2207, "Rockchip"),
    (0x1f3a, "Allwinner"),
    (0x3351, "Nothing"),
];

pub fn vendor_name(vid: u16) -> Option<&'static str> {
    ANDROID_VENDORS.iter().find(|(v, _)| *v == vid).map(|(_, n)| *n)
}

/// Rozpoznanie trybu po VID:PID i interfejsach.
pub fn classify(vid: u16, pid: u16, ifaces: &[(u8, u8, u8, Option<String>)]) -> Option<UsbKind> {
    match (vid, pid) {
        (0x04e8, 0x685d) | (0x04e8, 0x6601) | (0x04e8, 0x68c3) => return Some(UsbKind::SamsungDownload),
        (0x05c6, 0x9008) => return Some(UsbKind::QualcommEdl),
        (0x05c6, 0x900e) | (0x05c6, 0x901d) | (0x05c6, 0x9091) => return Some(UsbKind::QualcommDiag),
        (0x0e8d, 0x0003) => return Some(UsbKind::MediatekBrom),
        (0x0e8d, 0x2000) => return Some(UsbKind::MediatekPreloader),
        _ => {}
    }
    let has = |c: u8, s: u8, p: u8| ifaces.iter().any(|i| i.0 == c && i.1 == s && i.2 == p);
    if has(0xff, 0x42, 0x03) {
        return Some(UsbKind::Fastboot);
    }
    if has(0xff, 0x42, 0x01) {
        return Some(UsbKind::Adb);
    }
    if has(0x06, 0x01, 0x01)
        || ifaces
            .iter()
            .any(|i| i.3.as_deref().map(|s| s.to_uppercase().contains("MTP")).unwrap_or(false))
    {
        return Some(UsbKind::Mtp);
    }
    // bootloadery fastboot bez deskryptorów interfejsu (Windows) - znane PID-y
    if matches!((vid, pid), (0x18d1, 0x4ee0) | (0x18d1, 0xd00d) | (0x22b8, 0x2e80) | (0x22b8, 0x2e81)) {
        return Some(UsbKind::Fastboot);
    }
    vendor_name(vid).map(|_| UsbKind::Android)
}

/// Skan wszystkich urządzeń USB istotnych dla serwisu (blokujące - wołać w spawn_blocking).
pub fn scan() -> Vec<UsbDevice> {
    let Ok(list) = nusb::list_devices() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for d in list {
        let ifaces: Vec<(u8, u8, u8, Option<String>)> = d
            .interfaces()
            .map(|i| (i.class(), i.subclass(), i.protocol(), i.interface_string().map(str::to_string)))
            .collect();
        let Some(kind) = classify(d.vendor_id(), d.product_id(), &ifaces) else {
            continue;
        };

        let (drivers, accessible) = platform_details(&d);

        let has_driver = !drivers.is_empty();
        out.push(UsbDevice {
            vid: format!("{:04x}", d.vendor_id()),
            pid: format!("{:04x}", d.product_id()),
            vendor_name: vendor_name(d.vendor_id()).map(str::to_string),
            manufacturer: d.manufacturer_string().map(str::to_string),
            product: d.product_string().map(str::to_string),
            serial: d.serial_number().map(str::to_string),
            kind,
            drivers,
            has_driver,
            accessible,
            interfaces: ifaces
                .iter()
                .map(|i| format!("{:02x}/{:02x}/{:02x}", i.0, i.1, i.2))
                .collect(),
        });
    }
    out
}

#[cfg(target_os = "linux")]
fn platform_details(d: &nusb::DeviceInfo) -> (Vec<String>, Option<bool>) {
    let (drv, acc) = linux_details(d, !poll_paused());
    (drv, acc)
}

/// Windows: usługa sterownika węzła (None/pusta = brak sterownika).
#[cfg(target_os = "windows")]
fn platform_details(d: &nusb::DeviceInfo) -> (Vec<String>, Option<bool>) {
    let drivers = d
        .driver()
        .filter(|s| !s.is_empty())
        .map(|s| vec![s.to_string()])
        .unwrap_or_default();
    (drivers, None)
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn platform_details(_d: &nusb::DeviceInfo) -> (Vec<String>, Option<bool>) {
    (Vec::new(), None)
}

/// Linux: sterowniki interfejsów z sysfs + test otwarcia węzła /dev/bus/usb.
#[cfg(target_os = "linux")]
fn linux_details(d: &nusb::DeviceInfo, probe_access: bool) -> (Vec<String>, Option<bool>) {
    let dir = d.sysfs_path();
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut drivers = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if !n.starts_with(&format!("{name}:")) {
                continue;
            }
            if let Ok(link) = std::fs::read_link(e.path().join("driver")) {
                if let Some(drv) = link.file_name() {
                    let s = drv.to_string_lossy().to_string();
                    if !drivers.contains(&s) {
                        drivers.push(s);
                    }
                }
            }
        }
    }
    if !probe_access {
        // flashowanie w toku: nie otwieramy węzła USB (tylko sysfs)
        return (drivers, None);
    }
    let node = format!("/dev/bus/usb/{:03}/{:03}", d.bus_number(), d.device_address());
    let accessible = std::fs::OpenOptions::new().read(true).write(true).open(node).is_ok();
    (drivers, Some(accessible))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_special_modes() {
        assert_eq!(classify(0x04e8, 0x685d, &[]), Some(UsbKind::SamsungDownload));
        assert_eq!(classify(0x05c6, 0x9008, &[]), Some(UsbKind::QualcommEdl));
        assert_eq!(classify(0x0e8d, 0x0003, &[]), Some(UsbKind::MediatekBrom));
        assert_eq!(classify(0x2717, 0xff48, &[(0xff, 0x42, 0x01, None)]), Some(UsbKind::Adb));
        assert_eq!(classify(0x18d1, 0xd00d, &[]), Some(UsbKind::Fastboot));
        assert_eq!(classify(0x046d, 0xc077, &[]), None); // mysz Logitech
    }
}
