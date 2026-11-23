//! Leniwe listowanie JEDNEGO poziomu katalogu na urządzeniu.
//! Port `DirListThread` z `adb_pull_all_gui.py`.
//!
//! Używany w obu drzewkach: do pobierania i do wyboru celu wysyłania.
//! Markery __NOPERM__ / __DIRS__ / __FILES__ zachowane 1:1 z oryginału.

use serde::Serialize;

use crate::adb::root::RootMode;
use crate::adb::Adb;
use crate::error::{AdbError, Result};
use crate::utils::shell_quote;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirListing {
    pub path: String,
    pub dirs: Vec<String>,
    pub files: Vec<String>,
}

/// Listuje zawartość katalogu `path` jeden poziom w głąb.
/// Zwraca `AdbError::NoPermission` gdy katalog nieczytelny (marker __NOPERM__).
/// `root` - listowanie z eskalacją (su), np. /data/data na zrootowanym urządzeniu.
pub async fn list_dir(adb: &Adb, serial: &str, path: &str, root: RootMode) -> Result<DirListing> {
    let p = shell_quote(path);
    let cmd = format!(
        "if [ ! -r {p} ]; then echo __NOPERM__; \
         else echo __DIRS__; find -L {p} -mindepth 1 -maxdepth 1 -type d 2>/dev/null; \
         echo __FILES__; find -L {p} -mindepth 1 -maxdepth 1 -type f 2>/dev/null; fi"
    );

    let r = adb.shell(Some(serial), &root.wrap(&cmd), 30).await?;
    let mut lines = r.stdout.lines();

    // pierwsza niepusta linia może być markerem braku uprawnień
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut section: Option<char> = None;

    for line in lines.by_ref() {
        // tylko \r z powłoki - spacje na początku/końcu nazwy są częścią nazwy pliku
        let s = line.trim_end_matches('\r');
        match s.trim() {
            "__NOPERM__" => return Err(AdbError::NoPermission(path.to_string())),
            "__DIRS__" => {
                section = Some('d');
                continue;
            }
            "__FILES__" => {
                section = Some('f');
                continue;
            }
            "" => continue,
            _ => {}
        }
        match section {
            Some('d') => dirs.push(s.to_string()),
            Some('f') => files.push(s.to_string()),
            _ => {}
        }
    }

    Ok(DirListing {
        path: path.to_string(),
        dirs,
        files,
    })
}

/// Skanuje partycje/pamięci urządzenia (`df -h`) - port PartitionScanThread (uproszczony).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Partition {
    pub mount: String,
    pub size: String,
    pub used: String,
    pub avail: String,
    pub readable: bool,
}

pub async fn scan_partitions(adb: &Adb, serial: &str) -> Result<Vec<Partition>> {
    let r = adb.shell(Some(serial), "df -h 2>/dev/null", 15).await?;
    let mut out = Vec::new();
    for line in r.stdout.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 6 {
            continue;
        }
        // Filesystem Size Used Avail Use% Mounted-on
        let mount = cols[cols.len() - 1].to_string();
        // interesują nas realne magazyny danych, nie /proc itp.
        if !(mount.starts_with("/storage")
            || mount.starts_with("/data")
            || mount.starts_with("/sdcard")
            || mount == "/")
        {
            continue;
        }
        out.push(Partition {
            mount,
            size: cols[1].to_string(),
            used: cols[2].to_string(),
            avail: cols[3].to_string(),
            readable: true,
        });
    }
    Ok(out)
}
