//! Własny aktualizator (bez `tauri-plugin-updater`, bez tokenów i kluczy API).
//!
//! Źródło: statyczny manifest z publicznego wydania na GitHubie
//! `releases/latest/download/latest.json` (bez limitów API, bez autoryzacji).
//! Gdy manifestu brak - informacyjnie publiczne API `releases/latest`
//! (tylko numer wersji + link, bez instalacji).
//!
//! Bezpieczeństwo:
//!  - adresy plików MUSZĄ wskazywać na wydania tego repozytorium (HTTPS),
//!  - każdy plik jest pobierany do `*.part`, a SHA-256 i rozmiar są sprawdzane
//!    ZANIM cokolwiek zostanie uruchomione lub podmienione; niezgodność = plik
//!    usunięty, aktualizacja przerwana,
//!  - przy instalacji manifest jest pobierany ponownie przez backend (frontend
//!    nie podaje adresu ani sumy).
//!
//! Warianty (wykrywane w runtime):
//!  - Windows instalator (NSIS, obok `uninstall.exe`) -> `*-setup.exe /P /UPDATE /R`,
//!  - Windows portable -> zip z nowym `.exe`: `app.exe` -> `app.exe.old`, nowy na miejsce,
//!    restart; `.old` sprzątany przy starcie,
//!  - Linux AppImage (`$APPIMAGE`) -> nowy AppImage obok, `chmod +x`, atomowy `rename`, restart,
//!  - Linux portable (zapisywalny katalog) -> tar.gz, atomowa podmiana binarki, restart,
//!  - Linux systemowy (`/usr/bin`, .deb) -> `.deb` do ~/Downloads + polecenie / pkexec na żądanie.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::adb::events::Emitter;
use crate::adb::progress::Tracker;
use crate::error::{AdbError, Result};
use crate::utils::CancelToken;

pub const REPO: &str = "plhappylemonpl/adb-service-tool";
pub const MANIFEST_URL: &str =
    "https://github.com/plhappylemonpl/adb-service-tool/releases/latest/download/latest.json";
const API_LATEST: &str = "https://api.github.com/repos/plhappylemonpl/adb-service-tool/releases/latest";
pub const DOWNLOAD_PREFIX: &str = "https://github.com/plhappylemonpl/adb-service-tool/releases/download/";
pub const RELEASES_PAGE: &str = "https://github.com/plhappylemonpl/adb-service-tool/releases/latest";
/// Katalog roboczy pobierania obok binarki (ten sam wolumen = atomowy rename).
const WORK_DIR: &str = ".adbst-update";

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

// --------------------------------------------------------------------------- //
// Manifest
// --------------------------------------------------------------------------- //

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Asset {
    pub url: String,
    pub filename: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub published_at: Option<String>,
    /// tekst (Markdown) albo `{"pl": "...", "en": "..."}`
    #[serde(default)]
    pub notes: serde_json::Value,
    #[serde(default)]
    pub assets: BTreeMap<String, Asset>,
}

fn valid_filename(f: &str) -> bool {
    !f.is_empty()
        && f.len() <= 200
        && !f.starts_with('.')
        && f.chars().all(|c| c.is_ascii_alphanumeric() || "._-+".contains(c))
}

/// Parsuje i waliduje `latest.json`. Pozycje z niepoprawnym adresem/sumą są odrzucane.
pub fn parse_manifest(text: &str) -> Result<Manifest> {
    let mut m: Manifest = serde_json::from_str(text)
        .map_err(|e| AdbError::Parse(format!("latest.json nieczytelny / unreadable: {e}")))?;
    let v = m.version.trim().trim_start_matches('v').to_string();
    semver::Version::parse(&v).map_err(|e| AdbError::Parse(format!("latest.json: wersja '{}' - {e}", m.version)))?;
    m.version = v;
    m.assets.retain(|_, a| {
        a.sha256 = a.sha256.trim().to_ascii_lowercase();
        a.url.starts_with(DOWNLOAD_PREFIX)
            && !a.url.contains("..")
            && a.url.ends_with(&format!("/{}", a.filename))
            && valid_filename(&a.filename)
            && a.size > 0
            && a.sha256.len() == 64
            && a.sha256.chars().all(|c| c.is_ascii_hexdigit())
    });
    Ok(m)
}

