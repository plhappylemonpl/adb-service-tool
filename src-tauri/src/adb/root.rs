//! Wykrywanie roota i eskalacja poleceń.
//!
//! Tryby:
//! - `Adbd`  - adbd działa jako root (`adb root`, ROM-y userdebug/eng):
//!   `adb shell` i `adb pull` mają pełny dostęp bez dodatkowych sztuczek.
//! - `Su`    - Magisk / KernelSU / SuperSU: `su -c '<polecenie>'`.
//! - `SuZero`- su z AOSP (userdebug): `su 0 sh -c '<polecenie>'`.
//!
//! `adb pull` (protokół sync) zawsze działa z uprawnieniami adbd - przy `su`
//! NIE widzi plików chronionych. Dla nich kopiowanie idzie strumieniem
//! `adb exec-out su -c 'cat <plik>'` prosto do pliku lokalnego (bez kopii
//! pośredniej na karcie pamięci, bez zostawiania śladów na urządzeniu).

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::Mutex;

use crate::adb::Adb;
use crate::utils::shell_quote;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum RootMode {
    #[default]
    None,
    Adbd,
    Su,
    SuZero,
}

impl RootMode {
    pub fn is_root(self) -> bool {
        self != RootMode::None
    }

    /// Czy pliki chronione trzeba czytać przez `su` (adb pull ich nie zobaczy).
    pub fn needs_su_read(self) -> bool {
        matches!(self, RootMode::Su | RootMode::SuZero)
    }

    /// Opakowuje polecenie powłoki w eskalację (jeśli potrzebna).
    pub fn wrap(self, cmd: &str) -> String {
        match self {
            RootMode::Su => format!("su -c {}", shell_quote(cmd)),
            RootMode::SuZero => format!("su 0 sh -c {}", shell_quote(cmd)),
            RootMode::None | RootMode::Adbd => cmd.to_string(),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RootMode::None => "brak / none",
            RootMode::Adbd => "adbd root",
            RootMode::Su => "su (Magisk/KernelSU/SuperSU)",
            RootMode::SuZero => "su 0 (AOSP)",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootInfo {
    pub mode: RootMode,
    /// binarka su istnieje (nawet jeśli odmówiła)
    pub su_present: bool,
    pub detail: String,
}

/// Pamięć wykrytego trybu per urządzenie (żeby nie pytać su przy każdym folderze w drzewie).
pub type RootCache = Arc<Mutex<HashMap<String, RootMode>>>;

fn is_uid0(s: &str) -> bool {
    s.contains("uid=0(") || s.trim_start().starts_with("uid=0 ")
}

/// Wykrywa roota. Uwaga: pierwsze `su` może wyświetlić na urządzeniu okno
/// Magiska „Zezwolić?” - stąd dłuższy limit czasu.
pub async fn detect(adb: &Adb, serial: &str) -> RootInfo {
    // 1. adbd już jako root
    if let Ok(o) = adb.shell(Some(serial), "id", 10).await {
        if is_uid0(&o.stdout) {
            return RootInfo { mode: RootMode::Adbd, su_present: false, detail: o.stdout.trim().to_string() };
        }
    }
    // 2. czy su w ogóle jest (szybko, bez okna na urządzeniu)
    let present = adb
        .shell(
            Some(serial),
            "command -v su 2>/dev/null || which su 2>/dev/null || ls /system/xbin/su /system/bin/su /sbin/su /debug_ramdisk/su 2>/dev/null",
            10,
        )
        .await
        .map(|o| o.stdout.lines().any(|l| l.trim().ends_with("/su") || l.trim() == "su"))
        .unwrap_or(false);
    if !present {
        return RootInfo { mode: RootMode::None, su_present: false, detail: "brak su / no su binary".into() };
    }
    // 3. su -c (Magisk, KernelSU, SuperSU)
    let last = match adb.shell(Some(serial), "su -c id", 30).await {
        Ok(o) if is_uid0(&o.stdout) => {
            return RootInfo { mode: RootMode::Su, su_present: true, detail: o.stdout.trim().to_string() };
        }
        Ok(o) => format!("{} {}", o.stdout.trim(), o.stderr.trim()),
        Err(e) => e.to_string(),
    };
    // 4. su z AOSP: `su 0 <cmd>`
    if let Ok(o) = adb.shell(Some(serial), "su 0 id", 15).await {
        if is_uid0(&o.stdout) {
            return RootInfo { mode: RootMode::SuZero, su_present: true, detail: o.stdout.trim().to_string() };
        }
    }
    RootInfo {
        mode: RootMode::None,
        su_present: true,
        detail: format!(
            "su jest, ale odmówił dostępu - zezwól powłoce (Shell) w Magisk/KernelSU i spróbuj ponownie / su denied: {}",
            last.trim()
        ),
    }
}

/// Tryb z pamięci albo świeże wykrycie (wynik zapamiętywany).
pub async fn cached_or_detect(cache: &RootCache, adb: &Adb, serial: &str) -> RootInfo {
    if let Some(m) = cache.lock().await.get(serial).copied() {
        return RootInfo { mode: m, su_present: m.needs_su_read(), detail: "cache".into() };
    }
    let info = detect(adb, serial).await;
    cache.lock().await.insert(serial.to_string(), info.mode);
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_commands() {
        assert_eq!(RootMode::None.wrap("find '/a b'"), "find '/a b'");
        assert_eq!(RootMode::Su.wrap("find '/a b'"), "su -c 'find '\\''/a b'\\'''");
        assert_eq!(RootMode::SuZero.wrap("id"), "su 0 sh -c 'id'");
        assert!(is_uid0("uid=0(root) gid=0(root) context=u:r:magisk:s0"));
        assert!(!is_uid0("uid=2000(shell) gid=2000(shell)"));
    }
}
