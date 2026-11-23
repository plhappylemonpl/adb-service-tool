//! Pobieranie plików Z urządzenia na dysk - wierny port `PullThread` + ulepszenia.
//!
//! Priorytety:
//!  * RZETELNE: plik jest zaliczony jako skopiowany WYŁĄCZNIE gdy:
//!    kod wyjścia adb = 0, wyjście adb nie zawiera znanych błędów
//!    ("Permission denied", "0 files pulled", "skipped"...), plik fizycznie
//!    istnieje na dysku i ma rozmiar zgłoszony przez adb / zeskanowany na
//!    urządzeniu. Na końcu audyt: każdy zaliczony plik jest ponownie
//!    sprawdzany na dysku - żadnych fałszywych sukcesów w GUI.
//!  * ATOMOWE: adb zapisuje do krótkiego pliku tymczasowego
//!    (`<cel>/.adbst-partial/N.part`), a dopiero po weryfikacji Rust
//!    przenosi go na docelową ścieżkę. Niedokończony plik nigdy nie udaje
//!    gotowego, a adb nie dotyka długich ścieżek (Windows MAX_PATH -
//!    docelowe operacje używają prefiksu `\\?\`).
//!  * BEZ SHELLA: `adb pull` dostaje ścieżki jako osobne argumenty procesu
//!    (spacje, nawiasy, apostrofy w nazwach są bezpieczne). Ścieżki z drzewa
//!    są walidowane: tylko bezwzględne ścieżki Androida, nigdy `mtp:...`.
//!  * KOMPLETNE: skan to czysty `find -L <dir> -type f` czytany strumieniowo
//!    linia po linii (bez `-exec ... {} +` - toybox na dużych drzewach
//!    zwracał "exec stat: Argument list too long" i gubił całe partie plików).
//!    Rozmiary dociągane osobno w małych porcjach (`xargs -n 50`, awaryjnie
//!    porcje budowane w Rust). Błędy skanowania trafiają na listę błędów.
//!  * NTFS-FRIENDLY: opcjonalnie (domyślnie włączone, na Windows zawsze)
//!    nazwy są czyszczone z `: * ? " < > | \`, kropek/spacji na końcu i nazw
//!    zarezerwowanych - zapis na pendrive/dysk NTFS/FAT32/exFAT się nie wyłoży.
//!  * ODPORNE: pojedynczy nieudany plik nie przerywa całości; seria błędów
//!    rozłączenia pod rząd => przerwij resztę.
//!  * ROOT: wykrywany przed skanem (`adb shell id` -> adbd root, `su -c id`,
//!    `su 0 id`). Przy `su` skan (`find`/`stat`) idzie przez `su -c`, a pliki,
//!    których adbd nie widzi (np. /data/data, Android/data), są czytane
//!    strumieniem `adb exec-out su -c 'cat <plik>'` prosto do pliku lokalnego
//!    (weryfikacja rozmiaru z root-stat, data modyfikacji ustawiana w Rust).
//!    Odmowa FUSE (Android 11+) -> alias `/data/media/N/...`.
//!  * PEŁNA STRUKTURA: ukryte pliki/foldery (`.nomedia`), puste foldery
//!    odtwarzane lokalnie, dowiązania symboliczne rozwiązywane bez pętli
//!    (szczegóły w `scan.rs`).
//!  * WZNAWIALNE: checkpoint `<cel>/.adb_pull_progress.jsonl` tworzony na
//!    starcie zadania (append, fsync po każdym pliku, sync_all na końcu), nigdy
//!    nie usuwany. Wpis jest honorowany tylko gdy plik nadal leży na dysku
//!    z właściwym rozmiarem.
//!  * PŁYNNY POSTĘP: rozmiary z urządzenia (`stat`) + podgląd rosnącego pliku.

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use regex::Regex;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::adb::events::{Emitter, FailedItem};
use crate::adb::manager::Output;
use crate::adb::progress::Tracker;
use crate::adb::root::{self, RootCache, RootMode};
use crate::adb::scan::{self, scan_tree, CANCELLED};
use crate::adb::Adb;
use crate::error::AdbError;
use crate::utils::{shell_quote, CancelToken};

pub const CHECKPOINT_FILENAME: &str = ".adb_pull_progress.jsonl";
pub const PARTIAL_DIRNAME: &str = ".adbst-partial";
pub const DISCONNECT_STREAK_LIMIT: u32 = 5;

pub const DISCONNECT_PATTERNS: &[&str] = &[
    "device not found",
    "device offline",
    "no devices",
    "closed",
    "protocol fault",
    "connection reset",
    "cannot connect",
    "error: device",
];

pub fn looks_like_disconnect(err: &str) -> bool {
    let low = err.to_lowercase();
    DISCONNECT_PATTERNS.iter().any(|p| low.contains(p))
}

/// Słowa kluczowe w wyjściu `adb pull`, które oznaczają porażkę nawet przy kodzie 0.
const PULL_ERROR_PATTERNS: &[&str] = &[
    "permission denied",
    "operation not permitted",
    "adb: error",
    "error:",
    "failed to",
    "does not exist",
    "no such file",
    "not found",
    "read-only file system",
    "no space left",
    "cannot create",
    "couldn't create",
    "is a directory",
    "not a directory",
    "protocol fault",
    "device offline",
    "connection reset",
    "i/o error",
];

/// Element zaznaczony przez użytkownika w drzewie.
#[derive(Debug, Clone, Deserialize)]
pub struct SelectedItem {
    pub path: String,
    #[serde(rename = "isFile")]
    pub is_file: bool,
}

/// Parametry transferu (z GUI).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullOptions {
    pub items: Vec<SelectedItem>,
    pub local_root: String,
    #[serde(default = "default_workers")]
    pub workers: usize,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "default_true")]
    pub resume: bool,
    /// Pomijaj pliki istniejące lokalnie z tym samym rozmiarem.
    #[serde(default = "default_true")]
    pub skip_existing: bool,
    /// `adb pull -a` - zachowaj datę modyfikacji (ważne dla galerii zdjęć).
    #[serde(default = "default_true")]
    pub preserve_time: bool,
    /// Nazwy zgodne z NTFS/FAT32/exFAT (na Windows wymuszone).
    #[serde(default = "default_true")]
    pub ntfs_safe: bool,
    /// Użyj roota, jeśli urządzenie go ma (su / adbd root).
    #[serde(default = "default_true")]
    pub use_root: bool,
}

fn default_workers() -> usize {
    4
}
fn default_timeout() -> u64 {
    600
}
fn default_true() -> bool {
    true
}

/// Plik po stronie urządzenia do skopiowania.
#[derive(Debug, Clone)]
struct RemoteFile {
    /// ścieżka fizyczna na urządzeniu (z niej czytamy)
    path: String,
    /// ścieżka logiczna (checkpoint, log, lokalna struktura)
    logical: String,
    root: String,
    /// rozmiar ze skanu (0 = nieznany)
    size: u64,
    mtime: Option<i64>,
    /// docelowa ścieżka względna (po rozwiązaniu kolizji nazw)
    rel: String,
}

/// Plik zaliczony jako skopiowany - do audytu końcowego.
#[derive(Debug, Clone)]
struct Done {
    remote: String,
    rel: String,
    size: u64,
}

/// Wynik budowy listy.
struct Plan {
    files: Vec<RemoteFile>,
    skipped: u64,
    failures: Vec<FailedItem>,
    /// puste foldery: (ścieżka logiczna, korzeń zaznaczenia)
    empty_dirs: Vec<(String, String)>,
}

// --------------------------------------------------------------------------- //
// Ścieżki: walidacja zdalnych, długie lokalne (Windows)
// --------------------------------------------------------------------------- //

/// Normalizuje ścieżkę z drzewa do czystej, bezwzględnej ścieżki Androida.
/// Odrzuca `mtp:...`, URL-e, ścieżki Windows i względne.
pub fn normalize_remote(path: &str) -> std::result::Result<String, String> {
    let p = path.trim_end_matches(['\r', '\n']);
    let low = p.to_ascii_lowercase();
    if low.starts_with("mtp:") || low.contains("://") {
        return Err(format!(
            "Ścieżka MTP '{p}' nie może być użyta z adb - wybierz urządzenie w trybie ADB i zaznacz pliki w drzewie ADB / MTP path rejected"
        ));
    }
    if p.contains('\\') || p.contains('\0') || (p.len() >= 2 && p.as_bytes()[1] == b':') {
        return Err(format!("To nie jest ścieżka Androida: '{p}' / not an Android path"));
    }
    if !p.starts_with('/') {
        return Err(format!("Ścieżka musi być bezwzględna (od '/'): '{p}' / path must be absolute"));
    }
    let mut out = String::with_capacity(p.len());
    for seg in p.split('/').filter(|s| !s.is_empty() && *s != ".") {
        if seg == ".." {
            return Err(format!("Niedozwolony segment '..' w ścieżce: '{p}' / '..' not allowed"));
        }
        out.push('/');
        out.push_str(seg);
    }
    if out.is_empty() {
        out.push('/');
    }
    Ok(out)
}

