//! scrcpy: wykrywanie, automatyczne pobieranie (Windows), uruchamianie sesji.
//!
//! - Szukamy `scrcpy` w PATH, a potem w prywatnym folderze aplikacji
//!   (`%APPDATA%\com.drapco.adbservicetool\scrcpy\...` na Windows).
//! - Windows bez scrcpy: pobieramy najnowszy `scrcpy-win64-*.zip` z GitHuba
//!   (API releases/latest), rozpakowujemy do folderu aplikacji.
//! - Linux bez scrcpy: rozpoznajemy dystrybucję (/etc/os-release) i podajemy
//!   właściwą komendę instalacji.
//! - Sesja: scrcpy dostaje zmienną `ADB` = nasze adb, żeby dwa różne serwery
//!   adb (np. z paczki scrcpy i z platform-tools) nie zabijały się nawzajem.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::adb::events::{Emitter, LogLevel};
use crate::adb::manager::{hide_console, run_raw};
use crate::adb::progress::Tracker;
use crate::error::{AdbError, Result};
use crate::utils::CancelToken;

const GITHUB_LATEST: &str = "https://api.github.com/repos/Genymobile/scrcpy/releases/latest";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DistroHint {
    pub id: String,
    pub name: String,
    pub command: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrcpyStatus {
    pub path: Option<String>,
    pub version: Option<String>,
    /// true = scrcpy pobrany przez aplikację (folder prywatny)
    pub bundled: bool,
    pub os: String,
    pub distro: Option<DistroHint>,
    /// Automatyczne pobieranie dostępne (Windows)
    pub can_download: bool,
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

pub fn local_dir(app_data: &Path) -> PathBuf {
    app_data.join("scrcpy")
}

/// Szuka pliku `scrcpy(.exe)` w drzewie katalogów (max 3 poziomy).
fn find_in(dir: &Path, depth: u32) -> Option<PathBuf> {
    let target = exe("scrcpy");
    let rd = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.is_file() && p.file_name().map(|n| n == target.as_str()).unwrap_or(false) {
            return Some(p);
        }
        if p.is_dir() {
            subdirs.push(p);
        }
    }
    if depth == 0 {
        return None;
    }
    // najnowsza wersja (nazwy katalogów zawierają tag, np. scrcpy-win64-v3.1)
    subdirs.sort();
    subdirs.reverse();
    subdirs.into_iter().find_map(|d| find_in(&d, depth - 1))
}

/// Zwraca (ścieżka, czy_z_folderu_aplikacji).
pub fn locate(app_data: &Path) -> Option<(PathBuf, bool)> {
    if let Ok(p) = which::which("scrcpy") {
        return Some((p, false));
    }
    #[cfg(not(windows))]
    {
        for p in ["/usr/bin/scrcpy", "/usr/local/bin/scrcpy", "/snap/bin/scrcpy", "/var/lib/flatpak/exports/bin/scrcpy"] {
            let pb = PathBuf::from(p);
            if pb.is_file() {
                return Some((pb, false));
            }
        }
    }
    find_in(&local_dir(app_data), 3).map(|p| (p, true))
}

/// `scrcpy --version` -> "2.4" (pierwsza linia: "scrcpy 2.4 <https://...>")
pub async fn version(path: &Path) -> Option<String> {
    let mut cmd = Command::new(path);
    hide_console(&mut cmd);
    cmd.arg("--version");
    let out = run_raw(cmd, 15, None).await.ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    text.lines()
        .next()?
        .split_whitespace()
        .nth(1)
        .map(|v| v.trim_start_matches('v').to_string())
}

fn major(ver: Option<&str>) -> u32 {
    ver.and_then(|v| v.split('.').next())
        .and_then(|m| m.parse().ok())
        .unwrap_or(2)
}

pub async fn status(app_data: &Path) -> ScrcpyStatus {
    let found = locate(app_data);
    let ver = match &found {
        Some((p, _)) => version(p).await,
        None => None,
    };
    ScrcpyStatus {
        path: found.as_ref().map(|(p, _)| p.to_string_lossy().to_string()),
        version: ver,
        bundled: found.map(|(_, b)| b).unwrap_or(false),
        os: std::env::consts::OS.to_string(),
        distro: distro_hint(),
        can_download: cfg!(windows),
    }
}

// --------------------------------------------------------------------------- //
// Linux: rozpoznanie dystrybucji -> komenda instalacji
// --------------------------------------------------------------------------- //

fn distro_hint() -> Option<DistroHint> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let text = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .unwrap_or_default();
    Some(hint_from_os_release(&text))
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn hint_from_os_release(text: &str) -> DistroHint {
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .map(|v| v.trim().trim_matches('"').to_string())
            .unwrap_or_default()
    };
    let id = get("ID").to_lowercase();
    let like = get("ID_LIKE").to_lowercase();
    let name = {
        let n = get("PRETTY_NAME");
        if n.is_empty() {
            "Linux".to_string()
        } else {
            n
        }
    };
    let is = |k: &str| id == k || like.split_whitespace().any(|x| x == k);

    let (command, note): (&str, Option<&str>) = if is("arch") {
        ("sudo pacman -S --needed scrcpy", None)
    } else if is("ubuntu") || is("debian") {
        (
            "sudo apt install scrcpy",
            Some("Wersje w starszych wydaniach Debiana/Ubuntu bywają stare - nowszą dostaniesz przez: sudo snap install scrcpy / older distro releases ship an old scrcpy; snap has the latest"),
        )
    } else if is("fedora") || is("rhel") || is("centos") {
        ("sudo dnf copr enable zeno/scrcpy && sudo dnf install scrcpy", None)
    } else if is("opensuse") || is("suse") || id.starts_with("opensuse") {
        ("sudo zypper install scrcpy", None)
    } else if is("gentoo") {
        ("sudo emerge --ask app-mobilephone/scrcpy", None)
    } else if is("void") {
        ("sudo xbps-install -S scrcpy", None)
    } else if is("alpine") {
        ("sudo apk add scrcpy", None)
    } else if is("nixos") {
        ("nix-env -iA nixos.scrcpy", Some("lub dodaj pkgs.scrcpy do configuration.nix / or add pkgs.scrcpy to configuration.nix"))
    } else if is("solus") {
        ("sudo eopkg install scrcpy", None)
    } else {
        ("sudo snap install scrcpy", Some("Nie rozpoznano dystrybucji - uniwersalnie: snap lub flatpak / unknown distro"))
    };
    DistroHint {
        id: if id.is_empty() { "linux".into() } else { id },
        name,
        command: command.to_string(),
        note: note.map(str::to_string),
    }
}

