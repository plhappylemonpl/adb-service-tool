//! Flasher Samsunga (GUI) oparty na sprawdzonym silniku **Heimdall**
//! (otwarty, wieloplatformowy odpowiednik Odina).
//!
//! Dlaczego Heimdall, a nie własny zapis po USB: samo wykrycie i odczyt PIT
//! robimy natywnie (`hardware::loke`), ale ZAPIS firmware to operacja
//! destrukcyjna z ryzykiem ucegleniem przy błędnym PIT/repartition. Protokołu
//! zapisu nie da się bezpiecznie zweryfikować bez prawdziwego urządzenia, więc
//! flashowanie wykonuje Heimdall - ten sam protokół, latami sprawdzony w boju.
//!
//! Aplikacja robi resztę natywnie: indeks i weryfikację MD5 paczek `.tar.md5`,
//! strumieniową ekstrakcję tylko wybranych obrazów (bez rozpakowywania całych
//! gigabajtów), mapowanie obrazów na partycje z PIT i czytelny pasek postępu.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::adb::events::{Emitter, LogLevel};
use crate::adb::manager::{hide_console, run_raw};
use crate::adb::progress::Tracker;
use crate::error::{AdbError, Result};
use crate::hardware::loke;
use crate::parsers::samsung_tar::{self, FirmwareTar, TarRole};
use crate::utils::CancelToken;
use crate::vendors::heimdall_output::{FlashProgress, HeimdallEvent, HeimdallParser};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeimdallStatus {
    pub path: Option<String>,
    pub version: Option<String>,
    pub os: String,
    pub install_hint: Option<String>,
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

pub fn locate(app_data: &Path) -> Option<PathBuf> {
    if let Ok(p) = which::which("heimdall") {
        return Some(p);
    }
    [
        app_data.join("heimdall").join(exe("heimdall")),
        PathBuf::from("/usr/bin/heimdall"),
        PathBuf::from("/usr/local/bin/heimdall"),
    ]
    .into_iter()
    .find(|c| c.is_file())
}

fn install_hint() -> Option<String> {
    if cfg!(windows) {
        return Some(
            "Pobierz Heimdall Suite (glassechidna/heimdall-frontend) i zainstaluj sterownik Zadig (WinUSB) dla trybu Download / download Heimdall Suite + Zadig".into(),
        );
    }
    let t = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let id = t
        .lines()
        .find_map(|l| l.strip_prefix("ID="))
        .map(|v| v.trim_matches('"').to_lowercase())
        .unwrap_or_default();
    let like = t
        .lines()
        .find_map(|l| l.strip_prefix("ID_LIKE="))
        .map(|v| v.trim_matches('"').to_lowercase())
        .unwrap_or_default();
    let is = |k: &str| id == k || like.split_whitespace().any(|x| x == k);
    Some(
        if is("arch") {
            "sudo pacman -S heimdall  (lub AUR: heimdall-git)"
        } else if is("debian") || is("ubuntu") {
            "sudo apt install heimdall-flash"
        } else if is("fedora") {
            "sudo dnf install heimdall"
        } else {
            "zainstaluj pakiet 'heimdall' z repozytorium dystrybucji / install heimdall"
        }
        .to_string(),
    )
}

pub async fn status(app_data: &Path) -> HeimdallStatus {
    let path = locate(app_data);
    let version = match &path {
        Some(p) => {
            let mut cmd = Command::new(p);
            hide_console(&mut cmd);
            cmd.arg("version");
            run_raw(cmd, 10, None)
                .await
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
        }
        None => None,
    };
    HeimdallStatus {
        install_hint: path.is_none().then(install_hint).flatten(),
        path: path.map(|p| p.to_string_lossy().to_string()),
        version,
        os: std::env::consts::OS.into(),
    }
}

/// Indeksuje i (opcjonalnie) ocenia zestaw wybranych paczek firmware.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirmwareSet {
    pub parts: Vec<FirmwareTar>,
    pub total_images: usize,
    pub total_size: u64,
    pub has_userdata: bool,
}

pub async fn inspect(paths: Vec<String>) -> Result<FirmwareSet> {
    let parts = tokio::task::spawn_blocking(move || -> Result<Vec<FirmwareTar>> {
        paths.iter().map(|p| samsung_tar::index(Path::new(p))).collect()
    })
    .await??;
    let total_images = parts.iter().map(|p| p.images.len()).sum();
    let total_size = parts.iter().map(|p| p.total_size).sum();
    let has_userdata = parts.iter().any(|p| p.role == TarRole::Userdata || p.role == TarRole::Csc);
    Ok(FirmwareSet {
        parts,
        total_images,
        total_size,
        has_userdata,
    })
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlashOptions {
    /// ścieżki do wybranych paczek (BL/AP/CP/CSC/HOME_CSC)
    pub parts: Vec<String>,
    #[serde(default = "yes")]
    pub verify_md5: bool,
    #[serde(default = "yes")]
    pub auto_reboot: bool,
    /// repartition - wymaga pliku PIT; NIEBEZPIECZNE
    #[serde(default)]
    pub repartition: bool,
    pub pit_path: Option<String>,
    /// potwierdzenie, że użytkownik rozumie ryzyko (wymóg z GUI)
    #[serde(default)]
    pub acknowledged: bool,
}

fn yes() -> bool {
    true
}

/// Skąd pochodzi PIT użyty do mapowania obrazów na partycje.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PitSource {
    /// plik wskazany przez użytkownika
    UserFile(PathBuf),
    /// `*.pit` z paczki firmware (zwykle CSC_OMC_...) - wyciągnięty do katalogu roboczego
    Firmware { tar: String, name: String, path: PathBuf },
    /// `heimdall download-pit --no-reboot` - sesja Download Mode zostaje otwarta
    Device(PathBuf),
}