/// Ścieżka do operacji na dysku. Windows: prefiks `\\?\` (limit 260 znaków
/// MAX_PATH nie obowiązuje). Inne systemy: bez zmian.
#[cfg(windows)]
pub fn long_path(p: &Path) -> PathBuf {
    let s = p.as_os_str().to_string_lossy();
    if s.starts_with(r"\\?\") {
        return p.to_path_buf();
    }
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().map(|c| c.join(p)).unwrap_or_else(|_| p.to_path_buf())
    };
    // \\?\ wyłącza normalizację - same backslashe, bez "." i ".."
    let mut norm = PathBuf::new();
    for c in abs.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                norm.pop();
            }
            other => norm.push(other.as_os_str()),
        }
    }
    let s = norm.to_string_lossy().replace('/', "\\");
    match s.strip_prefix(r"\\") {
        Some(unc) => PathBuf::from(format!(r"\\?\UNC\{unc}")),
        None => PathBuf::from(format!(r"\\?\{s}")),
    }
}

#[cfg(not(windows))]
pub fn long_path(p: &Path) -> PathBuf {
    p.to_path_buf()
}

/// Znaki niedozwolone w nazwach na NTFS / FAT32 / exFAT.
const NTFS_FORBIDDEN: &[char] = &[':', '*', '?', '"', '<', '>', '|', '\\'];
const NTFS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2",
    "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Nazwa pliku/folderu zgodna z Windows (NTFS/FAT32/exFAT):
/// `: * ? " < > | \` i znaki sterujące -> `_`, bez kropek/spacji na końcu,
/// nazwy zarezerwowane (CON, NUL, COM1...) poprzedzone `_`.
pub fn sanitize_for_ntfs(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| if NTFS_FORBIDDEN.contains(&c) || c.is_control() { '_' } else { c })
        .collect();
    let trimmed = s.trim_end_matches(['.', ' ']).len();
    if trimmed < s.len() {
        // "Folder." / "Nazwa " -> "Folder_" (nie gubimy informacji, że coś tam było)
        s.truncate(trimmed);
        s.push('_');
    }
    let stem = s.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    if NTFS_RESERVED.contains(&stem.as_str()) {
        s.insert(0, '_');
    }
    if s.is_empty() {
        s.push('_');
    }
    s
}

/// Człon nazwy na dysku lokalnym. Bez trybu NTFS (Linux/macOS) nazwa zostaje
/// 1:1 - jedynie znak NUL jest niemożliwy (separatora `/` w członie nie ma).
fn safe_component(name: &str, ntfs: bool) -> String {
    if ntfs {
        sanitize_for_ntfs(name)
    } else {
        let s: String = name.chars().map(|c| if c == '\0' { '_' } else { c }).collect();
        if s.is_empty() || s == "." || s == ".." {
            "_".into()
        } else {
            s
        }
    }
}

/// Tryb NTFS: na Windows zawsze, gdzie indziej wg opcji z GUI.
fn effective_ntfs(opt: bool) -> bool {
    cfg!(windows) || opt
}

/// Klucz kolizji: systemy/nośniki z nazwami bez rozróżniania wielkości liter.
fn collision_key(rel: &str, ntfs: bool) -> String {
    if ntfs || cfg!(target_os = "macos") {
        rel.to_lowercase()
    } else {
        rel.to_string()
    }
}

/// "a/b/plik.jpg" -> "a/b/plik (2).jpg"
fn with_suffix(rel: &str, n: u32) -> String {
    let p = Path::new(rel);
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let name = format!("{stem} ({n}){ext}");
    match p.parent().filter(|x| !x.as_os_str().is_empty()) {
        Some(parent) => parent.join(name).to_string_lossy().to_string(),
        None => name,
    }
}

// --------------------------------------------------------------------------- //

/// Punkt wejścia: (root) -> buduje listę plików -> kopiuje równolegle.
pub async fn run_pull(
    adb: Arc<Adb>,
    serial: String,
    opts: PullOptions,
    root_cache: Option<RootCache>,
    emitter: Emitter,
    cancel: CancelToken,
) {
    if serial.to_ascii_lowercase().starts_with("mtp:") {
        let msg = "Urządzenie w trybie MTP - kopiowanie przez adb wymaga trybu ADB (włącz debugowanie USB) / \
                   MTP device selected, ADB mode required";
        emitter.error(msg);
        emitter.fatal(msg);
        return;
    }
    // checkpoint powstaje OD RAZU (także gdy nie będzie nic do skopiowania)
    let checkpoint = match open_checkpoint(&opts.local_root) {
        Ok(f) => {
            emitter.info(format!(
                "Checkpoint: {} (wznawianie przerwanych transferów) / resume file.",
                checkpoint_path(&opts.local_root).display()
            ));
            Arc::new(Mutex::new(Some(f)))
        }
        Err(e) => {
            let msg = format!(
                "Nie można utworzyć pliku checkpointu {} - folder docelowy nie jest zapisywalny? / cannot create checkpoint: {e}",
                checkpoint_path(&opts.local_root).display()
            );
            emitter.error(msg.clone());
            emitter.fatal(msg);
            return;
        }
    };

    // root: wykrycie przed skanem
    let root = if opts.use_root {
        emitter.info("Sprawdzam root (jeśli na urządzeniu pojawi się okno Magisk/KernelSU - zezwól) / checking root...");
        let info = match &root_cache {
            Some(c) => root::cached_or_detect(c, &adb, &serial).await,
            None => root::detect(&adb, &serial).await,
        };
        if info.mode.is_root() {
            emitter.ok(format!("Tryb Root Aktywny - Pełny Dostęp ({}) / root mode active.", info.mode.label()));
        } else if info.su_present {
            emitter.warn(info.detail.clone());
        } else {
            emitter.info("Brak roota - kopiuję wszystko, do czego ma dostęp ADB / no root.");
        }
        info.mode
    } else {
        RootMode::None
    };
    emitter.item("root", root.is_root(), serde_json::to_string(&root).unwrap_or_default().trim_matches('"'));

    match build_file_list(&adb, &serial, &opts, root, &emitter, &cancel).await {
        Ok(plan) => {
            if cancel.is_cancelled() {
                emitter.warn("Anulowano przed rozpoczęciem kopiowania / cancelled before start.");
                emitter.finished(0, plan.skipped, 0, plan.failures, true);
                return;
            }
            pull_all(adb, serial, opts, plan, root, checkpoint, emitter, cancel).await;
        }
        Err(msg) => {
            emitter.error(format!("Krytyczny błąd budowy listy plików / list build error: {msg}"));
            emitter.fatal(msg);
        }
    }
}

