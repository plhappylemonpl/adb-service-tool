//! Czytnik ZIP (APK) z dostępem swobodnym - działa ZDALNIE na urządzeniu:
//! zamiast pobierać całe APK (często 100+ MB), czytamy przez
//! `adb exec-out dd ...` tylko koniec pliku (katalog centralny) i potrzebne
//! wpisy (manifest, resources.arsc, ikona) - zwykle kilkaset KB.
//! Ten sam kod obsługuje lokalny plik (fallback: APK ściągnięte do tempa).

use std::collections::HashMap;
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncSeekExt};

use super::bytes::{u16_at, u32_at, u64_at};
use crate::adb::Adb;
use crate::error::{AdbError, Result};
use crate::utils::shell_quote;

/// Limit rozmiaru pojedynczego rozpakowanego wpisu (ochrona pamięci).
const MAX_ENTRY: u64 = 64 * 1024 * 1024;

pub enum ApkSource {
    Remote {
        adb: Arc<Adb>,
        serial: String,
        path: String,
        size: u64,
    },
    Local {
        path: PathBuf,
        size: u64,
    },
}

impl ApkSource {
    pub fn size(&self) -> u64 {
        match self {
            ApkSource::Remote { size, .. } | ApkSource::Local { size, .. } => *size,
        }
    }