// --------------------------------------------------------------------------- //
// Windows: automatyczne pobieranie najnowszej paczki z GitHuba
// --------------------------------------------------------------------------- //

/// Pobiera i rozpakowuje najnowszy scrcpy (win64) do folderu aplikacji.
/// Zwraca ścieżkę do `scrcpy.exe`. Postęp bajtowy idzie przez `emitter`.
pub async fn download_latest(app_data: &Path, emitter: &Emitter, cancel: &CancelToken) -> Result<PathBuf> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("adb-service-tool/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| AdbError::Io(e.to_string()))?;

    emitter.info("Sprawdzam najnowsze wydanie scrcpy na GitHubie / checking latest release...");
    let rel: serde_json::Value = client
        .get(GITHUB_LATEST)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| AdbError::Io(format!("GitHub API: {e}")))?
        .json()
        .await
        .map_err(|e| AdbError::Parse(format!("GitHub API: {e}")))?;

    let tag = rel["tag_name"].as_str().unwrap_or("latest").to_string();
    let assets = rel["assets"].as_array().cloned().unwrap_or_default();
    let pick = |needle: &str| {
        assets.iter().find(|a| {
            let n = a["name"].as_str().unwrap_or("");
            n.contains(needle) && n.ends_with(".zip")
        })
    };
    let asset = pick("win64")
        .or_else(|| pick("win32"))
        .ok_or_else(|| AdbError::Other(format!("Brak paczki Windows w wydaniu {tag} / no windows asset")))?;
    let url = asset["browser_download_url"].as_str().unwrap_or("").to_string();
    let name = asset["name"].as_str().unwrap_or("scrcpy.zip").to_string();
    let size = asset["size"].as_u64().unwrap_or(0);
    if url.is_empty() {
        return Err(AdbError::Other("Brak URL pobierania / no download url".into()));
    }

    let base = local_dir(app_data);
    tokio::fs::create_dir_all(&base).await?;
    let zip_path = base.join(&name);
    emitter.info(format!("Pobieram / downloading {name} ({tag})..."));

    let tracker = Tracker::new(1, size);
    tracker.set_current(name.clone());
    let ticker = tracker.start(emitter.clone());

    let res: Result<()> = async {
        let mut resp = client
            .get(&url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| AdbError::Io(format!("download: {e}")))?;
        let mut file = tokio::fs::File::create(&zip_path).await?;
        loop {
            let chunk = tokio::select! {
                c = resp.chunk() => c.map_err(|e| AdbError::Io(format!("download: {e}")))?,
                _ = cancel.cancelled() => return Err(AdbError::Cancelled),
            };
            let Some(bytes) = chunk else { break };
            file.write_all(&bytes).await?;
            tracker.add_bytes(bytes.len() as u64);
        }
        file.flush().await?;
        Ok(())
    }
    .await;
    tracker.item_done();
    tracker.shutdown(ticker).await;
    if let Err(e) = res {
        let _ = tokio::fs::remove_file(&zip_path).await;
        return Err(e);
    }

    emitter.info("Rozpakowuję / extracting...");
    let dest = base.join(name.trim_end_matches(".zip"));
    let zp = zip_path.clone();
    let dest2 = dest.clone();
    tokio::task::spawn_blocking(move || extract_zip(&zp, &dest2)).await??;
    let _ = tokio::fs::remove_file(&zip_path).await;

    let exe_path = find_in(&dest, 3)
        .ok_or_else(|| AdbError::Other("W paczce nie ma scrcpy.exe / scrcpy.exe not found in archive".into()))?;
    emitter.ok(format!("scrcpy {tag} gotowy / ready: {}", exe_path.display()));
    Ok(exe_path)
}

