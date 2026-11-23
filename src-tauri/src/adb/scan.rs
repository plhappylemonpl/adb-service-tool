//! Skaner drzewa plików urządzenia - kompletny, odporny na ARG_MAX i pętle.
//!
//! Jedna jednostka skanu = jeden katalog na urządzeniu (`dev`) widziany pod
//! ścieżką logiczną (`logical`). Dla każdej jednostki JEDNO wywołanie powłoki
//! zwraca strumieniowo, linia po linii:
//!  - `find -H <dir> -type f`        - wszystkie pliki, także ukryte (.nomedia),
//!  - `find -H <dir> -type d -empty` - puste foldery (odtwarzane lokalnie),
//!  - `find -H <dir> -type l`        - dowiązania symboliczne.
//!
//! `-H` podąża za dowiązaniem TYLKO w ścieżce startowej (np. `/sdcard` ->
//! `/storage/emulated/0`), wewnątrz drzewa NIE. Dowiązania z wnętrza drzewa
//! rozwiązuje Rust (`readlink -f`) i sam pilnuje pętli/duplikatów:
//!  - cel jest przodkiem/tym samym co już skanowany katalog -> pętla, pomiń,
//!  - cel leży wewnątrz już skanowanego drzewa -> duplikat danych, pomiń,
//!  - cel poza skanowanym obszarem -> nowa jednostka (folder) / plik do kopii,
//!  - dowiązanie zerwane -> notatka w logu.
//!
//! Rozmiary i daty: `find -print0 | xargs -0 -n 50 stat` (małe porcje), a gdy
//! xargs/stat zawiedzie - porcje `stat 'a' 'b' ...` składane w Rust (≤ 3 KB).
//! Wszystkie polecenia mogą być eskalowane rootem (`RootMode::wrap`).

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::adb::events::Emitter;
use crate::adb::root::RootMode;
use crate::adb::Adb;
use crate::utils::{shell_quote, CancelToken};

pub const FIND_TIMEOUT: u64 = 3600;
/// Maks. długość jednego polecenia `adb shell` przy porcjowaniu w Rust
/// (stare adbd, protokół v1: limit 4 KB na całe polecenie usługi).
pub const SHELL_CMD_BUDGET: usize = 3000;
/// Ile ścieżek `xargs` podaje jednemu `stat` (daleko od ARG_MAX).
const XARGS_BATCH: usize = 50;
/// Bezpiecznik: maks. liczba jednostek (folderów-celów dowiązań) na jeden skan.
const MAX_UNITS: usize = 256;
pub const CANCELLED: &str = "__cancelled__";

#[derive(Debug, Clone)]
pub struct ScannedFile {
    /// ścieżka, z której fizycznie czytamy (cel dowiązania, gdy plik był dowiązaniem)
    pub device: String,
    /// ścieżka logiczna (pod nią plik ląduje lokalnie i w checkpoincie)
    pub logical: String,
    /// 0 = nieznany
    pub size: u64,
    pub mtime: Option<i64>,
}

#[derive(Debug, Default)]
pub struct ScanResult {
    pub files: Vec<ScannedFile>,
    /// puste foldery (ścieżki logiczne)
    pub empty_dirs: Vec<String>,
    /// błędy = dane, których NIE skopiujemy (trafiają na listę błędów)
    pub errors: Vec<String>,
    /// informacje (pętle dowiązań, duplikaty, zerwane dowiązania, alias /data/media)
    pub notes: Vec<String>,
}

// --------------------------------------------------------------------------- //
// Strumieniowe adb shell
// --------------------------------------------------------------------------- //

/// `adb shell <cmd>`: stdout linia po linii (bajtowo, bezstratnie dla nie-UTF-8),
/// stderr zbierany osobno. Timeout i anulowanie zabijają proces adb.
pub async fn stream_shell(
    adb: &Adb,
    serial: &str,
    cmd: &str,
    timeout_s: u64,
    cancel: &CancelToken,
    mut on_line: impl FnMut(&str),
) -> std::result::Result<String, String> {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

    let mut c = adb.command(Some(serial), &["shell", cmd]);
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = c.spawn().map_err(|e| format!("adb: {e}"))?;
    let stdout = child.stdout.take().ok_or("adb: brak stdout")?;
    let mut stderr = child.stderr.take().ok_or("adb: brak stderr")?;
    let err_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).to_string()
    });

    let mut reader = BufReader::new(stdout);
    let mut buf: Vec<u8> = Vec::with_capacity(512);
    let deadline = tokio::time::sleep(Duration::from_secs(timeout_s.max(1)));
    tokio::pin!(deadline);
    loop {
        buf.clear();
        let n = tokio::select! {
            r = reader.read_until(b'\n', &mut buf) => r.map_err(|e| format!("adb: {e}"))?,
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                return Err(CANCELLED.into());
            }
            _ = &mut deadline => {
                let _ = child.kill().await;
                return Err(format!("przekroczono limit {timeout_s} s / timed out"));
            }
        };
        if n == 0 {
            break;
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim_end_matches(['\n', '\r']);
        if !line.is_empty() {
            on_line(line);
        }
    }
    let _ = child.wait().await;
    Ok(err_task.await.unwrap_or_default())
}