impl PitSource {
    pub fn path(&self) -> &Path {
        match self {
            PitSource::UserFile(p) | PitSource::Device(p) => p,
            PitSource::Firmware { path, .. } => path,
        }
    }

    /// `--resume` tylko wtedy, gdy wcześniej działał `download-pit --no-reboot`:
    /// bootloader przyjmuje handshake ODIN/LOKE raz na wejście w Download Mode,
    /// więc drugi proces Heimdalla musi kontynuować istniejącą sesję.
    pub fn needs_resume(&self) -> bool {
        matches!(self, PitSource::Device(_))
    }
}

/// `*.pit` w wybranych paczkach - CSC ma pierwszeństwo, potem HOME_CSC i reszta.
pub fn find_firmware_pit(parts: &[FirmwareTar]) -> Option<(&FirmwareTar, &samsung_tar::TarImage)> {
    let rank = |r: TarRole| match r {
        TarRole::Csc => 0,
        TarRole::HomeCsc => 1,
        _ => 2,
    };
    let mut found: Vec<(&FirmwareTar, &samsung_tar::TarImage)> = parts
        .iter()
        .flat_map(|fw| {
            fw.images
                .iter()
                .filter(|i| i.size > 0 && i.name.to_ascii_lowercase().ends_with(".pit"))
                .map(move |i| (fw, i))
        })
        .collect();
    found.sort_by_key(|(fw, _)| rank(fw.role));
    found.into_iter().next()
}

/// Podpowiedź dla `Protocol initialisation failed` / zawieszonej sesji Download Mode.
pub fn protocol_hint() -> String {
    let mut h = String::from(
        "Telefon nie przyjął ponownego połączenia (sesja Download Mode jest już zużyta lub zawieszona). \
         Odłącz kabel, wyłącz telefon (Vol- + Power ~7 s), wejdź ponownie w Download Mode i uruchom flashowanie od nowa. / \
         Restart the phone into Download Mode and try again.",
    );
    if cfg!(target_os = "linux") {
        h.push_str(
            "\nLinux: ModemManager potrafi zająć port Samsunga (04E8) po zwolnieniu interfejsu - na czas flashowania: \
             sudo systemctl stop ModemManager (albo reguła udev z ENV{ID_MM_DEVICE_IGNORE}=\"1\" dla 04e8:685d).",
        );
    } else if cfg!(windows) {
        h.push_str(
            "\nWindows: zamknij Odin, Smart Switch i Kies (trzymają port), sterownik WinUSB (Zadig) musi być przypisany do 04E8:685D.",
        );
    }
    h
}

fn is_protocol_failure(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("protocol initialisation failed") || t.contains("protocol initialization failed")
}

/// Pobiera PIT z urządzenia przez Heimdall (download-pit --no-reboot).
/// Sprawdza kod wyjścia i wypisuje stderr/stdout Heimdalla - zamiast mylącego
/// "bad PIT header", gdy urządzenie jest w zawieszonej sesji.
async fn download_pit(heimdall: &Path, dir: &Path, emitter: &Emitter) -> Result<PathBuf> {
    let pit = dir.join("device.pit");
    let _ = tokio::fs::remove_file(&pit).await;
    let mut cmd = Command::new(heimdall);
    hide_console(&mut cmd);
    cmd.args(["download-pit", "--output"])
        .arg(&pit)
        .arg("--no-reboot");
    emitter.info("Heimdall: pobieram PIT z urządzenia (download-pit --no-reboot) / downloading PIT...");
    let out = run_raw(cmd, 60, None).await?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let text = text.trim().to_string();
    let size = tokio::fs::metadata(&pit).await.map(|m| m.len()).unwrap_or(0);
    if out.status != 0 || size < 28 {
        for l in text.lines().filter(|l| !l.trim().is_empty()) {
            emitter.log(LogLevel::Error, format!("[heimdall] {}", l.trim()));
        }
        if is_protocol_failure(&text) {
            emitter.warn(protocol_hint());
        }
        return Err(AdbError::Command(format!(
            "heimdall download-pit nie powiodło się (kod {}, PIT {size} B) - sprawdź log; urządzenie w Download Mode? sterownik/udev? / download-pit failed: {}",
            out.status,
            text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("")
        )));
    }
    Ok(pit)
}