/// Buduje pełną, odduplikowaną listę plików (i pustych folderów) z zaznaczonych gałęzi.
async fn build_file_list(
    adb: &Adb,
    serial: &str,
    opts: &PullOptions,
    root: RootMode,
    emitter: &Emitter,
    cancel: &CancelToken,
) -> std::result::Result<Plan, String> {
    let mut all_files: Vec<RemoteFile> = Vec::new();
    let mut seen_real: HashMap<String, String> = HashMap::new();
    let mut scan_failures: Vec<FailedItem> = Vec::new();
    let mut empty_dirs: Vec<(String, String)> = Vec::new();
    let empty_plan = |f: Vec<FailedItem>| Plan { files: vec![], skipped: 0, failures: f, empty_dirs: vec![] };

    for item in &opts.items {
        if cancel.is_cancelled() {
            return Ok(empty_plan(scan_failures));
        }
        let path = match normalize_remote(&item.path) {
            Ok(p) => p,
            Err(e) => {
                emitter.error(e.clone());
                scan_failures.push(FailedItem { path: item.path.clone(), error: e });
                continue;
            }
        };

        if item.is_file {
            let (size, mtime) = scan::remote_stat(adb, serial, root, &path).await.unwrap_or((0, None));
            all_files.push(RemoteFile {
                path: path.clone(),
                logical: path.clone(),
                root: path,
                size,
                mtime,
                rel: String::new(),
            });
            continue;
        }

        // dedup katalogów po realpath (symlinki/bind-mounty wskazujące to samo)
        if let Some(real) = scan::realpath(adb, serial, root, &path).await {
            if let Some(prev) = seen_real.get(&real) {
                emitter.warn(format!("Pomijam '{path}' - duplikat '{prev}' (te same dane) / skipping duplicate."));
                continue;
            }
            seen_real.insert(real, path.clone());
        }

        emitter.info(format!("Skanuję / scanning: {path} ..."));
        let res = scan_tree(adb, serial, &path, root, Some(emitter), cancel).await;
        if cancel.is_cancelled() {
            return Ok(empty_plan(scan_failures));
        }
        for n in res.notes.iter().take(50) {
            emitter.warn(format!("[skan] {n}"));
        }
        if res.notes.len() > 50 {
            emitter.warn(format!("[skan] ... +{} kolejnych uwag / more notes", res.notes.len() - 50));
        }
        for e in res.errors.iter().take(20) {
            emitter.error(format!("[skan] {e}"));
        }
        if res.errors.len() > 20 {
            emitter.error(format!("[skan] ... +{} kolejnych błędów / more scan errors", res.errors.len() - 20));
        }
        // błąd skanu = część danych NIE zostanie skopiowana -> musi być widoczny jako błąd
        for e in &res.errors {
            scan_failures.push(FailedItem {
                path: scan::scan_error_path(e).unwrap_or_else(|| path.clone()),
                error: format!("Skanowanie / scan: {e}"),
            });
        }
        let bytes: u64 = res.files.iter().map(|f| f.size).sum();
        emitter.info(format!(
            "Znaleziono {} plików ({}), {} pustych folderów w {path}.",
            res.files.len(),
            human_bytes(bytes),
            res.empty_dirs.len()
        ));
        for d in res.empty_dirs {
            empty_dirs.push((d, path.clone()));
        }
        for f in res.files {
            match (normalize_remote(&f.device), normalize_remote(&f.logical)) {
                (Ok(dp), Ok(lp)) => all_files.push(RemoteFile {
                    path: dp,
                    logical: lp,
                    root: path.clone(),
                    size: f.size,
                    mtime: f.mtime,
                    rel: String::new(),
                }),
                (Err(e), _) | (_, Err(e)) => scan_failures.push(FailedItem { path: f.logical, error: e }),
            }
        }
    }

    // dedup po ścieżce fizycznej (te same dane widziane dwa razy) - pierwszy root wygrywa
    let mut seen: HashSet<String> = HashSet::new();
    all_files.retain(|f| seen.insert(f.path.clone()));

    // docelowe ścieżki + rozwiązanie kolizji (dwie różne nazwy zdalne -> ta sama lokalna)
    let ntfs = effective_ntfs(opts.ntfs_safe);
    let mut taken: HashSet<String> = HashSet::new();
    let mut collisions = 0usize;
    let mut renamed = 0usize;
    for f in all_files.iter_mut() {
        let base = relative_local_path(&f.logical, &f.root, ntfs);
        if ntfs && base != relative_local_path(&f.logical, &f.root, false) {
            renamed += 1;
        }
        let mut rel = base.clone();
        let mut n = 2;
        while !taken.insert(collision_key(&rel, ntfs)) {
            rel = with_suffix(&base, n);
            n += 1;
        }
        if rel != base {
            collisions += 1;
            if collisions <= 20 {
                emitter.warn(format!("Kolizja nazw lokalnych: {} -> {rel} / local name collision", f.logical));
            }
        }
        f.rel = rel;
    }
    if collisions > 20 {
        emitter.warn(format!("Łącznie kolizji nazw / total name collisions: {collisions}"));
    }
    if renamed > 0 {
        emitter.info(format!(
            "NTFS-Friendly: {renamed} nazw dostosowano do Windows (np. ':' '?' '|' -> '_') / names adjusted for NTFS."
        ));
    }

    let local_root = PathBuf::from(&opts.local_root);
    let completed = if opts.resume { load_completed(&opts.local_root) } else { HashMap::new() };
    let skip_existing = opts.skip_existing;
    let files = std::mem::take(&mut all_files);

    // resume + pomijanie istniejących: zawsze z fizyczną weryfikacją pliku na dysku
    let (keep, by_checkpoint, by_disk, stale) = tokio::task::spawn_blocking(move || {
        let mut keep = Vec::with_capacity(files.len());
        let (mut by_checkpoint, mut by_disk, mut stale) = (0u64, 0u64, 0u64);
        for f in files {
            let lp = long_path(&local_root.join(&f.rel));
            let on_disk = std::fs::metadata(&lp).ok().filter(|m| m.is_file()).map(|m| m.len());
            let size_ok = |len: u64| if f.size > 0 { len == f.size } else { true };
            if let Some(cp_rel) = completed.get(&f.logical) {
                let cp_disk = std::fs::metadata(long_path(&local_root.join(cp_rel)))
                    .ok()
                    .filter(|m| m.is_file())
                    .map(|m| m.len());
                if cp_disk.map(size_ok).unwrap_or(false) {
                    by_checkpoint += 1;
                    continue;
                }
                // wpis w checkpoincie, ale pliku brak / inny rozmiar -> kopiuj ponownie
                stale += 1;
            }
            if skip_existing && f.size > 0 && on_disk == Some(f.size) {
                by_disk += 1;
                continue;
            }
            keep.push(f);
        }
        (keep, by_checkpoint, by_disk, stale)
    })
    .await
    .map_err(|e| e.to_string())?;
    all_files = keep;

    if by_checkpoint > 0 {
        emitter.info(format!(
            "Wznawianie: pomijam {by_checkpoint} plików już skopiowanych i obecnych na dysku (wg {CHECKPOINT_FILENAME}) / resuming."
        ));
    }
    if stale > 0 {
        emitter.warn(format!(
            "{stale} wpisów checkpointu nie ma na dysku (usunięte/uszkodzone) - zostaną skopiowane ponownie / stale checkpoint entries."
        ));
    }
    if by_disk > 0 {
        emitter.info(format!("Pomijam {by_disk} plików już obecnych na dysku (ten sam rozmiar) / already present."));
    }

    Ok(Plan { files: all_files, skipped: by_checkpoint + by_disk, failures: scan_failures, empty_dirs })
}

