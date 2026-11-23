//! APK Inspector & DevTools - legalna, deweloperska modyfikacja struktury APK:
//! flaga `debuggable`, zmiana nazwy pakietu (klon), etykieta, podmiana grafik
//! i assetów. Bez ingerencji w kod (smali/dex), licencje czy weryfikację podpisu.
//!
//! Łańcuch: Pull APK z urządzenia (lub plik) -> Dekompilacja (apktool d) ->
//! Edycja -> Budowanie (apktool b) -> Podpisanie (uber-apk-signer + własny
//! klucz debug) -> Instalacja (adb install / install-multiple).
//! Każdy etap zwraca `Result`; błąd zatrzymuje łańcuch z czytelnym opisem
//! (który etap, co poszło nie tak, co zrobić).

pub mod manifest;
pub mod runner;
pub mod toolchain;
pub mod workspace;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::adb::apps::pm_path;
use crate::adb::events::{Emitter, FailedItem};
use crate::adb::Adb;
use crate::error::{AdbError, Result};
use crate::utils::{is_valid_package, sanitize_filename, shell_quote, CancelToken};

use manifest::PatchOptions;
use runner::{explain, explain_install, run_jar, step, Stepper};
use toolchain::{Toolchain, KS_ALIAS, KS_PASS};
use workspace::{BuildInfo, Workspace, WorkspaceMeta};