/// Argumenty `heimdall flash`. `--resume` wyłącznie po `download-pit --no-reboot`.
pub fn build_flash_args(
    pit: &PitSource,
    repartition: bool,
    auto_reboot: bool,
    flash: &[(String, PathBuf)],
) -> Vec<String> {
    let mut args: Vec<String> = vec!["flash".into()];
    if pit.needs_resume() {
        args.push("--resume".into());
    }
    if repartition {
        args.push("--REPARTITION".into());
        args.push("--pit".into());
        args.push(pit.path().to_string_lossy().to_string());
    }
    if !auto_reboot {
        args.push("--no-reboot".into());
    }
    for (part, file) in flash {
        args.push(format!("--{part}"));
        args.push(file.to_string_lossy().to_string());
    }
    args
}

/// Obraz z paczki do wgrania (przed mapowaniem na partycję).
struct Candidate {
    tar_path: String,
    tar_name: String,
    img: samsung_tar::TarImage,
}

/// Rozpakowuje obraz (strumieniowo, `.lz4` w locie) do `work/<nazwa bez .lz4>`.
async fn extract_candidate(
    c: &Candidate,
    work: &Path,
    emitter: &Emitter,
    cancel: &CancelToken,
    tracker: &Arc<Tracker>,
) -> Result<(PathBuf, u64)> {
    let target = samsung_tar::target_name(&c.img.name);
    let dest = work.join(&target);
    if samsung_tar::is_lz4(&c.img.name) {
        emitter.info(format!(
            "{} -> {target} (dekompresja LZ4 / decompressing, {})",
            c.img.name,
            crate::adb::pull::human_bytes(c.img.size)
        ));
    } else {
        emitter.info(format!("{} ({})", c.img.name, crate::adb::pull::human_bytes(c.img.size)));
    }
    tracker.set_current(format!(
        "{} {}",
        if samsung_tar::is_lz4(&c.img.name) { "Dekompresja / decompressing" } else { "Wyciąganie / extracting" },
        c.img.name
    ));
    let (tp, im, d, cn, tr) = (PathBuf::from(&c.tar_path), c.img.clone(), dest.clone(), cancel.clone(), tracker.clone());
    let written = tokio::task::spawn_blocking(move || {
        samsung_tar::extract_one_with(&tp, &im, &d, &cn, &|n| tr.add_bytes(n))
    })
    .await??;
    tracker.item_done();
    if samsung_tar::is_lz4(&c.img.name) {
        emitter.info(format!("  {target}: {}", crate::adb::pull::human_bytes(written)));
    }
    Ok((dest, written))
}

pub async fn run_flash(
    app_data: PathBuf,
    opts: FlashOptions,
    emitter: Emitter,
    cancel: CancelToken,
) -> Result<()> {
    if !opts.acknowledged {
        return Err(AdbError::InvalidArg("brak potwierdzenia ryzyka / not acknowledged".into()));
    }
    // przez całe zadanie (MD5, dekompresja, Heimdall) aplikacja nie dotyka USB
    let _usb_pause = crate::hardware::usb::PollPause::acquire();
    let heimdall = locate(&app_data).ok_or_else(|| {
        AdbError::BinaryNotFound(format!(
            "heimdall - {}",
            install_hint().unwrap_or_default()
        ))
    })?;
    if opts.parts.is_empty() {
        return Err(AdbError::InvalidArg("nie wybrano paczek firmware / no firmware selected".into()));
    }
    if opts.repartition && opts.pit_path.is_none() {
        return Err(AdbError::InvalidArg(
            "repartition wymaga pliku PIT / repartition needs a PIT file".into(),
        ));
    }

    // 1. indeks + weryfikacja MD5
    let set = inspect(opts.parts.clone()).await?;
    emitter.info(format!(
        "Firmware: {} paczek, {} obrazów, {} / parts, images",
        set.parts.len(),
        set.total_images,
        crate::adb::pull::human_bytes(set.total_size)
    ));
    if opts.verify_md5 {
        for fw in &set.parts {
            if cancel.is_cancelled() {
                return Err(AdbError::Cancelled);
            }
            if fw.has_md5 {
                let ok = samsung_tar::verify_md5(fw.clone(), emitter.clone(), cancel.clone()).await?;
                if !ok {
                    return Err(AdbError::Other(format!(
                        "Błąd sumy kontrolnej MD5: {} - flashowanie wstrzymane / MD5 checksum error",
                        fw.filename
                    )));
                }
            } else {
                emitter.warn(format!("{}: brak sumy MD5 (nie zweryfikowano) / no MD5", fw.filename));
            }
        }
    }

    // 2. katalog roboczy na wyciągnięte obrazy
    let work = app_data.join("samsung-flash-tmp");
    let _ = tokio::fs::remove_dir_all(&work).await;
    tokio::fs::create_dir_all(&work).await?;
    let res = flash_inner(&heimdall, &opts, &set, &work, &emitter, &cancel).await;
    let _ = tokio::fs::remove_dir_all(&work).await;
    res
}

