//! Wysyłanie plików Z DYSKU na urządzenie - port `PushThread` + ulepszenia.
//!
//!  * Sam tworzy brakujące foldery docelowe (`mkdir -p` w paczkach).
//!  * Pojedynczy nieudany plik nie przerywa reszty wysyłki.
//!  * WZNAWIANIE: pliki, które już są na urządzeniu z identycznym rozmiarem,
//!    są pomijane - ponowne uruchomienie po przerwaniu kontynuuje pracę.
//!  * Postęp bajtowy: duże pliki (>32 MB) są podglądane `stat`-em w trakcie.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::Mutex;

use crate::adb::events::{Emitter, FailedItem};
use crate::adb::progress::Tracker;
use crate::adb::pull::{find_files_sized, human_bytes, looks_like_disconnect, DISCONNECT_STREAK_LIMIT};
use crate::adb::Adb;
use crate::error::AdbError;
use crate::utils::{posix_dirname, remote_join, shell_quote, CancelToken};

const LARGE_FILE: u64 = 32 * 1024 * 1024;
const MKDIR_CHUNK: usize = 3000;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushOptions {
    /// Lokalne pliki/foldery.
    pub items: Vec<String>,
    /// Katalog docelowy na urządzeniu, np. /sdcard/Download
    pub remote_dest: String,
    #[serde(default = "default_workers")]
    pub workers: usize,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "default_true")]
    pub skip_existing: bool,
    /// Pomijaj foldery `.thumbnails` (pliki robocze narzędzia pomijane są zawsze).
    #[serde(default)]
    pub exclude_junk: bool,
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

#[derive(Debug, Clone)]
struct PushFile {
    local: PathBuf,
    remote: String,
    size: u64,
}