/// Notatki w danym języku (`pl`/`en`), z rozsądnym fallbackiem.
pub fn notes_for(notes: &serde_json::Value, lang: &str) -> String {
    match notes {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(o) => o
            .get(lang)
            .or_else(|| o.get("en"))
            .or_else(|| o.get("pl"))
            .or_else(|| o.values().next())
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

/// `latest` > `current` wg semver (wersje przedpremierowe < wydanie).
pub fn is_newer(latest: &str, current: &str) -> bool {
    let p = |s: &str| semver::Version::parse(s.trim().trim_start_matches('v')).ok();
    match (p(latest), p(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

// --------------------------------------------------------------------------- //
// Wariant instalacji
// --------------------------------------------------------------------------- //

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InstallKind {
    WindowsInstaller,
    WindowsPortable,
    LinuxAppImage,
    LinuxPortable,
    LinuxSystem,
    /// build developerski (`tauri dev`) - tylko sprawdzanie
    Dev,
    Unsupported,
}

impl InstallKind {
    pub fn asset_key(self) -> Option<&'static str> {
        match self {
            InstallKind::WindowsInstaller => Some("windows-installer"),
            InstallKind::WindowsPortable => Some("windows-portable"),
            InstallKind::LinuxAppImage => Some("linux-appimage"),
            InstallKind::LinuxPortable => Some("linux-tar"),
            InstallKind::LinuxSystem => Some("linux-deb"),
            InstallKind::Dev | InstallKind::Unsupported => None,
        }
    }
}

/// Czysta logika wykrywania (testowalna).
pub fn detect_kind(
    os: &str,
    exe: &Path,
    appimage: Option<&Path>,
    has_uninstaller: bool,
    dir_writable: bool,
    debug_build: bool,
) -> InstallKind {
    if debug_build {
        return InstallKind::Dev;
    }
    match os {
        "windows" => {
            if has_uninstaller {
                InstallKind::WindowsInstaller
            } else {
                InstallKind::WindowsPortable
            }
        }
        "linux" => {
            if appimage.is_some() {
                return InstallKind::LinuxAppImage;
            }
            let s = exe.to_string_lossy();
            let system = ["/usr/", "/opt/", "/bin/", "/sbin/", "/snap/", "/nix/"].iter().any(|p| s.starts_with(p));
            if system || !dir_writable {
                InstallKind::LinuxSystem
            } else {
                InstallKind::LinuxPortable
            }
        }
        _ => InstallKind::Unsupported,
    }
}

fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".adbst-write-test-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn appimage_path() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_file())
}

pub fn current_kind() -> InstallKind {
    let Ok(exe) = std::env::current_exe() else {
        return InstallKind::Unsupported;
    };
    let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    detect_kind(
        std::env::consts::OS,
        &exe,
        appimage_path().as_deref(),
        dir.join("uninstall.exe").is_file(),
        dir_writable(&dir),
        cfg!(debug_assertions),
    )
}

/// Przy starcie: usuń `*.old` po podmianie (Windows portable) i katalog roboczy.
pub fn cleanup_after_update() {
    let Ok(exe) = std::env::current_exe() else { return };
    let mut old = exe.as_os_str().to_owned();
    old.push(".old");
    let _ = std::fs::remove_file(PathBuf::from(old));
    if let Some(dir) = exe.parent() {
        let _ = std::fs::remove_dir_all(dir.join(WORK_DIR));
    }
}

// --------------------------------------------------------------------------- //
// Sprawdzanie
// --------------------------------------------------------------------------- //

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub current: String,
    pub latest: Option<String>,
    pub available: bool,
    pub notes_pl: String,
    pub notes_en: String,
    pub published_at: Option<String>,
    pub kind: InstallKind,
    pub asset: Option<Asset>,
    /// "manifest" | "api"
    pub source: String,
    pub release_url: String,
    /// czy da się zainstalować z aplikacji (jest plik dla wariantu, nie dev)
    pub can_install: bool,
}

fn client(timeout: Option<Duration>) -> Result<reqwest::Client> {
    let mut b = reqwest::Client::builder()
        .user_agent(concat!("adb-service-tool/", env!("CARGO_PKG_VERSION"), " updater"))
        .connect_timeout(Duration::from_secs(8))
        .https_only(true);
    if let Some(t) = timeout {
        b = b.timeout(t);
    }
    b.build().map_err(|e| AdbError::Io(e.to_string()))
}

async fn fetch_manifest(timeout: Duration) -> Result<Option<Manifest>> {
    let resp = client(Some(timeout))?
        .get(MANIFEST_URL)
        .send()
        .await
        .map_err(|e| AdbError::Io(format!("update check: {e}")))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let resp = resp.error_for_status().map_err(|e| AdbError::Io(format!("update check: {e}")))?;
    let text = resp.text().await.map_err(|e| AdbError::Io(e.to_string()))?;
    if text.len() > 256 * 1024 {
        return Err(AdbError::Parse("latest.json za duży / too large".into()));
    }
    parse_manifest(&text).map(Some)
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
}

pub async fn check() -> Result<UpdateInfo> {
    let current = current_version().to_string();
    let kind = current_kind();
    let timeout = Duration::from_secs(10);
    if let Some(m) = fetch_manifest(timeout).await? {
        let asset = kind.asset_key().and_then(|k| m.assets.get(k)).cloned();
        let available = is_newer(&m.version, &current);
        return Ok(UpdateInfo {
            notes_pl: notes_for(&m.notes, "pl"),
            notes_en: notes_for(&m.notes, "en"),
            published_at: m.published_at.clone(),
            can_install: available && asset.is_some() && kind != InstallKind::Dev,
            release_url: format!("https://github.com/{REPO}/releases/tag/v{}", m.version),
            latest: Some(m.version),
            available,
            kind,
            asset,
            source: "manifest".into(),
            current,
        });
    }
    // brak latest.json w wydaniu - tylko informacja o wersji
    let r: ApiRelease = client(Some(timeout))?
        .get(API_LATEST)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| AdbError::Io(format!("update check (api): {e}")))?
        .json()
        .await
        .map_err(|e| AdbError::Parse(format!("update check (api): {e}")))?;
    let latest = r.tag_name.trim_start_matches('v').to_string();
    let notes = r.body.unwrap_or_default();
    Ok(UpdateInfo {
        available: is_newer(&latest, &current),
        latest: Some(latest),
        notes_pl: notes.clone(),
        notes_en: notes,
        published_at: r.published_at,
        kind,
        asset: None,
        source: "api".into(),
        release_url: r.html_url.unwrap_or_else(|| RELEASES_PAGE.into()),
        can_install: false,
        current,
    })
}

// --------------------------------------------------------------------------- //
// Pobieranie z weryfikacją SHA-256
// --------------------------------------------------------------------------- //

/// SHA-256 pliku (hex, małe litery).
pub fn sha256_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(crate::utils::download::hex(&h.finalize()))
}

