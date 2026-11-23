//! ADB Service Tool - wejście biblioteki Tauri + definicje komend IPC.
//!
//! Frontend (SvelteKit) woła te komendy przez `@tauri-apps/api/core::invoke`.
//! Długie operacje (pull/push/apps) startują zadanie w tle i od razu zwracają
//! `jobId`; log/postęp płyną przez `tauri::ipc::Channel`. GUI nigdy nie czeka.
//! Lista urządzeń jest odświeżana automatycznie (zdarzenie `devices-changed`).

mod adb;
mod apk;
mod devtools;
mod error;
mod hardware;
#[cfg(target_os = "linux")]
mod linux_desktop;
mod mirror;
mod mtp_handler;
mod parsers;
mod state;
mod updater;
mod utils;
mod vendors;

use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use adb::apps::{AppAction, AppEntry, AppsLoadEvent, InstallOptions};
use adb::device::{Device, DeviceInfo};
use adb::events::{Emitter, TransferEvent};
use adb::manager::Output;
use adb::pull::PullOptions;
use adb::push::PushOptions;
use adb::tree::{DirListing, Partition};
use adb::Adb;
use error::{AdbError, Result};
use state::AppState;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};
use utils::CancelToken;

// trait z tauri koliduje nazwą z naszym `Emitter` - importujemy pod aliasem
use tauri::Emitter as TauriEmit;

/// Info o wykrytych binarkach (baner w GUI).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolsInfo {
    adb_path: Option<String>,
    fastboot_path: Option<String>,
    adb_version: Option<String>,
}

async fn tools_info(adb: Option<Arc<Adb>>) -> ToolsInfo {
    match adb {
        Some(a) => {
            let ver = a
                .run(None, &["version"], 5)
                .await
                .ok()
                .and_then(|o| o.stdout.lines().next().map(|l| l.trim().to_string()));
            ToolsInfo {
                adb_path: Some(a.adb.to_string_lossy().to_string()),
                fastboot_path: a.fastboot.as_ref().map(|p| p.to_string_lossy().to_string()),
                adb_version: ver,
            }
        }
        None => ToolsInfo {
            adb_path: None,
            fastboot_path: None,
            adb_version: None,
        },
    }
}

/// Zwraca info o binarkach; gdy adb brak - próbuje zlokalizować ponownie
/// (użytkownik mógł doinstalować platform-tools w trakcie).
#[tauri::command]
async fn locate_tools(state: State<'_, AppState>) -> Result<ToolsInfo> {
    let mut guard = state.adb.lock().await;
    if guard.is_none() {
        *guard = Adb::locate().ok().map(Arc::new);
    }
    let a = guard.clone();
    drop(guard);
    Ok(tools_info(a).await)
}

/// Ręczne wskazanie pliku `adb` (np. przenośne platform-tools).
#[tauri::command]
async fn set_adb_path(state: State<'_, AppState>, path: String) -> Result<ToolsInfo> {
    let a = Arc::new(Adb::from_path(PathBuf::from(path))?);
    *state.adb.lock().await = Some(a.clone());
    Ok(tools_info(Some(a)).await)
}

#[tauri::command]
async fn scan_devices(state: State<'_, AppState>) -> Result<Vec<Device>> {
    let adb = state.adb().await?;
    let mtp = mtp_handler::scan().await;
    adb::device::scan(&adb, Some(&mtp)).await
}

#[tauri::command]
async fn device_info(state: State<'_, AppState>, serial: String, mode: String) -> Result<DeviceInfo> {
    if mode == "mtp" {
        // MTP nie wymaga adb
        let adb = state.adb().await.ok();
        return match adb {
            Some(a) => adb::device::info(&a, &serial, &mode).await,
            None => adb::device::info(&Adb { adb: PathBuf::new(), fastboot: None }, &serial, &mode).await,
        };
    }
    let adb = state.adb().await?;
    adb::device::info(&adb, &serial, &mode).await
}

/// `root` = listuj z uprawnieniami root (tylko gdy wykryto root dla tego urządzenia).
#[tauri::command]
async fn list_dir(state: State<'_, AppState>, serial: String, path: String, root: Option<bool>) -> Result<DirListing> {
    let adb = state.adb().await?;
    let mode = if root.unwrap_or(false) {
        state.root.lock().await.get(&serial).copied().unwrap_or_default()
    } else {
        adb::root::RootMode::None
    };
    adb::tree::list_dir(&adb, &serial, &path, mode).await
}

/// Wykrywa root (adbd root / su). `force` = pomiń pamięć (np. po zezwoleniu w Magisk).
#[tauri::command]
async fn detect_root(state: State<'_, AppState>, serial: String, force: Option<bool>) -> Result<adb::root::RootInfo> {
    let adb = state.adb().await?;
    if force.unwrap_or(false) {
        state.root.lock().await.remove(&serial);
    }
    Ok(adb::root::cached_or_detect(&state.root, &adb, &serial).await)
}

#[tauri::command]
async fn scan_partitions(state: State<'_, AppState>, serial: String) -> Result<Vec<Partition>> {
    let adb = state.adb().await?;
    adb::tree::scan_partitions(&adb, &serial).await
}

/// Quick Reboot. `via` = "adb" | "fastboot" (wg trybu wybranego urządzenia).
#[tauri::command]
async fn reboot(state: State<'_, AppState>, serial: String, mode: String, via: String) -> Result<()> {
    let adb = state.adb().await?;
    adb::power::reboot(&adb, &serial, &mode, via == "fastboot").await
}