pub async fn run_push(adb: Arc<Adb>, serial: String, opts: PushOptions, emitter: Emitter, cancel: CancelToken) {
    let dest = {
        let d = opts.remote_dest.trim().trim_end_matches('/');
        if d.is_empty() {
            "/".to_string()
        } else {
            d.to_string()
        }
    };
    if !dest.starts_with('/') {
        emitter.fatal("Katalog docelowy musi być ścieżką bezwzględną (np. /sdcard/Download) / destination must be absolute.");
        return;
    }

    // 1. Zbierz pliki lokalne (operacje dyskowe poza wątkiem async)
    emitter.info("Przygotowuję listę plików lokalnych / collecting local files...");
    let items = opts.items.clone();
    let dest2 = dest.clone();
    let junk = opts.exclude_junk;
    let collected = tokio::task::spawn_blocking(move || collect_local(&items, &dest2, junk)).await;
    let (mut files, targets, warnings) = match collected {
        Ok(v) => v,
        Err(e) => {
            emitter.fatal(format!("Błąd zbierania plików / collect error: {e}"));
            return;
        }
    };
    for w in warnings {
        emitter.warn(w);
    }
    if files.is_empty() {
        emitter.warn("Brak plików do wysłania / nothing to send.");
        emitter.finished(0, 0, 0, vec![], false);
        return;
    }

    // 2. Pomijanie plików już obecnych na urządzeniu (ten sam rozmiar)
    let mut skipped = 0u64;
    if opts.skip_existing {
        let mut remote_sizes: HashMap<String, u64> = HashMap::new();
        for t in &targets {
            if cancel.is_cancelled() {
                break;
            }
            for f in find_files_sized(&adb, &serial, t).await.files {
                remote_sizes.insert(f.device, f.size);
            }
        }
        let before = files.len();
        files.retain(|f| remote_sizes.get(&f.remote) != Some(&f.size) || f.size == 0);
        skipped = (before - files.len()) as u64;
        if skipped > 0 {
            emitter.info(format!(
                "Pomijam {skipped} plików już obecnych na urządzeniu (ten sam rozmiar) / already on device."
            ));
        }
    }

    if files.is_empty() {
        emitter.ok("Wszystko już jest na urządzeniu / everything already on device.");
        emitter.finished(0, skipped, 0, vec![], false);
        return;
    }

    // 3. Foldery docelowe
    prepare_remote_dirs(&adb, &serial, &files, &emitter, &cancel).await;
    if cancel.is_cancelled() {
        emitter.warn("Anulowano przed wysłaniem plików / cancelled.");
        emitter.finished(0, skipped, 0, vec![], true);
        return;
    }

    // 4. Wysyłka równoległa
    let total = files.len() as u64;
    let bytes_total: u64 = files.iter().map(|f| f.size).sum();
    let workers = opts.workers.clamp(1, 16);
    emitter.info(format!(
        "Łącznie do wysłania: {total} plików, {} -> {dest} ({workers} wątków) / starting.",
        human_bytes(bytes_total)
    ));

    let tracker = Tracker::new(total, bytes_total);
    let ticker = tracker.start(emitter.clone());
    let files = Arc::new(files);
    let next = Arc::new(AtomicUsize::new(0));
    let ok_count = Arc::new(AtomicU64::new(0));
    let disconnects = Arc::new(AtomicU32::new(0));
    let failures = Arc::new(Mutex::new(Vec::<FailedItem>::new()));

    let mut handles = Vec::new();
    for _ in 0..workers {
        let files = files.clone();
        let next = next.clone();
        let adb = adb.clone();
        let serial = serial.clone();
        let emitter = emitter.clone();
        let cancel = cancel.clone();
        let ok_count = ok_count.clone();
        let disconnects = disconnects.clone();
        let failures = failures.clone();
        let tracker = tracker.clone();
        let timeout_s = opts.timeout_seconds.max(10);

        handles.push(tokio::spawn(async move {
            loop {
                if cancel.is_cancelled() {
                    break;
                }
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(f) = files.get(i) else { break };
                tracker.set_current(f.remote.clone());

                let res = push_single(&adb, &serial, f, timeout_s, &cancel, &tracker).await;
                if matches!(res, Err(AdbError::Cancelled)) {
                    break;
                }
                let d = tracker.item_done();
                match res {
                    Ok(()) => {
                        ok_count.fetch_add(1, Ordering::SeqCst);
                        disconnects.store(0, Ordering::SeqCst);
                        emitter.ok(format!("[{d}/{total}] OK    {}", f.remote));
                    }
                    Err(e) => {
                        let err = e.to_string();
                        failures.lock().await.push(FailedItem {
                            path: f.local.to_string_lossy().to_string(),
                            error: err.clone(),
                        });
                        emitter.error(format!("[{d}/{total}] BŁĄD  {} :: {err}", f.remote));
                        if looks_like_disconnect(&err) {
                            let streak = disconnects.fetch_add(1, Ordering::SeqCst) + 1;
                            if streak >= DISCONNECT_STREAK_LIMIT && !cancel.is_cancelled() {
                                cancel.cancel();
                                emitter.error(
                                    "Powtarzające się błędy połączenia - przerywam / repeated disconnects, aborting.",
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

    let was_cancelled = cancel.is_cancelled();
    if was_cancelled {
        emitter.warn("Wysyłanie przerwane. Uruchom ponownie, by kontynuować / aborted, re-run to resume.");
    }
    let fails = failures.lock().await.clone();
    let ok = ok_count.load(Ordering::SeqCst);
    emitter.ok(format!("Zakończono: OK {ok}, błędy {}, pominięte {skipped} / done.", fails.len()));
    emitter.finished(ok, skipped, total, fails, was_cancelled);
}

async fn push_single(
    adb: &Adb,
    serial: &str,
    f: &PushFile,
    timeout_s: u64,
    cancel: &CancelToken,
    tracker: &Tracker,
) -> std::result::Result<(), AdbError> {
    let local = f.local.to_string_lossy().to_string();
    let args = ["push", local.as_str(), f.remote.as_str()];
    let fut = adb.run_c(Some(serial), &args, timeout_s, cancel);
    tokio::pin!(fut);

    let mut counted = 0u64;
    let poll_every = Duration::from_millis(1500);
    let quoted = shell_quote(&f.remote);
    let res = loop {
        tokio::select! {
            r = &mut fut => break r,
            _ = tokio::time::sleep(poll_every), if f.size >= LARGE_FILE => {
                let cmd = format!("stat -c %s {quoted} 2>/dev/null");
                if let Ok(o) = adb.shell(Some(serial), &cmd, 5).await {
                    if let Ok(s) = o.stdout.trim().parse::<u64>() {
                        let s = s.min(f.size);
                        if s > counted {
                            tracker.add_bytes(s - counted);
                            counted = s;
                        }
                    }
                }
            }
        }
    };
    tracker.add_bytes(f.size.saturating_sub(counted));

    let out = res?;
    if out.ok() {
        Ok(())
    } else {
        let e = out.err_text();
        Err(AdbError::Command(if e.is_empty() {
            format!("adb push zwrócił kod {}", out.status)
        } else {
            e
        }))
    }
}

/// Tworzy foldery docelowe na urządzeniu jednym `mkdir -p` na paczkę ścieżek.
async fn prepare_remote_dirs(adb: &Adb, serial: &str, files: &[PushFile], emitter: &Emitter, cancel: &CancelToken) {
    let dirs: BTreeSet<String> = files
        .iter()
        .map(|f| posix_dirname(&f.remote))
        .filter(|d| !d.is_empty() && d != "/")
        .collect();
    if dirs.is_empty() {
        return;
    }
    emitter.info(format!(
        "Przygotowuję {} folder(ów) docelowych na urządzeniu / creating remote dirs...",
        dirs.len()
    ));

    let mut batch = String::from("mkdir -p");
    let flush = |cmd: String| async move {
        match adb.shell(Some(serial), &cmd, 60).await {
            Ok(o) if !o.ok() => emitter.warn(format!("mkdir: {}", o.err_text())),
            Err(e) => emitter.warn(format!("mkdir: {e}")),
            _ => {}
        }
    };
    for d in dirs {
        if cancel.is_cancelled() {
            return;
        }
        let q = shell_quote(&d);
        if batch.len() + q.len() + 1 > MKDIR_CHUNK && batch != "mkdir -p" {
            flush(std::mem::replace(&mut batch, String::from("mkdir -p"))).await;
        }
        batch.push(' ');
        batch.push_str(&q);
    }
    if batch != "mkdir -p" {
        flush(batch).await;
    }
}

/// Zbiera pliki z lokalnych elementów. Zwraca (pliki, cele_na_urządzeniu, ostrzeżenia).
fn collect_local(items: &[String], dest: &str, junk: bool) -> (Vec<PushFile>, Vec<String>, Vec<String>) {
    let mut files = Vec::new();
    let mut targets = Vec::new();
    let mut warnings = Vec::new();

    for item in items {
        let p = Path::new(item);
        let base = p
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "folder".into());
        match std::fs::metadata(p) {
            Ok(m) if m.is_file() => {
                let remote = remote_join(dest, &base);
                targets.push(remote.clone());
                files.push(PushFile {
                    local: p.to_path_buf(),
                    remote,
                    size: m.len(),
                });
            }
            Ok(m) if m.is_dir() => {
                let remote_base = remote_join(dest, &base);
                targets.push(remote_base.clone());
                walk_dir(p, p, &remote_base, junk, &mut files, &mut warnings);
            }
            _ => warnings.push(format!("Pomijam '{item}' - nie znaleziono na dysku / not found.")),
        }
    }
    (files, targets, warnings)
}

/// Rekurencyjny spacer po katalogu (bez podążania za symlinkami katalogów - jak os.walk).
fn walk_dir(root: &Path, dir: &Path, remote_base: &str, junk: bool, out: &mut Vec<PushFile>, warnings: &mut Vec<String>) {
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            warnings.push(format!("Nie można odczytać {} / cannot read: {e}", dir.display()));
            return;
        }
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        let Ok(ft) = e.file_type() else { continue };
        // pliki robocze narzędzia (checkpoint pobierania, .adbst-partial) nigdy na urządzenie
        if crate::adb::pull::is_junk_path(&e.file_name().to_string_lossy(), junk) {
            continue;
        }
        if ft.is_dir() {
            walk_dir(root, &path, remote_base, junk, out, warnings);
        } else if ft.is_file() || ft.is_symlink() {
            let Ok(m) = std::fs::metadata(&path) else { continue };
            if !m.is_file() {
                continue;
            }
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let rel_posix: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect();
            out.push(PushFile {
                local: path.clone(),
                remote: remote_join(remote_base, &rel_posix.join("/")),
                size: m.len(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_skips_tool_files_and_thumbnails() {
        let dir = std::env::temp_dir().join(format!("adbst-push-junk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for p in ["Download/doc.pdf", "Download/.adb_pull_progress.jsonl", "Download/.adbst-partial/1.part", "Download/.thumbnails/t.jpg"] {
            let f = dir.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, b"x").unwrap();
        }
        let item = vec![dir.join("Download").to_string_lossy().to_string()];
        let (f, _, _) = collect_local(&item, "/sdcard", false);
        let r: Vec<&str> = f.iter().map(|x| x.remote.as_str()).collect();
        assert_eq!(r, vec!["/sdcard/Download/.thumbnails/t.jpg", "/sdcard/Download/doc.pdf"], "pliki robocze zawsze pominięte");
        let (f, _, _) = collect_local(&item, "/sdcard", true);
        assert_eq!(f.len(), 1, ".thumbnails pominięte przy exclude_junk");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