/// Sprawdza rozmiar i SHA-256 pliku; przy niezgodności usuwa go.
pub fn verify_file(path: &Path, asset: &Asset) -> Result<()> {
    let size = std::fs::metadata(path)?.len();
    if size != asset.size {
        let _ = std::fs::remove_file(path);
        return Err(AdbError::Other(format!(
            "{}: rozmiar {size} B, oczekiwano {} B - aktualizacja przerwana / size mismatch",
            asset.filename, asset.size
        )));
    }
    let got = sha256_file(path)?;
    if !got.eq_ignore_ascii_case(&asset.sha256) {
        let _ = std::fs::remove_file(path);
        return Err(AdbError::Other(format!(
            "{}: SHA-256 niezgodne (plik {got}, oczekiwano {}) - plik usunięty, aktualizacja przerwana / checksum mismatch",
            asset.filename, asset.sha256
        )));
    }
    Ok(())
}

/// Pobiera plik do `dest` (przez `dest.part`) i weryfikuje go. Postęp w bajtach.
async fn download_verified(asset: &Asset, dest: &Path, emitter: &Emitter, cancel: &CancelToken) -> Result<()> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut part = dest.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);

    let tracker = Tracker::new(1, asset.size);
    tracker.set_current(asset.filename.clone());
    let ticker = tracker.start(emitter.clone());
    let res: Result<()> = async {
        let mut resp = client(None)?
            .get(&asset.url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| AdbError::Io(format!("{}: {e}", asset.filename)))?;
        let mut file = tokio::fs::File::create(&part).await?;
        let mut got: u64 = 0;
        loop {
            let chunk = tokio::select! {
                c = tokio::time::timeout(Duration::from_secs(60), resp.chunk()) => match c {
                    Ok(r) => r.map_err(|e| AdbError::Io(format!("{}: {e}", asset.filename)))?,
                    Err(_) => return Err(AdbError::Io("brak danych od 60 s / download stalled".into())),
                },
                _ = cancel.cancelled() => return Err(AdbError::Cancelled),
            };
            let Some(chunk) = chunk else { break };
            got += chunk.len() as u64;
            if got > asset.size {
                return Err(AdbError::Other(format!("{}: plik większy niż w manifeście / oversized", asset.filename)));
            }
            file.write_all(&chunk).await?;
            tracker.add_bytes(chunk.len() as u64);
        }
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        tracker.set_current(format!("SHA-256 {}", asset.filename));
        let (p, a) = (part.clone(), asset.clone());
        tokio::task::spawn_blocking(move || verify_file(&p, &a)).await??;
        tokio::fs::rename(&part, dest).await?;
        tracker.item_done();
        Ok(())
    }
    .await;
    tracker.shutdown(ticker).await;
    if res.is_err() {
        let _ = tokio::fs::remove_file(&part).await;
    }
    res
}