    /// Czyta `len` bajtów od `off`.
    pub async fn read(&self, off: u64, len: u64) -> Result<Vec<u8>> {
        let len = len.min(self.size().saturating_sub(off));
        if len == 0 {
            return Ok(Vec::new());
        }
        match self {
            ApkSource::Local { path, .. } => {
                let mut f = tokio::fs::File::open(path).await?;
                f.seek(std::io::SeekFrom::Start(off)).await?;
                let mut buf = vec![0u8; len as usize];
                f.read_exact(&mut buf).await?;
                Ok(buf)
            }
            ApkSource::Remote { adb, serial, path, .. } => {
                // dd z wyrównaniem do bloków - działa na toybox/toolbox/busybox
                let bs: u64 = if len > 256 * 1024 { 65536 } else { 4096 };
                let skip = off / bs;
                let start_in = (off % bs) as usize;
                let count = (start_in as u64 + len).div_ceil(bs);
                let cmd = format!(
                    "dd if={} bs={bs} skip={skip} count={count} 2>/dev/null",
                    shell_quote(path)
                );
                let timeout = 30 + len / (1024 * 1024) * 2;
                let raw = adb.exec_out(serial, &cmd, timeout).await?;
                let end = (start_in + len as usize).min(raw.len());
                if raw.len() <= start_in || end - start_in < len as usize {
                    return Err(AdbError::Io(format!(
                        "krótki odczyt z urządzenia / short read ({} z {len} B)",
                        raw.len().saturating_sub(start_in)
                    )));
                }
                Ok(raw[start_in..end].to_vec())
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    method: u16,
    comp: u64,
    uncomp: u64,
    local_off: u64,
    name_len: u64,
}

pub struct Zip {
    entries: HashMap<String, Entry>,
}

impl Zip {
    pub async fn open(src: &ApkSource) -> Result<Zip> {
        let size = src.size();
        if size < 22 {
            return Err(AdbError::Parse("APK za mały / too small".into()));
        }
        let tail_len = size.min(66 * 1024);
        let tail_off = size - tail_len;
        let tail = src.read(tail_off, tail_len).await?;

        // EOCD: sygnatura 0x06054b50, szukamy od końca
        let eocd = (0..=tail.len().saturating_sub(22))
            .rev()
            .find(|&i| u32_at(&tail, i) == 0x0605_4b50)
            .ok_or_else(|| AdbError::Parse("brak EOCD - to nie jest ZIP/APK".into()))?;

        let mut cd_size = u32_at(&tail, eocd + 12) as u64;
        let mut cd_off = u32_at(&tail, eocd + 16) as u64;

        // ZIP64
        if (cd_off == 0xffff_ffff || cd_size == 0xffff_ffff) && eocd >= 20 {
            let loc = eocd - 20;
            if u32_at(&tail, loc) == 0x0706_4b50 {
                let z64_off = u64_at(&tail, loc + 8);
                let z = src.read(z64_off, 56).await?;
                if u32_at(&z, 0) == 0x0606_4b50 {
                    cd_size = u64_at(&z, 40);
                    cd_off = u64_at(&z, 48);
                }
            }
        }
        if cd_off.saturating_add(cd_size) > size || cd_size > 32 * 1024 * 1024 {
            return Err(AdbError::Parse("uszkodzony katalog centralny / bad central directory".into()));
        }

        let cd = if cd_off >= tail_off {
            let s = (cd_off - tail_off) as usize;
            tail.get(s..s + cd_size as usize)
                .map(|x| x.to_vec())
                .unwrap_or_default()
        } else {
            src.read(cd_off, cd_size).await?
        };

        let mut entries = HashMap::new();
        let mut p = 0usize;
        while p + 46 <= cd.len() && u32_at(&cd, p) == 0x0201_4b50 {
            let method = u16_at(&cd, p + 10);
            let comp = u32_at(&cd, p + 20) as u64;
            let uncomp = u32_at(&cd, p + 24) as u64;
            let n = u16_at(&cd, p + 28) as usize;
            let e = u16_at(&cd, p + 30) as usize;
            let c = u16_at(&cd, p + 32) as usize;
            let local_off = u32_at(&cd, p + 42) as u64;
            let name = cd
                .get(p + 46..p + 46 + n)
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default();
            entries.insert(
                name,
                Entry {
                    method,
                    comp,
                    uncomp,
                    local_off,
                    name_len: n as u64,
                },
            );
            p += 46 + n + e + c;
        }
        if entries.is_empty() {
            return Err(AdbError::Parse("pusty ZIP / empty zip".into()));
        }
        Ok(Zip { entries })
    }

    pub fn names(&self) -> impl Iterator<Item = &String> {
        self.entries.keys()
    }

    pub fn has(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    /// Czyta i (jeśli trzeba) rozpakowuje wpis.
    pub async fn read(&self, src: &ApkSource, name: &str) -> Result<Vec<u8>> {
        let e = self
            .entries
            .get(name)
            .ok_or_else(|| AdbError::Parse(format!("brak wpisu {name}")))?;
        if e.uncomp > MAX_ENTRY || e.comp > MAX_ENTRY {
            return Err(AdbError::Parse(format!("wpis {name} za duży / too large")));
        }
        // jeden odczyt: nagłówek lokalny + zapas na pole extra + dane
        let guess = 30 + e.name_len + 128 + e.comp;
        let mut buf = src.read(e.local_off, guess).await?;
        if u32_at(&buf, 0) != 0x0403_4b50 {
            return Err(AdbError::Parse("zły nagłówek lokalny / bad local header".into()));
        }
        let n = u16_at(&buf, 26) as u64;
        let x = u16_at(&buf, 28) as u64;
        let data_start = 30 + n + x;
        if data_start + e.comp > buf.len() as u64 {
            buf = src.read(e.local_off + data_start, e.comp).await?;
        } else {
            buf = buf[data_start as usize..(data_start + e.comp) as usize].to_vec();
        }
        match e.method {
            0 => Ok(buf),
            8 => {
                let mut out = Vec::with_capacity(e.uncomp.min(MAX_ENTRY) as usize);
                flate2::read::DeflateDecoder::new(&buf[..])
                    .take(MAX_ENTRY)
                    .read_to_end(&mut out)
                    .map_err(|err| AdbError::Parse(format!("deflate {name}: {err}")))?;
                Ok(out)
            }
            m => Err(AdbError::Parse(format!("nieobsługiwana kompresja {m}"))),
        }
    }
}