/// Bezpieczne rozpakowanie (ochrona przed path traversal - `enclosed_name`).
fn extract_zip(zip_path: &Path, dest: &Path) -> Result<()> {
    let f = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(f).map_err(|e| AdbError::Parse(format!("zip: {e}")))?;
    std::fs::create_dir_all(dest)?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| AdbError::Parse(format!("zip: {e}")))?;
        let Some(rel) = entry.enclosed_name() else { continue };
        let out = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut w = std::fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut w)?;
    }
    Ok(())
}

// --------------------------------------------------------------------------- //
// Uruchamianie sesji
// --------------------------------------------------------------------------- //

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ScrcpyOptions {
    /// Maksymalny wymiar obrazu w px (0 = natywna rozdzielczość)
    pub max_size: u32,
    /// Bitrate wideo w Mb/s (0 = domyślny scrcpy, 8M)
    pub bit_rate_mbps: u32,
    /// Limit FPS (0 = bez limitu)
    pub max_fps: u32,
    pub stay_awake: bool,
    pub show_touches: bool,
    pub turn_screen_off: bool,
    /// Bez przesyłania dźwięku (audio działa dopiero od Androida 11)
    pub no_audio: bool,
    pub always_on_top: bool,
    pub borderless: bool,
    pub fullscreen: bool,
    /// Tylko podgląd, bez sterowania myszą/klawiaturą
    pub view_only: bool,
}

pub fn build_args(serial: &str, o: &ScrcpyOptions, version: Option<&str>, title: &str) -> Vec<String> {
    let v2 = major(version) >= 2;
    let mut a = vec!["-s".to_string(), serial.to_string(), format!("--window-title={title}")];
    if o.max_size > 0 {
        a.push(format!("--max-size={}", o.max_size));
    }
    if o.bit_rate_mbps > 0 {
        // scrcpy 1.x: --bit-rate, 2.x+: --video-bit-rate
        let flag = if v2 { "--video-bit-rate" } else { "--bit-rate" };
        a.push(format!("{flag}={}M", o.bit_rate_mbps));
    }
    if o.max_fps > 0 {
        a.push(format!("--max-fps={}", o.max_fps));
    }
    if o.stay_awake {
        a.push("--stay-awake".into());
    }
    if o.show_touches {
        a.push("--show-touches".into());
    }
    if o.turn_screen_off {
        a.push("--turn-screen-off".into());
    }
    if o.no_audio && v2 {
        a.push("--no-audio".into());
    }
    if o.always_on_top {
        a.push("--always-on-top".into());
    }
    if o.borderless {
        a.push("--window-borderless".into());
    }
    if o.fullscreen {
        a.push("--fullscreen".into());
    }
    if o.view_only {
        a.push("--no-control".into());
    }
    a
}