/// Katalogi modułu: narzędzia (trwałe, app_data) i przestrzenie robocze (cache).
#[derive(Debug, Clone)]
pub struct Paths {
    pub tools: PathBuf,
    pub root: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum OpenSource {
    /// Aplikacja zainstalowana na urządzeniu.
    Device { serial: String, package: String },
    /// Plik .apk / .apks / .xapk albo folder z base.apk + split_*.apk.
    /// `serial` (opcjonalnie) - urządzenie, z którego w razie potrzeby pobrać framework.
    File { path: String, serial: Option<String> },
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct BuildOptions {
    pub patch: PatchOptions,
    pub install: bool,
    pub serial: Option<String>,
    /// Odinstaluj oryginał przed instalacją (inny podpis). Usuwa dane aplikacji!
    pub uninstall_first: bool,
    pub export_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildResult {
    pub workspace: String,
    pub package: String,
    pub files: Vec<String>,
    pub exported: Vec<String>,
    pub changes: Vec<String>,
    pub installed: bool,
}

const DECODE_TIMEOUT: u64 = 1200;
const BUILD_TIMEOUT: u64 = 2400;
const SIGN_TIMEOUT: u64 = 900;

// --------------------------------------------------------------------------- //
// Zadania (wywoływane z lib.rs przez spawn_job)
// --------------------------------------------------------------------------- //

/// Wspólne zakończenie zadania: sukces / anulowanie / czytelny błąd etapu.
async fn conclude(stepper: Arc<Stepper>, ticker: tokio::task::JoinHandle<()>, emitter: &Emitter, res: Result<()>) {
    let failed_step = stepper.current_key();
    stepper.shutdown(ticker).await;
    match res {
        Ok(()) => {
            stepper.complete();
            emitter.finished(1, 0, 1, vec![], false);
        }
        Err(AdbError::Cancelled) => {
            emitter.warn("Anulowano / cancelled.");
            emitter.finished(0, 1, 1, vec![], true);
        }
        Err(e) => {
            let msg = match e {
                AdbError::Other(m) => m,
                other => other.to_string(),
            };
            emitter.error(msg.clone());
            emitter.item(format!("step:{failed_step}"), false, msg.clone());
            emitter.finished(0, 0, 1, vec![FailedItem { path: failed_step, error: msg }], false);
        }
    }
}

/// Tylko przygotowanie narzędzi (Java, apktool, signer, klucz).
pub async fn run_setup(paths: Paths, emitter: Emitter, cancel: CancelToken) {
    let stepper = Stepper::new(vec![step("tools", 1)], emitter.clone());
    let ticker = stepper.start();
    stepper.begin("tools", "");
    let res = toolchain::ensure(&paths.tools, &emitter, &cancel).await.map(|_| ());
    if res.is_ok() {
        emitter.ok("Narzędzia gotowe / toolchain ready.");
    }
    conclude(stepper, ticker, &emitter, res).await;
}

/// Pull/kopiowanie + dekompilacja. Wynik: item "workspace" = id.
pub async fn run_open(adb: Option<Arc<Adb>>, paths: Paths, source: OpenSource, emitter: Emitter, cancel: CancelToken) {
    let stepper = Stepper::new(
        vec![step("tools", 5), step("pull", 20), step("decode", 75)],
        emitter.clone(),
    );
    let ticker = stepper.start();
    let res = async {
        let tc = tools_step(&paths, &stepper, &emitter, &cancel).await?;
        let ws = open_step(adb.as_deref(), &paths, &tc, &source, &stepper, &emitter, &cancel).await?;
        emitter.item("workspace", true, ws);
        Ok(())
    }
    .await;
    conclude(stepper, ticker, &emitter, res).await;
}

/// Edycja + budowanie + podpis (+ instalacja, + eksport) istniejącej przestrzeni.
pub async fn run_build(
    adb: Option<Arc<Adb>>,
    paths: Paths,
    id: String,
    opts: BuildOptions,
    emitter: Emitter,
    cancel: CancelToken,
) {
    let mut steps = vec![step("tools", 3), step("patch", 2), step("build", 55), step("sign", 20)];
    if opts.install {
        steps.push(step("install", 20));
    }
    let stepper = Stepper::new(steps, emitter.clone());
    let ticker = stepper.start();
    let res = async {
        let tc = tools_step(&paths, &stepper, &emitter, &cancel).await?;
        let ws = Workspace::open(&paths.root, &id)?;
        let r = build_steps(adb.as_deref(), &tc, &ws, &opts, &stepper, &emitter, &cancel).await?;
        emit_result(&emitter, &r);
        Ok(())
    }
    .await;
    conclude(stepper, ticker, &emitter, res).await;
}

/// Pełny automat: urządzenie -> dekompilacja -> edycja -> build -> podpis -> instalacja.
pub async fn run_auto(
    adb: Option<Arc<Adb>>,
    paths: Paths,
    serial: String,
    package: String,
    mut opts: BuildOptions,
    emitter: Emitter,
    cancel: CancelToken,
) {
    opts.install = true;
    opts.serial = Some(serial.clone());
    let stepper = Stepper::new(
        vec![
            step("tools", 3),
            step("pull", 12),
            step("decode", 30),
            step("patch", 2),
            step("build", 30),
            step("sign", 10),
            step("install", 13),
        ],
        emitter.clone(),
    );
    let ticker = stepper.start();
    let res = async {
        let tc = tools_step(&paths, &stepper, &emitter, &cancel).await?;
        let source = OpenSource::Device { serial, package };
        let id = open_step(adb.as_deref(), &paths, &tc, &source, &stepper, &emitter, &cancel).await?;
        emitter.item("workspace", true, id.clone());
        let ws = Workspace::open(&paths.root, &id)?;
        let r = build_steps(adb.as_deref(), &tc, &ws, &opts, &stepper, &emitter, &cancel).await?;
        emit_result(&emitter, &r);
        Ok(())
    }
    .await;
    conclude(stepper, ticker, &emitter, res).await;
}

fn emit_result(emitter: &Emitter, r: &BuildResult) {
    let json = serde_json::to_string(r).unwrap_or_default();
    emitter.item("result", true, json);
}

// --------------------------------------------------------------------------- //
// Etapy
// --------------------------------------------------------------------------- //

async fn tools_step(paths: &Paths, stepper: &Stepper, emitter: &Emitter, cancel: &CancelToken) -> Result<Toolchain> {
    stepper.begin("tools", "");
    let tc = toolchain::ensure(&paths.tools, emitter, cancel).await?;
    stepper.finish_step();
    emitter.item("step:tools", true, "");
    Ok(tc)
}

fn need_adb(adb: Option<&Adb>) -> Result<&Adb> {
    adb.ok_or_else(|| AdbError::BinaryNotFound("adb".into()))
}

/// Pobiera APK z urządzenia albo kopiuje z dysku, potem dekompiluje.
/// Zwraca id przestrzeni roboczej. Przy błędzie/anulowaniu sprząta.
async fn open_step(
    adb: Option<&Adb>,
    paths: &Paths,
    tc: &Toolchain,
    source: &OpenSource,
    stepper: &Stepper,
    emitter: &Emitter,
    cancel: &CancelToken,
) -> Result<String> {
    tokio::fs::create_dir_all(&paths.root).await?;
    let (hint, serial) = match source {
        OpenSource::Device { serial, package } => {
            if !is_valid_package(package) {
                return Err(AdbError::InvalidArg(format!("pakiet / package: {package}")));
            }
            (package.clone(), Some(serial.clone()))
        }
        OpenSource::File { path, serial } => {
            let stem = Path::new(path)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "apk".into());
            (stem, serial.clone().filter(|s| !s.is_empty()))
        }
    };
    let id = Workspace::new_id(&hint);
    let ws = Workspace::create(&paths.root, &id)?;
    let res = async {
        stepper.begin("pull", hint.clone());
        let (apks, size) = match source {
            OpenSource::Device { serial, package } => {
                pull_from_device(need_adb(adb)?, serial, package, &ws.orig(), stepper, emitter, cancel).await?
            }
            OpenSource::File { path, .. } => {
                let (src, dst) = (PathBuf::from(path), ws.orig());
                tokio::task::spawn_blocking(move || import_files(&src, &dst)).await??
            }
        };
        stepper.finish_step();
        emitter.item("step:pull", true, "");
        emitter.ok(format!(
            "APK gotowe / ready: {} ({} plik(ów), {:.1} MB)",
            apks.join(", "),
            apks.len(),
            size as f64 / 1_048_576.0
        ));

        let mut meta = WorkspaceMeta {
            id: id.clone(),
            source_kind: if matches!(source, OpenSource::Device { .. }) { "device".into() } else { "file".into() },
            source: match source {
                OpenSource::Device { serial, .. } => serial.clone(),
                OpenSource::File { path, .. } => path.clone(),
            },
            created: chrono::Local::now().timestamp(),
            apks,
            size,
            ..Default::default()
        };
        ws.save(&meta)?;

        stepper.begin("decode", "apktool d");
        decode(adb, serial.as_deref(), tc, &ws, stepper, emitter, cancel).await?;
        ws.snapshot_originals()?;
        let info = ws.manifest_info()?;
        meta.package = info.package.clone();
        meta.version_name = info.version_name.clone();
        meta.label = resolve_label(&ws.src(), info.label.as_deref());
        meta.decoded = true;
        ws.save(&meta)?;
        stepper.finish_step();
        emitter.item("step:decode", true, "");
        emitter.ok(format!(
            "Zdekompilowano / decoded: {} {} ({} aktywności, {} usług, debuggable={})",
            meta.package,
            meta.version_name.clone().unwrap_or_default(),
            info.activities,
            info.services,
            info.debuggable
        ));
        Ok::<(), AdbError>(())
    }
    .await;
    if let Err(e) = res {
        let _ = tokio::fs::remove_dir_all(&ws.dir).await;
        return Err(e);
    }
    Ok(id)
}

/// `adb pull` wszystkich plików APK pakietu (base + splity) z postępem w bajtach.
async fn pull_from_device(
    adb: &Adb,
    serial: &str,
    package: &str,
    dest: &Path,
    stepper: &Stepper,
    emitter: &Emitter,
    cancel: &CancelToken,
) -> Result<(Vec<String>, u64)> {
    emitter.info(format!("{package}: szukam plików APK (pm path) / locating..."));
    let remotes = pm_path(adb, serial, package).await.map_err(AdbError::Other)?;
    // rozmiary do paska postępu
    let quoted: Vec<String> = remotes.iter().map(|r| shell_quote(r)).collect();
    let sizes: Vec<u64> = adb
        .shell(Some(serial), &format!("stat -c %s {}", quoted.join(" ")), 20)
        .await
        .map(|o| o.stdout.lines().filter_map(|l| l.trim().parse().ok()).collect())
        .unwrap_or_default();
    let total: u64 = sizes.iter().sum::<u64>().max(1);
    let mut done = 0u64;
    let mut names = Vec::new();
    for (i, remote) in remotes.iter().enumerate() {
        let fname = sanitize_filename(remote.rsplit('/').next().unwrap_or("base.apk"));
        let local = dest.join(&fname);
        let local_s = local.to_string_lossy().to_string();
        emitter.info(format!("adb pull {remote}"));
        let argv = ["pull", remote.as_str(), local_s.as_str()];
        let pull = adb.run_c(Some(serial), &argv, 900, cancel);
        tokio::pin!(pull);
        let mut iv = tokio::time::interval(Duration::from_millis(250));
        let out = loop {
            tokio::select! {
                r = &mut pull => break r?,
                _ = iv.tick() => {
                    let cur = std::fs::metadata(&local).map(|m| m.len()).unwrap_or(0);
                    stepper.exact((done + cur) as f64 / total as f64, Some(fname.clone()));
                }
            }
        };
        if !out.ok() || !local.is_file() {
            return Err(AdbError::Other(format!(
                "Nie udało się pobrać {remote} z urządzenia / pull failed: {}",
                out.err_text()
            )));
        }
        done += sizes.get(i).copied().unwrap_or_else(|| std::fs::metadata(&local).map(|m| m.len()).unwrap_or(0));
        names.push(fname);
    }
    names.sort_by_key(|n| (n != "base.apk", n.clone()));
    if names.first().map(|n| n != "base.apk").unwrap_or(true) {
        // pojedynczy APK o innej nazwie (stare Androidy: /data/app/pkg-1.apk)
        if names.len() == 1 {
            std::fs::rename(dest.join(&names[0]), dest.join("base.apk"))?;
            names[0] = "base.apk".into();
        } else {
            return Err(AdbError::Other("Nie rozpoznano base.apk wśród plików pakietu / base.apk not found".into()));
        }
    }
    Ok((names, done))
}

/// Kopiuje APK z dysku do orig/: plik .apk, archiwum .apks/.xapk/.apkm albo folder.
fn import_files(src: &Path, dest: &Path) -> Result<(Vec<String>, u64)> {
    let is_apk = |p: &Path| p.extension().map(|e| e.eq_ignore_ascii_case("apk")).unwrap_or(false);
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    if src.is_dir() {
        for e in std::fs::read_dir(src)?.flatten() {
            let p = e.path();
            if p.is_file() && is_apk(&p) {
                files.push((p.file_name().unwrap_or_default().to_string_lossy().to_string(), p));
            }
        }
    } else if src.is_file() && is_apk(src) {
        files.push(("base.apk".into(), src.to_path_buf()));
    } else if src.is_file() {
        let ext = src.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if !matches!(ext.as_str(), "apks" | "xapk" | "apkm" | "zip") {
            return Err(AdbError::InvalidArg(format!(
                "Nieobsługiwany plik (dozwolone: .apk, .apks, .xapk, folder z APK) / unsupported: {}",
                src.display()
            )));
        }
        // pakiet splitów - wypakuj wpisy *.apk z głównego katalogu archiwum
        let f = std::fs::File::open(src)?;
        let mut z = zip::ZipArchive::new(f).map_err(|e| {
            AdbError::Other(format!("Archiwum uszkodzone lub szyfrowane (.apkm) / corrupt or encrypted bundle: {e}"))
        })?;
        let mut out = Vec::new();
        let mut size = 0u64;
        for i in 0..z.len() {
            let mut entry = z.by_index(i).map_err(|e| AdbError::Parse(format!("zip: {e}")))?;
            let Some(rel) = entry.enclosed_name() else { continue };
            if entry.is_dir() || rel.components().count() != 1 || !is_apk(&rel) {
                continue;
            }
            let name = sanitize_filename(&rel.to_string_lossy());
            let mut w = std::fs::File::create(dest.join(&name))?;
            size += std::io::copy(&mut entry, &mut w)?;
            out.push(name);
        }
        return finish_import(dest, out, size);
    } else {
        return Err(AdbError::InvalidArg(format!("nie znaleziono / not found: {}", src.display())));
    }
    if files.is_empty() {
        return Err(AdbError::InvalidArg("Brak plików .apk w folderze / no .apk files".into()));
    }
    let mut out = Vec::new();
    let mut size = 0u64;
    for (name, p) in files {
        let name = sanitize_filename(&name);
        size += std::fs::copy(&p, dest.join(&name))?;
        out.push(name);
    }
    finish_import(dest, out, size)
}

/// Ustala base.apk w zestawie (folder/archiwum nie zawsze ma tę nazwę).
fn finish_import(dest: &Path, mut names: Vec<String>, size: u64) -> Result<(Vec<String>, u64)> {
    if names.is_empty() {
        return Err(AdbError::InvalidArg("Brak plików .apk / no .apk files".into()));
    }
    if !names.iter().any(|n| n == "base.apk") {
        let candidates: Vec<&String> = names
            .iter()
            .filter(|n| !n.starts_with("split_") && !n.starts_with("config.") && !n.contains(".config."))
            .collect();
        if candidates.len() != 1 {
            return Err(AdbError::InvalidArg(format!(
                "Nie można ustalić głównego APK (base.apk) wśród: {} / cannot find base apk",
                names.join(", ")
            )));
        }
        let c = candidates[0].clone();
        std::fs::rename(dest.join(&c), dest.join("base.apk"))?;
        for n in names.iter_mut() {
            if *n == c {
                *n = "base.apk".into();
            }
        }
    }
    names.sort_by_key(|n| (n != "base.apk", n.clone()));
    Ok((names, size))
}

/// `apktool d -s` (bez dekompilacji kodu do smali - szybko i bez ryzyka
/// zepsucia dex). Brak frameworku producenta -> pobierz z urządzenia i ponów.
async fn decode(
    adb: Option<&Adb>,
    serial: Option<&str>,
    tc: &Toolchain,
    ws: &Workspace,
    stepper: &Stepper,
    emitter: &Emitter,
    cancel: &CancelToken,
) -> Result<()> {
    tokio::fs::create_dir_all(&tc.frameworks).await?;
    let args = || -> Vec<OsString> {
        vec![
            "d".into(),
            "-s".into(),
            "-f".into(),
            "-p".into(),
            tc.frameworks.clone().into(),
            "-o".into(),
            ws.src().into(),
            ws.orig().join("base.apk").into(),
        ]
    };
    let run = run_jar(tc, &tc.apktool, args(), &ws.dir, emitter, Some(stepper), cancel, DECODE_TIMEOUT).await?;
    if run.ok() && ws.src().join("AndroidManifest.xml").is_file() {
        return Ok(());
    }
    let tail = run.tail_text();
    let framework_missing = tail.contains("CantFindFrameworkResException") || tail.contains("framework resources");
    if framework_missing {
        if let (Some(adb), Some(serial)) = (adb, serial) {
            emitter.warn("APK wymaga frameworku producenta - pobieram z urządzenia / fetching vendor framework...");
            stepper.begin("decode", "framework");
            if fetch_frameworks(adb, serial, tc, ws, emitter, cancel).await? > 0 {
                stepper.begin("decode", "apktool d");
                let run = run_jar(tc, &tc.apktool, args(), &ws.dir, emitter, Some(stepper), cancel, DECODE_TIMEOUT).await?;
                if run.ok() && ws.src().join("AndroidManifest.xml").is_file() {
                    return Ok(());
                }
                return Err(AdbError::Other(explain("decode", &run.tail_text())));
            }
        }
    }
    Err(AdbError::Other(explain("decode", &tail)))
}

/// Pobiera pliki frameworku (framework-res, twframework-res, miui...) z urządzenia
/// i rejestruje je w apktool (`apktool if`). Zwraca liczbę zaimportowanych.
async fn fetch_frameworks(
    adb: &Adb,
    serial: &str,
    tc: &Toolchain,
    ws: &Workspace,
    emitter: &Emitter,
    cancel: &CancelToken,
) -> Result<usize> {
    let list = adb
        .shell(
            Some(serial),
            "ls /system/framework/*.apk /system/framework/*/*.apk /system_ext/framework/*.apk \
             /product/framework/*.apk /vendor/framework/*.apk /system/app/miui/*.apk \
             /system/app/miuisystem/*.apk 2>/dev/null",
            30,
        )
        .await?;
    let wanted: Vec<String> = list
        .stdout
        .lines()
        .map(str::trim)
        .filter(|l| {
            let n = l.rsplit('/').next().unwrap_or("").to_lowercase();
            l.starts_with('/') && (n.contains("res") || n == "miui.apk" || n == "miuisystem.apk")
        })
        .map(str::to_string)
        .collect();
    if wanted.is_empty() {
        emitter.warn("Nie znaleziono plików frameworku na urządzeniu / no framework files found.");
        return Ok(0);
    }
    let dir = ws.dir.join("fw");
    tokio::fs::create_dir_all(&dir).await?;
    let mut n = 0;
    for remote in wanted {
        let name = sanitize_filename(&remote.trim_start_matches('/').replace('/', "_"));
        let local = dir.join(&name);
        let local_s = local.to_string_lossy().to_string();
        let o = adb.run_c(Some(serial), &["pull", &remote, &local_s], 300, cancel).await?;
        if !o.ok() || !local.is_file() {
            emitter.warn(format!("{remote}: {}", o.err_text()));
            continue;
        }
        let run = run_jar(
            tc,
            &tc.apktool,
            vec!["if".into(), "-p".into(), tc.frameworks.clone().into(), local.into()],
            &ws.dir,
            emitter,
            None,
            cancel,
            300,
        )
        .await?;
        if run.ok() {
            n += 1;
            emitter.ok(format!("Framework zaimportowany / installed: {remote}"));
        }
    }
    Ok(n)
}

/// Ręczny import pliku frameworku (np. framework-res.apk skopiowany z innego urządzenia).
pub async fn import_framework(paths: &Paths, file: &Path) -> Result<String> {
    let tc = toolchain::existing(&paths.tools).await.ok_or_else(|| {
        AdbError::Other("Najpierw przygotuj narzędzia (Java + apktool) / prepare the toolchain first".into())
    })?;
    if !file.is_file() {
        return Err(AdbError::InvalidArg(format!("nie znaleziono / not found: {}", file.display())));
    }
    tokio::fs::create_dir_all(&tc.frameworks).await?;
    let work = paths.root.join("_framework");
    tokio::fs::create_dir_all(&work).await?;
    let tx_em = Emitter::silent();
    let run = run_jar(
        &tc,
        &tc.apktool,
        vec!["if".into(), "-p".into(), tc.frameworks.clone().into(), file.into()],
        &work,
        &tx_em,
        None,
        &CancelToken::new(),
        300,
    )
    .await?;
    let _ = tokio::fs::remove_dir_all(&work).await;
    if run.ok() {
        Ok(run.tail.into_iter().filter(|l| l.starts_with("I:")).collect::<Vec<_>>().join("\n"))
    } else {
        Err(AdbError::Other(explain("framework", &run.tail_text())))
    }
}

/// Etykieta: tekst wprost albo `@string/x` z res/values/strings.xml.
fn resolve_label(src: &Path, label: Option<&str>) -> Option<String> {
    let l = label?;
    let Some(key) = l.strip_prefix("@string/") else {
        return Some(l.to_string());
    };
    let xml = std::fs::read_to_string(src.join("res/values/strings.xml")).ok()?;
    let d = roxmltree::Document::parse(&xml).ok()?;
    let found = d
        .descendants()
        .find(|n| n.has_tag_name("string") && n.attribute("name") == Some(key))
        .map(|n| n.text().unwrap_or("").trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty());
    found.or_else(|| Some(l.to_string()))
}

/// Edycja manifestu -> apktool b -> podpis -> (instalacja) -> (eksport).
async fn build_steps(
    adb: Option<&Adb>,
    tc: &Toolchain,
    ws: &Workspace,
    opts: &BuildOptions,
    stepper: &Stepper,
    emitter: &Emitter,
    cancel: &CancelToken,
) -> Result<BuildResult> {
    let mut meta = ws.load()?;
    if !meta.decoded {
        return Err(AdbError::Other("Przestrzeń nie została zdekompilowana / workspace not decoded".into()));
    }

    // 1. Edycja manifestu (zawsze od oryginału)
    stepper.begin("patch", "");
    let new_pkg = opts.patch.new_package.as_deref().map(str::trim).filter(|p| !p.is_empty() && *p != meta.package);
    if new_pkg.is_some() && meta.is_split() {
        return Err(AdbError::Other(
            "Klonowanie (zmiana nazwy pakietu) działa tylko dla aplikacji z jednym plikiem APK. Ta aplikacja \
             składa się z części split - zmiana nazwy wymagałaby przebudowy każdej z nich / cloning split apps is not supported."
                .into(),
        ));
    }
    let mut patch = opts.patch.clone();
    patch.new_package = new_pkg.map(str::to_string);
    let changes = ws.apply_patch(&patch)?;
    if changes.is_empty() && meta.replaced.is_empty() {
        emitter.warn("Brak zmian - APK zostanie tylko przebudowany i podpisany kluczem debug / no changes, rebuild only.");
    }
    for c in &changes {
        emitter.info(format!("Manifest: {c}"));
    }
    for r in &meta.replaced {
        emitter.info(format!("Zasób podmieniony / replaced: {r}"));
    }
    meta.patch = patch.clone();
    ws.save(&meta)?;
    stepper.finish_step();
    emitter.item("step:patch", true, "");

    // 2. apktool b (z wymuszeniem pełnej przebudowy - wykrywanie zmian bywa zawodne)
    stepper.begin("build", "apktool b");
    let out = ws.out();
    let _ = tokio::fs::remove_dir_all(&out).await;
    let _ = tokio::fs::remove_dir_all(ws.src().join("build")).await;
    let sign_dir = out.join("signed");
    tokio::fs::create_dir_all(&sign_dir).await?;
    let unsigned = out.join("base-unsigned.apk");
    let run = run_jar(
        tc,
        &tc.apktool,
        vec![
            "b".into(),
            "-f".into(),
            "-p".into(),
            tc.frameworks.clone().into(),
            "-o".into(),
            unsigned.clone().into(),
            ws.src().into(),
        ],
        &ws.dir,
        emitter,
        Some(stepper),
        cancel,
        BUILD_TIMEOUT,
    )
    .await?;
    if !run.ok() || !unsigned.is_file() {
        return Err(AdbError::Other(explain("build", &run.tail_text())));
    }
    stepper.finish_step();
    emitter.item("step:build", true, "");

    // 3. Podpis: base + (oryginalne) splity tym samym kluczem debug
    stepper.begin("sign", "uber-apk-signer");
    tokio::fs::rename(&unsigned, sign_dir.join("base.apk")).await?;
    for split in meta.apks.iter().filter(|n| *n != "base.apk") {
        tokio::fs::copy(ws.orig().join(split), sign_dir.join(split)).await?;
    }
    let mut args: Vec<OsString> = vec![
        "-a".into(),
        sign_dir.clone().into(),
        "--ks".into(),
        tc.keystore.clone().into(),
        "--ksAlias".into(),
        KS_ALIAS.into(),
        "--ksPass".into(),
        KS_PASS.into(),
        "--ksKeyPass".into(),
        KS_PASS.into(),
        "--allowResign".into(),
        "--overwrite".into(),
    ];
    if let Ok(z) = which::which("zipalign") {
        args.push("--zipAlignPath".into());
        args.push(z.into());
    }
    let run = run_jar(tc, &tc.signer, args, &ws.dir, emitter, Some(stepper), cancel, SIGN_TIMEOUT).await?;
    let text = run.tail_text();
    if !run.ok() || text.contains("VERIFY FAILED") || text.contains("[ERROR]") {
        return Err(AdbError::Other(explain("sign", &text)));
    }
    let mut files: Vec<PathBuf> = meta.apks.iter().map(|n| sign_dir.join(n)).collect();
    if let Some(missing) = files.iter().find(|p| !p.is_file()) {
        return Err(AdbError::Other(format!("Brak podpisanego pliku / signed file missing: {}", missing.display())));
    }
    files.sort_by_key(|p| p.file_name() != Some(std::ffi::OsStr::new("base.apk")));
    stepper.finish_step();
    emitter.item("step:sign", true, "");
    emitter.ok("APK zbudowany i podpisany kluczem debug / built and signed.");

    let target_pkg = patch.new_package.clone().unwrap_or_else(|| meta.package.clone());

    // 4. Eksport (opcjonalnie)
    let mut exported = Vec::new();
    if let Some(dir) = opts.export_dir.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        let ver = meta.version_name.clone().unwrap_or_default();
        let base_name = sanitize_filename(&format!("{target_pkg}_{ver}_mod").replace(' ', "_"));
        let dest_dir = if files.len() > 1 { PathBuf::from(dir).join(&base_name) } else { PathBuf::from(dir) };
        tokio::fs::create_dir_all(&dest_dir).await?;
        for f in &files {
            let name = if files.len() > 1 {
                f.file_name().unwrap_or_default().to_string_lossy().to_string()
            } else {
                format!("{base_name}.apk")
            };
            let d = dest_dir.join(name);
            tokio::fs::copy(f, &d).await?;
            exported.push(d.to_string_lossy().to_string());
        }
        emitter.ok(format!("Zapisano / exported: {}", dest_dir.display()));
    }

    // 5. Instalacja (opcjonalnie)
    let mut installed = false;
    if opts.install {
        stepper.begin("install", target_pkg.clone());
        let serial = opts
            .serial
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AdbError::Other("Nie wybrano urządzenia do instalacji / no device selected".into()))?;
        let adb = need_adb(adb)?;
        let clone = patch.new_package.is_some();
        if opts.uninstall_first && !clone {
            emitter.warn(format!("Odinstalowuję oryginał / uninstalling original: {}", meta.package));
            let o = adb.run_c(Some(serial), &["uninstall", &meta.package], 120, cancel).await?;
            let txt = format!("{} {}", o.stdout.trim(), o.stderr.trim());
            if txt.contains("Success") {
                emitter.ok("Oryginał odinstalowany / original removed.");
            } else {
                emitter.warn(format!("uninstall: {}", txt.trim()));
            }
        }
        let mut argv: Vec<String> =
            vec![if files.len() > 1 { "install-multiple" } else { "install" }.into(), "-r".into(), "-t".into()];
        argv.extend(files.iter().map(|p| p.to_string_lossy().to_string()));
        let argv_ref: Vec<&str> = argv.iter().map(String::as_str).collect();
        emitter.info(format!("adb {} ...", argv[0]));
        stepper.milestone(0.1, 0.9);
        let o = adb.run_c(Some(serial), &argv_ref, 900, cancel).await?;
        let txt = format!("{}\n{}", o.stdout.trim(), o.stderr.trim());
        if !o.ok() || !o.stdout.contains("Success") {
            return Err(AdbError::Other(explain_install(&txt, clone)));
        }
        installed = true;
        stepper.finish_step();
        emitter.item("step:install", true, "");
        emitter.ok(format!("Zainstalowano / installed: {target_pkg}"));
        if patch.debuggable {
            emitter.info(format!(
                "Aplikacja jest debuggable: widoczna w Android Studio (Attach debugger), działa `adb shell run-as {target_pkg}` / app is now debuggable."
            ));
        }
    }