/// Kopiowanie równoległe: N wątków roboczych pobiera kolejne pliki z kolejki.
#[allow(clippy::too_many_arguments)]
async fn pull_all(
    adb: Arc<Adb>,
    serial: String,
    opts: PullOptions,
    plan: Plan,
    root: RootMode,
    checkpoint: Arc<Mutex<Option<std::fs::File>>>,
    emitter: Emitter,
    cancel: CancelToken,
) {
    let Plan { files, skipped, failures: mut scan_failures, empty_dirs } = plan;
    let total = files.len() as u64;
    let local_root = PathBuf::from(&opts.local_root);

    // puste foldery - odtwarzamy strukturę 1:1
    if !empty_dirs.is_empty() {
        let ntfs = effective_ntfs(opts.ntfs_safe);
        let lr = local_root.clone();
        let (made, errs) = tokio::task::spawn_blocking(move || {
            let mut made = 0usize;
            let mut errs = Vec::new();
            for (logical, root) in empty_dirs {
                let dir = long_path(&lr.join(relative_local_path(&logical, &root, ntfs)));
                match std::fs::create_dir_all(&dir) {
                    Ok(()) => made += 1,
                    Err(e) => errs.push(FailedItem {
                        path: logical,
                        error: format!("Nie można utworzyć pustego folderu / cannot create empty dir: {e}"),
                    }),
                }
            }
            (made, errs)
        })
        .await
        .unwrap_or_default();
        if made > 0 {
            emitter.ok(format!("Odtworzono {made} pustych folderów / empty folders recreated."));
        }
        for e in &errs {
            emitter.error(format!("{} :: {}", e.path, e.error));
        }
        scan_failures.extend(errs);
    }

    if total == 0 {
        if scan_failures.is_empty() {
            emitter.ok("Brak plików do skopiowania (lub wszystko już skopiowane) / nothing to copy.");
        } else {
            emitter.error(format!(
                "Brak plików do skopiowania, ale {} błędów skanowania - część danych NIE jest dostępna / scan errors.",
                scan_failures.len()
            ));
        }
        emitter.finished(0, skipped, 0, scan_failures, false);
        return;
    }

    let partial_dir = local_root.join(PARTIAL_DIRNAME);
    if let Err(e) = std::fs::create_dir_all(long_path(&partial_dir)) {
        let msg = format!("Nie można utworzyć folderu roboczego {} / cannot create work dir: {e}", partial_dir.display());
        emitter.error(msg.clone());
        emitter.fatal(msg);
        return;
    }

    let bytes_total: u64 = files.iter().map(|f| f.size).sum();
    let workers = opts.workers.clamp(1, 16);
    emitter.info(format!(
        "Łącznie do skopiowania: {total} plików, {}. Start ({workers} wątków) / starting.",
        human_bytes(bytes_total)
    ));

    let tracker = Tracker::new(total, bytes_total);
    let ticker = tracker.start(emitter.clone());

    let files = Arc::new(files);
    let next = Arc::new(AtomicUsize::new(0));
    let ok_count = Arc::new(AtomicU64::new(0));
    let disconnects = Arc::new(AtomicU32::new(0));
    let failures = Arc::new(Mutex::new(scan_failures));
    let done_list = Arc::new(Mutex::new(Vec::<Done>::new()));
    // foldery, w których adb pull dostał odmowę -> od razu ścieżka root (cat)
    let protected = Arc::new(StdMutex::new(HashSet::<String>::new()));
    let via_root = Arc::new(AtomicU64::new(0));

    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let files = files.clone();
        let next = next.clone();
        let adb = adb.clone();
        let serial = serial.clone();
        let emitter = emitter.clone();
        let cancel = cancel.clone();
        let local_root = local_root.clone();
        let partial_dir = partial_dir.clone();
        let checkpoint = checkpoint.clone();
        let ok_count = ok_count.clone();
        let disconnects = disconnects.clone();
        let failures = failures.clone();
        let done_list = done_list.clone();
        let protected = protected.clone();
        let via_root = via_root.clone();
        let tracker = tracker.clone();
        let timeout_s = opts.timeout_seconds.max(10);
        let preserve = opts.preserve_time;

        handles.push(tokio::spawn(async move {
            loop {
                if cancel.is_cancelled() {
                    break;
                }
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(f) = files.get(i) else { break };
                tracker.set_current(f.logical.clone());

                let tmp = partial_dir.join(format!("{i}.part"));
                let ctx = PullCtx {
                    adb: &adb,
                    serial: &serial,
                    local_root: &local_root,
                    tmp: &tmp,
                    timeout_s,
                    preserve_time: preserve,
                    cancel: &cancel,
                    tracker: &tracker,
                    root,
                    protected: &protected,
                };
                let res = pull_single(&ctx, f).await;
                if matches!(res, Err(ref e) if e == CANCELLED) {
                    break;
                }
                let d = tracker.item_done();
                match res {
                    Ok(PullOk { bytes, note, root_read }) => {
                        ok_count.fetch_add(1, Ordering::SeqCst);
                        disconnects.store(0, Ordering::SeqCst);
                        if root_read {
                            via_root.fetch_add(1, Ordering::SeqCst);
                        }
                        append_checkpoint(&checkpoint, &f.logical, &f.rel).await;
                        done_list.lock().await.push(Done { remote: f.logical.clone(), rel: f.rel.clone(), size: bytes });
                        emitter.ok(format!("[{d}/{total}] OK{}  {}", if root_read { " (root)" } else { "" }, f.logical));
                        if let Some(n) = note {
                            emitter.warn(format!("{} :: {n}", f.logical));
                        }
                    }
                    Err(err) => {
                        failures.lock().await.push(FailedItem { path: f.logical.clone(), error: err.clone() });
                        emitter.error(format!("[{d}/{total}] BŁĄD  {} :: {err}", f.logical));
                        if looks_like_disconnect(&err) {
                            let streak = disconnects.fetch_add(1, Ordering::SeqCst) + 1;
                            if streak >= DISCONNECT_STREAK_LIMIT && !cancel.is_cancelled() {
                                cancel.cancel();
                                emitter.error(
                                    "Powtarzające się błędy połączenia - prawdopodobnie odłączono kabel. \
                                     Przerywam (wznowisz później z checkpointu) / repeated disconnects, aborting.",
                                );
                            }
                        } else {
                            disconnects.store(0, Ordering::SeqCst);
                        }
                    }
                }
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }
    tracker.shutdown(ticker).await;

    // audyt końcowy: każdy zaliczony plik musi NADAL istnieć na dysku z właściwym rozmiarem
    let done = std::mem::take(&mut *done_list.lock().await);
    let audited = done.len();
    let root2 = local_root.clone();
    let missing: Vec<FailedItem> = tokio::task::spawn_blocking(move || {
        done.into_iter()
            .filter_map(|d| {
                let lp = long_path(&root2.join(&d.rel));
                match std::fs::metadata(&lp) {
                    Ok(m) if m.is_file() && m.len() == d.size => None,
                    Ok(m) if m.is_file() => Some(FailedItem {
                        path: d.remote,
                        error: format!("Audyt: rozmiar na dysku {} B, oczekiwano {} B / size mismatch", m.len(), d.size),
                    }),
                    _ => Some(FailedItem {
                        path: d.remote,
                        error: format!("Audyt: pliku nie ma na dysku ({}) / missing after copy", lp.display()),
                    }),
                }
            })
            .collect()
    })
    .await
    .unwrap_or_default();
    let mut ok = ok_count.load(Ordering::SeqCst);
    let n_root = via_root.load(Ordering::SeqCst);
    if n_root > 0 {
        emitter.info(format!("{n_root} plików odczytano z uprawnieniami root (su + exec-out) / read via root."));
    }
    if !missing.is_empty() {
        emitter.error(format!(
            "Audyt: {} z {audited} skopiowanych plików nie zgadza się z dyskiem - oznaczone jako błędy / audit failures.",
            missing.len()
        ));
        ok = ok.saturating_sub(missing.len() as u64);
        failures.lock().await.extend(missing);
    } else if audited > 0 {
        emitter.ok(format!("Audyt: {audited} plików potwierdzonych na dysku / verified on disk."));
    }

    // checkpoint: pełny zapis na dysk; plik ZOSTAJE (to on pozwala wznowić / zweryfikować transfer)
    if let Some(f) = checkpoint.lock().await.as_mut() {
        let _ = f.flush();
        let _ = f.sync_all();
    }
    // sprzątanie WYŁĄCZNIE folderu roboczego .adbst-partial (zostaje tylko gdy coś w nim utknęło)
    let _ = std::fs::remove_dir(long_path(&partial_dir));

    let was_cancelled = cancel.is_cancelled();
    if was_cancelled {
        emitter.warn(
            "Kopiowanie przerwane (anulowanie lub utrata połączenia). Uruchom ponownie, by wznowić / aborted, re-run to resume.",
        );
    }

    let fails = failures.lock().await.clone();
    if fails.is_empty() {
        emitter.ok(format!("Zakończono: OK {ok}, błędy 0, pominięte {skipped} / done."));
    } else {
        emitter.error(format!(
            "Zakończono Z BŁĘDAMI: OK {ok}, błędy {}, pominięte {skipped} - sprawdź listę błędów / done with errors.",
            fails.len()
        ));
    }
    emitter.finished(ok, skipped, total, fails, was_cancelled);
}

struct PullCtx<'a> {
    adb: &'a Adb,
    serial: &'a str,
    local_root: &'a Path,
    tmp: &'a Path,
    timeout_s: u64,
    preserve_time: bool,
    cancel: &'a CancelToken,
    tracker: &'a Tracker,
    root: RootMode,
    protected: &'a StdMutex<HashSet<String>>,
}

struct PullOk {
    /// rozmiar zapisanego pliku
    bytes: u64,
    /// ostrzeżenie (np. plik zmienił rozmiar od skanu)
    note: Option<String>,
    /// odczyt przez su (exec-out cat)
    root_read: bool,
}

/// Ścieżki, których adbd (użytkownik shell) nie zobaczy nawet przy rootowanym
/// telefonie - przy `su` od razu czytamy je przez root.
fn is_protected_path(p: &str) -> bool {
    (p.starts_with("/data/") && !p.starts_with("/data/local/tmp/"))
        || p.contains("/Android/data/")
        || p.contains("/Android/obb/")
        || ["/data_mirror/", "/mnt/", "/persist/", "/efs/", "/cache/", "/metadata/", "/proc/", "/sys/"]
            .iter()
            .any(|x| p.starts_with(x))
}

/// Błąd `adb pull`, który przy roocie warto ponowić przez `su`.
fn root_retryable(err: &str) -> bool {
    let l = err.to_lowercase();
    l.contains("permission denied")
        || l.contains("operation not permitted")
        || l.contains("does not exist")
        || l.contains("no such file")
        || l.contains("failed to stat")
}

fn parent_dir(p: &str) -> &str {
    p.rsplit_once('/').map(|(a, _)| a).unwrap_or("/")
}

fn re_pulled() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(\d+) files? pulled").expect("regex"))
}
fn re_skipped() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(\d+) files? skipped|pulled,\s*(\d+) skipped").expect("regex"))
}
fn re_bytes() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\((\d+) bytes in").expect("regex"))
}

/// Analiza wyjścia `adb pull`. Kod 0 to za mało - adb potrafi zgłosić
/// "Permission denied" / "0 files pulled" / "1 skipped" i zwrócić 0.
/// Zwraca liczbę bajtów zgłoszoną przez adb (jeśli podał).
pub fn check_pull_output(out: &Output, remote: &str, tmp: &str) -> std::result::Result<Option<u64>, String> {
    let raw = format!("{}\n{}", out.stdout.trim(), out.stderr.trim());
    // nazwy plików mogą zawierać "error"/"failed" - usuń je przed szukaniem słów kluczowych
    let scrubbed = raw.replace(remote, "<remote>").replace(tmp, "<local>");
    let low = scrubbed.to_lowercase();
    let first_line = || {
        raw.lines()
            .map(str::trim)
            .find(|l| {
                let ll = l.to_lowercase();
                PULL_ERROR_PATTERNS.iter().any(|p| ll.contains(p))
            })
            .or_else(|| raw.lines().map(str::trim).rfind(|l| !l.is_empty()))
            .unwrap_or("")
            .to_string()
    };

    if out.status != 0 {
        let detail = first_line();
        return Err(if detail.is_empty() {
            format!("adb pull zakończył się kodem {} / exit code {}", out.status, out.status)
        } else {
            detail
        });
    }
    if let Some(p) = PULL_ERROR_PATTERNS.iter().find(|p| low.contains(*p)) {
        return Err(format!("adb zgłosił błąd mimo kodu 0 ('{p}'): {}", first_line()));
    }
    if let Some(c) = re_pulled().captures(&low) {
        if c.get(1).and_then(|m| m.as_str().parse::<u64>().ok()) == Some(0) {
            return Err(format!("adb: 0 plików skopiowano / 0 files pulled: {}", first_line()));
        }
    }
    if let Some(c) = re_skipped().captures(&low) {
        let n = c.get(1).or_else(|| c.get(2)).and_then(|m| m.as_str().parse::<u64>().ok()).unwrap_or(0);
        if n > 0 {
            return Err(format!("adb pominął plik ({n} skipped): {}", first_line()));
        }
    }
    Ok(re_bytes()
        .captures(&low)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok()))
}

