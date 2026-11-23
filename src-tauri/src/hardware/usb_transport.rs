//! Cienki, asynchroniczny transport USB nad `nusb` (bez libusb).
//!
//! Wspólny dla LOKE (tryb Download Samsung) i niskopoziomowych komend MTP.
//! Otwiera urządzenie po VID:PID, odnajduje interfejs z dwoma endpointami
//! bulk (IN/OUT), udostępnia wysyłanie/odbiór z twardym timeoutem i mapuje
//! błędy `nusb`/USB na czytelne komunikaty PL/EN (kabel, uprawnienia, reset).
#![allow(dead_code)]

use std::time::Duration;

use nusb::transfer::{Completion, ControlOut, Direction, EndpointType, RequestBuffer};
use nusb::{Device, Interface};
use tokio::time::timeout;

use crate::error::{AdbError, Result};

/// Otwarte połączenie z urządzeniem w trybie niskopoziomowym.
pub struct UsbLink {
    pub serial: Option<String>,
    pub product: Option<String>,
    iface: Interface,
    ep_in: u8,
    ep_out: u8,
    /// maksymalny rozmiar pakietu endpointu OUT (do pakietów zero-length)
    pub out_packet: u16,
    #[allow(dead_code)]
    device: Device,
}

/// Czytelny błąd USB - tłumaczy natywne komunikaty `nusb` na język serwisu.
fn map_usb(e: impl std::fmt::Display) -> AdbError {
    let s = e.to_string();
    let low = s.to_lowercase();
    if low.contains("permission") || low.contains("access") || low.contains("denied") {
        AdbError::NoPermission(format!(
            "Brak uprawnień do urządzenia USB - zainstaluj sterownik (Windows) lub reguły udev (Linux) / USB permission denied: {s}"
        ))
    } else if low.contains("no such device")
        || low.contains("disconnect")
        || low.contains("no device")
        || low.contains("not found")
        || low.contains("entity")
    {
        AdbError::Other("Urządzenie odłączone (sprawdź kabel USB) / device disconnected, check USB cable".into())
    } else if low.contains("busy") || low.contains("in use") {
        AdbError::Other(
            "Port zajęty przez inny program (Odin / Kies / Smart Switch / Heimdall) - zamknij go / port busy".into(),
        )
    } else if low.contains("stall") {
        AdbError::Command(format!("USB STALL - urządzenie odrzuciło transfer / endpoint stalled: {s}"))
    } else {
        AdbError::Io(format!("Błąd USB / USB error: {s}"))
    }
}

/// Znajduje pierwszą parę endpointów bulk (IN, OUT) w urządzeniu i numer interfejsu.
fn find_bulk(dev: &Device) -> Result<(u8, u8, u8, u16)> {
    let cfg = dev
        .active_configuration()
        .or_else(|_| {
            dev.configurations()
                .next()
                .ok_or(())
                .map_err(|_| AdbError::Other("brak konfiguracji USB / no USB configuration".into()))
        })
        .map_err(|e| AdbError::Other(format!("konfiguracja USB / config: {e}")))?;

    for grp in cfg.interfaces() {
        let inum = grp.interface_number();
        for alt in grp.alt_settings() {
            let (mut ep_in, mut ep_out, mut out_mps) = (None, None, 64u16);
            for ep in alt.endpoints() {
                if ep.transfer_type() != EndpointType::Bulk {
                    continue;
                }
                match ep.direction() {
                    Direction::In if ep_in.is_none() => ep_in = Some(ep.address()),
                    Direction::Out if ep_out.is_none() => {
                        ep_out = Some(ep.address());
                        out_mps = ep.max_packet_size() as u16;
                    }
                    _ => {}
                }
            }
            if let (Some(i), Some(o)) = (ep_in, ep_out) {
                return Ok((inum, i, o, out_mps.max(1)));
            }
        }
    }
    Err(AdbError::Other(
        "Urządzenie nie udostępnia endpointów bulk (zły tryb USB?) / no bulk endpoints".into(),
    ))
}

impl UsbLink {
    /// Otwiera pierwsze urządzenie o danym VID:PID i przejmuje interfejs bulk.
    /// `device_info` pozwala wołającemu dać już znalezione `DeviceInfo`.
    pub fn open(info: &nusb::DeviceInfo) -> Result<UsbLink> {
        let serial = info.serial_number().map(str::to_string);
        let product = info.product_string().map(str::to_string);
        let device = info.open().map_err(map_usb)?;
        let (inum, ep_in, ep_out, out_packet) = find_bulk(&device)?;
        // detach_and_claim: na Linuksie odpina sterownik jądra (np. usb-storage), na Windows = claim
        let iface = device
            .detach_and_claim_interface(inum)
            .or_else(|_| device.claim_interface(inum))
            .map_err(map_usb)?;
        Ok(UsbLink {
            serial,
            product,
            iface,
            ep_in,
            ep_out,
            out_packet,
            device,
        })
    }

    /// Znajduje i otwiera pierwsze urządzenie pasujące do (vid, pid).
    pub fn open_vid_pid(vid: u16, pid: u16) -> Result<UsbLink> {
        let info = nusb::list_devices()
            .map_err(map_usb)?
            .find(|d| d.vendor_id() == vid && d.product_id() == pid)
            .ok_or_else(|| {
                AdbError::Other(format!(
                    "Nie znaleziono urządzenia {vid:04X}:{pid:04X} (podłącz urządzenie w odpowiednim trybie) / device not found"
                ))
            })?;
        Self::open(&info)
    }

    /// Wysyła bufor na endpoint OUT. Zwraca liczbę wysłanych bajtów.
    pub async fn send(&self, data: Vec<u8>, timeout_s: u64) -> Result<usize> {
        let n = data.len();
        let fut = self.iface.bulk_out(self.ep_out, data);
        match timeout(Duration::from_secs(timeout_s.max(1)), fut).await {
            Ok(c) => {
                let c: Completion<_> = c;
                c.into_result().map(|r| r.actual_length()).map_err(map_usb)?;
                Ok(n)
            }
            Err(_) => Err(AdbError::Timeout(timeout_s)),
        }
    }

    /// Odbiera do `len` bajtów z endpointu IN.
    pub async fn recv(&self, len: usize, timeout_s: u64) -> Result<Vec<u8>> {
        let fut = self.iface.bulk_in(self.ep_in, RequestBuffer::new(len));
        match timeout(Duration::from_secs(timeout_s.max(1)), fut).await {
            Ok(c) => c.into_result().map_err(map_usb),
            Err(_) => Err(AdbError::Timeout(timeout_s)),
        }
    }

    /// Pakiet zero-length na OUT (ZLP) - niektóre protokoły wymagają go po pełnym pakiecie.
    pub async fn send_zlp(&self, timeout_s: u64) -> Result<()> {
        self.send(Vec::new(), timeout_s).await.map(|_| ())
    }

    /// Transfer kontrolny (class/vendor) - używany przez komendy MTP/SCSI.
    #[allow(clippy::too_many_arguments)]
    pub async fn control_out(
        &self,
        control_type: nusb::transfer::ControlType,
        recipient: nusb::transfer::Recipient,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
        timeout_s: u64,
    ) -> Result<()> {
        let fut = self.iface.control_out(ControlOut {
            control_type,
            recipient,
            request,
            value,
            index,
            data,
        });
        match timeout(Duration::from_secs(timeout_s.max(1)), fut).await {
            Ok(c) => c.into_result().map(|_| ()).map_err(map_usb),
            Err(_) => Err(AdbError::Timeout(timeout_s)),
        }
    }
}