async fn flash_inner(
    heimdall: &Path,
    opts: &FlashOptions,
    set: &FirmwareSet,
    work: &Path,
    emitter: &Emitter,
    cancel: &CancelToken,
) -> Result<()> {
    // 3. PIT: plik użytkownika > *.pit z paczki (CSC) > download-pit (na końcu, tuż przed flash)
    let mut pit_source: Option<PitSource> = None;
    let mut pit: Option<loke::PitData> = None;
    if let Some(p) = &opts.pit_path {
        let src = PitSource::UserFile(PathBuf::from(p));
        let data = loke::parse_pit(&tokio::fs::read(src.path()).await?)?;
        emitter.info(format!("PIT z pliku / PIT file: {p}"));
        pit = Some(data);
        pit_source = Some(src);
    } else if let Some((fw, img)) = find_firmware_pit(&set.parts) {
        let dest = work.join("firmware.pit");
        let (tp, im, d, cn) = (PathBuf::from(&fw.path), img.clone(), dest.clone(), cancel.clone());
        tokio::task::spawn_blocking(move || samsung_tar::extract_one(&tp, &im, &d, &cn)).await??;
        match loke::parse_pit(&tokio::fs::read(&dest).await?) {
            Ok(data) => {
                emitter.info(format!(
                    "PIT z paczki {} ({}) - bez download-pit, heimdall flash otworzy jedyną sesję USB / PIT from firmware",
                    fw.filename, img.name
                ));
                pit = Some(data);
                pit_source = Some(PitSource::Firmware { tar: fw.filename.clone(), name: img.name.clone(), path: dest });
            }
            Err(e) => emitter.warn(format!(
                "{} w {} nieczytelny ({e}) - PIT zostanie pobrany z urządzenia / unreadable PIT in firmware",
                img.name, fw.filename
            )),
        }
    } else {
        emitter.info(
            "W paczkach brak pliku .pit (np. sam AP) - PIT pobiorę z urządzenia tuż przed flashowaniem (heimdall flash --resume) / no PIT in firmware",
        );
    }
    if let Some(p) = &pit {
        check_pit(p, emitter)?;
    }

    // 4. obrazy do wgrania
    let mut candidates: Vec<Candidate> = Vec::new();
    for fw in &set.parts {
        for img in &fw.images {
            // pomijamy metadane, które nie są partycjami
            let low = img.name.to_lowercase();
            if low.ends_with(".pit") || low == "meta-data" || low.starts_with("meta") {
                continue;
            }
            candidates.push(Candidate { tar_path: fw.path.clone(), tar_name: fw.filename.clone(), img: img.clone() });
        }
    }

    // 5. ekstrakcja (+ dekompresja .lz4). Gdy PIT jest znany - tylko pasujące obrazy.
    //    Bez PIT: najpierw wszystko, potem download-pit i od razu flash (krótka przerwa w sesji USB).
    emitter.info("Wyciągam obrazy z paczek (strumieniowo, .lz4 rozpakowywane) / extracting images...");
    let wanted: Vec<usize> = (0..candidates.len())
        .filter(|&i| pit.as_ref().map_or(true, |p| match_pit(p, &candidates[i].img.name).is_some()))
        .collect();
    // pasek: bajty archiwów do przeczytania (skompresowane), licznik: obrazy
    let ex_tracker = Tracker::new(wanted.len() as u64, wanted.iter().map(|&i| candidates[i].img.size).sum());
    let ex_ticker = ex_tracker.start(emitter.clone());
    let mut extracted: Vec<(usize, PathBuf, u64)> = Vec::new();
    let mut ex_err = None;
    for &i in &wanted {
        if cancel.is_cancelled() {
            ex_err = Some(AdbError::Cancelled);
            break;
        }
        match extract_candidate(&candidates[i], work, emitter, cancel, &ex_tracker).await {
            Ok((dest, size)) => extracted.push((i, dest, size)),
            Err(e) => {
                ex_err = Some(e);
                break;
            }
        }
    }
    ex_tracker.shutdown(ex_ticker).await;
    if let Some(e) = ex_err {
        return Err(e);
    }

    let pit_source = match pit_source {
        Some(s) => s,
        None => {
            if cancel.is_cancelled() {
                return Err(AdbError::Cancelled);
            }
            let path = download_pit(heimdall, work, emitter).await?;
            let data = loke::parse_pit(&tokio::fs::read(&path).await?).map_err(|e| {
                AdbError::Parse(format!("PIT pobrany z urządzenia jest nieczytelny ({e}) - spróbuj ponownie po restarcie do Download Mode"))
            })?;
            check_pit(&data, emitter)?;
            pit = Some(data);
            PitSource::Device(path)
        }
    };
    let pit = pit.expect("PIT ustalony powyżej");

    // 6. mapowanie obraz -> partycja
    let mut flash_args: Vec<(String, PathBuf)> = Vec::new(); // (PARTITION, plik)
    let mut flash_sizes: Vec<u64> = Vec::new(); // rozmiar po dekompresji (postęp ważony bajtami)
    let mut skipped: Vec<String> = Vec::new();
    for (idx, c) in candidates.iter().enumerate() {
        let Some(part) = match_pit(&pit, &c.img.name) else {
            emitter.warn(format!(
                "Pomijam {} (szukano '{}') - brak w PIT / not in PIT",
                c.img.name,
                samsung_tar::target_name(&c.img.name)
            ));
            skipped.push(c.img.name.clone());
            continue;
        };
        let Some((_, dest, size)) = extracted.iter().find(|(i, _, _)| *i == idx) else {
            continue;
        };
        // ta sama partycja w dwóch paczkach (np. CSC i HOME_CSC) - wygrywa późniejsza
        if let Some(prev) = flash_args.iter_mut().find(|(p, _)| p == &part.partition_name) {
            emitter.warn(format!(
                "{}: partycja {} już wybrana - używam pliku z {} / duplicate partition",
                c.img.name, part.partition_name, c.tar_name
            ));
            prev.1 = dest.clone();
            if let Some(pos) = flash_args.iter().position(|(p, _)| p == &part.partition_name) {
                flash_sizes[pos] = *size;
            }
        } else {
            flash_args.push((part.partition_name.clone(), dest.clone()));
            flash_sizes.push(*size);
        }
    }
    // obrazy wyciągnięte "na zapas" (bez PIT), które nie pasują - zwolnij miejsce
    for (_, dest, _) in &extracted {
        if !flash_args.iter().any(|(_, f)| f == dest) {
            let _ = tokio::fs::remove_file(dest).await;
        }
    }
    if flash_args.is_empty() {
        let known: Vec<&str> = pit
            .entries
            .iter()
            .filter(|e| !e.flash_filename.is_empty())
            .map(|e| e.flash_filename.as_str())
            .take(40)
            .collect();
        return Err(AdbError::Other(format!(
            "Żaden obraz nie pasuje do PIT / no images matched PIT.\nObrazy: {}\nPliki w PIT: {}",
            skipped.join(", "),
            if known.is_empty() { "(brak nazw plików w PIT)".to_string() } else { known.join(", ") }
        )));
    }

    // 7. heimdall flash (z --resume wyłącznie po download-pit)
    let args = build_flash_args(&pit_source, opts.repartition, opts.auto_reboot, &flash_args);
    emitter.info(format!(
        "heimdall flash ({} partycji{}) / partitions...",
        flash_args.len(),
        if pit_source.needs_resume() { ", --resume: kontynuacja sesji po download-pit" } else { "" }
    ));
    for (p, _) in &flash_args {
        emitter.info(format!("  --{p}"));
    }

    let progress = FlashProgress::new(
        flash_args.iter().zip(&flash_sizes).map(|((p, _), s)| (p.clone(), *s)).collect(),
    );
    let tracker = Tracker::new(progress.total_count(), progress.total_bytes());
    tracker.set_current(progress.label());
    let ticker = tracker.start(emitter.clone());
    let result = spawn_heimdall(heimdall, &args, emitter, &tracker, progress, cancel).await;
    tracker.shutdown(ticker).await;
    result?;
    emitter.ok("Flashowanie zakończone pomyślnie / flashing complete.");
    Ok(())
}