/// Kopiuje jeden plik. Strategia:
///  - bez roota / adbd root: `adb pull`,
///  - su: ścieżki chronione od razu przez `exec-out su -c cat`; pozostałe
///    `adb pull`, a przy odmowie - ponownie przez root (i zapamiętanie folderu).
///
/// Potem: weryfikacja rozmiaru -> przeniesienie na docelową ścieżkę.
async fn pull_single(ctx: &PullCtx<'_>, f: &RemoteFile) -> std::result::Result<PullOk, String> {
    let su = ctx.root.needs_su_read();
    let parent = parent_dir(&f.path).to_string();
    let known_protected = su
        && (is_protected_path(&f.path) || ctx.protected.lock().map(|s| s.contains(&parent)).unwrap_or(false));

    let (len, note, root_read) = if known_protected {
        let len = root_cat_to_tmp(ctx, f, false).await?;
        (len, None, true)
    } else {
        match adb_pull_to_tmp(ctx, f).await {
            Ok((len, note)) => (len, note, false),
            Err(e) if su && e != CANCELLED && root_retryable(&e) => {
                if let Ok(mut s) = ctx.protected.lock() {
                    s.insert(parent);
                }
                let len = root_cat_to_tmp(ctx, f, true).await.map_err(|e2| format!("{e} | root: {e2}"))?;
                (len, None, true)
            }
            Err(e) => return Err(e),
        }
    };
    // data modyfikacji przy odczycie przez root (adb pull -a robi to sam)
    if root_read && ctx.preserve_time {
        if let Some(m) = f.mtime.filter(|m| *m > 0) {
            let t = std::time::UNIX_EPOCH + Duration::from_secs(m as u64);
            if let Ok(file) = std::fs::OpenOptions::new().write(true).open(long_path(ctx.tmp)) {
                let _ = file.set_modified(t);
            }
        }
    }
    move_into_place(ctx, f, len).await.map(|bytes| PullOk { bytes, note, root_read })
}

/// `adb pull <zdalny> <tmp>` + analiza wyjścia + weryfikacja. Zwraca (rozmiar, uwaga).
async fn adb_pull_to_tmp(ctx: &PullCtx<'_>, f: &RemoteFile) -> std::result::Result<(u64, Option<String>), String> {
    let tracker = ctx.tracker;
    let tmp = ctx.tmp;
    // stary .part (np. po awarii) nie może udawać nowego pliku
    let _ = tokio::fs::remove_file(long_path(tmp)).await;

    let tmp_str = tmp.to_string_lossy().to_string();
    let mut args: Vec<&str> = vec!["pull"];
    if ctx.preserve_time {
        args.push("-a");
    }
    args.push(&f.path);
    args.push(&tmp_str);

    let fut = ctx.adb.run_c(Some(ctx.serial), &args, ctx.timeout_s, ctx.cancel);
    tokio::pin!(fut);

    // podgląd postępu w obrębie pliku (duże filmy itp.)
    let mut counted: u64 = 0;
    let res = loop {
        tokio::select! {
            r = &mut fut => break r,
            _ = tokio::time::sleep(Duration::from_millis(300)) => {
                if f.size > 0 {
                    if let Ok(m) = tokio::fs::metadata(long_path(tmp)).await {
                        let s = m.len().min(f.size);
                        if s > counted {
                            tracker.add_bytes(s - counted);
                            counted = s;
                        }
                    }
                }
            }
        }
    };
    // dobij licznik bajtów do pełnego rozmiaru pliku (OK lub błąd - "przetworzony")
    tracker.add_bytes(f.size.saturating_sub(counted));

    let out = match res {
        Ok(out) => out,
        Err(AdbError::Cancelled) => {
            let _ = tokio::fs::remove_file(long_path(tmp)).await;
            return Err(CANCELLED.to_string());
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(long_path(tmp)).await;
            return Err(e.to_string());
        }
    };

    let fail = |e: String| async move {
        let _ = tokio::fs::remove_file(long_path(tmp)).await;
        Err::<(u64, Option<String>), String>(e)
    };

    // 1. wyjście adb
    let reported = match check_pull_output(&out, &f.path, &tmp_str) {
        Ok(r) => r,
        Err(e) => return fail(e).await,
    };

    // 2. plik fizycznie na dysku + rozmiar
    let len = match tokio::fs::metadata(long_path(tmp)).await {
        Ok(m) if m.is_file() => m.len(),
        Ok(_) => return fail("adb utworzył folder zamiast pliku / destination is not a file".into()).await,
        Err(_) => {
            return fail(format!(
                "adb zwrócił sukces, ale plik nie pojawił się na dysku / reported success but no file written ({})",
                out.stdout.trim()
            ))
            .await
        }
    };
    let mut note = None;
    match reported {
        Some(r) if r != len => {
            return fail(format!("Niekompletny plik: adb zgłosił {r} B, na dysku {len} B / incomplete")).await;
        }
        Some(r) if f.size > 0 && r != f.size => {
            // plik zmienił się na urządzeniu między skanem a kopiowaniem - kopia zgodna z tym, co wysłał adb
            note = Some(format!("rozmiar zmienił się od skanu ({} -> {r} B) / file changed on device", f.size));
        }
        None if f.size > 0 && len != f.size => {
            return fail(format!("Rozmiar na dysku {len} B, na urządzeniu {} B / size mismatch", f.size)).await;
        }
        _ => {}
    }
    Ok((len, note))
}

/// Odczyt przez root: `adb exec-out su -c 'cat <plik>'` strumieniem prosto do
/// pliku tymczasowego. Weryfikacja: liczba bajtów = rozmiar z root-stat.
/// Przy niezgodności (np. odmowa FUSE) - drugie podejście przez /data/media.
/// `counted` = bajty tego pliku już doliczone do paska (po nieudanym adb pull).
async fn root_cat_to_tmp(ctx: &PullCtx<'_>, f: &RemoteFile, counted: bool) -> std::result::Result<u64, String> {
    let mut expected = f.size;
    if expected == 0 {
        // rozmiar nieznany ze skanu albo plik pusty - zapytaj roota
        match scan::remote_stat(ctx.adb, ctx.serial, ctx.root, &f.path).await {
            Some((s, _)) => expected = s,
            None => {
                let alias = scan::data_media_alias(&f.path);
                match alias {
                    Some(a) => {
                        expected = scan::remote_stat(ctx.adb, ctx.serial, ctx.root, &a)
                            .await
                            .map(|x| x.0)
                            .ok_or_else(|| format!("Root: plik nieczytelny / unreadable via su: {}", f.path))?
                    }
                    None => return Err(format!("Root: plik nieczytelny / unreadable via su: {}", f.path)),
                }
            }
        }
    }
    let mut candidates = vec![f.path.clone()];
    if let Some(a) = scan::data_media_alias(&f.path) {
        candidates.push(a);
    }
    let mut last = String::new();
    let mut counted_total = if counted { u64::MAX } else { 0 };
    for src in candidates {
        let _ = tokio::fs::remove_file(long_path(ctx.tmp)).await;
        if expected == 0 {
            // plik pusty: wystarczy potwierdzić, że root go widzi
            let cmd = ctx.root.wrap(&format!("[ -f {0} ] && [ -r {0} ] && echo __OK__", shell_quote(&src)));
            let ok = ctx
                .adb
                .shell(Some(ctx.serial), &cmd, 30)
                .await
                .map(|o| o.stdout.contains("__OK__"))
                .unwrap_or(false);
            if ok {
                tokio::fs::write(long_path(ctx.tmp), b"").await.map_err(|e| format!("zapis / write: {e}"))?;
                return Ok(0);
            }
            last = format!("Root: brak dostępu do pustego pliku {src}");
            continue;
        }
        match exec_out_cat(ctx, &src, expected, &mut counted_total).await {
            Ok(n) if n == expected => return Ok(n),
            Ok(n) => last = format!("Root: odczytano {n} B z {expected} B ({src}) / incomplete read"),
            Err(e) if e == CANCELLED => {
                let _ = tokio::fs::remove_file(long_path(ctx.tmp)).await;
                return Err(e);
            }
            Err(e) => last = e,
        }
    }
    if counted_total != u64::MAX {
        ctx.tracker.add_bytes(f.size.saturating_sub(counted_total));
    }
    let _ = tokio::fs::remove_file(long_path(ctx.tmp)).await;
    Err(last)
}

