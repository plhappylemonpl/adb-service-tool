//! Protokół LOKE (tryb Download Samsung / "Odin") nad USB (`nusb`).
//!
//! ZAKRES: części NIEDESTRUKCYJNE, zaimplementowane i odpytujące urządzenie:
//!   - handshake ODIN -> LOKE,
//!   - otwarcie i zamknięcie sesji (begin/end), ustawienie rozmiaru paczki,
//!   - POBRANIE i PARSOWANIE pliku PIT (tablica partycji) - identyfikacja
//!     urządzenia i partycji w trybie Download,
//!   - reboot urządzenia.
//!
//! CELOWO NIE MA tu zapisu firmware przez USB. Zapis (flash) to operacja
//! destrukcyjna z realnym ryzykiem ucegleniem przy błędnym PIT/repartition,
//! a protokołu zapisu NIE da się zweryfikować bez prawdziwego Samsunga.
//! Flashowanie realizuje sprawdzony silnik Heimdall (patrz `vendors::samsung_flash`).
//! Ten moduł daje natomiast pewną, bezpieczną diagnostykę Download Mode.
//!
//! Stałe protokołu zgodne z publiczną, otwartą implementacją Heimdall
//! (Benjamin Dobell, GPL): typy pakietów 0x64..0x67, żądania PIT 0..3.

use serde::Serialize;

use crate::adb::events::Emitter;
use crate::error::{AdbError, Result};
use crate::hardware::usb::{UsbDevice, UsbKind};
use crate::hardware::usb_transport::UsbLink;
use crate::utils::CancelToken;

pub const SAMSUNG_VID: u16 = 0x04e8;
pub const DOWNLOAD_PIDS: &[u16] = &[0x685d, 0x6601, 0x68c3];

// Typy pakietów kontrolnych (pierwszy u32 LE)
const CTL_SESSION: u32 = 0x64;
const CTL_PIT: u32 = 0x65;
const CTL_END_SESSION: u32 = 0x67;

// Żądania sesji (0x64)
const SESSION_BEGIN: u32 = 0;
const SESSION_FILE_PART_SIZE: u32 = 5;

// Żądania pliku PIT (0x65)
const PIT_DUMP: u32 = 0x01; // rozpocznij odczyt, zwraca rozmiar
const PIT_PART: u32 = 0x02; // pobierz kolejny fragment
const PIT_END: u32 = 0x03; // zakończ transfer

// Żądania zakończenia sesji (0x67)
const END_REBOOT: u32 = 0x01;
const END_SHUTDOWN: u32 = 0x02;
const END_NORMAL: u32 = 0x00; // zakończ bez restartu (zostań w Download)

const PIT_CHUNK: usize = 500;
const RESP_LEN: usize = 8;
const TIMEOUT: u64 = 15;

/// Buduje 1024-bajtowy pakiet kontrolny: [u32 typ][u32 żądanie][reszta zera].
fn ctl(packet_type: u32, request: u32, extra: &[u32]) -> Vec<u8> {
    let mut v = vec![0u8; 1024];
    v[0..4].copy_from_slice(&packet_type.to_le_bytes());
    v[4..8].copy_from_slice(&request.to_le_bytes());
    for (i, x) in extra.iter().enumerate() {
        let o = 8 + i * 4;
        v[o..o + 4].copy_from_slice(&x.to_le_bytes());
    }
    v
}