// --------------------------------------------------------------------------- //
// Zadania w tle
// --------------------------------------------------------------------------- //

static JOB_SEQ: AtomicU64 = AtomicU64::new(1);

/// Rejestruje zadanie (token anulowania), uruchamia je w tle i zwraca jego ID.
/// Po zakończeniu zadanie samo wypisuje się z rejestru.
async fn spawn_job<F, Fut>(state: &AppState, prefix: &str, f: F) -> String
where
    F: FnOnce(CancelToken) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let cancel = CancelToken::new();
    let id = format!("{prefix}-{}", JOB_SEQ.fetch_add(1, Ordering::SeqCst));
    state.jobs.lock().await.insert(id.clone(), cancel.clone());
    let jobs = state.jobs.clone();
    let id2 = id.clone();
    let fut = f(cancel);
    tauri::async_runtime::spawn(async move {
        fut.await;
        jobs.lock().await.remove(&id2);
    });
    id
}

#[tauri::command]
async fn start_pull(
    state: State<'_, AppState>,
    serial: String,
    options: PullOptions,
    on_event: Channel<TransferEvent>,
) -> Result<String> {
    let adb = state.adb().await?;
    if options.items.is_empty() || options.local_root.trim().is_empty() {
        return Err(AdbError::InvalidArg("brak elementów lub folderu docelowego / nothing selected".into()));
    }
    let em = Emitter::new(on_event);
    let cache = state.root.clone();
    Ok(spawn_job(&state, "pull", move |c| adb::pull::run_pull(adb, serial, options, Some(cache), em, c)).await)
}

#[tauri::command]
async fn start_push(
    state: State<'_, AppState>,
    serial: String,
    options: PushOptions,
    on_event: Channel<TransferEvent>,
) -> Result<String> {
    let adb = state.adb().await?;
    if options.items.is_empty() {
        return Err(AdbError::InvalidArg("brak plików / nothing selected".into()));
    }
    let em = Emitter::new(on_event);
    Ok(spawn_job(&state, "push", move |c| adb::push::run_push(adb, serial, options, em, c)).await)
}

/// Kontekst agenta + rejestracja przycisku "Gotowe" (ręczna instalacja) dla urządzenia.
async fn agent_ctx(app: &AppHandle, state: &AppState, serial: &str) -> Result<adb::backup::AgentCtx> {
    let adb = state.adb().await?;
    let resource_apk = app.path().resource_dir().ok().map(|d| d.join("resources").join("agent").join("Agent.apk"));
    let cache_dir = app_cache(app)?.join("backup-agent");
    let confirm = Arc::new(tokio::sync::Notify::new());
    state.agent_confirm.lock().await.insert(serial.to_string(), confirm.clone());
    Ok(adb::backup::AgentCtx { adb, serial: serial.to_string(), resource_apk, cache_dir, confirm })
}

/// Kontakty + SMS przez aplikację pomocniczą (instalacja -> zgoda użytkownika -> pobranie -> sprzątanie).
#[tauri::command]
async fn start_contacts_backup(
    app: AppHandle,
    state: State<'_, AppState>,
    serial: String,
    options: adb::backup::BackupOptions,
    on_event: Channel<TransferEvent>,
) -> Result<String> {
    if options.dest_dir.trim().is_empty() {
        return Err(AdbError::InvalidArg("wybierz folder docelowy / choose a destination folder".into()));
    }
    let ctx = agent_ctx(&app, &state, &serial).await?;
    let em = Emitter::new(on_event);
    Ok(spawn_job(&state, "backup", move |c| adb::backup::run_backup(ctx, options, em, c)).await)
}

/// Przywracanie kontaktów i SMS z kopii na komputerze (push -> agent w trybie restore -> sprzątanie).
#[tauri::command]
async fn start_contacts_restore(
    app: AppHandle,
    state: State<'_, AppState>,
    serial: String,
    options: adb::backup::RestoreOptions,
    on_event: Channel<TransferEvent>,
) -> Result<String> {
    // walidacja folderu przed instalacją czegokolwiek na urządzeniu
    adb::backup::find_restore_files(std::path::Path::new(&options.source_dir))?;
    let ctx = agent_ctx(&app, &state, &serial).await?;
    let em = Emitter::new(on_event);
    Ok(spawn_job(&state, "restore", move |c| adb::backup::run_restore(ctx, options, em, c)).await)
}

/// Pliki kopii w folderze (podgląd w GUI przed przywracaniem).
#[tauri::command]
async fn inspect_restore_dir(dir: String) -> Result<serde_json::Value> {
    let (vcf, xml) = adb::backup::find_restore_files(std::path::Path::new(&dir))?;
    let info = |p: Option<std::path::PathBuf>| {
        p.map(|p| {
            let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            serde_json::json!({ "path": p.to_string_lossy(), "size": size })
        })
    };
    Ok(serde_json::json!({ "contacts": info(vcf), "sms": info(xml) }))
}

/// Przycisk "Gotowe" w oknie ręcznej instalacji agenta. Zwraca false, gdy nic nie czeka.
#[tauri::command]
async fn agent_install_confirm(state: State<'_, AppState>, serial: String) -> Result<bool> {
    Ok(match state.agent_confirm.lock().await.get(&serial) {
        Some(n) => {
            n.notify_one();
            true
        }
        None => false,
    })
}

// --------------------------------------------------------------------------- //
// Aktualizacje (własny aktualizator, publiczne wydania GitHub - bez tokenów)
// --------------------------------------------------------------------------- //