/// Uruchamia scrcpy i nadzoruje go do zamknięcia okna lub anulowania.
/// stdout/stderr scrcpy trafiają do logu w GUI.
pub async fn run_session(scrcpy: PathBuf, adb_path: PathBuf, args: Vec<String>, emitter: Emitter, cancel: CancelToken) {
    let mut cmd = Command::new(&scrcpy);
    hide_console(&mut cmd);
    cmd.args(&args)
        .env("ADB", &adb_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // scrcpy szuka scrcpy-server obok siebie - ustawiamy katalog roboczy
    if let Some(dir) = scrcpy.parent() {
        cmd.current_dir(dir);
    }

    emitter.info(format!("scrcpy {}", args.join(" ")));
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            emitter.error(format!("Nie udało się uruchomić scrcpy / spawn failed: {e}"));
            emitter.finished(0, 0, 1, vec![], false);
            return;
        }
    };

    let pipe_log = |em: Emitter, r: Box<dyn tokio::io::AsyncRead + Unpin + Send>| {
        tokio::spawn(async move {
            let mut lines = BufReader::new(r).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let t = l.trim();
                if t.is_empty() {
                    continue;
                }
                let level = if t.starts_with("ERROR") {
                    LogLevel::Error
                } else if t.starts_with("WARN") {
                    LogLevel::Warn
                } else {
                    LogLevel::Info
                };
                em.log(level, format!("[scrcpy] {t}"));
            }
        })
    };
    if let Some(out) = child.stdout.take() {
        pipe_log(emitter.clone(), Box::new(out));
    }
    if let Some(err) = child.stderr.take() {
        pipe_log(emitter.clone(), Box::new(err));
    }

    let status = tokio::select! {
        s = child.wait() => s.ok(),
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            None
        }
    };
    match status {
        Some(s) if s.success() => {
            emitter.ok("Sesja scrcpy zakończona / session closed.");
            emitter.finished(1, 0, 1, vec![], false);
        }
        Some(s) => {
            emitter.error(format!("scrcpy zakończył się kodem / exited with {s}"));
            emitter.finished(0, 0, 1, vec![], false);
        }
        None => {
            emitter.warn("Sesja scrcpy zatrzymana / session stopped.");
            emitter.finished(0, 0, 1, vec![], true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distro_detection() {
        let arch = hint_from_os_release("NAME=\"EndeavourOS\"\nID=endeavouros\nID_LIKE=arch\nPRETTY_NAME=\"EndeavourOS\"\n");
        assert!(arch.command.contains("pacman"));
        let mint = hint_from_os_release("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n");
        assert!(mint.command.contains("apt"));
        let fed = hint_from_os_release("ID=fedora\n");
        assert!(fed.command.contains("dnf"));
        let unk = hint_from_os_release("");
        assert!(unk.command.contains("snap"));
    }

    #[test]
    fn args_v1_v2() {
        let o = ScrcpyOptions {
            bit_rate_mbps: 4,
            max_size: 1024,
            no_audio: true,
            stay_awake: true,
            ..Default::default()
        };
        let a2 = build_args("X", &o, Some("2.4"), "T");
        assert!(a2.contains(&"--video-bit-rate=4M".to_string()));
        assert!(a2.contains(&"--no-audio".to_string()));
        let a1 = build_args("X", &o, Some("1.25"), "T");
        assert!(a1.contains(&"--bit-rate=4M".to_string()));
        assert!(!a1.contains(&"--no-audio".to_string()));
    }
}