// --------------------------------------------------------------------------- //
// Instalacja
// --------------------------------------------------------------------------- //

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "action")]
pub enum InstallOutcome {
    /// nowa wersja uruchomiona / instalator wystartował - aplikacja się zamyka
    Restart,
    /// pakiet systemowy pobrany - użytkownik instaluje sam (lub pkexec)
    Manual { path: String, command: String },
}

/// Ścieżka wewnątrz archiwum bezpieczna do rozpakowania (bez `..`, bez ścieżek bezwzględnych).
fn safe_rel(p: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::Normal(s) => out.push(s),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Rozpakowuje zip/tar.gz do `dir`. Zwraca listę plików (ścieżki względne).
pub fn extract_archive(archive: &Path, dir: &Path) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    let mut files = Vec::new();
    let name = archive.to_string_lossy().to_ascii_lowercase();
    if name.ends_with(".zip") {
        let mut z = zip::ZipArchive::new(std::fs::File::open(archive)?)
            .map_err(|e| AdbError::Parse(format!("zip: {e}")))?;
        for i in 0..z.len() {
            let mut e = z.by_index(i).map_err(|e| AdbError::Parse(format!("zip: {e}")))?;
            if e.is_dir() {
                continue;
            }
            let Some(rel) = e.enclosed_name().and_then(|p| safe_rel(&p)) else { continue };
            let dest = dir.join(&rel);
            if let Some(p) = dest.parent() {
                std::fs::create_dir_all(p)?;
            }
            let mut out = std::fs::File::create(&dest)?;
            std::io::copy(&mut e, &mut out)?;
            files.push(rel);
        }
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let gz = flate2::read::GzDecoder::new(std::fs::File::open(archive)?);
        let mut t = tar::Archive::new(gz);
        for entry in t.entries().map_err(|e| AdbError::Parse(format!("tar: {e}")))? {
            let mut entry = entry.map_err(|e| AdbError::Parse(format!("tar: {e}")))?;
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let Some(rel) = entry.path().ok().and_then(|p| safe_rel(&p)) else { continue };
            let dest = dir.join(&rel);
            if let Some(p) = dest.parent() {
                std::fs::create_dir_all(p)?;
            }
            entry.unpack(&dest).map_err(|e| AdbError::Io(format!("tar: {e}")))?;
            files.push(rel);
        }
    } else {
        return Err(AdbError::InvalidArg(format!("nieznany format archiwum: {}", archive.display())));
    }
    Ok(files)
}