/// Wykonuje `prefix 'arg1' 'arg2' ...` w porcjach ≤ SHELL_CMD_BUDGET.
#[allow(clippy::too_many_arguments)]
async fn chunked(
    adb: &Adb,
    serial: &str,
    root: RootMode,
    prefix: &str,
    suffix: &str,
    args: &[&str],
    cancel: &CancelToken,
    mut on_line: impl FnMut(&str),
) -> std::result::Result<(), String> {
    let mut i = 0;
    while i < args.len() {
        let mut cmd = String::from(prefix);
        let start = i;
        while i < args.len() && (i == start || cmd.len() + args[i].len() + 4 + suffix.len() < SHELL_CMD_BUDGET) {
            cmd.push(' ');
            cmd.push_str(&shell_quote(args[i]));
            i += 1;
        }
        cmd.push_str(suffix);
        stream_shell(adb, serial, &root.wrap(&cmd), 120, cancel, &mut on_line).await?;
    }
    Ok(())
}

// --------------------------------------------------------------------------- //
// Pomocnicze ścieżki
// --------------------------------------------------------------------------- //

/// `a` leży wewnątrz `b` (ścisłe zawieranie).
pub fn is_under(a: &str, b: &str) -> bool {
    let b = b.trim_end_matches('/');
    b.is_empty() || a.starts_with(&format!("{b}/"))
}

/// Ścieżka logiczna dla pliku znalezionego w jednostce (dev -> logical).
pub fn map_logical(found: &str, dev: &str, logical: &str) -> String {
    if dev == logical {
        return found.to_string();
    }
    let dev = dev.trim_end_matches('/');
    match found.strip_prefix(dev) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{}{rest}", logical.trim_end_matches('/')),
        _ => found.to_string(),
    }
}

/// Pamięć wewnętrzna widziana przez FUSE (/sdcard, /storage/emulated/N) ->
/// surowa ścieżka `/data/media/N/...` (root omija ograniczenia FUSE Androida 11+).
pub fn data_media_alias(path: &str) -> Option<String> {
    let map = |user: &str, rest: &str| Some(format!("/data/media/{user}{rest}"));
    if let Some(rest) = path.strip_prefix("/sdcard") {
        if rest.is_empty() || rest.starts_with('/') {
            return map("0", rest);
        }
    }
    if let Some(rest) = path.strip_prefix("/storage/self/primary") {
        if rest.is_empty() || rest.starts_with('/') {
            return map("0", rest);
        }
    }
    if let Some(rest) = path.strip_prefix("/storage/emulated/") {
        let (user, tail) = rest.split_once('/').map(|(u, t)| (u, format!("/{t}"))).unwrap_or((rest, String::new()));
        if !user.is_empty() && user.bytes().all(|b| b.is_ascii_digit()) {
            return map(user, &tail);
        }
    }
    None
}

/// Ścieżka z komunikatu `find: '/sdcard/x': Permission denied`.
pub fn scan_error_path(err: &str) -> Option<String> {
    let start = err.find(['\'', '‘', '`'])?;
    let rest = &err[start + err[start..].chars().next()?.len_utf8()..];
    let end = rest.find(['\'', '’'])?;
    let p = &rest[..end];
    p.starts_with('/').then(|| p.to_string())
}

fn is_denied(e: &str) -> bool {
    let l = e.to_lowercase();
    l.contains("permission denied") || l.contains("operation not permitted")
}

/// `readlink -f` (z eskalacją) - kanoniczna ścieżka katalogu.
pub async fn realpath(adb: &Adb, serial: &str, root: RootMode, path: &str) -> Option<String> {
    let cmd = format!("readlink -f {} 2>/dev/null || realpath {} 2>/dev/null", shell_quote(path), shell_quote(path));
    let r = adb.shell(Some(serial), &root.wrap(&cmd), 20).await.ok()?;
    let v = r.stdout.lines().next().unwrap_or("").trim_end_matches('\r').to_string();
    v.starts_with('/').then_some(v)
}