    let result = BuildResult {
        workspace: meta.id.clone(),
        package: target_pkg.clone(),
        files: files.iter().map(|p| p.to_string_lossy().to_string()).collect(),
        exported,
        changes: changes.clone(),
        installed,
    };
    meta.last_build = Some(BuildInfo {
        at: chrono::Local::now().timestamp(),
        package: target_pkg,
        files: result.files.clone(),
        changes,
    });
    ws.save(&meta)?;
    Ok(result)
}

// --------------------------------------------------------------------------- //
// Testy end-to-end (wymagają Javy i jarów; uruchamiane gdy ustawione zmienne)
//   ADBST_APKTOOL=/path/apktool.jar ADBST_SIGNER=/path/uber.jar ADBST_APK=/path/app.apk
// --------------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;

    fn e2e_env() -> Option<(PathBuf, PathBuf, PathBuf)> {
        Some((
            std::env::var_os("ADBST_APKTOOL")?.into(),
            std::env::var_os("ADBST_SIGNER")?.into(),
            std::env::var_os("ADBST_APK")?.into(),
        ))
    }

    #[test]
    fn import_detects_base() {
        let d = std::env::temp_dir().join(format!("adbst-imp-{}", std::process::id()));
        let (s, o) = (d.join("s"), d.join("o"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(&o).unwrap();
        std::fs::write(s.join("app.apk"), b"x").unwrap();
        std::fs::write(s.join("split_config.arm64_v8a.apk"), b"y").unwrap();
        let (names, size) = import_files(&s, &o).unwrap();
        assert_eq!(names, vec!["base.apk".to_string(), "split_config.arm64_v8a.apk".to_string()]);
        assert_eq!(size, 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn e2e_decode_patch_build_sign() {
        let Some((apktool, signer, apk)) = e2e_env() else {
            eprintln!("pominięto (brak ADBST_APKTOOL/ADBST_SIGNER/ADBST_APK)");
            return;
        };
        let base = std::env::temp_dir().join(format!("adbst-e2e-{}", std::process::id()));
        let tools = base.join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::copy(&apktool, tools.join("apktool_2.11.1.jar")).unwrap();
        std::fs::copy(&signer, tools.join("uber-apk-signer-1.3.0.jar")).unwrap();
        let paths = Paths { tools, root: base.join("ws") };
        let em = Emitter::silent();
        let cancel = CancelToken::new();
        let stepper = Stepper::new(vec![step("x", 1)], em.clone());

        let tc = toolchain::ensure(&paths.tools, &em, &cancel).await.unwrap();
        assert!(tc.keystore.is_file());
        let src = OpenSource::File { path: apk.to_string_lossy().to_string(), serial: None };
        let id = open_step(None, &paths, &tc, &src, &stepper, &em, &cancel).await.unwrap();
        let ws = Workspace::open(&paths.root, &id).unwrap();
        let meta = ws.load().unwrap();
        assert!(meta.decoded && !meta.package.is_empty());

        // podmiana grafiki: znajdź PNG (nie 9-patch) i podmień kopią innego PNG
        let pngs: Vec<String> = ["res/mipmap-hdpi", "res/mipmap-xhdpi", "res/drawable-hdpi", "res/drawable-xhdpi"]
            .iter()
            .filter_map(|d| workspace::list_dir(&ws.src(), d, &[]).ok())
            .flatten()
            .filter(|e| e.name.ends_with(".png") && !e.name.ends_with(".9.png"))
            .map(|e| e.rel)
            .collect();
        if pngs.len() >= 2 {
            let other = ws.src().join(&pngs[1]);
            let w = workspace::replace(&ws, &pngs[0], &other).unwrap();
            assert!(w.is_empty());
            assert_eq!(ws.load().unwrap().replaced, vec![pngs[0].clone()]);
            // zły format odrzucony
            let fake = base.join("fake.png");
            std::fs::write(&fake, b"GIF89a....").unwrap();
            assert!(workspace::replace(&ws, &pngs[0], &fake).is_err());
        }

        let opts = BuildOptions {
            patch: PatchOptions {
                debuggable: true,
                new_package: Some(format!("{}.devclone", meta.package)),
                new_label: Some("Klon testowy".into()),
            },
            export_dir: Some(base.join("export").to_string_lossy().to_string()),
            ..Default::default()
        };
        let r = build_steps(None, &tc, &ws, &opts, &stepper, &em, &cancel).await.unwrap();
        assert_eq!(r.files.len(), 1);
        assert_eq!(r.exported.len(), 1);
        assert!(r.package.ends_with(".devclone"));

        // sprawdzenie wyniku: zdekoduj zbudowany APK i porównaj manifest
        let check = base.join("check");
        let run = run_jar(
            &tc,
            &tc.apktool,
            vec!["d".into(), "-s".into(), "-f".into(), "-p".into(), tc.frameworks.clone().into(), "-o".into(), check.clone().into(), PathBuf::from(&r.files[0]).into()],
            &base,
            &em,
            None,
            &cancel,
            600,
        )
        .await
        .unwrap();
        assert!(run.ok(), "{}", run.tail_text());
        let xml = std::fs::read_to_string(check.join("AndroidManifest.xml")).unwrap();
        let yml = std::fs::read_to_string(check.join("apktool.yml")).unwrap();
        let info = manifest::inspect(&xml, Some(&yml)).unwrap();
        // apktool przy dekodowaniu przywraca nazwę z tablicy zasobów, a nową
        // (rzeczywistą, z binarnego manifestu) zapisuje jako renameManifestPackage
        assert_eq!(info.rename_package, Some(format!("{}.devclone", meta.package)));
        assert!(info.debuggable);
        assert_eq!(info.label.as_deref(), Some("Klon testowy"));
        for p in &info.providers {
            assert!(p.contains(".devclone"), "authority not renamed: {p}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