/// Wybiera nową binarkę z rozpakowanego archiwum: plik o nazwie bieżącej binarki,
/// potem `adb-service-tool[.exe]`, potem jedyny `*.exe` (Windows).
pub fn pick_binary(files: &[PathBuf], current_name: &str, windows: bool) -> Option<PathBuf> {
    let name_of = |p: &PathBuf| p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let default = if windows { "adb-service-tool.exe" } else { "adb-service-tool" };
    files
        .iter()
        .find(|p| name_of(p).eq_ignore_ascii_case(current_name))
        .or_else(|| files.iter().find(|p| name_of(p).eq_ignore_ascii_case(default)))
        .or_else(|| {
            if windows {
                let exes: Vec<&PathBuf> =
                    files.iter().filter(|p| name_of(p).to_ascii_lowercase().ends_with(".exe")).collect();
                (exes.len() == 1).then(|| exes[0])
            } else {
                None
            }
        })
        .cloned()
}

/// Pliki towarzyszące binarce w archiwum: (ścieżka w archiwum, ścieżka względem katalogu programu).
/// Tylko pliki z folderu binarki (i jego podfolderów) - górny folder archiwum jest pomijany.
pub fn companion_files(files: &[PathBuf], bin: &Path) -> Vec<(PathBuf, PathBuf)> {
    let root = bin.parent().map(Path::to_path_buf).unwrap_or_default();
    files
        .iter()
        .filter(|f| f.as_path() != bin)
        .filter_map(|f| f.strip_prefix(&root).ok().map(|r| (f.clone(), r.to_path_buf())))
        .filter(|(_, r)| !r.as_os_str().is_empty())
        .collect()
}

/// Podmiana działającej binarki: `cur` -> `cur.old`, `new` -> `cur` (z wycofaniem przy błędzie).
/// Windows pozwala zmienić nazwę uruchomionego pliku, Linux - podmienić go `rename`.
pub fn swap_binary(cur: &Path, new: &Path) -> Result<()> {
    let mut old = cur.as_os_str().to_owned();
    old.push(".old");
    let old = PathBuf::from(old);
    let _ = std::fs::remove_file(&old);
    if cfg!(windows) {
        std::fs::rename(cur, &old).map_err(|e| AdbError::Io(format!("{} -> .old: {e}", cur.display())))?;
        if let Err(e) = std::fs::rename(new, cur) {
            let _ = std::fs::rename(&old, cur);
            return Err(AdbError::Io(format!("podmiana {}: {e}", cur.display())));
        }
    } else {
        // atomowo: rename nadpisuje cel; stary i-węzeł zostaje dla działającego procesu
        std::fs::rename(new, cur).map_err(|e| AdbError::Io(format!("podmiana {}: {e}", cur.display())))?;
    }
    Ok(())
}

#[cfg(unix)]
fn make_executable(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_p: &Path) -> Result<()> {
    Ok(())
}

fn spawn_detached(program: &Path, args: &[&str]) -> Result<()> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(dir) = program.parent() {
        cmd.current_dir(dir);
    }
    cmd.spawn().map_err(|e| AdbError::Io(format!("uruchomienie {}: {e}", program.display())))?;
    Ok(())
}

fn downloads_dir() -> PathBuf {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    let candidates = home
        .iter()
        .flat_map(|h| [h.join("Downloads"), h.join("Pobrane")])
        .collect::<Vec<_>>();
    candidates.into_iter().find(|p| p.is_dir()).unwrap_or_else(std::env::temp_dir)
}