#[tauri::command]
async fn update_check() -> Result<updater::UpdateInfo> {
    updater::check().await
}

/// Pobiera, weryfikuje (SHA-256) i instaluje najnowszą wersję. Przy restarcie
/// (instalator / podmiana binarki) aplikacja zamyka się sama po chwili.
#[tauri::command]
async fn update_install(app: AppHandle, state: State<'_, AppState>, on_event: Channel<TransferEvent>) -> Result<String> {
    let em = Emitter::new(on_event);
    Ok(spawn_job(&state, "update", move |c| async move {
        match updater::run_install(em.clone(), c.clone()).await {
            Ok(outcome) => {
                em.item("result", true, serde_json::to_string(&outcome).unwrap_or_default());
                em.finished(1, 0, 1, vec![], false);
                if matches!(outcome, updater::InstallOutcome::Restart) {
                    tokio::time::sleep(Duration::from_millis(1200)).await;
                    app.exit(0);
                }
            }
            Err(AdbError::Cancelled) => em.finished(0, 0, 1, vec![], true),
            Err(e) => em.fatal(e.to_string()),
        }
    })
    .await)
}

/// Instalacja pobranego pakietu .deb przez pkexec (tylko po potwierdzeniu w GUI).
#[tauri::command]
async fn update_install_package(path: String) -> Result<String> {
    updater::install_system_package(&path).await
}

#[tauri::command]
async fn cancel_job(state: State<'_, AppState>, job_id: String) -> Result<()> {
    if let Some(tok) = state.jobs.lock().await.get(&job_id) {
        tok.cancel();
    }
    Ok(())
}

// --------------------------------------------------------------------------- //
// Aplikacje
// --------------------------------------------------------------------------- //

#[tauri::command]
async fn list_apps(state: State<'_, AppState>, serial: String) -> Result<Vec<AppEntry>> {
    let adb = state.adb().await?;
    adb::apps::list(&adb, &serial).await
}

/// Lista aplikacji + nazwy/ikony ze strumieniowym postępem (pasek nad listą).
/// Zwraca `jobId` - ponowne wczytanie / zmiana urządzenia anuluje poprzednie.
#[tauri::command]
async fn load_apps(
    app: AppHandle,
    state: State<'_, AppState>,
    serial: String,
    lang: String,
    on_event: Channel<AppsLoadEvent>,
) -> Result<String> {
    let adb = state.adb().await?;
    let cache = app
        .path()
        .app_cache_dir()
        .map_err(|e| AdbError::Io(e.to_string()))?
        .join("app-icons");
    Ok(spawn_job(&state, "apps-load", move |c| {
        adb::apps::run_load(adb, serial, lang, cache, on_event, c)
    })
    .await)
}

#[tauri::command]
async fn apps_action(
    state: State<'_, AppState>,
    serial: String,
    packages: Vec<String>,
    action: AppAction,
    on_event: Channel<TransferEvent>,
) -> Result<String> {
    let adb = state.adb().await?;
    let em = Emitter::new(on_event);
    Ok(spawn_job(&state, "apps", move |c| adb::apps::run_action(adb, serial, packages, action, em, c)).await)
}

#[tauri::command]
async fn extract_apks(
    state: State<'_, AppState>,
    serial: String,
    packages: Vec<String>,
    out_dir: String,
    on_event: Channel<TransferEvent>,
) -> Result<String> {
    let adb = state.adb().await?;
    let em = Emitter::new(on_event);
    Ok(spawn_job(&state, "extract", move |c| adb::apps::run_extract(adb, serial, packages, out_dir, em, c)).await)
}

#[tauri::command]
async fn install_apks(
    state: State<'_, AppState>,
    serial: String,
    paths: Vec<String>,
    options: Option<InstallOptions>,
    on_event: Channel<TransferEvent>,
) -> Result<String> {
    let adb = state.adb().await?;
    let em = Emitter::new(on_event);
    let opts = options.unwrap_or_default();
    Ok(spawn_job(&state, "install", move |c| adb::apps::run_install(adb, serial, paths, opts, em, c)).await)
}

// --------------------------------------------------------------------------- //
// Zaawansowane
// --------------------------------------------------------------------------- //

/// Jednorazowe polecenie `adb shell` (konsola serwisowa).
#[tauri::command]
async fn shell_command(state: State<'_, AppState>, serial: String, command: String) -> Result<Output> {
    let adb = state.adb().await?;
    adb.shell(Some(&serial), &command, 120).await
}

/// `adb sideload <zip>` (urządzenie w trybie sideload w recovery).
#[tauri::command]
async fn start_sideload(
    state: State<'_, AppState>,
    serial: String,
    path: String,
    on_event: Channel<TransferEvent>,
) -> Result<String> {
    let adb = state.adb().await?;
    let em = Emitter::new(on_event);
    Ok(spawn_job(&state, "sideload", move |c| async move {
        em.info(format!("adb sideload {path} ..."));
        match adb.run_c(Some(&serial), &["sideload", &path], 3600, &c).await {
            Ok(o) if o.ok() => {
                em.ok(format!("Sideload OK. {}", o.stdout.trim()));
                em.finished(1, 0, 1, vec![], false);
            }
            Ok(o) => {
                em.error(o.err_text());
                em.finished(0, 0, 1, vec![], false);
            }
            Err(e) => {
                em.error(e.to_string());
                em.finished(0, 0, 1, vec![], c.is_cancelled());
            }
        }
    })
    .await)
}

// --------------------------------------------------------------------------- //
// Zrzut / Mirroring
// --------------------------------------------------------------------------- //

