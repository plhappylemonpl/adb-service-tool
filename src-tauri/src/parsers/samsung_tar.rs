//! Strumieniowe czytanie firmware Samsunga (`.tar` / `.tar.md5`) BEZ
//! wypakowywania gigabajtów na dysk.
//!
//! `.tar.md5` = zwykły tar z dopisaną na końcu linią `md5sum`:
//! `<32 hex>  <nazwa>\n`. MD5 liczony jest po bajtach archiwum PRZED tą linią.
//!
//! Tu: indeksowanie wpisów (nazwa + rozmiar) z nagłówków ustar (bloki 512 B),
//! rozpoznanie roli paczki (BL/AP/CP/CSC) po nazwie pliku oraz strumieniowa
//! weryfikacja MD5 z paskiem postępu. Ekstrakcja pojedynczego obrazu też
//! strumieniowa (kopiuje tylko wybrany wpis, resztę pomija przez `seek`).

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use md5::{Digest, Md5};
use serde::Serialize;

use crate::adb::events::Emitter;
use crate::adb::progress::Tracker;
use crate::error::{AdbError, Result};
use crate::utils::CancelToken;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TarRole {
    /// Bootloader (sboot, vbmeta, ...)
    Bl,
    /// AP / PDA (boot, system, super, ...)
    Ap,
    /// Modem / CP
    Cp,
    /// CSC (region, data)
    Csc,
    /// HOME_CSC (CSC bez kasowania danych)
    HomeCsc,
    /// USERDATA
    Userdata,
    Unknown,
}