/// (rozmiar, mtime) pojedynczego pliku.
pub async fn remote_stat(adb: &Adb, serial: &str, root: RootMode, path: &str) -> Option<(u64, Option<i64>)> {
    let cmd = format!("stat -L -c '%s|%Y' {} 2>/dev/null", shell_quote(path));
    let r = adb.shell(Some(serial), &root.wrap(&cmd), 20).await.ok()?;
    let line = r.stdout.lines().next()?.trim();
    let (s, m) = line.split_once('|').unwrap_or((line, ""));
    Some((s.parse().ok()?, m.parse().ok()))
}

/// Linia `stat -c '%s|%Y|%n'` -> (ścieżka, rozmiar, mtime). Zgodne też z `%s|%n`.
fn parse_stat_line(line: &str) -> Option<(&str, u64, Option<i64>)> {
    let (s, rest) = line.split_once('|')?;
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let size = s.parse().ok()?;
    if let Some((m, p)) = rest.split_once('|') {
        if !m.is_empty() && m.bytes().all(|b| b.is_ascii_digit() || b == b'-') && p.starts_with('/') {
            return Some((p, size, m.parse().ok()));
        }
    }
    rest.starts_with('/').then_some((rest, size, None))
}

// --------------------------------------------------------------------------- //
// Skan
// --------------------------------------------------------------------------- //

struct Unit {
    dev: String,
    logical: String,
}

/// Pełny skan katalogu `root_path` (z obsługą dowiązań, pustych folderów,
/// eskalacji root i aliasu /data/media przy odmowie FUSE).
pub async fn scan_tree(
    adb: &Adb,
    serial: &str,
    root_path: &str,
    root: RootMode,
    emitter: Option<&Emitter>,
    cancel: &CancelToken,
) -> ScanResult {
    let mut res = ScanResult::default();
    let start_real = realpath(adb, serial, root, root_path).await.unwrap_or_else(|| root_path.to_string());
    let mut visited: Vec<String> = vec![start_real];
    let mut queue: Vec<Unit> = vec![Unit { dev: root_path.to_string(), logical: root_path.to_string() }];
    let mut done_units = 0usize;
    let mut seen_files: HashSet<String> = HashSet::new();
    let mut link_files: Vec<(String, String)> = Vec::new(); // (cel, ścieżka logiczna)
    let mut alias_tried: HashSet<String> = HashSet::new();

    while let Some(u) = queue.pop() {
        if cancel.is_cancelled() {
            return res;
        }
        done_units += 1;
        if done_units > MAX_UNITS {
            res.errors.push(format!(
                "Przerwano rozwijanie dowiązań po {MAX_UNITS} folderach (zbyt wiele dowiązań) / too many linked folders"
            ));
            break;
        }

        let mut unit_errors: Vec<String> = Vec::new();
        let scanned = scan_unit(adb, serial, root, &u, emitter, cancel, &mut res, &mut seen_files, &mut unit_errors).await;
        let links = match scanned {
            Ok(l) => l,
            Err(e) if e == CANCELLED => return res,
            Err(e) => {
                unit_errors.push(format!("Skan przerwany / scan aborted '{}': {e}", u.logical));
                Vec::new()
            }
        };

        // root + odmowa FUSE (Android 11+: /sdcard/Android/data) -> skan przez /data/media
        if root.is_root() {
            let mut keep = Vec::new();
            for e in unit_errors {
                let alias = scan_error_path(&e)
                    .filter(|_| is_denied(&e))
                    .and_then(|p| data_media_alias(&p).map(|a| (p, a)))
                    .filter(|(p, _)| alias_tried.insert(p.clone()));
                match alias {
                    Some((p, a)) => {
                        res.notes.push(format!("Odmowa FUSE dla {p} - skanuję przez {a} (root) / via /data/media"));
                        let logical = map_logical(&p, &u.dev, &u.logical);
                        queue.push(Unit { dev: a, logical });
                    }
                    None => keep.push(e),
                }
            }
            unit_errors = keep;
        }
        res.errors.extend(unit_errors);

        // dowiązania: pętle / duplikaty / nowe obszary
        if links.is_empty() {
            continue;
        }
        let paths: Vec<&str> = links.iter().map(String::as_str).collect();
        let mut resolved: Vec<(char, String, String)> = Vec::new();
        let r = chunked(
            adb,
            serial,
            root,
            "for l in",
            r#"; do if [ -d "$l" ]; then t=d; elif [ -f "$l" ]; then t=f; else t=x; fi; printf '%s|%s|%s\n' "$t" "$(readlink -f "$l" 2>/dev/null)" "$l"; done"#,
            &paths,
            cancel,
            |line| {
                let mut it = line.splitn(3, '|');
                if let (Some(t), Some(target), Some(link)) = (it.next(), it.next(), it.next()) {
                    resolved.push((t.chars().next().unwrap_or('x'), target.to_string(), link.to_string()));
                }
            },
        )
        .await;
        if let Err(e) = r {
            if e == CANCELLED {
                return res;
            }
            res.notes.push(format!("Nie udało się rozwiązać dowiązań w {}: {e}", u.logical));
        }
        for (t, target, link) in resolved {
            let logical = map_logical(&link, &u.dev, &u.logical);
            match t {
                'd' if target.starts_with('/') => {
                    if visited.iter().any(|v| *v == target || is_under(v, &target)) {
                        res.notes.push(format!(
                            "Pętla dowiązań: {logical} -> {target} (wskazuje na folder nadrzędny / już skanowany) - pominięto / symlink loop skipped"
                        ));
                    } else if visited.iter().any(|v| is_under(&target, v)) {
                        res.notes.push(format!(
                            "Dowiązanie {logical} -> {target} wskazuje na dane już kopiowane - pominięto duplikat / duplicate skipped"
                        ));
                    } else {
                        visited.push(target.clone());
                        queue.push(Unit { dev: target, logical });
                    }
                }
                'f' if target.starts_with('/') => {
                    if visited.iter().any(|v| is_under(&target, v)) {
                        res.notes.push(format!("Dowiązanie {logical} -> {target}: plik już w kopii - pominięto duplikat"));
                    } else {
                        link_files.push((target, logical));
                    }
                }
                _ => res.notes.push(format!("Zerwane dowiązanie (cel nie istnieje): {logical} -> {target}")),
            }
        }
    }

    // pliki-dowiązania spoza skanowanych obszarów: rozmiar/data celu
    if !link_files.is_empty() {
        let targets: Vec<&str> = link_files.iter().map(|(t, _)| t.as_str()).collect();
        let mut st: HashMap<String, (u64, Option<i64>)> = HashMap::new();
        let _ = chunked(adb, serial, root, "stat -L -c '%s|%Y|%n'", " 2>/dev/null", &targets, cancel, |l| {
            if let Some((p, s, m)) = parse_stat_line(l) {
                st.insert(p.to_string(), (s, m));
            }
        })
        .await;
        for (target, logical) in link_files {
            if seen_files.insert(target.clone()) {
                let (size, mtime) = st.get(&target).copied().unwrap_or((0, None));
                res.files.push(ScannedFile { device: target, logical, size, mtime });
            }
        }
    }
    res
}