fn app_data(app: &AppHandle) -> Result<PathBuf> {
    app.path().app_data_dir().map_err(|e| AdbError::Io(e.to_string()))
}

#[tauri::command]
async fn scrcpy_status(app: AppHandle) -> Result<mirror::scrcpy::ScrcpyStatus> {
    Ok(mirror::scrcpy::status(&app_data(&app)?).await)
}

/// Windows: pobiera najnowszy scrcpy z GitHuba do folderu aplikacji (zadanie w tle).
#[tauri::command]
async fn scrcpy_download(
    app: AppHandle,
    state: State<'_, AppState>,
    on_event: Channel<TransferEvent>,
) -> Result<String> {
    if !cfg!(windows) {
        return Err(AdbError::Other(
            "Automatyczne pobieranie scrcpy dostępne tylko na Windows - zainstaluj z menedżera pakietów / Windows only".into(),
        ));
    }
    let dir = app_data(&app)?;
    let em = Emitter::new(on_event);
    Ok(spawn_job(&state, "scrcpy-dl", move |c| async move {
        match mirror::scrcpy::download_latest(&dir, &em, &c).await {
            Ok(_) => em.finished(1, 0, 1, vec![], false),
            Err(AdbError::Cancelled) => em.finished(0, 0, 1, vec![], true),
            Err(e) => em.fatal(e.to_string()),
        }
    })
    .await)
}

/// Uruchamia sesję scrcpy (osobne okno z pełnym sterowaniem). Zwraca `jobId`
/// - `cancel_job` zamyka okno.
#[tauri::command]
async fn scrcpy_start(
    app: AppHandle,
    state: State<'_, AppState>,
    serial: String,
    title: String,
    options: mirror::scrcpy::ScrcpyOptions,
    on_event: Channel<TransferEvent>,
) -> Result<String> {
    let adb = state.adb().await?;
    let (path, _) = mirror::scrcpy::locate(&app_data(&app)?)
        .ok_or_else(|| AdbError::BinaryNotFound("scrcpy".into()))?;
    let ver = mirror::scrcpy::version(&path).await;
    let args = mirror::scrcpy::build_args(&serial, &options, ver.as_deref(), &title);
    let em = Emitter::new(on_event);
    let adb_path = adb.adb.clone();
    Ok(spawn_job(&state, "scrcpy", move |c| mirror::scrcpy::run_session(path, adb_path, args, em, c)).await)
}

/// Natywny zrzut ekranu (`adb exec-out screencap -p`) - zapis PNG + podgląd.
#[tauri::command]
async fn take_screenshot(
    app: AppHandle,
    state: State<'_, AppState>,
    serial: String,
) -> Result<mirror::screenshot::Screenshot> {
    let adb = state.adb().await?;
    let dir = match app.path().picture_dir() {
        Ok(p) => p.join("ADB Service Tool"),
        Err(_) => app_data(&app)?.join("screenshots"),
    };
    mirror::screenshot::take(&adb, &serial, &dir).await
}

/// Pokazuje plik w menedżerze plików systemu.
#[tauri::command]
async fn reveal_path(path: String) -> Result<()> {
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err(AdbError::Io(format!("nie istnieje / not found: {path}")));
    }
    #[cfg(windows)]
    let r = {
        // raw_arg: explorer wymaga dokładnie składni /select,"ścieżka" (spacje w ścieżce)
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", p.display()))
            .spawn()
    };
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg("-R").arg(&p).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open")
        .arg(p.parent().unwrap_or(&p))
        .spawn();
    r.map(|_| ()).map_err(|e| AdbError::Io(e.to_string()))
}


// --------------------------------------------------------------------------- //
// Sterowniki / udev
// --------------------------------------------------------------------------- //

fn app_config(app: &AppHandle) -> Result<PathBuf> {
    app.path().app_config_dir().map_err(|e| AdbError::Io(e.to_string()))
}

fn app_cache(app: &AppHandle) -> Result<PathBuf> {
    app.path().app_cache_dir().map_err(|e| AdbError::Io(e.to_string()))
}

#[tauri::command]
async fn drivers_report(app: AppHandle) -> Result<hardware::drivers::DriversReport> {
    hardware::drivers::report(&app_config(&app)?).await
}

/// Zapamiętuje adres pobierania / argumenty / SHA-256 dla sterownika.
#[tauri::command]
async fn drivers_set_override(
    app: AppHandle,
    id: String,
    url: Option<String>,
    args: Option<String>,
    sha256: Option<String>,
) -> Result<()> {
    let dir = app_config(&app)?;
    let mut o = hardware::drivers::load_overrides(&dir);
    let clean = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let url = clean(url);
    if let Some(u) = &url {
        if !u.starts_with("https://") {
            return Err(AdbError::InvalidArg("adres musi zaczynać się od https:// / https only".into()));
        }
    }
    o.insert(
        id,
        hardware::drivers::DriverOverride {
            url,
            args: clean(args),
            sha256: clean(sha256),
        },
    );
    hardware::drivers::save_overrides(&dir, &o)
}

/// Windows: pobranie + cicha instalacja sterownika (zadanie w tle).
#[tauri::command]
async fn driver_install(
    app: AppHandle,
    state: State<'_, AppState>,
    request: hardware::drivers::InstallRequest,
    on_event: Channel<TransferEvent>,
) -> Result<String> {
    let (cfg, cache) = (app_config(&app)?, app_cache(&app)?);
    let em = Emitter::new(on_event);
    Ok(spawn_job(&state, "driver", move |c| async move {
        match hardware::drivers::install_windows(request, cfg, cache, em.clone(), c.clone()).await {
            Ok(()) => em.finished(1, 0, 1, vec![], false),
            Err(AdbError::Cancelled) => em.finished(0, 0, 1, vec![], true),
            Err(e) => em.fatal(e.to_string()),
        }
    })
    .await)
}