/// Kontrola PIT przed użyciem (pusty / ucięty) + diagnostyka w logu.
fn check_pit(pit: &loke::PitData, emitter: &Emitter) -> Result<()> {
    log_pit(pit, emitter);
    if pit.entries.is_empty() {
        return Err(AdbError::Parse(format!(
            "PIT nie zawiera żadnych wpisów (nagłówek: {}) - przerwano / empty PIT",
            pit.entry_count
        )));
    }
    Ok(())
}

/// Wpis PIT dla obrazu z paczki: porównanie z `flash_filename` bez rozszerzenia
/// `.lz4`, bez katalogu i bez rozróżniania wielkości liter (jak Odin).
pub fn match_pit<'a>(pit: &'a loke::PitData, image_name: &str) -> Option<&'a loke::PitEntry> {
    let want = samsung_tar::target_name(image_name);
    if want.is_empty() {
        return None;
    }
    pit.entries.iter().find(|e| {
        !e.flash_filename.is_empty() && samsung_tar::target_name(&e.flash_filename).eq_ignore_ascii_case(&want)
    })
}

/// Diagnostyka PIT w logu operacji: liczba wpisów i mapowanie partycja <- plik.
fn log_pit(pit: &loke::PitData, emitter: &Emitter) {
    let with_file = pit.entries.iter().filter(|e| !e.flash_filename.is_empty()).count();
    emitter.info(format!(
        "Odczytano {} wpisów z tablicy PIT (nagłówek: {}, z nazwą pliku: {with_file}, projekt: {}) / PIT entries read",
        pit.entries.len(),
        pit.entry_count,
        if pit.project_name.is_empty() { "?" } else { &pit.project_name }
    ));
    if pit.entries.len() as u32 != pit.entry_count {
        emitter.warn(format!(
            "PIT zapowiada {} wpisów, odczytano {} - plik PIT może być ucięty / truncated PIT",
            pit.entry_count,
            pit.entries.len()
        ));
    }
    let map: Vec<String> = pit
        .entries
        .iter()
        .filter(|e| !e.flash_filename.is_empty())
        .map(|e| format!("{}={}", e.partition_name, e.flash_filename))
        .collect();
    for chunk in map.chunks(8) {
        emitter.info(format!("  PIT: {}", chunk.join("  ")));
    }
}