/// Jedna jednostka: pliki + puste foldery + lista dowiązań (do rozwiązania).
#[allow(clippy::too_many_arguments)]
async fn scan_unit(
    adb: &Adb,
    serial: &str,
    root: RootMode,
    u: &Unit,
    emitter: Option<&Emitter>,
    cancel: &CancelToken,
    res: &mut ScanResult,
    seen_files: &mut HashSet<String>,
    errors: &mut Vec<String>,
) -> std::result::Result<Vec<String>, String> {
    let p = shell_quote(&u.dev);
    let cmd = format!(
        "if [ ! -r {p} ]; then echo __NOPERM__; else \
         echo __F__; find -H {p} -type f; \
         echo __E__; find -H {p} -type d -empty; \
         echo __L__; find -H {p} -type l; fi"
    );
    let mut section = ' ';
    let mut files: Vec<String> = Vec::new();
    let mut empties: Vec<String> = Vec::new();
    let mut links: Vec<String> = Vec::new();
    let mut noperm = false;
    let mut odd: Vec<String> = Vec::new();
    let mut next_report = 5000usize;
    let stderr = stream_shell(adb, serial, &root.wrap(&cmd), FIND_TIMEOUT, cancel, |line| match line {
        "__NOPERM__" => noperm = true,
        "__F__" => section = 'f',
        "__E__" => section = 'e',
        "__L__" => section = 'l',
        l if l.starts_with('/') => match section {
            'f' => {
                files.push(l.to_string());
                if files.len() >= next_report {
                    next_report += 5000;
                    if let Some(em) = emitter {
                        em.info(format!("  ... {} plików / files ({})", files.len(), u.logical));
                    }
                }
            }
            'e' => empties.push(l.to_string()),
            'l' => links.push(l.to_string()),
            _ => odd.push(l.to_string()),
        },
        l => odd.push(l.to_string()),
    })
    .await?;

    if noperm {
        errors.push(format!("Brak uprawnień do odczytu / permission denied: '{}'", u.dev));
        return Ok(vec![]);
    }
    for l in stderr.lines().map(str::trim).filter(|l| !l.is_empty()) {
        // stary find bez -empty: tracimy tylko puste foldery, nie dane
        if l.contains("-empty") {
            res.notes.push(format!("find nie obsługuje -empty - puste foldery nie zostaną odtworzone: {l}"));
        } else {
            errors.push(l.to_string());
        }
    }
    for l in odd {
        errors.push(format!("Nierozpoznana pozycja skanu (nietypowa nazwa pliku?) / unparsed: {l}"));
    }

    // rozmiary + daty w małych porcjach
    let mut sizes: HashMap<String, (u64, Option<i64>)> = HashMap::with_capacity(files.len());
    if !files.is_empty() {
        let wanted: HashSet<&str> = files.iter().map(String::as_str).collect();
        let size_cmd = format!(
            "find -H {p} -type f -print0 2>/dev/null | xargs -0 -n {XARGS_BATCH} stat -c '%s|%Y|%n' 2>/dev/null"
        );
        let mut parse = |line: &str| {
            if let Some((path, s, m)) = parse_stat_line(line) {
                if wanted.contains(path) {
                    sizes.insert(path.to_string(), (s, m));
                }
            }
        };
        match stream_shell(adb, serial, &root.wrap(&size_cmd), FIND_TIMEOUT, cancel, &mut parse).await {
            Err(e) if e == CANCELLED => return Err(e),
            _ => {}
        }
        let missing: Vec<&str> = files.iter().map(String::as_str).filter(|f| !sizes.contains_key(*f)).collect();
        if !missing.is_empty() {
            let mut extra: Vec<(String, u64, Option<i64>)> = Vec::new();
            match chunked(adb, serial, root, "stat -L -c '%s|%Y|%n'", " 2>/dev/null", &missing, cancel, |l| {
                if let Some((path, s, m)) = parse_stat_line(l) {
                    extra.push((path.to_string(), s, m));
                }
            })
            .await
            {
                Err(e) if e == CANCELLED => return Err(e),
                _ => {}
            }
            for (path, s, m) in extra {
                sizes.insert(path, (s, m));
            }
        }
        let unknown = files.iter().filter(|f| !sizes.contains_key(f.as_str())).count();
        if unknown > 0 {
            res.notes.push(format!(
                "{unknown} plików w {} bez ustalonego rozmiaru (zostaną skopiowane i zweryfikowane wg adb) / sizes unknown",
                u.logical
            ));
        }
    }

    for f in files {
        if !seen_files.insert(f.clone()) {
            continue;
        }
        let (size, mtime) = sizes.get(&f).copied().unwrap_or((0, None));
        let logical = map_logical(&f, &u.dev, &u.logical);
        res.files.push(ScannedFile { device: f, logical, size, mtime });
    }
    res.empty_dirs.extend(empties.iter().map(|e| map_logical(e, &u.dev, &u.logical)));
    Ok(links)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert!(is_under("/a/b/c", "/a/b"));
        assert!(!is_under("/a/bc", "/a/b"));
        assert!(!is_under("/a/b", "/a/b"));
        assert_eq!(map_logical("/storage/x/y.jpg", "/storage/x", "/sdcard/link"), "/sdcard/link/y.jpg");
        assert_eq!(map_logical("/same/a", "/same", "/same"), "/same/a");
        assert_eq!(data_media_alias("/sdcard/Android/data/com.x").as_deref(), Some("/data/media/0/Android/data/com.x"));
        assert_eq!(data_media_alias("/storage/emulated/10/DCIM").as_deref(), Some("/data/media/10/DCIM"));
        assert_eq!(data_media_alias("/sdcardx/a"), None);
        assert_eq!(data_media_alias("/storage/3A1F-0E12/x"), None);
    }

    #[test]
    fn stat_lines() {
        assert_eq!(parse_stat_line("12|1700000000|/sdcard/a|b.jpg"), Some(("/sdcard/a|b.jpg", 12, Some(1700000000))));
        assert_eq!(parse_stat_line("12|/sdcard/old.jpg"), Some(("/sdcard/old.jpg", 12, None)));
        assert_eq!(parse_stat_line("stat: bad"), None);
    }
}