/// Pobiera i instaluje najnowszą wersję dla bieżącego wariantu.
pub async fn run_install(emitter: Emitter, cancel: CancelToken) -> Result<InstallOutcome> {
    let kind = current_kind();
    if kind == InstallKind::Dev {
        return Err(AdbError::InvalidArg(
            "build developerski - aktualizacja tylko w wersji wydanej / dev build".into(),
        ));
    }
    let m = fetch_manifest(Duration::from_secs(15))
        .await?
        .ok_or_else(|| AdbError::Other("wydanie nie zawiera latest.json / no latest.json in release".into()))?;
    if !is_newer(&m.version, current_version()) {
        return Err(AdbError::Other(format!("brak nowszej wersji (najnowsza: {}) / up to date", m.version)));
    }
    let key = kind.asset_key().ok_or_else(|| AdbError::Other("nieobsługiwany system / unsupported".into()))?;
    let asset = m
        .assets
        .get(key)
        .cloned()
        .ok_or_else(|| AdbError::Other(format!("wydanie {} nie ma pliku '{key}' / asset missing", m.version)))?;
    let exe = std::env::current_exe().map_err(|e| AdbError::Io(e.to_string()))?;
    let exe_dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    emitter.info(format!("Aktualizacja {} -> {} ({key}: {})", current_version(), m.version, asset.filename));

    match kind {
        InstallKind::WindowsInstaller => {
            let dest = std::env::temp_dir().join("adbst-update").join(&asset.filename);
            download_verified(&asset, &dest, &emitter, &cancel).await?;
            emitter.ok("SHA-256 OK - uruchamiam instalator / starting installer");
            spawn_detached(&dest, &["/P", "/UPDATE", "/R"])?;
            Ok(InstallOutcome::Restart)
        }
        InstallKind::WindowsPortable | InstallKind::LinuxPortable => {
            let work = exe_dir.join(WORK_DIR);
            let _ = tokio::fs::remove_dir_all(&work).await;
            let archive = work.join(&asset.filename);
            download_verified(&asset, &archive, &emitter, &cancel).await?;
            emitter.ok("SHA-256 OK - rozpakowuję / extracting");
            let unpack = work.join("unpacked");
            let (a, u) = (archive.clone(), unpack.clone());
            let files = tokio::task::spawn_blocking(move || extract_archive(&a, &u)).await??;
            let windows = kind == InstallKind::WindowsPortable;
            let cur_name = exe.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let bin = pick_binary(&files, &cur_name, windows)
                .ok_or_else(|| AdbError::Other(format!("{}: brak pliku programu w archiwum", asset.filename)))?;
            // pozostałe pliki (np. resources/agent/Agent.apk, README) obok programu -
            // ścieżki względem folderu, w którym leży binarka w archiwum
            for (f, rel) in companion_files(&files, &bin) {
                let dst = exe_dir.join(rel);
                if let Some(p) = dst.parent() {
                    tokio::fs::create_dir_all(p).await?;
                }
                tokio::fs::copy(unpack.join(f), &dst).await?;
            }
            let staged = exe_dir.join(format!("{cur_name}.new"));
            tokio::fs::copy(unpack.join(&bin), &staged).await?;
            make_executable(&staged)?;
            swap_binary(&exe, &staged)?;
            let _ = tokio::fs::remove_dir_all(&work).await;
            emitter.ok("Nowa wersja na miejscu - restart / restarting");
            spawn_detached(&exe, &[])?;
            Ok(InstallOutcome::Restart)
        }
        InstallKind::LinuxAppImage => {
            let target = appimage_path().ok_or_else(|| AdbError::Other("$APPIMAGE nie wskazuje pliku".into()))?;
            let mut new = target.as_os_str().to_owned();
            new.push(".new");
            let new = PathBuf::from(new);
            download_verified(&asset, &new, &emitter, &cancel).await?;
            make_executable(&new)?;
            tokio::fs::rename(&new, &target)
                .await
                .map_err(|e| AdbError::Io(format!("podmiana {}: {e}", target.display())))?;
            emitter.ok("AppImage podmieniony - restart / restarting");
            spawn_detached(&target, &[])?;
            Ok(InstallOutcome::Restart)
        }
        InstallKind::LinuxSystem => {
            let dest = downloads_dir().join(&asset.filename);
            download_verified(&asset, &dest, &emitter, &cancel).await?;
            let path = dest.to_string_lossy().to_string();
            emitter.ok(format!("Pobrano i zweryfikowano / downloaded: {path}"));
            Ok(InstallOutcome::Manual { command: format!("sudo apt install '{path}'"), path })
        }
        InstallKind::Dev | InstallKind::Unsupported => Err(AdbError::Other("nieobsługiwany wariant".into())),
    }
}