fn resp_u32(buf: &[u8], word: usize) -> u32 {
    let o = word * 4;
    buf.get(o..o + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .unwrap_or(0)
}

/// Jedno wejście tablicy partycji (PIT).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PitEntry {
    pub partition_name: String,
    pub flash_filename: String,
    pub partition_id: u32,
    pub block_count: u32,
    /// typ binarki: 0 = AP, 1 = modem/CP
    pub binary_type: u32,
    pub device_type: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PitData {
    pub entry_count: u32,
    pub gang_name: String,
    pub project_name: String,
    pub entries: Vec<PitEntry>,
}

/// Parsuje binarny plik PIT (format Samsung, magic 0x12349876).
pub fn parse_pit(buf: &[u8]) -> Result<PitData> {
    if buf.len() < 28 || resp_u32(buf, 0) != 0x1234_9876 {
        return Err(AdbError::Parse("nieprawidłowy nagłówek PIT / bad PIT header".into()));
    }
    let entry_count = resp_u32(buf, 1);
    let read_str = |b: &[u8]| String::from_utf8_lossy(b).trim_end_matches('\0').trim().to_string();
    let gang_name = read_str(buf.get(8..16).unwrap_or_default());
    let project_name = read_str(buf.get(16..24).unwrap_or_default());

    const HEADER: usize = 28;
    const ENTRY: usize = 132;
    let mut entries = Vec::new();
    for i in 0..entry_count as usize {
        let base = HEADER + i * ENTRY;
        let Some(e) = buf.get(base..base + ENTRY) else { break };
        let w = |word: usize| resp_u32(e, word);
        // układ wpisu PIT: binaryType(0) deviceType(1) identifier(2) attributes(3)
        // updateAttributes(4) blockSizeOrOffset(5) blockCount(6) ... nazwy od offsetu 36 i 100
        entries.push(PitEntry {
            binary_type: w(0),
            device_type: w(1),
            partition_id: w(2),
            block_count: w(6),
            partition_name: read_str(e.get(36..68).unwrap_or_default()),
            flash_filename: read_str(e.get(68..100).unwrap_or_default()),
        });
    }
    Ok(PitData {
        entry_count,
        gang_name,
        project_name,
        entries,
    })
}

/// Otwiera urządzenie Download Mode i wykonuje handshake ODIN/LOKE + begin session.
pub struct LokeSession {
    link: UsbLink,
}

impl LokeSession {
    pub async fn connect() -> Result<LokeSession> {
        let info = nusb::list_devices()
            .map_err(|e| AdbError::Io(e.to_string()))?
            .find(|d| d.vendor_id() == SAMSUNG_VID && DOWNLOAD_PIDS.contains(&d.product_id()))
            .ok_or_else(|| {
                AdbError::Other(
                    "Nie wykryto urządzenia w trybie Download (04E8:685D) - wejdź w Download Mode / no Download device".into(),
                )
            })?;
        let link = UsbLink::open(&info)?;
        let mut s = LokeSession { link };
        s.handshake().await?;
        s.begin().await?;
        Ok(s)
    }

    async fn handshake(&mut self) -> Result<()> {
        self.link.send(b"ODIN".to_vec(), TIMEOUT).await?;
        let r = self.link.recv(4, TIMEOUT).await?;
        if r != b"LOKE" {
            return Err(AdbError::Command(format!(
                "Handshake nieudany - urządzenie odpowiedziało {:02X?} zamiast LOKE / handshake failed",
                r
            )));
        }
        Ok(())
    }

    /// Wysyła pakiet kontrolny i czyta 8-bajtową odpowiedź; sprawdza echo typu.
    async fn ctl_req(&self, packet_type: u32, request: u32, extra: &[u32]) -> Result<Vec<u8>> {
        self.link.send(ctl(packet_type, request, extra), TIMEOUT).await?;
        let r = self.link.recv(RESP_LEN, TIMEOUT).await?;
        if r.len() < 4 || resp_u32(&r, 0) != packet_type {
            return Err(AdbError::Command(format!(
                "Nieoczekiwana odpowiedź LOKE na typ {packet_type:#x} / unexpected response"
            )));
        }
        Ok(r)
    }

    async fn begin(&self) -> Result<()> {
        // begin session: zwraca wersję protokołu w słowie 1
        let _ = self.ctl_req(CTL_SESSION, SESSION_BEGIN, &[0]).await?;
        // ustaw rozmiar paczki pliku (standard Heimdall: 131072 B)
        let _ = self.ctl_req(CTL_SESSION, SESSION_FILE_PART_SIZE, &[131072]).await;
        Ok(())
    }

    /// Pobiera i parsuje PIT z urządzenia (niedestrukcyjne).
    pub async fn read_pit(&self, emitter: &Emitter, cancel: &CancelToken) -> Result<(Vec<u8>, PitData)> {
        emitter.info("LOKE: pobieram tablicę partycji (PIT) / downloading PIT...");
        let head = self.ctl_req(CTL_PIT, PIT_DUMP, &[]).await?;
        let size = resp_u32(&head, 1) as usize;
        if size == 0 || size > 1 << 20 {
            return Err(AdbError::Parse(format!("nieprawidłowy rozmiar PIT / bad PIT size: {size}")));
        }
        let parts = size.div_ceil(PIT_CHUNK);
        let mut buf = Vec::with_capacity(size);
        for i in 0..parts {
            if cancel.is_cancelled() {
                return Err(AdbError::Cancelled);
            }
            // żądanie fragmentu i: pakiet kontrolny + odbiór do 500 B danych
            self.link.send(ctl(CTL_PIT, PIT_PART, &[i as u32]), TIMEOUT).await?;
            let chunk = self.link.recv(PIT_CHUNK, TIMEOUT).await?;
            buf.extend_from_slice(&chunk);
        }
        buf.truncate(size);
        // potwierdź koniec transferu PIT
        let _ = self.ctl_req(CTL_PIT, PIT_END, &[]).await;
        let pit = parse_pit(&buf)?;
        emitter.ok(format!(
            "PIT: {} partycji, projekt {} / {} partitions",
            pit.entry_count, pit.project_name, pit.entry_count
        ));
        Ok((buf, pit))
    }

    /// Kończy sesję: reboot, shutdown lub pozostań w Download.
    pub async fn end(&self, mode: &str) -> Result<()> {
        let req = match mode {
            "reboot" => END_REBOOT,
            "shutdown" => END_SHUTDOWN,
            _ => END_NORMAL,
        };
        let _ = self.ctl_req(CTL_END_SESSION, req, &[]).await;
        Ok(())
    }
}

/// Wysokopoziomowo: wykryj Download Mode (USB) i zbuduj opis dla GUI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProbe {
    pub present: bool,
    pub devices: Vec<UsbDevice>,
    pub accessible: bool,
    /// trwa flashowanie - sonda wstrzymana (urządzenie zajęte przez Heimdall)
    pub busy: bool,
}