pub fn classify(filename: &str) -> TarRole {
    let f = filename.to_uppercase();
    let base = f.rsplit(['/', '\\']).next().unwrap_or(&f);
    if base.starts_with("BL_") {
        TarRole::Bl
    } else if base.starts_with("AP_") {
        TarRole::Ap
    } else if base.starts_with("CP_") {
        TarRole::Cp
    } else if base.starts_with("HOME_CSC_") {
        TarRole::HomeCsc
    } else if base.starts_with("CSC_") {
        TarRole::Csc
    } else if base.starts_with("USERDATA_") {
        TarRole::Userdata
    } else {
        TarRole::Unknown
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TarImage {
    pub name: String,
    pub size: u64,
    /// offset danych w pliku tar (do strumieniowej ekstrakcji)
    pub offset: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirmwareTar {
    pub path: String,
    pub filename: String,
    pub role: TarRole,
    pub images: Vec<TarImage>,
    pub total_size: u64,
    pub file_size: u64,
    pub has_md5: bool,
    /// oczekiwana suma z linii md5sum (gdy .tar.md5)
    pub md5_expected: Option<String>,
}

/// Parsuje oktalne pole nagłówka ustar.
fn octal(field: &[u8]) -> u64 {
    let s: String = field
        .iter()
        .take_while(|&&c| c != 0 && c != b' ')
        .map(|&c| c as char)
        .collect();
    u64::from_str_radix(s.trim(), 8).unwrap_or(0)
}

/// Szuka stopki md5sum w ostatnich bajtach pliku; zwraca (offset_stopki, suma).
fn find_md5_footer(f: &mut std::fs::File, file_size: u64) -> Option<(u64, String)> {
    let tail = file_size.min(8192);
    let start = file_size - tail;
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = vec![0u8; tail as usize];
    f.read_exact(&mut buf).ok()?;
    // Footer md5sum jest "sklejony" z końcem tar (bez poprzedzającego newline):
    // <32 hex><2 spacje><nazwa>\n. Szukamy od końca 32 heksów zakończonych spacją,
    // poprzedzonych bajtem nie-heksowym (zwykle 0x00 z wyrównania tar).
    let n = buf.len();
    if n < 34 {
        return None;
    }
    for k in (0..=n - 34).rev() {
        if buf[k..k + 32].iter().all(|b| b.is_ascii_hexdigit())
            && (buf[k + 32] == b' ' || buf[k + 32] == b'\t')
            && (k == 0 || !buf[k - 1].is_ascii_hexdigit())
        {
            // reszta do końca musi być drukowalną nazwą (+ opcjonalny newline)
            let rest_ok = buf[k + 32..].iter().all(|&c| c == b'\n' || c == b'\r' || (0x20..0x7f).contains(&c));
            if rest_ok {
                let hex = String::from_utf8_lossy(&buf[k..k + 32]).to_lowercase();
                return Some((start + k as u64, hex));
            }
        }
    }
    None
}

/// Indeksuje tar/tar.md5 (blokujące - wołać w spawn_blocking).
pub fn index(path: &Path) -> Result<FirmwareTar> {
    let mut f = std::fs::File::open(path)?;
    let file_size = f.metadata()?.len();
    let filename = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();

    let (md5_expected, archive_end) = if filename.to_lowercase().ends_with(".md5") {
        match find_md5_footer(&mut f, file_size) {
            Some((off, sum)) => (Some(sum), off),
            None => (None, file_size),
        }
    } else {
        (None, file_size)
    };

    // parsowanie nagłówków ustar
    f.seek(SeekFrom::Start(0))?;
    let mut images = Vec::new();
    let mut total = 0u64;
    let mut pos = 0u64;
    let mut hdr = [0u8; 512];
    let mut empty_streak = 0;
    while pos + 512 <= archive_end {
        f.seek(SeekFrom::Start(pos))?;
        if f.read_exact(&mut hdr).is_err() {
            break;
        }
        if hdr.iter().all(|&b| b == 0) {
            empty_streak += 1;
            pos += 512;
            if empty_streak >= 2 {
                break;
            }
            continue;
        }
        empty_streak = 0;
        // nazwa: pole 0..100 (+ prefix 345..500 dla ustar)
        let name_main = String::from_utf8_lossy(&hdr[0..100]);
        let name_main = name_main.trim_end_matches('\0');
        let prefix = String::from_utf8_lossy(&hdr[345..500]);
        let prefix = prefix.trim_end_matches('\0');
        let name = if prefix.is_empty() {
            name_main.to_string()
        } else {
            format!("{prefix}/{name_main}")
        };
        let size = octal(&hdr[124..136]);
        let typeflag = hdr[156];
        let data_off = pos + 512;
        // 0 lub '0' = zwykły plik
        if (typeflag == 0 || typeflag == b'0') && !name.is_empty() {
            images.push(TarImage {
                name: name.clone(),
                size,
                offset: data_off,
            });
            total += size;
        }
        // przejdź za dane (zaokrąglone do 512)
        pos = data_off + size.div_ceil(512) * 512;
    }

    Ok(FirmwareTar {
        role: classify(&filename),
        path: path.to_string_lossy().to_string(),
        filename,
        images,
        total_size: total,
        file_size,
        has_md5: md5_expected.is_some(),
        md5_expected,
    })
}

/// Strumieniowo weryfikuje MD5 całej paczki .tar.md5 (postęp bajtowy).
pub async fn verify_md5(fw: FirmwareTar, emitter: Emitter, cancel: CancelToken) -> Result<bool> {
    let Some(expected) = fw.md5_expected.clone() else {
        return Err(AdbError::InvalidArg("plik nie ma sumy MD5 (.tar bez .md5) / no md5".into()));
    };
    // md5 liczymy po bajtach archiwum przed stopką md5sum
    let mut f = std::fs::File::open(&fw.path)?;
    let archive_end = {
        let size = f.metadata()?.len();
        match find_md5_footer(&mut f, size) {
            Some((off, _)) => off,
            None => size,
        }
    };
    f.seek(SeekFrom::Start(0)).map_err(AdbError::from)?;

    let tracker = Tracker::new(1, archive_end);
    tracker.set_current(format!("MD5 {}", fw.filename));
    let ticker = tracker.start(emitter.clone());
    let tk = tracker.clone();

    let res = tokio::task::spawn_blocking(move || -> Result<String> {
        let mut h = Md5::new();
        let mut remaining = archive_end;
        let mut buf = vec![0u8; 4 << 20];
        while remaining > 0 {
            if cancel.is_cancelled() {
                return Err(AdbError::Cancelled);
            }
            let want = (buf.len() as u64).min(remaining) as usize;
            f.read_exact(&mut buf[..want]).map_err(AdbError::from)?;
            h.update(&buf[..want]);
            tracker.add_bytes(want as u64);
            remaining -= want as u64;
        }
        Ok(crate::utils::download::hex(&h.finalize()))
    })
    .await?;
    tk.item_done();
    tk.shutdown(ticker).await;
    let got = match res {
        Ok(g) => g,
        Err(e) => return Err(e),
    };

    let ok = got.eq_ignore_ascii_case(&expected);
    if ok {
        emitter.ok(format!("MD5 OK: {got}"));
    } else {
        emitter.error(format!("MD5 niezgodne / mismatch — plik: {got}, oczekiwano: {expected}"));
    }
    Ok(ok)
}

/// Nazwa obrazu w archiwum bez katalogu (np. `meta-data/fota.zip` -> `fota.zip`).
fn base_name(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// Czy obraz jest skompresowany LZ4 (firmware Samsunga od ~2019: `boot.img.lz4`, `super.img.lz4`...).
pub fn is_lz4(name: &str) -> bool {
    base_name(name).to_ascii_lowercase().ends_with(".lz4")
}

/// Nazwa pliku docelowego - taka, jaką zna PIT (`flash_filename`): bez katalogu i bez `.lz4`.
/// `sboot.bin.lz4` -> `sboot.bin`, `super.img.lz4` -> `super.img`, `modem.bin` -> `modem.bin`.
pub fn target_name(name: &str) -> String {
    let b = base_name(name);
    if is_lz4(b) {
        b[..b.len() - 4].to_string()
    } else {
        b.to_string()
    }
}

const LZ4_FRAME_MAGIC: u32 = 0x184D_2204;
const LZ4_LEGACY_MAGIC: u32 = 0x184C_2102;

/// Czytnik `Read`, który sprawdza anulowanie przy każdej porcji danych
/// i raportuje liczbę przeczytanych (skompresowanych) bajtów - do paska postępu.
struct CancelRead<'a, R> {
    inner: R,
    cancel: CancelToken,
    on_read: &'a (dyn Fn(u64) + Send + Sync),
}

impl<R: Read> Read for CancelRead<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = self.inner.read(buf)?;
        (self.on_read)(n as u64);
        Ok(n)
    }
}

/// Strumieniowo wyciąga JEDEN obraz z tar do pliku (bez rozpakowywania reszty).
/// Obraz `*.lz4` jest w locie dekompresowany (ramka LZ4, `lz4_flex`) - do pliku
/// trafia czysty `.img` / `.bin`, bo Heimdall nie rozumie LZ4.
/// Zapis do `<dest>.part` + rename - przerwana ekstrakcja nie zostawia "dobrego" pliku.
/// Zwraca liczbę zapisanych bajtów.
pub fn extract_one(tar_path: &Path, img: &TarImage, dest: &Path, cancel: &CancelToken) -> Result<u64> {
    extract_one_with(tar_path, img, dest, cancel, &|_| {})
}

/// Jak [`extract_one`], z raportowaniem przeczytanych bajtów archiwum (`on_read(n)`).
pub fn extract_one_with(
    tar_path: &Path,
    img: &TarImage,
    dest: &Path,
    cancel: &CancelToken,
    on_read: &(dyn Fn(u64) + Send + Sync),
) -> Result<u64> {
    let mut f = std::fs::File::open(tar_path)?;
    f.seek(SeekFrom::Start(img.offset))?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut part = dest.as_os_str().to_owned();
    part.push(".part");
    let part = std::path::PathBuf::from(part);
    let mut out = std::io::BufWriter::with_capacity(4 << 20, std::fs::File::create(&part)?);
    let src = CancelRead {
        inner: std::io::BufReader::with_capacity(4 << 20, (&mut f).take(img.size)),
        cancel: cancel.clone(),
        on_read,
    };
    let res = if is_lz4(&img.name) {
        decompress_lz4(src, &mut out, &img.name)
    } else {
        let mut src = src;
        std::io::copy(&mut src, &mut out).map_err(|e| io_err(e, &img.name))
    };
    let written = match res.and_then(|n| {
        std::io::Write::flush(&mut out).map_err(|e| io_err(e, &img.name))?;
        Ok(n)
    }) {
        Ok(n) => n,
        Err(e) => {
            drop(out);
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
    };
    drop(out);
    if !is_lz4(&img.name) && written != img.size {
        let _ = std::fs::remove_file(&part);
        return Err(AdbError::Other(format!(
            "{}: wyciągnięto {written} z {} B - archiwum uszkodzone / truncated",
            img.name, img.size
        )));
    }
    std::fs::rename(&part, dest)?;
    Ok(written)
}

fn io_err(e: std::io::Error, name: &str) -> AdbError {
    if e.kind() == std::io::ErrorKind::Interrupted && e.to_string() == "cancelled" {
        AdbError::Cancelled
    } else {
        AdbError::Io(format!("{name}: {e}"))
    }
}

/// Dekompresja strumienia ramek LZ4 (`lz4 -B6 --content-size`, tak pakuje Samsung).
/// Kilka ramek jedna po drugiej jest obsługiwanych (FrameDecoder czyta kolejne ramki).
pub fn decompress_lz4<R: Read, W: std::io::Write>(src: R, out: &mut W, name: &str) -> Result<u64> {
    let mut src = std::io::BufReader::new(src);
    // magic na początku - czytelny błąd zamiast "corrupt data" dla innego formatu
    let head = std::io::BufRead::fill_buf(&mut src).map_err(|e| io_err(e, name))?;
    if head.len() < 4 {
        return Err(AdbError::Parse(format!("{name}: pusty / za krótki plik LZ4")));
    }
    let magic = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
    if magic == LZ4_LEGACY_MAGIC {
        return Err(AdbError::Parse(format!(
            "{name}: stary format LZ4 (legacy, lz4 -l) - nieobsługiwany / legacy LZ4 frame"
        )));
    }
    if magic != LZ4_FRAME_MAGIC {
        return Err(AdbError::Parse(format!(
            "{name}: to nie jest ramka LZ4 (magic {magic:#010x}) / not an LZ4 frame"
        )));
    }
    let mut dec = lz4_flex::frame::FrameDecoder::new(src);
    std::io::copy(&mut dec, out).map_err(|e| {
        if e.kind() == std::io::ErrorKind::Interrupted {
            io_err(e, name)
        } else {
            AdbError::Parse(format!("{name}: uszkodzone dane LZ4 / corrupt LZ4: {e}"))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tar_header(name: &str, size: u64) -> [u8; 512] {
        let mut h = [0u8; 512];
        h[0..name.len()].copy_from_slice(name.as_bytes());
        let sz = format!("{size:011o}\0");
        h[124..124 + sz.len()].copy_from_slice(sz.as_bytes());
        h[156] = b'0';
        // suma kontrolna nagłówka nie jest nam potrzebna do indeksowania
        h
    }

    #[test]
    fn classifies_roles() {
        assert_eq!(classify("BL_A525FXXU8_CL.tar.md5"), TarRole::Bl);
        assert_eq!(classify("AP_A525F.tar.md5"), TarRole::Ap);
        assert_eq!(classify("HOME_CSC_OXM_A525F.tar.md5"), TarRole::HomeCsc);
        assert_eq!(classify("CSC_OXM_A525F.tar.md5"), TarRole::Csc);
    }

    #[test]
    fn indexes_and_verifies() {
        let dir = std::env::temp_dir().join(format!("adbst_tar_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("AP_test.tar.md5");
        let mut data = Vec::new();
        let img = b"BOOTIMAGEDATA___"; // 16 B
        data.extend_from_slice(&tar_header("boot.img", img.len() as u64));
        data.extend_from_slice(img);
        data.extend_from_slice(&vec![0u8; 512 - img.len()]); // padding bloku
        data.extend_from_slice(&[0u8; 1024]); // koniec archiwum
        let archive_end = data.len() as u64;
        let sum = crate::utils::download::hex(&Md5::digest(&data));
        data.extend_from_slice(format!("{sum}  AP_test.tar\n").as_bytes());
        std::fs::File::create(&path).unwrap().write_all(&data).unwrap();

        let fw = index(&path).unwrap();
        assert_eq!(fw.role, TarRole::Ap);
        assert_eq!(fw.images.len(), 1);
        assert_eq!(fw.images[0].name, "boot.img");
        assert_eq!(fw.images[0].size, 16);
        assert_eq!(fw.md5_expected.as_deref(), Some(sum.as_str()));

        // weryfikacja md5 liczona do archive_end
        let mut f = std::fs::File::open(&path).unwrap();
        let fsz = f.metadata().unwrap().len();
        let (off, got) = find_md5_footer(&mut f, fsz).unwrap();
        assert_eq!(off, archive_end);
        assert_eq!(got, sum);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn target_names_without_lz4() {
        assert_eq!(target_name("boot.img.lz4"), "boot.img");
        assert_eq!(target_name("SUPER.IMG.LZ4"), "SUPER.IMG");
        assert_eq!(target_name("sboot.bin.lz4"), "sboot.bin");
        assert_eq!(target_name("modem.bin"), "modem.bin");
        assert_eq!(target_name("meta-data/fota.zip"), "fota.zip");
        assert!(is_lz4("vbmeta.img.Lz4") && !is_lz4("lz4.img"));
    }

    /// Obraz .lz4 w tar -> extract_one zapisuje rozpakowane dane (strumieniowo, LZ4 frame).
    #[test]
    fn extracts_and_decompresses_lz4() {
        let dir = std::env::temp_dir().join(format!("adbst_lz4_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // ~3 MB danych częściowo powtarzalnych (kilka bloków LZ4)
        let raw: Vec<u8> = (0..3_000_000u32).map(|i| ((i / 7) % 251) as u8 ^ (i % 3) as u8).collect();
        let mut enc = lz4_flex::frame::FrameEncoder::new(Vec::new());
        enc.write_all(&raw).unwrap();
        let comp = enc.finish().unwrap();
        assert!(comp.len() < raw.len());

        let mut data = Vec::new();
        data.extend_from_slice(&tar_header("boot.img.lz4", comp.len() as u64));
        data.extend_from_slice(&comp);
        data.extend_from_slice(&vec![0u8; (512 - comp.len() % 512) % 512]);
        data.extend_from_slice(&[0u8; 1024]);
        let path = dir.join("AP_lz4.tar");
        std::fs::write(&path, &data).unwrap();

        let fw = index(&path).unwrap();
        assert_eq!(fw.images[0].name, "boot.img.lz4");
        let dest = dir.join(target_name(&fw.images[0].name));
        let n = extract_one(&path, &fw.images[0], &dest, &CancelToken::new()).unwrap();
        assert_eq!(n, raw.len() as u64);
        assert_eq!(std::fs::read(&dest).unwrap(), raw);
        assert!(!dir.join("boot.img.part").exists());

        // uszkodzony strumień -> błąd, brak pliku wynikowego
        let mut bad = comp.clone();
        let mid = bad.len() / 2;
        bad.truncate(mid);
        let mut out = Vec::new();
        assert!(decompress_lz4(&bad[..], &mut out, "x.lz4").is_err());
        assert!(decompress_lz4(&b"not lz4 data"[..], &mut out, "x.lz4").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