/// `stdbuf -o0 -e0` (coreutils) - wyłącza blokowe buforowanie stdout Heimdalla na pipe,
/// żeby procenty 5 GB `SUPER` spływały na bieżąco. Tylko Linux/macOS, gdy dostępny.
fn stdbuf_path() -> Option<PathBuf> {
    if cfg!(windows) {
        return None;
    }
    which::which("stdbuf").ok()
}

fn heimdall_command(heimdall: &Path, args: &[String], stdbuf: Option<&Path>) -> Command {
    let mut cmd = match stdbuf {
        Some(sb) => {
            let mut c = Command::new(sb);
            c.args(["-o0", "-e0"]).arg(heimdall);
            c
        }
        None => Command::new(heimdall),
    };
    hide_console(&mut cmd);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd
}

/// Uruchamia heimdall i czyta wyjście SUROWYMI porcjami bajtów (bez czekania na `\n`):
/// procenty (`0%\x08\x0825%...`) -> pasek postępu ważony bajtami, czyste linie statusu -> log.
async fn spawn_heimdall(
    heimdall: &Path,
    args: &[String],
    emitter: &Emitter,
    tracker: &Arc<Tracker>,
    progress: FlashProgress,
    cancel: &CancelToken,
) -> Result<()> {
    let stdbuf = stdbuf_path();
    let mut child = match heimdall_command(heimdall, args, stdbuf.as_deref()).spawn() {
        Ok(c) => c,
        // stdbuf jest, ale nie da się go uruchomić - bezpośrednio
        Err(_) if stdbuf.is_some() => heimdall_command(heimdall, args, None)
            .spawn()
            .map_err(|e| AdbError::Io(format!("heimdall: {e}")))?,
        Err(e) => return Err(AdbError::Io(format!("heimdall: {e}"))),
    };

    // oba strumienie -> jeden kanał zdarzeń (kolejność w obrębie strumienia zachowana)
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<HeimdallEvent>();
    let pipe = |mut r: Box<dyn tokio::io::AsyncRead + Unpin + Send>, tx: tokio::sync::mpsc::UnboundedSender<HeimdallEvent>| {
        tokio::spawn(async move {
            let mut parser = HeimdallParser::new();
            let mut buf = [0u8; 4096];
            loop {
                match r.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        for ev in parser.feed(&buf[..n]) {
                            let _ = tx.send(ev);
                        }
                    }
                }
            }
            for ev in parser.finish() {
                let _ = tx.send(ev);
            }
        })
    };
    let mut readers = Vec::new();
    if let Some(o) = child.stdout.take() {
        readers.push(pipe(Box::new(o), tx.clone()));
    }
    if let Some(e) = child.stderr.take() {
        readers.push(pipe(Box::new(e), tx.clone()));
    }
    drop(tx);

    let em = emitter.clone();
    let tr = tracker.clone();
    let consumer = tokio::spawn(async move {
        let mut progress = progress;
        let mut proto_failed = false;
        while let Some(ev) = rx.recv().await {
            match &ev {
                HeimdallEvent::Line(l) => {
                    let low = l.to_lowercase();
                    if is_protocol_failure(&low) {
                        proto_failed = true;
                    }
                    let level = if low.contains("error") || low.contains("failed") {
                        LogLevel::Error
                    } else if low.contains("warning") {
                        LogLevel::Warn
                    } else {
                        LogLevel::Info
                    };
                    em.log(level, format!("[heimdall] {l}"));
                }
                HeimdallEvent::Uploading(p) => em.info(format!("[heimdall] Uploading {p}")),
                HeimdallEvent::Uploaded(p) => em.ok(format!("[heimdall] {p} upload successful")),
                HeimdallEvent::Percent(_) => {}
            }
            if progress.apply(&ev) {
                tr.set_bytes(progress.bytes_done());
                tr.set_done(progress.done_count());
                tr.set_current(progress.label());
            }
        }
        proto_failed
    });

    let status = tokio::select! {
        s = child.wait() => s.map_err(|e| AdbError::Io(e.to_string()))?,
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            return Err(AdbError::Cancelled);
        }
    };
    // doczytaj resztę wyjścia (komunikat błędu bywa w ostatniej linii)
    for r in readers {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), r).await;
    }
    let proto_failed = tokio::time::timeout(std::time::Duration::from_secs(2), consumer)
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or(false);
    if status.success() {
        Ok(())
    } else if proto_failed {
        emitter.warn(protocol_hint());
        Err(AdbError::Command(format!(
            "heimdall: Protocol initialisation failed (kod {:?}) - telefon odrzucił połączenie; zrestartuj go do Download Mode (szczegóły w logu)",
            status.code()
        )))
    } else {
        Err(AdbError::Command(format!(
            "heimdall flash zakończył się błędem (kod {:?}) - sprawdź log / heimdall failed",
            status.code()
        )))
    }
}