pub async fn probe() -> Result<DownloadProbe> {
    if crate::hardware::usb::poll_paused() {
        return Ok(DownloadProbe { present: true, devices: Vec::new(), accessible: true, busy: true });
    }
    let devs: Vec<UsbDevice> = tokio::task::spawn_blocking(crate::hardware::usb::scan)
        .await?
        .into_iter()
        .filter(|d| d.kind == UsbKind::SamsungDownload)
        .collect();
    let accessible = devs.first().map(|d| d.accessible.unwrap_or(true)).unwrap_or(false);
    Ok(DownloadProbe {
        present: !devs.is_empty(),
        accessible,
        devices: devs,
        busy: false,
    })
}

/// Zadanie w tle: połącz, pobierz PIT, zapisz do pliku, zakończ sesję bez restartu.
pub async fn run_read_pit(save_path: Option<String>, emitter: Emitter, cancel: CancelToken) {
    let res: Result<PitData> = async {
        if crate::hardware::usb::poll_paused() {
            return Err(AdbError::Other("Trwa flashowanie - odczyt PIT niedostępny / flashing in progress".into()));
        }
        let session = LokeSession::connect().await?;
        emitter.ok("Połączono z urządzeniem w trybie Download (LOKE) / connected.");
        let (raw, pit) = session.read_pit(&emitter, &cancel).await?;
        if let Some(path) = save_path {
            tokio::fs::write(&path, &raw).await?;
            emitter.ok(format!("Zapisano PIT do / saved PIT to: {path}"));
        }
        // zostań w Download Mode (serwisant decyduje o kolejnym kroku)
        let _ = session.end("normal").await;
        for e in &pit.entries {
            emitter.info(format!(
                "  #{:<2} {:<16} -> {:<20} ({} bloków, {})",
                e.partition_id,
                e.partition_name,
                if e.flash_filename.is_empty() { "-" } else { &e.flash_filename },
                e.block_count,
                if e.binary_type == 1 { "CP/modem" } else { "AP" }
            ));
        }
        Ok(pit)
    }
    .await;

    match res {
        Ok(_) => emitter.finished(1, 0, 1, vec![], false),
        Err(AdbError::Cancelled) => emitter.finished(0, 0, 1, vec![], true),
        Err(e) => emitter.fatal(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_pit() -> Vec<u8> {
        let mut v = vec![0u8; 28 + 132 * 2];
        v[0..4].copy_from_slice(&0x1234_9876u32.to_le_bytes());
        v[4..8].copy_from_slice(&2u32.to_le_bytes());
        v[8..16].copy_from_slice(b"COM_TAR2");
        v[16..24].copy_from_slice(b"A525F\0\0\0");
        let mut write_entry = |idx: usize, bin: u32, id: u32, blocks: u32, name: &str, file: &str| {
            let b = 28 + idx * 132;
            v[b..b + 4].copy_from_slice(&bin.to_le_bytes());
            v[b + 8..b + 12].copy_from_slice(&id.to_le_bytes());
            v[b + 24..b + 28].copy_from_slice(&blocks.to_le_bytes());
            v[b + 36..b + 36 + name.len()].copy_from_slice(name.as_bytes());
            v[b + 68..b + 68 + file.len()].copy_from_slice(file.as_bytes());
        };
        write_entry(0, 0, 80, 7000, "BOOT", "boot.img");
        write_entry(1, 1, 90, 120000, "MODEM", "modem.bin");
        v
    }

    #[test]
    fn parses_pit() {
        let pit = parse_pit(&fake_pit()).unwrap();
        assert_eq!(pit.entry_count, 2);
        assert_eq!(pit.project_name, "A525F");
        assert_eq!(pit.entries[0].partition_name, "BOOT");
        assert_eq!(pit.entries[0].flash_filename, "boot.img");
        assert_eq!(pit.entries[0].block_count, 7000);
        assert_eq!(pit.entries[1].binary_type, 1);
    }

    #[test]
    fn rejects_bad_magic() {
        assert!(parse_pit(&[0u8; 64]).is_err());
    }

    #[test]
    fn builds_ctl_packet() {
        let p = ctl(CTL_PIT, PIT_PART, &[7]);
        assert_eq!(p.len(), 1024);
        assert_eq!(resp_u32(&p, 0), 0x65);
        assert_eq!(resp_u32(&p, 1), 2);
        assert_eq!(resp_u32(&p, 2), 7);
    }
}