/// Instalacja pobranego `.deb` przez `pkexec` - tylko na wyraźne żądanie użytkownika.
pub async fn install_system_package(path: &str) -> Result<String> {
    let p = Path::new(path);
    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if !p.is_file() || !name.ends_with(".deb") || !valid_filename(&name) || !p.is_absolute() {
        return Err(AdbError::InvalidArg(format!("nieprawidłowy pakiet: {path}")));
    }
    let pkexec = which::which("pkexec").map_err(|_| AdbError::BinaryNotFound("pkexec".into()))?;
    let mut cmd = tokio::process::Command::new(pkexec);
    if which::which("apt-get").is_ok() {
        cmd.args(["apt-get", "install", "-y"]).arg(p);
    } else {
        cmd.args(["dpkg", "-i"]).arg(p);
    }
    let out = cmd.output().await.map_err(|e| AdbError::Io(e.to_string()))?;
    let text = format!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if out.status.success() {
        Ok(text.trim().to_string())
    } else {
        Err(AdbError::Command(format!("instalacja pakietu nie powiodła się: {}", text.trim())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"{
      "version": "v1.0.1",
      "published_at": "2026-12-01T10:00:00Z",
      "notes": {"pl": "Poprawki flashera", "en": "Flasher fixes"},
      "assets": {
        "windows-installer": {"url": "https://github.com/plhappylemonpl/adb-service-tool/releases/download/v1.0.1/ADB-Service-Tool_1.0.1_x64-setup.exe",
          "filename": "ADB-Service-Tool_1.0.1_x64-setup.exe", "size": 123, "sha256": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"},
        "linux-appimage": {"url": "https://evil.example/ADB.AppImage", "filename": "ADB.AppImage", "size": 1, "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
        "linux-deb": {"url": "https://github.com/plhappylemonpl/adb-service-tool/releases/download/v1.0.1/x.deb", "filename": "x.deb", "size": 5, "sha256": "short"},
        "linux-tar": {"url": "https://github.com/plhappylemonpl/adb-service-tool/releases/download/v1.0.1/../../x.tar.gz", "filename": "x.tar.gz", "size": 5, "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
      }
    }"#;

    #[test]
    fn semver_compare() {
        assert!(is_newer("1.0.1", "1.0.0"));
        assert!(is_newer("v1.10.0", "1.9.9"));
        assert!(is_newer("1.0.0", "1.0.0-rc.1"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("0.9.9", "1.0.0"));
        assert!(!is_newer("1.0.0-beta", "1.0.0"));
        assert!(!is_newer("garbage", "1.0.0"));
    }

    #[test]
    fn parses_and_validates_manifest() {
        let m = parse_manifest(MANIFEST).unwrap();
        assert_eq!(m.version, "1.0.1");
        assert_eq!(notes_for(&m.notes, "pl"), "Poprawki flashera");
        assert_eq!(notes_for(&m.notes, "en"), "Flasher fixes");
        assert_eq!(notes_for(&serde_json::json!("## Zmiany"), "en"), "## Zmiany");
        // tylko poprawny wpis: obcy host, zła suma i "../" odrzucone
        assert_eq!(m.assets.keys().collect::<Vec<_>>(), ["windows-installer"]);
        assert_eq!(m.assets["windows-installer"].sha256, "a".repeat(64));
        assert!(parse_manifest(r#"{"version":"jeden"}"#).is_err());
        assert!(parse_manifest("not json").is_err());
    }

    #[test]
    fn sha256_verification() {
        let dir = std::env::temp_dir().join(format!("adbst_upd_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("pkg.bin");
        std::fs::write(&f, b"abc").unwrap();
        let ok = Asset {
            url: format!("{DOWNLOAD_PREFIX}v1/pkg.bin"),
            filename: "pkg.bin".into(),
            size: 3,
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
        };
        verify_file(&f, &ok).unwrap();
        let bad = Asset { sha256: "0".repeat(64), ..ok.clone() };
        assert!(verify_file(&f, &bad).is_err());
        assert!(!f.exists(), "plik z błędną sumą musi zostać usunięty");
        std::fs::write(&f, b"abcd").unwrap();
        assert!(verify_file(&f, &ok).is_err(), "zły rozmiar");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detects_install_kind() {
        use InstallKind::*;
        let win = Path::new(r"C:\Users\u\AppData\Local\ADB Service Tool\adb-service-tool.exe");
        assert_eq!(detect_kind("windows", win, None, true, true, false), WindowsInstaller);
        assert_eq!(detect_kind("windows", Path::new(r"D:\tools\adb.exe"), None, false, true, false), WindowsPortable);
        let ai = Path::new("/home/u/Apps/ADB.AppImage");
        assert_eq!(detect_kind("linux", Path::new("/tmp/.mount_x/usr/bin/adb-service-tool"), Some(ai), false, false, false), LinuxAppImage);
        assert_eq!(detect_kind("linux", Path::new("/usr/bin/adb-service-tool"), None, false, false, false), LinuxSystem);
        assert_eq!(detect_kind("linux", Path::new("/home/u/adb/adb-service-tool"), None, false, true, false), LinuxPortable);
        assert_eq!(detect_kind("linux", Path::new("/home/u/ro/adb-service-tool"), None, false, false, false), LinuxSystem);
        assert_eq!(detect_kind("linux", Path::new("/home/u/x"), None, false, true, true), Dev);
        assert_eq!(detect_kind("macos", Path::new("/Applications/x"), None, false, true, false), Unsupported);
        assert_eq!(WindowsPortable.asset_key(), Some("windows-portable"));
        assert_eq!(LinuxSystem.asset_key(), Some("linux-deb"));
    }

    #[test]
    fn extracts_archives_and_swaps_binary() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("adbst_upd_arc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // zip (Windows portable) z exe + zasobem + próbą wyjścia poza katalog
        let zp = dir.join("p.zip");
        {
            let mut z = zip::ZipWriter::new(std::fs::File::create(&zp).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("ADB-Service-Tool/adb-service-tool.exe", o).unwrap();
            z.write_all(b"NEWEXE").unwrap();
            z.start_file("ADB-Service-Tool/resources/agent/Agent.apk", o).unwrap();
            z.write_all(b"APK").unwrap();
            z.start_file("../evil.txt", o).unwrap();
            z.write_all(b"x").unwrap();
            z.finish().unwrap();
        }
        let files = extract_archive(&zp, &dir.join("u")).unwrap();
        assert_eq!(files.len(), 2, "{files:?}");
        assert!(!dir.join("evil.txt").exists());
        let bin = pick_binary(&files, "ADB-Service-Tool-portable.exe", true).unwrap();
        assert!(bin.ends_with("adb-service-tool.exe"));
        let comp = companion_files(&files, &bin);
        assert_eq!(comp.len(), 1);
        assert_eq!(comp[0].1, PathBuf::from("resources/agent/Agent.apk"));

        // tar.gz (Linux portable)
        let tp = dir.join("p.tar.gz");
        {
            let gz = flate2::write::GzEncoder::new(std::fs::File::create(&tp).unwrap(), flate2::Compression::fast());
            let mut t = tar::Builder::new(gz);
            let mut h = tar::Header::new_gnu();
            h.set_size(6);
            h.set_mode(0o755);
            h.set_cksum();
            t.append_data(&mut h, "adb-service-tool-1.0.1/adb-service-tool", &b"NEWBIN"[..]).unwrap();
            t.into_inner().unwrap().finish().unwrap();
        }
        let files = extract_archive(&tp, &dir.join("t")).unwrap();
        let bin = pick_binary(&files, "adb-service-tool", false).unwrap();

        // podmiana binarki
        let cur = dir.join("adb-service-tool");
        std::fs::write(&cur, b"OLD").unwrap();
        let staged = dir.join("adb-service-tool.new");
        std::fs::copy(dir.join("t").join(&bin), &staged).unwrap();
        swap_binary(&cur, &staged).unwrap();
        assert_eq!(std::fs::read(&cur).unwrap(), b"NEWBIN");
        assert!(!staged.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