/// Zadanie w tle: natywny odczyt PIT (bez Heimdalla) - deleguje do `loke`.
pub async fn run_read_pit(save_path: Option<String>, emitter: Emitter, cancel: CancelToken) {
    loke::run_read_pit(save_path, emitter, cancel).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_nonempty() {
        assert!(super::install_hint().is_some());
    }

    /// Binarny PIT jak z `heimdall download-pit` (magic 0x12349876, wpisy po 132 B).
    fn fake_pit(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&0x1234_9876u32.to_le_bytes());
        b.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        b.extend_from_slice(b"COM_TAR2");
        b.extend_from_slice(b"SM5345\0\0");
        b.extend_from_slice(&[0u8; 4]);
        for (i, (part, file)) in entries.iter().enumerate() {
            let mut e = [0u8; 132];
            e[8..12].copy_from_slice(&(i as u32 + 1).to_le_bytes());
            e[24..28].copy_from_slice(&1024u32.to_le_bytes());
            e[36..36 + part.len()].copy_from_slice(part.as_bytes());
            e[68..68 + file.len()].copy_from_slice(file.as_bytes());
            b.extend_from_slice(&e);
        }
        b
    }

    #[test]
    fn matches_lz4_images_to_pit() {
        let raw = fake_pit(&[
            ("BOOTLOADER", "sboot.bin"),
            ("BOOT", "boot.img"),
            ("SUPER", "super.img"),
            ("RADIO", "modem.bin"),
            ("VBMETA", "VBMETA.IMG"),
            ("EFS", ""),
        ]);
        let pit = loke::parse_pit(&raw).unwrap();
        assert_eq!(pit.entries.len(), 6);
        assert_eq!(pit.entry_count, 6);

        let part = |n: &str| match_pit(&pit, n).map(|e| e.partition_name.as_str());
        assert_eq!(part("boot.img.lz4"), Some("BOOT"));
        assert_eq!(part("super.img.lz4"), Some("SUPER"));
        assert_eq!(part("sboot.bin.lz4"), Some("BOOTLOADER"));
        assert_eq!(part("BOOT.IMG.LZ4"), Some("BOOT"));
        assert_eq!(part("vbmeta.img.lz4"), Some("VBMETA"));
        assert_eq!(part("modem.bin"), Some("RADIO"));
        assert_eq!(part("recovery.img.lz4"), None);
        // pusta nazwa pliku w PIT nie pasuje do niczego
        assert_eq!(part(".lz4"), None);
        assert_eq!(part(""), None);
    }

    fn tar_with(dir: &Path, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let mut data = Vec::new();
        for (fname, body) in files {
            let mut h = [0u8; 512];
            h[..fname.len()].copy_from_slice(fname.as_bytes());
            let sz = format!("{:011o}\0", body.len());
            h[124..124 + sz.len()].copy_from_slice(sz.as_bytes());
            h[156] = b'0';
            data.extend_from_slice(&h);
            data.extend_from_slice(body);
            data.extend_from_slice(&vec![0u8; (512 - body.len() % 512) % 512]);
        }
        data.extend_from_slice(&[0u8; 1024]);
        let p = dir.join(name);
        std::fs::write(&p, data).unwrap();
        p
    }

    /// `.pit` z CSC wygrywa z innymi paczkami; da się go wyciągnąć i sparsować.
    #[test]
    fn finds_and_extracts_pit_from_csc() {
        let dir = std::env::temp_dir().join(format!("adbst_pit_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pit_csc = fake_pit(&[("BOOT", "boot.img"), ("SUPER", "super.img")]);
        let pit_other = fake_pit(&[("BOOT", "boot.img")]);
        let ap = tar_with(&dir, "AP_G525F.tar.md5", &[("boot.img.lz4", b"x"), ("odd.pit", &pit_other)]);
        let csc = tar_with(&dir, "CSC_OXM_G525F.tar.md5", &[("cache.img.lz4", b"y"), ("LSI3830.pit", &pit_csc)]);
        let bl = tar_with(&dir, "BL_G525F.tar.md5", &[("sboot.bin.lz4", b"z")]);

        let parts: Vec<FirmwareTar> =
            [&bl, &ap, &csc].iter().map(|p| samsung_tar::index(p).unwrap()).collect();
        let (fw, img) = find_firmware_pit(&parts).expect("pit");
        assert_eq!(fw.role, TarRole::Csc);
        assert_eq!(img.name, "LSI3830.pit");

        let dest = dir.join("firmware.pit");
        samsung_tar::extract_one(Path::new(&fw.path), img, &dest, &CancelToken::new()).unwrap();
        let pit = loke::parse_pit(&std::fs::read(&dest).unwrap()).unwrap();
        assert_eq!(pit.entries.len(), 2);
        assert_eq!(pit.entries[1].flash_filename, "super.img");

        // sam AP + BL bez .pit -> brak (trzeba download-pit)
        let no_pit: Vec<FirmwareTar> = [&bl].iter().map(|p| samsung_tar::index(p).unwrap()).collect();
        assert!(find_firmware_pit(&no_pit).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `--resume` tylko po `download-pit --no-reboot`; PIT z pliku / CSC - bez.
    #[test]
    fn resume_only_after_download_pit() {
        let flash = vec![("BOOT".to_string(), PathBuf::from("/w/boot.img"))];
        let device = PitSource::Device(PathBuf::from("/w/device.pit"));
        let csc = PitSource::Firmware { tar: "CSC.tar.md5".into(), name: "x.pit".into(), path: PathBuf::from("/w/firmware.pit") };
        let user = PitSource::UserFile(PathBuf::from("/home/u/my.pit"));

        let a = build_flash_args(&device, false, true, &flash);
        assert_eq!(a, ["flash", "--resume", "--BOOT", "/w/boot.img"]);
        let b = build_flash_args(&csc, false, false, &flash);
        assert_eq!(b, ["flash", "--no-reboot", "--BOOT", "/w/boot.img"]);
        assert!(!build_flash_args(&user, false, true, &flash).contains(&"--resume".to_string()));
        // repartition używa wskazanego pliku PIT
        let r = build_flash_args(&user, true, true, &flash);
        assert_eq!(&r[..4], ["flash", "--REPARTITION", "--pit", "/home/u/my.pit"]);
        assert!(device.needs_resume() && !csc.needs_resume() && !user.needs_resume());
    }

    #[test]
    fn detects_protocol_failure() {
        assert!(is_protocol_failure("ERROR: Protocol initialisation failed!"));
        assert!(!is_protocol_failure("Initialising protocol..."));
        assert!(protocol_hint().contains("Download Mode"));
    }

    /// E2E z atrapą heimdalla: procenty bez '\n' (z backspace) muszą dotrzeć do paska
    /// W TRAKCIE wgrywania, licznik rośnie po "upload successful", log bez \x08.
    #[cfg(unix)]
    #[tokio::test]
    async fn live_progress_from_fake_heimdall() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("adbst_hd_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("heimdall");
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             printf 'Initialising protocol...\\nProtocol initialisation successful.\\n\\nUploading BOOT\\n'\n\
             printf '0%%\\b\\b25%%\\b\\b\\b100%%\\n'\n\
             printf 'BOOT upload successful\\n\\nUploading SUPER\\n0%%'\n\
             sleep 0.4; printf '\\b\\b47%%'\n\
             sleep 0.6; printf '\\b\\b\\b100%%\\nSUPER upload successful\\n\\nEnding session...\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let events: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let ev = events.clone();
        let ch = tauri::ipc::Channel::new(move |body| {
            if let tauri::ipc::InvokeResponseBody::Json(s) = body {
                ev.lock().unwrap().push(s);
            }
            Ok(())
        });
        let emitter = Emitter::new(ch);
        let progress = FlashProgress::new(vec![("BOOT".into(), 100), ("SUPER".into(), 900)]);
        let tracker = Tracker::new(progress.total_count(), progress.total_bytes());
        let ticker = tracker.start(emitter.clone());
        spawn_heimdall(&fake, &[], &emitter, &tracker, progress, &CancelToken::new()).await.unwrap();
        tracker.shutdown(ticker).await;

        let evs: Vec<serde_json::Value> =
            events.lock().unwrap().iter().filter_map(|s| serde_json::from_str(s).ok()).collect();
        let prog: Vec<&serde_json::Value> = evs.iter().filter(|v| v["kind"] == "progress").collect();
        // w trakcie SUPER (47%) - przed '\n' i przed "upload successful"
        let mid = prog
            .iter()
            .find(|v| v["current"].as_str().unwrap_or("").starts_with("SUPER (47%)"))
            .unwrap_or_else(|| panic!("brak postępu 47%: {prog:#?}"));
        assert_eq!(mid["done"], 1);
        assert_eq!(mid["bytesDone"], 100 + 900 * 47 / 100);
        let last = prog.last().unwrap();
        assert_eq!((last["done"].as_u64(), last["total"].as_u64()), (Some(2), Some(2)));
        assert_eq!(last["bytesDone"], 1000);
        for v in evs.iter().filter(|v| v["kind"] == "log") {
            let m = v["message"].as_str().unwrap_or("");
            assert!(!m.contains('\u{8}') && !m.trim_end().ends_with('%'), "śmieci w logu: {m:?}");
        }
        assert!(evs.iter().any(|v| v["message"] == "[heimdall] SUPER upload successful"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn poll_pause_is_scoped() {
        {
            let _g = crate::hardware::usb::PollPause::acquire();
            assert!(crate::hardware::usb::poll_paused());
        }
    }
}