/// Linux: instalacja reguł udev (pkexec, a gdy trzeba - sudo z hasłem z GUI).
#[tauri::command]
async fn udev_install(
    state: State<'_, AppState>,
    password: Option<String>,
    on_event: Channel<TransferEvent>,
) -> Result<hardware::drivers::UdevInstallResult> {
    let em = Emitter::new(on_event);
    let r = hardware::drivers::install_udev(password, &em).await?;
    if matches!(r, hardware::drivers::UdevInstallResult::Installed { .. }) {
        // serwer adb musi otworzyć urządzenia na nowo z nowymi uprawnieniami
        if let Ok(adb) = state.adb().await {
            let _ = adb.run(None, &["kill-server"], 10).await;
        }
    }
    Ok(r)
}

#[tauri::command]
async fn usb_scan() -> Result<Vec<hardware::usb::UsbDevice>> {
    Ok(tokio::task::spawn_blocking(hardware::usb::scan).await?)
}

// --------------------------------------------------------------------------- //
// Narzędzia producentów
// --------------------------------------------------------------------------- //

#[tauri::command]
async fn samsung_status(state: State<'_, AppState>, serial: String) -> Result<vendors::samsung::SamsungStatus> {
    let adb = state.adb().await?;
    vendors::samsung::status(&adb, &serial).await
}

#[tauri::command]
async fn samsung_download_mode() -> Result<vendors::samsung::DownloadModeStatus> {
    vendors::samsung::download_mode().await
}

#[tauri::command]
async fn xiaomi_arb(state: State<'_, AppState>, serial: String) -> Result<vendors::xiaomi::ArbInfo> {
    let adb = state.adb().await?;
    vendors::xiaomi::arb(&adb, &serial).await
}

#[tauri::command]
async fn xiaomi_firmware_arb(folder: String) -> Result<vendors::xiaomi::FirmwareArb> {
    tokio::task::spawn_blocking(move || vendors::xiaomi::firmware_arb(std::path::Path::new(&folder))).await?
}

#[tauri::command]
async fn edl_status() -> Result<vendors::xiaomi::EdlStatus> {
    vendors::xiaomi::edl().await
}

#[tauri::command]
async fn moto_load(dir: String, xml_file: Option<String>) -> Result<parsers::moto_xml::MotoFirmware> {
    tokio::task::spawn_blocking(move || vendors::motorola::load(&dir, xml_file.as_deref())).await?
}

#[tauri::command]
async fn moto_flash(
    state: State<'_, AppState>,
    serial: String,
    options: vendors::motorola::FlashOptions,
    on_event: Channel<TransferEvent>,
) -> Result<String> {
    let adb = state.adb().await?;
    let em = Emitter::new(on_event);
    Ok(spawn_job(&state, "moto", move |c| vendors::motorola::run_flash(adb, serial, options, em, c)).await)
}

// --------------------------------------------------------------------------- //
// Diagnostyka sprzętu + Live Logcat
// --------------------------------------------------------------------------- //

#[tauri::command]
async fn diag_snapshot(state: State<'_, AppState>, serial: String) -> Result<hardware::diagnostics::DiagSnapshot> {
    let adb = state.adb().await?;
    hardware::diagnostics::snapshot(&adb, &serial).await
}

#[tauri::command]
async fn diag_start(
    state: State<'_, AppState>,
    serial: String,
    interval_ms: u64,
    on_event: Channel<hardware::diagnostics::DiagEvent>,
) -> Result<String> {
    let adb = state.adb().await?;
    Ok(spawn_job(&state, "diag", move |c| hardware::diagnostics::run_stream(adb, serial, interval_ms, on_event, c)).await)
}

#[tauri::command]
async fn logcat_start(
    state: State<'_, AppState>,
    serial: String,
    options: hardware::logcat::LogcatOptions,
    on_event: Channel<hardware::logcat::LogcatEvent>,
) -> Result<String> {
    let adb = state.adb().await?;
    let session = hardware::logcat::LogcatSession::new();
    let sessions = state.logcat.clone();
    let sess2 = session.clone();
    let id = spawn_job(&state, "logcat", move |c| async move {
        hardware::logcat::run(adb, serial, options, sess2.clone(), on_event, c).await;
        sessions.lock().await.retain(|_, s| !Arc::ptr_eq(s, &sess2));
    })
    .await;
    state.logcat.lock().await.insert(id.clone(), session);
    Ok(id)
}

#[tauri::command]
async fn logcat_filter(
    state: State<'_, AppState>,
    job_id: String,
    pattern: String,
    min_level: String,
    case_insensitive: bool,
) -> Result<()> {
    let s = state
        .logcat
        .lock()
        .await
        .get(&job_id)
        .cloned()
        .ok_or_else(|| AdbError::InvalidArg("sesja logcat nie istnieje / no session".into()))?;
    s.set_filter(&pattern, min_level.chars().next().unwrap_or('V'), case_insensitive)
}

#[tauri::command]
async fn logcat_export(state: State<'_, AppState>, job_id: String, path: String) -> Result<usize> {
    let s = state
        .logcat
        .lock()
        .await
        .get(&job_id)
        .cloned()
        .ok_or_else(|| AdbError::InvalidArg("sesja logcat nie istnieje / no session".into()))?;
    tokio::task::spawn_blocking(move || s.export(std::path::Path::new(&path))).await?
}