/// Strumień `adb exec-out <su -c 'cat plik 2>/dev/null'>` -> plik tymczasowy.
async fn exec_out_cat(
    ctx: &PullCtx<'_>,
    src: &str,
    expected: u64,
    counted_total: &mut u64,
) -> std::result::Result<u64, String> {
    use std::process::Stdio;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let cmd = ctx.root.wrap(&format!("cat {} 2>/dev/null", shell_quote(src)));
    let mut c = ctx.adb.command(Some(ctx.serial), &["exec-out", &cmd]);
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    let mut child = c.spawn().map_err(|e| format!("adb exec-out: {e}"))?;
    let mut out = child.stdout.take().ok_or("adb exec-out: brak stdout")?;
    let mut file = tokio::fs::File::create(long_path(ctx.tmp)).await.map_err(|e| format!("zapis / write: {e}"))?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut n_total = 0u64;
    let deadline = tokio::time::sleep(Duration::from_secs(ctx.timeout_s.max(10)));
    tokio::pin!(deadline);
    loop {
        let n = tokio::select! {
            r = out.read(&mut buf) => r.map_err(|e| format!("adb exec-out: {e}"))?,
            _ = ctx.cancel.cancelled() => {
                let _ = child.kill().await;
                return Err(CANCELLED.into());
            }
            _ = &mut deadline => {
                let _ = child.kill().await;
                return Err(format!("Root: przekroczono limit {} s / timed out", ctx.timeout_s));
            }
        };
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).await.map_err(|e| format!("zapis / write: {e}"))?;
        n_total += n as u64;
        // postęp tylko do oczekiwanego rozmiaru (bez podwójnego liczenia przy drugim podejściu)
        let cap = expected.min(n_total);
        if cap > *counted_total {
            ctx.tracker.add_bytes(cap - *counted_total);
            *counted_total = cap;
        }
    }
    file.flush().await.map_err(|e| format!("zapis / write: {e}"))?;
    file.sync_all().await.map_err(|e| format!("zapis / write: {e}"))?;
    let _ = child.wait().await;
    Ok(n_total)
}

/// Przeniesienie zweryfikowanego pliku tymczasowego na docelową ścieżkę
/// (długie ścieżki obsługuje Rust, nie adb) + kontrola po zapisie.
async fn move_into_place(ctx: &PullCtx<'_>, f: &RemoteFile, len: u64) -> std::result::Result<u64, String> {
    let tmp = ctx.tmp;
    let final_path = long_path(&ctx.local_root.join(&f.rel));
    let fail = |e: String| async move {
        let _ = tokio::fs::remove_file(long_path(tmp)).await;
        Err::<u64, String>(e)
    };
    match tokio::fs::metadata(long_path(tmp)).await {
        Ok(m) if m.is_file() && m.len() == len => {}
        Ok(m) => return fail(format!("Plik tymczasowy ma {} B zamiast {len} B / temp size mismatch", m.len())).await,
        Err(e) => return fail(format!("Brak pliku tymczasowego / temp file missing: {e}")).await,
    }
    if let Some(parent) = final_path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return fail(format!("Błąd tworzenia folderu lokalnego / mkdir {}: {e}", parent.display())).await;
        }
    }
    if let Err(e) = tokio::fs::rename(long_path(tmp), &final_path).await {
        // inny wolumen (np. dowiązany podfolder) -> kopia + usunięcie
        match tokio::fs::copy(long_path(tmp), &final_path).await {
            Ok(_) => {
                let _ = tokio::fs::remove_file(long_path(tmp)).await;
            }
            Err(e2) => {
                return fail(format!("Nie można zapisać {} / cannot write: {e} / {e2}", final_path.display())).await;
            }
        }
    }
    match tokio::fs::metadata(&final_path).await {
        Ok(m) if m.is_file() && m.len() == len => Ok(len),
        Ok(m) => Err(format!("Po zapisie rozmiar {} B zamiast {len} B / size changed after move", m.len())),
        Err(e) => Err(format!("Plik zniknął po zapisie / missing after move: {e}")),
    }
}

// --------------------------------------------------------------------------- //
// Pomocnicze - ścieżki, checkpoint, find, realpath
// --------------------------------------------------------------------------- //

/// Lokalna ścieżka względna dla pliku. Port `_relative_local_path`.
fn relative_local_path(remote_path: &str, root_path: &str, ntfs: bool) -> String {
    let root_norm = root_path.trim_end_matches('/');
    let remote_norm = remote_path.trim_end_matches('/');

    if remote_norm == root_norm {
        let base = basename(remote_norm);
        return safe_component(if base.is_empty() { "plik" } else { base }, ntfs);
    }

    let root_label_raw = {
        let b = basename(root_norm);
        if b.is_empty() {
            let stripped = root_norm.trim_matches('/').replace('/', "_");
            if stripped.is_empty() {
                "root".to_string()
            } else {
                stripped
            }
        } else {
            b.to_string()
        }
    };
    let root_label = safe_component(&root_label_raw, ntfs);

    let rel_posix = posix_relpath(remote_path, root_norm);
    let parts: Vec<String> = rel_posix
        .split('/')
        .filter(|p| !p.is_empty() && *p != "." && *p != "..")
        .map(|p| safe_component(p, ntfs))
        .collect();

    let mut pb = PathBuf::from(root_label);
    for p in parts {
        pb.push(p);
    }
    pb.to_string_lossy().to_string()
}

/// Port `_posix_relpath`.
fn posix_relpath(path: &str, root: &str) -> String {
    let path = path.trim_matches('/');
    let root = root.trim_matches('/');
    if path == root {
        return basename(path).to_string();
    }
    if root.is_empty() {
        return path.to_string();
    }
    let prefix = format!("{root}/");
    path.strip_prefix(&prefix).unwrap_or(path).to_string()
}

fn basename(p: &str) -> &str {
    p.trim_end_matches('/').rsplit('/').next().unwrap_or(p)
}

/// Skan dla modułu wysyłania (push): (rozmiar, ścieżka) bez roota.
pub async fn find_files_sized(adb: &Adb, serial: &str, root_path: &str) -> scan::ScanResult {
    scan_tree(adb, serial, root_path, RootMode::None, None, &CancelToken::new()).await
}

pub fn human_bytes(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

// --- checkpoint (.adb_pull_progress.jsonl) --------------------------------- //

fn checkpoint_path(local_root: &str) -> PathBuf {
    PathBuf::from(local_root).join(CHECKPOINT_FILENAME)
}

/// Wczytuje skopiowane pliki z checkpointu: zdalna ścieżka -> lokalna względna.
fn load_completed(local_root: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Ok(content) = std::fs::read_to_string(long_path(&checkpoint_path(local_root))) {
        for line in content.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                if let (Some(rp), Some(lp)) =
                    (v.get("remote").and_then(|x| x.as_str()), v.get("local").and_then(|x| x.as_str()))
                {
                    map.insert(rp.to_string(), lp.to_string());
                }
            }
        }
    }
    map
}

/// Otwiera (tworzy) checkpoint w trybie dopisywania. Błąd = folder docelowy niezapisywalny.
fn open_checkpoint(local_root: &str) -> std::io::Result<std::fs::File> {
    std::fs::create_dir_all(long_path(Path::new(local_root)))?;
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(long_path(&checkpoint_path(local_root)))?;
    // od razu fizycznie na dysku (także pusty) - widać, że zadanie wystartowało
    f.sync_all()?;
    Ok(f)
}