#[tauri::command]
async fn logcat_clear(state: State<'_, AppState>, serial: String) -> Result<()> {
    let adb = state.adb().await?;
    hardware::logcat::clear_device(&adb, &serial).await
}


// --------------------------------------------------------------------------- //
// Samsung Flasher (Download Mode / LOKE + Heimdall) + PIT
// --------------------------------------------------------------------------- //

#[tauri::command]
async fn download_probe() -> Result<hardware::loke::DownloadProbe> {
    hardware::loke::probe().await
}

/// Natywny odczyt PIT z urządzenia w Download Mode (bez Heimdalla, niedestrukcyjny).
#[tauri::command]
async fn read_pit(
    state: State<'_, AppState>,
    save_path: Option<String>,
    on_event: Channel<TransferEvent>,
) -> Result<String> {
    let em = Emitter::new(on_event);
    Ok(spawn_job(&state, "pit", move |c| vendors::samsung_flash::run_read_pit(save_path, em, c)).await)
}

#[tauri::command]
async fn heimdall_status(app: AppHandle) -> Result<vendors::samsung_flash::HeimdallStatus> {
    Ok(vendors::samsung_flash::status(&app_data(&app)?).await)
}

/// Indeks + role paczek firmware (.tar / .tar.md5) bez wypakowywania.
#[tauri::command]
async fn firmware_inspect(paths: Vec<String>) -> Result<vendors::samsung_flash::FirmwareSet> {
    vendors::samsung_flash::inspect(paths).await
}

#[tauri::command]
async fn samsung_flash(
    app: AppHandle,
    state: State<'_, AppState>,
    options: vendors::samsung_flash::FlashOptions,
    on_event: Channel<TransferEvent>,
) -> Result<String> {
    let dir = app_data(&app)?;
    let em = Emitter::new(on_event);
    Ok(spawn_job(&state, "odin", move |c| async move {
        match vendors::samsung_flash::run_flash(dir, options, em.clone(), c.clone()).await {
            Ok(()) => em.finished(1, 0, 1, vec![], false),
            Err(AdbError::Cancelled) => em.finished(0, 0, 1, vec![], true),
            Err(e) => em.fatal(e.to_string()),
        }
    })
    .await)
}

// --------------------------------------------------------------------------- //
// Enterprise & Provisioning
// --------------------------------------------------------------------------- //

#[tauri::command]
async fn enterprise_adb(state: State<'_, AppState>, serial: String, action: vendors::enterprise::AdbAction) -> Result<Output> {
    let adb = state.adb().await?;
    vendors::enterprise::adb_action(&adb, &serial, action).await
}

#[tauri::command]
async fn enterprise_fastboot(state: State<'_, AppState>, serial: String, id: String) -> Result<Output> {
    let adb = state.adb().await?;
    vendors::enterprise::fastboot_command(&adb, &serial, &id).await
}

#[tauri::command]
async fn enterprise_mtp_reset() -> Result<String> {
    vendors::enterprise::mtp_reset().await
}

// --------------------------------------------------------------------------- //
// ADB przez Wi-Fi
// --------------------------------------------------------------------------- //

fn wifi_stage_fn(ch: Channel<adb::wireless::WifiStage>) -> impl Fn(&str, String) {
    move |stage: &str, detail: String| {
        let _ = ch.send(adb::wireless::WifiStage { stage: stage.to_string(), detail });
    }
}

/// Android 11+: `adb pair IP:PORT KOD`, potem `adb connect` (port ręczny lub mDNS).
#[tauri::command]
async fn wifi_pair(
    state: State<'_, AppState>,
    request: adb::wireless::PairRequest,
    on_stage: Channel<adb::wireless::WifiStage>,
) -> Result<adb::wireless::WifiResult> {
    let adb = state.adb().await?;
    adb::wireless::pair_and_connect(&adb, &request, wifi_stage_fn(on_stage)).await
}

/// Bezpośrednie `adb connect IP:PORT` (urządzenie już sparowane / w trybie tcpip).
#[tauri::command]
async fn wifi_connect(state: State<'_, AppState>, host: String, port: u16) -> Result<adb::wireless::WifiResult> {
    let adb = state.adb().await?;
    adb::wireless::connect(&adb, &host, port).await
}

/// Android 10 i starsze: urządzenie na kablu -> `adb tcpip` -> `adb connect IP:5555`.
#[tauri::command]
async fn wifi_tcpip(
    state: State<'_, AppState>,
    serial: String,
    port: Option<u16>,
    on_stage: Channel<adb::wireless::WifiStage>,
) -> Result<adb::wireless::WifiResult> {
    let adb = state.adb().await?;
    adb::wireless::tcpip_switch(&adb, &serial, port.unwrap_or(5555), wifi_stage_fn(on_stage)).await
}

#[tauri::command]
async fn wifi_disconnect(state: State<'_, AppState>, serial: String) -> Result<String> {
    let adb = state.adb().await?;
    adb::wireless::disconnect(&adb, &serial).await
}

// --------------------------------------------------------------------------- //
// APK Inspector & DevTools
// --------------------------------------------------------------------------- //

fn devtools_paths(app: &AppHandle) -> Result<devtools::Paths> {
    Ok(devtools::Paths {
        tools: app_data(app)?.join("apk-devtools").join("tools"),
        root: app_cache(app)?.join("apk-devtools"),
    })
}

fn devtools_ws(app: &AppHandle, id: &str) -> Result<devtools::workspace::Workspace> {
    devtools::workspace::Workspace::open(&devtools_paths(app)?.root, id)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DevtoolsWorkspace {
    meta: devtools::workspace::WorkspaceMeta,
    manifest: devtools::manifest::ManifestInfo,
}

#[tauri::command]
async fn devtools_status(app: AppHandle) -> Result<devtools::toolchain::ToolchainStatus> {
    Ok(devtools::toolchain::status(&devtools_paths(&app)?.tools).await)
}

/// Pobiera/przygotowuje Javę, apktool, uber-apk-signer i klucz debug (zadanie w tle).
#[tauri::command]
async fn devtools_setup(app: AppHandle, state: State<'_, AppState>, on_event: Channel<TransferEvent>) -> Result<String> {
    let paths = devtools_paths(&app)?;
    let em = Emitter::new(on_event);
    Ok(spawn_job(&state, "dt-setup", move |c| devtools::run_setup(paths, em, c)).await)
}

/// Otwiera APK (z urządzenia lub z dysku) i dekompiluje do nowej przestrzeni roboczej.
#[tauri::command]
async fn devtools_open(
    app: AppHandle,
    state: State<'_, AppState>,
    source: devtools::OpenSource,
    on_event: Channel<TransferEvent>,
) -> Result<String> {
    let paths = devtools_paths(&app)?;
    let adb = state.adb().await.ok();
    if matches!(source, devtools::OpenSource::Device { .. }) && adb.is_none() {
        return Err(AdbError::BinaryNotFound("adb".into()));
    }
    let em = Emitter::new(on_event);
    Ok(spawn_job(&state, "dt-open", move |c| devtools::run_open(adb, paths, source, em, c)).await)
}

/// Edycja + build + podpis (+ instalacja/eksport) istniejącej przestrzeni.
#[tauri::command]
async fn devtools_build(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    options: devtools::BuildOptions,
    on_event: Channel<TransferEvent>,
) -> Result<String> {
    let paths = devtools_paths(&app)?;
    devtools::workspace::Workspace::open(&paths.root, &id)?;
    let adb = state.adb().await.ok();
    if options.install && adb.is_none() {
        return Err(AdbError::BinaryNotFound("adb".into()));
    }
    let em = Emitter::new(on_event);
    Ok(spawn_job(&state, "dt-build", move |c| devtools::run_build(adb, paths, id, options, em, c)).await)
}

/// Pełny automat: Pull -> Dekompilacja -> Edycja -> Build -> Podpis -> Instalacja.
#[tauri::command]
async fn devtools_auto(
    app: AppHandle,
    state: State<'_, AppState>,
    serial: String,
    package: String,
    options: devtools::BuildOptions,
    on_event: Channel<TransferEvent>,
) -> Result<String> {
    let paths = devtools_paths(&app)?;
    let adb = state.adb().await?;
    let em = Emitter::new(on_event);
    Ok(spawn_job(&state, "dt-auto", move |c| {
        devtools::run_auto(Some(adb), paths, serial, package, options, em, c)
    })
    .await)
}

#[tauri::command]
async fn devtools_workspaces(app: AppHandle) -> Result<Vec<devtools::workspace::WorkspaceMeta>> {
    let root = devtools_paths(&app)?.root;
    tokio::task::spawn_blocking(move || {
        devtools::workspace::cleanup_broken(&root);
        Ok(devtools::workspace::list_all(&root))
    })
    .await?
}

#[tauri::command]
async fn devtools_workspace(app: AppHandle, id: String) -> Result<DevtoolsWorkspace> {
    let ws = devtools_ws(&app, &id)?;
    Ok(DevtoolsWorkspace { meta: ws.load()?, manifest: ws.manifest_info()? })
}

#[tauri::command]
async fn devtools_delete(app: AppHandle, id: String) -> Result<()> {
    let ws = devtools_ws(&app, &id)?;
    tokio::task::spawn_blocking(move || ws.delete()).await?
}

/// Podgląd manifestu po zmianach (bez zapisu) - diff w GUI.
#[tauri::command]
async fn devtools_preview_patch(
    app: AppHandle,
    id: String,
    options: devtools::manifest::PatchOptions,
) -> Result<devtools::manifest::PatchResult> {
    let ws = devtools_ws(&app, &id)?;
    let xml = ws.manifest_text(true)?;
    devtools::manifest::patch(&xml, &options)
}

#[tauri::command]
async fn devtools_manifest(app: AppHandle, id: String) -> Result<String> {
    devtools_ws(&app, &id)?.manifest_text(true)
}

#[tauri::command]
async fn devtools_res_list(app: AppHandle, id: String, rel: String) -> Result<Vec<devtools::workspace::ResEntry>> {
    let ws = devtools_ws(&app, &id)?;
    let replaced = ws.load()?.replaced;
    tokio::task::spawn_blocking(move || devtools::workspace::list_dir(&ws.src(), &rel, &replaced)).await?
}

#[tauri::command]
async fn devtools_res_preview(app: AppHandle, id: String, rel: String) -> Result<devtools::workspace::ResPreview> {
    let ws = devtools_ws(&app, &id)?;
    let replaced = ws.load()?.replaced;
    tokio::task::spawn_blocking(move || devtools::workspace::preview(&ws.src(), &rel, &replaced)).await?
}

/// Podmiana zasobu plikiem z dysku. Zwraca ostrzeżenia (np. 9-patch).
#[tauri::command]
async fn devtools_res_replace(app: AppHandle, id: String, rel: String, file: String) -> Result<Vec<String>> {
    let ws = devtools_ws(&app, &id)?;
    tokio::task::spawn_blocking(move || devtools::workspace::replace(&ws, &rel, &PathBuf::from(file))).await?
}

#[tauri::command]
async fn devtools_res_restore(app: AppHandle, id: String, rel: String) -> Result<()> {
    let ws = devtools_ws(&app, &id)?;
    tokio::task::spawn_blocking(move || devtools::workspace::restore(&ws, &rel)).await?
}

#[tauri::command]
async fn devtools_res_export(app: AppHandle, id: String, rel: String, dest: String) -> Result<()> {
    let ws = devtools_ws(&app, &id)?;
    tokio::task::spawn_blocking(move || devtools::workspace::export_file(&ws.src(), &rel, &PathBuf::from(dest))).await?
}

/// Ręczny import frameworku producenta (framework-res.apk itp.).
#[tauri::command]
async fn devtools_import_framework(app: AppHandle, path: String) -> Result<String> {
    devtools::import_framework(&devtools_paths(&app)?, &PathBuf::from(path)).await
}

// --------------------------------------------------------------------------- //
// Automatyczna detekcja urządzeń
// --------------------------------------------------------------------------- //

/// Co 2 s skanuje ADB/Fastboot/MTP i emituje `devices-changed`, gdy lista się zmieni.
/// Na Windows MTP (PowerShell) odświeżamy rzadziej - co ~10 s.
async fn watch_devices(app: AppHandle) {
    let mut last = String::new();
    let mut tick: u64 = 0;
    let mut mtp_cache = Vec::new();
    loop {
        // flashowanie (Heimdall) ma wyłączność na USB - nie skanujemy adb/fastboot/MTP
        if hardware::usb::poll_paused() {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        let adb = app.state::<AppState>().adb().await.ok();
        if cfg!(not(windows)) || tick % 5 == 0 {
            mtp_cache = mtp_handler::scan().await;
        }
        let devs: Vec<Device> = match adb {
            Some(a) => adb::device::scan(&a, Some(&mtp_cache)).await.unwrap_or_default(),
            None => Vec::new(),
        };
        let sig = serde_json::to_string(&devs).unwrap_or_default();
        if sig != last {
            let _ = app.emit("devices-changed", &devs);
            last = sig;
        }
        tick = tick.wrapping_add(1);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Linux: app_id / WM_CLASS musi być ustawiony PRZED startem GTK
    #[cfg(target_os = "linux")]
    linux_desktop::early_init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::new())
        .setup(|app| {
            // po aktualizacji wersji portable: usuń *.exe.old i katalog roboczy
            std::thread::spawn(updater::cleanup_after_update);
            // Ikona okna dla X11 (_NET_WM_ICON - pasek XFCE, MATE, KDE X11, Alt+Tab).
            // UWAGA: domyślna ikona Tauri to icon.png 512 px - GDK po cichu NIE ustawia
            // _NET_WM_ICON przy tak dużym obrazie (limit rozmiaru żądania X11), więc
            // okno zostaje bez ikony. 128 px mieści się zawsze (64 KB).
            if let Some(w) = app.get_webview_window("main") {
                if let Err(e) = w.set_icon(tauri::include_image!("icons/128x128.png")) {
                    eprintln!("[desktop] nie ustawiono ikony okna: {e}");
                }
            }
            // Linux: wpis .desktop + ikony hicolor (KDE Wayland dopasowuje ikonę po app_id)
            #[cfg(target_os = "linux")]
            std::thread::spawn(|| {
                if let Err(e) = linux_desktop::integrate() {
                    eprintln!("[desktop] integracja z pulpitem nie powiodła się: {e}");
                }
            });
            tauri::async_runtime::spawn(watch_devices(app.handle().clone()));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            locate_tools,
            set_adb_path,
            scan_devices,
            device_info,
            list_dir,
            detect_root,
            scan_partitions,
            reboot,
            start_pull,
            start_push,
            start_contacts_backup,
            start_contacts_restore,
            inspect_restore_dir,
            agent_install_confirm,
            cancel_job,
            update_check,
            update_install,
            update_install_package,
            list_apps,
            load_apps,
            scrcpy_status,
            scrcpy_download,
            scrcpy_start,
            take_screenshot,
            reveal_path,
            drivers_report,
            drivers_set_override,
            driver_install,
            udev_install,
            usb_scan,
            samsung_status,
            samsung_download_mode,
            xiaomi_arb,
            xiaomi_firmware_arb,
            edl_status,
            moto_load,
            moto_flash,
            diag_snapshot,
            diag_start,
            logcat_start,
            logcat_filter,
            logcat_export,
            logcat_clear,
            download_probe,
            read_pit,
            heimdall_status,
            firmware_inspect,
            samsung_flash,
            enterprise_adb,
            enterprise_fastboot,
            enterprise_mtp_reset,
            wifi_pair,
            wifi_connect,
            wifi_tcpip,
            wifi_disconnect,
            devtools_status,
            devtools_setup,
            devtools_open,
            devtools_build,
            devtools_auto,
            devtools_workspaces,
            devtools_workspace,
            devtools_delete,
            devtools_preview_patch,
            devtools_manifest,
            devtools_res_list,
            devtools_res_preview,
            devtools_res_replace,
            devtools_res_restore,
            devtools_res_export,
            devtools_import_framework,
            apps_action,
            extract_apks,
            install_apks,
            shell_command,
            start_sideload,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