/// Dopisuje wpis do checkpointu i wymusza fsync (odporność na nagłe odcięcie).
async fn append_checkpoint(file: &Arc<Mutex<Option<std::fs::File>>>, remote: &str, local: &str) {
    let rec = serde_json::json!({
        "remote": remote,
        "local": local,
        "ts": chrono::Utc::now().to_rfc3339(),
    });
    let line = format!("{rec}\n");
    let mut guard = file.lock().await;
    if let Some(f) = guard.as_mut() {
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
        let _ = f.sync_data();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adb::scan::scan_error_path;

    fn out(status: i32, stdout: &str, stderr: &str) -> Output {
        Output { status, stdout: stdout.into(), stderr: stderr.into() }
    }

    #[test]
    fn rel_path_dir() {
        let r = relative_local_path("/sdcard/DCIM/Camera/a.jpg", "/sdcard/DCIM", true);
        assert_eq!(PathBuf::from(r), PathBuf::from("DCIM").join("Camera").join("a.jpg"));
    }

    #[test]
    fn rel_path_spaces() {
        let r = relative_local_path(
            "/sdcard/Android/media/com.whatsapp/WhatsApp/Media/WhatsApp Video/VID 1.mp4",
            "/sdcard/Android/media",
            true,
        );
        assert_eq!(
            PathBuf::from(r),
            ["media", "com.whatsapp", "WhatsApp", "Media", "WhatsApp Video", "VID 1.mp4"].iter().collect::<PathBuf>()
        );
    }

    #[test]
    fn rel_path_single_file() {
        assert_eq!(relative_local_path("/sdcard/x:y.txt", "/sdcard/x:y.txt", true), "x_y.txt");
        if !cfg!(windows) {
            // bez trybu NTFS na Linuksie nazwa zostaje 1:1
            assert_eq!(relative_local_path("/sdcard/x:y.txt", "/sdcard/x:y.txt", false), "x:y.txt");
        }
    }

    #[test]
    fn ntfs_names() {
        assert_eq!(sanitize_for_ntfs("a:b*c?d\"e<f>g|h\\i"), "a_b_c_d_e_f_g_h_i");
        assert_eq!(sanitize_for_ntfs("Folder."), "Folder_");
        assert_eq!(sanitize_for_ntfs("Nazwa  "), "Nazwa_");
        assert_eq!(sanitize_for_ntfs("CON"), "_CON");
        assert_eq!(sanitize_for_ntfs("nul.txt"), "_nul.txt");
        assert_eq!(sanitize_for_ntfs("console.log"), "console.log");
        assert_eq!(sanitize_for_ntfs("WhatsApp Video"), "WhatsApp Video");
        assert_eq!(sanitize_for_ntfs("tab\there"), "tab_here");
        assert_eq!(collision_key("A/B.jpg", true), "a/b.jpg");
    }

    #[test]
    fn remote_paths_validated() {
        assert_eq!(normalize_remote("/sdcard//DCIM/./Camera/").unwrap(), "/sdcard/DCIM/Camera");
        assert_eq!(normalize_remote("/sdcard/WhatsApp Video\r").unwrap(), "/sdcard/WhatsApp Video");
        assert!(normalize_remote("mtp:04e8:6860/Internal storage/DCIM").is_err());
        assert!(normalize_remote("MTP:/x").is_err());
        assert!(normalize_remote("C:\\Users\\x").is_err());
        assert!(normalize_remote("sdcard/DCIM").is_err());
        assert!(normalize_remote("/sdcard/../data").is_err());
    }

    #[test]
    fn pull_output_checks() {
        let r = "/sdcard/a b.mp4";
        let t = "/tmp/x/0.part";
        let ok = out(0, "/sdcard/a b.mp4: 1 file pulled, 0 skipped. 31.2 MB/s (1048576 bytes in 0.032s)\n", "");
        assert_eq!(check_pull_output(&ok, r, t).unwrap(), Some(1048576));
        assert_eq!(check_pull_output(&out(0, "1234 KB/s (5 bytes in 0.001s)", ""), r, t).unwrap(), Some(5));
        assert!(check_pull_output(&out(0, "", "adb: error: failed to stat remote object '/sdcard/a b.mp4': Permission denied"), r, t).is_err());
        assert!(check_pull_output(&out(0, "/sdcard/a b.mp4: 0 files pulled, 0 skipped.", ""), r, t).is_err());
        assert!(check_pull_output(&out(0, "/sdcard/a b.mp4: 0 files pulled, 1 skipped.", ""), r, t).is_err());
        assert!(check_pull_output(&out(1, "", "adb: error: remote object does not exist"), r, t).is_err());
        let r2 = "/sdcard/error failed.log";
        assert!(check_pull_output(&out(0, "/sdcard/error failed.log: 1 file pulled, 0 skipped. (10 bytes in 0.0s)", ""), r2, t).is_ok());
    }

    #[test]
    fn suffix_and_scan_path() {
        assert_eq!(PathBuf::from(with_suffix("a/b/x.jpg", 2)), PathBuf::from("a/b/x (2).jpg"));
        assert_eq!(with_suffix("noext", 3), "noext (3)");
        assert_eq!(
            scan_error_path("find: '/sdcard/Android/media/com.whatsapp/WhatsApp Video': Permission denied").as_deref(),
            Some("/sdcard/Android/media/com.whatsapp/WhatsApp Video")
        );
    }

    #[test]
    fn human() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
    }

    #[test]
    fn disconnect() {
        assert!(looks_like_disconnect("error: device 'X' not found"));
        assert!(!looks_like_disconnect("Permission denied"));
    }

    // ------------------------------------------------------------------ //
    // E2E na Linuksie z atrapą `adb` (skrypt sh): `find -exec ... +` ZABRONIONY
    // (symulacja "Argument list too long"), opcjonalnie brak xargs, ciche błędy
    // adb, ukryte pliki, puste foldery, dowiązania (pętla, duplikat, cel
    // zewnętrzny, zerwane), Android/data (adb pull: odmowa) i root (su).
    // ------------------------------------------------------------------ //

    #[cfg(unix)]
    struct Run {
        base: PathBuf,
        local: PathBuf,
        events: Vec<serde_json::Value>,
        fin: serde_json::Value,
        failed: Vec<String>,
    }

    #[cfg(unix)]
    async fn scenario(tag: &str, block_xargs: bool, rooted: bool) -> Run {
        use std::os::unix::fs::{symlink, PermissionsExt};
        use std::sync::Mutex as SM;

        let base = std::env::temp_dir().join(format!("adbst-pull-e2e-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let android = base.join("dev/sdcard/Android");
        let dev = android.join("media/com.whatsapp/WhatsApp/Media");
        let video = dev.join("WhatsApp Video");
        std::fs::create_dir_all(video.join("Sent")).unwrap();
        std::fs::write(video.join("VID 2024 01.mp4"), vec![7u8; 4096]).unwrap();
        std::fs::write(video.join("Sent/it's (1).mp4"), b"abc").unwrap();
        std::fs::write(video.join("Sent/what?: now|.jpg"), b"ntfs").unwrap();
        let many = dev.join("WhatsApp Images/Private Folder With Spaces");
        std::fs::create_dir_all(&many).unwrap();
        for i in 0..1200 {
            std::fs::write(many.join(format!("IMG-20240101-WA{i:04} long descriptive name.jpg")), format!("{i}")).unwrap();
        }
        // ukryte pliki i puste foldery
        std::fs::create_dir_all(dev.join(".Statuses")).unwrap();
        std::fs::write(dev.join(".Statuses/.nomedia"), b"").unwrap();
        std::fs::write(dev.join(".Statuses/s1.jpg"), b"status").unwrap();
        std::fs::create_dir_all(dev.join("EmptyDir")).unwrap();
        std::fs::create_dir_all(dev.join(".hidden_empty")).unwrap();
        // ciche błędy adb
        for n in ["SILENT zero.jpg", "DENY.jpg", "NOFILE.jpg"] {
            std::fs::write(dev.join(n), b"x").unwrap();
        }
        std::fs::write(dev.join("SHORT.jpg"), b"0123456789").unwrap();
        // chronione (Android/data) z datą modyfikacji
        let secret = android.join("data/com.app/files/secret.db");
        std::fs::create_dir_all(secret.parent().unwrap()).unwrap();
        std::fs::write(&secret, vec![9u8; 777]).unwrap();
        let t0 = std::time::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        std::fs::OpenOptions::new().write(true).open(&secret).unwrap().set_modified(t0).unwrap();
        // dowiązania
        std::fs::create_dir_all(base.join("dev/external")).unwrap();
        std::fs::write(base.join("dev/external/ext.txt"), b"ext").unwrap();
        std::fs::create_dir_all(base.join("dev/external2")).unwrap();
        std::fs::write(base.join("dev/external2/single.bin"), b"single").unwrap();
        symlink("../../../../../..", dev.join("loop")).unwrap(); // -> dev/sdcard (przodek)
        symlink("WhatsApp Video", dev.join("dup_link")).unwrap();
        symlink(base.join("dev/external"), dev.join("ext_link")).unwrap();
        symlink(base.join("dev/external2/single.bin"), dev.join("file_link")).unwrap();
        symlink("/nonexistent/target", dev.join("broken")).unwrap();

        // atrapa su
        let bin = base.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("su"), "#!/bin/sh\nif [ \"$1\" = \"-c\" ]; then shift; exec sh -c \"$1\"; fi\nif [ \"$1\" = \"0\" ]; then shift; exec \"$@\"; fi\nexec sh\n").unwrap();
        std::fs::set_permissions(bin.join("su"), std::fs::Permissions::from_mode(0o755)).unwrap();

        let block = if block_xargs {
            r#"case "$1" in *xargs*) echo "/system/bin/sh: xargs: not found" >&2; exit 127 ;; esac"#
        } else {
            ""
        };
        let no_root = if rooted {
            ""
        } else {
            r#"case "$1" in "command -v su"*) exit 1 ;; "su "*) echo "su: Permission denied" >&2; exit 1 ;; esac"#
        };
        let fake = base.join("adb");
        std::fs::write(
            &fake,
            format!(
                r#"#!/bin/sh
export PATH="{bin}:$PATH"
[ "$1" = "-s" ] && shift 2
cmd="$1"; shift
case "$cmd" in
  shell)
    case "$1" in
      *"-exec"*"+"*) echo "find: exec stat: Argument list too long" >&2; exit 1 ;;
      id) echo "uid=2000(shell) gid=2000(shell) context=u:r:shell:s0"; exit 0 ;;
    esac
    {block}
    {no_root}
    exec sh -c "$1" ;;
  exec-out)
    {no_root}
    exec sh -c "$1" ;;
  pull)
    [ "$1" = "-a" ] && shift
    r="$1"; l="$2"
    case "$r" in
      */Android/data/*) echo "adb: error: failed to stat remote object '$r': Permission denied" >&2; exit 1 ;;
      *SILENT*) echo "$r: 0 files pulled, 0 skipped."; exit 0 ;;
      *DENY*) echo "adb: error: failed to copy '$r' to '$l': remote Permission denied" >&2; exit 0 ;;
      *NOFILE*) echo "$r: 1 file pulled, 0 skipped. (1 bytes in 0.001s)"; exit 0 ;;
      *SHORT*) head -c 4 "$r" > "$l"; echo "$r: 1 file pulled, 0 skipped. (10 bytes in 0.001s)"; exit 0 ;;
    esac
    cp -p "$r" "$l" || exit 1
    n=$(wc -c < "$l" | tr -d ' ')
    echo "$r: 1 file pulled, 0 skipped. 1.0 MB/s ($n bytes in 0.001s)"
    ;;
esac
"#,
                bin = bin.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let events: Arc<SM<Vec<String>>> = Arc::new(SM::new(Vec::new()));
        let ev = events.clone();
        let ch = tauri::ipc::Channel::new(move |body| {
            if let tauri::ipc::InvokeResponseBody::Json(s) = body {
                ev.lock().unwrap().push(s);
            }
            Ok(())
        });
        let adb = Arc::new(Adb { adb: fake, fastboot: None });
        let root = android.to_string_lossy().to_string();
        let local = base.join("out");
        std::fs::create_dir_all(&local).unwrap();
        // stary checkpoint: plik "skopiowany", ale usunięty z dysku -> musi być skopiowany ponownie
        let stale = format!("{root}/media/com.whatsapp/WhatsApp/Media/WhatsApp Video/VID 2024 01.mp4");
        std::fs::write(
            local.join(CHECKPOINT_FILENAME),
            format!("{}\n", serde_json::json!({"remote": stale, "local": "Android/x.mp4"})),
        )
        .unwrap();

        let opts = PullOptions {
            items: vec![
                SelectedItem { path: root.clone(), is_file: false },
                SelectedItem { path: "mtp:04e8:6860/Phone/DCIM".into(), is_file: false },
            ],
            local_root: local.to_string_lossy().to_string(),
            workers: 4,
            timeout_seconds: 30,
            resume: true,
            skip_existing: true,
            preserve_time: true,
            ntfs_safe: true,
            use_root: true,
        };
        run_pull(adb, "SERIAL".into(), opts, None, Emitter::new(ch), CancelToken::new()).await;

        let events: Vec<serde_json::Value> =
            events.lock().unwrap().iter().filter_map(|s| serde_json::from_str(s).ok()).collect();
        let fin = events.iter().find(|v| v["kind"] == "finished").expect("finished").clone();
        let failed = fin["failures"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap().to_string()).collect();
        Run { base, local, events, fin, failed }
    }

    #[cfg(unix)]
    fn logs(r: &Run) -> String {
        r.events
            .iter()
            .filter(|v| v["kind"] == "log")
            .map(|v| v["message"].as_str().unwrap_or("").to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[cfg(unix)]
    fn common_checks(r: &Run, tag: &str) {
        let a = r.local.join("Android");
        let media = a.join("media/com.whatsapp/WhatsApp/Media");
        assert_eq!(std::fs::metadata(media.join("WhatsApp Video/VID 2024 01.mp4")).unwrap().len(), 4096, "[{tag}]");
        assert_eq!(std::fs::read(media.join("WhatsApp Video/Sent/it's (1).mp4")).unwrap(), b"abc");
        assert_eq!(std::fs::read(media.join("WhatsApp Video/Sent/what__ now_.jpg")).unwrap(), b"ntfs");
        let n = std::fs::read_dir(media.join("WhatsApp Images/Private Folder With Spaces")).unwrap().count();
        assert_eq!(n, 1200, "[{tag}] {n}/1200");
        // ukryte + puste
        assert!(media.join(".Statuses/.nomedia").is_file(), "[{tag}] .nomedia");
        assert_eq!(std::fs::read(media.join(".Statuses/s1.jpg")).unwrap(), b"status");
        assert!(media.join("EmptyDir").is_dir(), "[{tag}] pusty folder");
        assert!(media.join(".hidden_empty").is_dir(), "[{tag}] ukryty pusty folder");
        // dowiązania: cel zewnętrzny skopiowany pod nazwą dowiązania, pętla/duplikat pominięte
        assert_eq!(std::fs::read(media.join("ext_link/ext.txt")).unwrap(), b"ext");
        assert_eq!(std::fs::read(media.join("file_link")).unwrap(), b"single");
        assert!(!media.join("loop").exists(), "[{tag}] pętla nie może być kopiowana");
        assert!(!media.join("dup_link").exists(), "[{tag}] duplikat");
        let l = logs(r);
        assert!(l.contains("Pętla dowiązań"), "[{tag}] brak logu pętli");
        assert!(l.contains("Zerwane dowiązanie"), "[{tag}] brak logu zerwanego dowiązania");
        assert!(r.failed.iter().any(|p| p.starts_with("mtp:")), "[{tag}] ścieżka MTP");
        // NIGDY cichej straty: każdy "problematyczny" plik jest albo poprawnie na dysku, albo na liście błędów
        for (bad, content) in [("SILENT zero.jpg", &b"x"[..]), ("DENY.jpg", b"x"), ("NOFILE.jpg", b"x"), ("SHORT.jpg", b"0123456789")] {
            let on_disk = std::fs::read(media.join(bad)).ok();
            let listed = r.failed.iter().any(|p| p.contains(bad));
            assert!(listed != on_disk.is_some(), "[{tag}] {bad}: na dysku={on_disk:?}, błąd={listed}");
            if let Some(c) = on_disk {
                assert_eq!(c, content, "[{tag}] {bad} uszkodzony");
            }
        }
        assert!(!r.local.join(PARTIAL_DIRNAME).exists());
        // checkpoint zostaje: 1 stary wpis + każdy skopiowany plik
        let cp = std::fs::read_to_string(r.local.join(CHECKPOINT_FILENAME)).expect("checkpoint");
        assert_eq!(cp.lines().count() as u64, 1 + r.fin["ok"].as_u64().unwrap(), "[{tag}] checkpoint");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn e2e_no_root_xargs() {
        let r = scenario("v1", false, false).await;
        common_checks(&r, "v1");
        // bez roota: Android/data i wszystkie ciche błędy adb to błędy (widoczne!)
        assert!(r.failed.iter().any(|p| p.ends_with("secret.db")), "{:?}", r.failed);
        for bad in ["SILENT zero.jpg", "DENY.jpg", "NOFILE.jpg", "SHORT.jpg"] {
            assert!(r.failed.iter().any(|p| p.contains(bad)), "{bad}: {:?}", r.failed);
        }
        assert_eq!(r.failed.len(), 6, "{:?}", r.failed);
        assert_eq!(r.fin["ok"], 1207, "{}", r.fin);
        assert!(!r.local.join("Android/data/com.app/files/secret.db").exists());
        let _ = std::fs::remove_dir_all(&r.base);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn e2e_no_root_without_xargs() {
        let r = scenario("v2", true, false).await;
        common_checks(&r, "v2");
        assert_eq!(r.fin["ok"], 1207, "{}", r.fin);
        let _ = std::fs::remove_dir_all(&r.base);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn e2e_root_su_reads_protected() {
        let r = scenario("v3", false, true).await;
        common_checks(&r, "v3");
        let l = logs(&r);
        assert!(l.contains("Tryb Root Aktywny"), "brak informacji o roocie");
        assert!(r.events.iter().any(|v| v["kind"] == "item" && v["id"] == "root" && v["ok"] == true));
        // Android/data przez su + exec-out, z zachowaną datą
        let secret = r.local.join("Android/data/com.app/files/secret.db");
        assert_eq!(std::fs::read(&secret).unwrap(), vec![9u8; 777]);
        let m = std::fs::metadata(&secret).unwrap().modified().unwrap();
        assert_eq!(m.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(), 1_600_000_000);
        // odmowa adb pull ("Permission denied") -> automatycznie ponowione przez root
        assert_eq!(std::fs::read(r.local.join("Android/media/com.whatsapp/WhatsApp/Media/DENY.jpg")).unwrap(), b"x");
        assert!(!r.failed.iter().any(|p| p.ends_with("secret.db")), "{:?}", r.failed);
        // bilans: OK + błędy (bez MTP) = wszystkie pliki (1205 media + 4 problematyczne + 2 dowiązania + secret.db)
        assert_eq!(r.fin["ok"].as_u64().unwrap() + r.failed.len() as u64 - 1, 1212, "{} {:?}", r.fin, r.failed);
        let _ = std::fs::remove_dir_all(&r.base);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn checkpoint_created_even_when_nothing_to_copy() {
        let base = std::env::temp_dir().join(format!("adbst-cp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let f = open_checkpoint(&base.to_string_lossy()).unwrap();
        drop(f);
        assert!(base.join(CHECKPOINT_FILENAME).is_file());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn protected_paths() {
        assert!(is_protected_path("/data/data/com.x/db"));
        assert!(is_protected_path("/sdcard/Android/data/com.x/f"));
        assert!(!is_protected_path("/sdcard/DCIM/a.jpg"));
        assert!(!is_protected_path("/data/local/tmp/x"));
        assert!(root_retryable("adb: error: failed to stat remote object '/x': Permission denied"));
        assert!(!root_retryable("0 files pulled"));
    }
}
