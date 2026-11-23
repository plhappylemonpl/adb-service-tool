//! Menadżer aplikacji: lista pakietów, szczegóły (nazwa/wersja/ikona z APK),
//! ekstrakcja APK (z split-APK), instalacja, debloat bez roota.
//! Porty: AppListThread, ApkExtractThread, ApkInstallThread.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::adb::events::{Emitter, FailedItem};
use crate::adb::progress::Tracker;
use crate::adb::Adb;
use crate::apk::{self, zip::ApkSource};
use crate::error::{AdbError, Result};
use crate::utils::{is_valid_package, sanitize_filename, shell_quote, CancelToken};

/// Pakiety, których usunięcie/wyłączenie może zablokować urządzenie (bootloop,
/// brak ekranu głównego, brak dzwonienia). GUI wymaga dodatkowego potwierdzenia.
const CRITICAL: &[&str] = &[
    "android",
    "com.android.systemui",
    "com.android.settings",
    "com.android.phone",
    "com.android.shell",
    "com.android.server.telecom",
    "com.android.packageinstaller",
    "com.google.android.packageinstaller",
    "com.android.permissioncontroller",
    "com.google.android.permissioncontroller",
    "com.android.launcher3",
    "com.google.android.apps.nexuslauncher",
    "com.sec.android.app.launcher",
    "com.miui.home",
    "com.huawei.android.launcher",
    "com.google.android.gms",
    "com.google.android.gsf",
    "com.android.vending",
    "com.android.inputmethod.latin",
    "com.google.android.inputmethod.latin",
    "com.samsung.android.honeyboard",
    "com.android.bluetooth",
    "com.android.nfc",
    "com.android.externalstorage",
    "com.android.documentsui",
    "com.google.android.documentsui",
    "com.android.location.fused",
    "com.android.keychain",
    "com.android.se",
    "com.android.networkstack",
    "com.google.android.networkstack",
    "com.android.webview",
    "com.google.android.webview",
    "com.android.carrierconfig",
    "com.android.emergency",
    "com.android.wifi.resources",
    "com.android.ims",
    "com.android.mms.service",
    "com.qualcomm.qti.telephonyservice",
    "com.samsung.android.providers.context",
    "com.sec.android.app.SecSetupWizard",
    "com.google.android.setupwizard",
    "com.android.providers.downloads",
];

fn is_critical(pkg: &str) -> bool {
    CRITICAL.contains(&pkg) || pkg.starts_with("com.android.providers.") || pkg.starts_with("com.android.modules.")
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppEntry {
    pub package: String,
    pub apk_path: Option<String>,
    pub system: bool,
    pub enabled: bool,
    /// false = odinstalowana dla użytkownika 0 (do przywrócenia)
    pub installed: bool,
    pub critical: bool,
}

/// Jedno wywołanie powłoki zwraca wszystkie potrzebne listy (oddzielone markerami).
pub async fn list(adb: &Adb, serial: &str) -> Result<Vec<AppEntry>> {
    let script = "echo __F__; pm list packages -f -u 2>/dev/null; \
                  echo __3__; pm list packages -3 2>/dev/null; \
                  echo __D__; pm list packages -d 2>/dev/null; \
                  echo __I__; pm list packages 2>/dev/null; echo __END__";
    let r = adb.shell(Some(serial), script, 60).await?;

    let mut paths: Vec<(String, Option<String>)> = Vec::new();
    let mut third: HashSet<String> = HashSet::new();
    let mut disabled: HashSet<String> = HashSet::new();
    let mut installed: HashSet<String> = HashSet::new();
    let mut section = "";

    for line in r.stdout.lines() {
        let l = line.trim();
        match l {
            "__F__" | "__3__" | "__D__" | "__I__" => {
                section = l;
                continue;
            }
            "__END__" => break,
            _ => {}
        }
        let Some(rest) = l.strip_prefix("package:") else { continue };
        match section {
            "__F__" => match rest.rsplit_once('=') {
                // package:/data/app/~~x==/com.foo-y==/base.apk=com.foo  (split na OSTATNIM '=')
                Some((p, pkg)) => paths.push((pkg.to_string(), Some(p.to_string()))),
                None => paths.push((rest.to_string(), None)),
            },
            "__3__" => {
                third.insert(rest.to_string());
            }
            "__D__" => {
                disabled.insert(rest.to_string());
            }
            "__I__" => {
                installed.insert(rest.to_string());
            }
            _ => {}
        }
    }

    // bardzo stare Androidy nie znają "-u"/"-f" razem - fallback z listy zainstalowanych
    if paths.is_empty() {
        paths = installed.iter().map(|p| (p.clone(), None)).collect();
    }
    if paths.is_empty() && !r.ok() {
        return Err(AdbError::Command(r.err_text()));
    }

    let mut out: Vec<AppEntry> = paths
        .into_iter()
        .filter(|(p, _)| is_valid_package(p))
        .map(|(package, apk_path)| AppEntry {
            system: !third.contains(&package),
            enabled: !disabled.contains(&package),
            installed: installed.is_empty() || installed.contains(&package),
            critical: is_critical(&package),
            apk_path,
            package,
        })
        .collect();
    out.sort_by_key(|a| a.package.to_lowercase());
    out.dedup_by(|a, b| a.package == b.package);
    Ok(out)
}

// --------------------------------------------------------------------------- //
// Szczegóły + ikona (z cache na dysku)
// --------------------------------------------------------------------------- //

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AppDetails {
    pub package: String,
    pub label: Option<String>,
    pub version_name: Option<String>,
    /// data:URL gotowy do <img src>
    pub icon: Option<String>,
    pub icon_bg: Option<String>,
    pub icon_bg_image: Option<String>,
    pub adaptive: bool,
    pub apk_size: u64,
}

fn data_url(mime: &str, data: &[u8]) -> String {
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(data)
    )
}

/// Szczegóły aplikacji. Najpierw cache (klucz: pakiet + rozmiar + mtime APK),
/// potem odczyt zdalny (dd po kawałku), a w ostateczności pobranie APK do tempa.
pub async fn details(
    adb: Arc<Adb>,
    serial: &str,
    package: &str,
    apk_path: &str,
    lang: &str,
    cache_dir: &Path,
) -> Result<AppDetails> {
    if !is_valid_package(package) {
        return Err(AdbError::InvalidArg(package.into()));
    }
    let st = adb
        .shell(Some(serial), &format!("stat -c '%s %Y' {} 2>/dev/null", shell_quote(apk_path)), 10)
        .await?;
    let mut it = st.stdout.split_whitespace();
    let size: u64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let mtime: u64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    if size == 0 {
        return Err(AdbError::Command(format!("nie można odczytać {apk_path}")));
    }

    let cache_file = cache_dir.join(format!("{}_{size}_{mtime}_{lang}.json", sanitize_filename(package)));
    if let Ok(txt) = tokio::fs::read_to_string(&cache_file).await {
        if let Ok(d) = serde_json::from_str::<AppDetails>(&txt) {
            return Ok(d);
        }
    }

    let remote = ApkSource::Remote {
        adb: adb.clone(),
        serial: serial.to_string(),
        path: apk_path.to_string(),
        size,
    };
    let parsed = match apk::read_details(&remote, lang).await {
        Ok(d) => d,
        Err(_) if size <= 200 * 1024 * 1024 => {
            // fallback: całe APK do tempa (np. brak `dd` na urządzeniu)
            let tmp = cache_dir.join(format!("tmp_{}.apk", sanitize_filename(package)));
            let tmp_s = tmp.to_string_lossy().to_string();
            let pulled = adb.run(Some(serial), &["pull", apk_path, &tmp_s], 300).await;
            let res = match pulled {
                Ok(o) if o.ok() => {
                    let local = ApkSource::Local {
                        path: tmp.clone(),
                        size: tokio::fs::metadata(&tmp).await.map(|m| m.len()).unwrap_or(0),
                    };
                    apk::read_details(&local, lang).await
                }
                Ok(o) => Err(AdbError::Command(o.err_text())),
                Err(e) => Err(e),
            };
            let _ = tokio::fs::remove_file(&tmp).await;
            res?
        }
        Err(e) => return Err(e),
    };

    let mut d = AppDetails {
        package: package.to_string(),
        label: parsed.label,
        version_name: parsed.version_name,
        apk_size: size,
        ..Default::default()
    };
    if let Some(ic) = parsed.icon {
        d.icon = Some(data_url(&ic.mime, &ic.data));
        d.icon_bg = ic.bg_color;
        d.icon_bg_image = ic.bg_image.map(|(m, b)| data_url(&m, &b));
        d.adaptive = ic.adaptive;
    }

    let _ = tokio::fs::create_dir_all(cache_dir).await;
    if let Ok(js) = serde_json::to_string(&d) {
        let _ = tokio::fs::write(&cache_file, js).await;
    }
    Ok(d)
}

// --------------------------------------------------------------------------- //
// Debloat / akcje na pakietach (z obsługą zgodności Android 5 ... 15)
// --------------------------------------------------------------------------- //

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AppAction {
    /// pm uninstall -k --user 0 (bez roota, odwracalne, dane zachowane)
    Uninstall,
    /// pm uninstall (pełne usunięcie - działa dla aplikacji użytkownika;
    /// dla systemowych bez roota zwykle DELETE_FAILED_INTERNAL_ERROR)
    UninstallFull,
    /// pm disable-user --user 0
    Disable,
    /// pm enable
    Enable,
    /// cmd package install-existing / pm install-existing (przywrócenie dla usera 0)
    Restore,
    /// am force-stop
    ForceStop,
    /// pm clear (kasuje dane aplikacji!)
    Clear,
}

impl AppAction {
    /// Minimalne SDK, od którego komenda istnieje w `pm`/`am`.
    pub fn min_sdk(self) -> u32 {
        match self {
            // `--user` w `pm uninstall` od Androida 5.0
            AppAction::Uninstall => 21,
            // `pm install-existing` od Androida 5.0 (`cmd package` od 7.0)
            AppAction::Restore => 21,
            // `pm disable-user` od Androida 4.0
            AppAction::Disable => 14,
            _ => 1,
        }
    }

    fn command(self, pkg: &str, sdk: u32) -> String {
        match self {
            AppAction::Uninstall => format!("pm uninstall -k --user 0 {pkg}"),
            AppAction::UninstallFull => format!("pm uninstall {pkg}"),
            AppAction::Disable => {
                if sdk >= 17 {
                    format!("pm disable-user --user 0 {pkg}")
                } else {
                    format!("pm disable-user {pkg}")
                }
            }
            AppAction::Enable => format!("pm enable {pkg}"),
            AppAction::Restore => {
                if sdk >= 24 {
                    format!("cmd package install-existing --user 0 {pkg} 2>&1 || pm install-existing --user 0 {pkg}")
                } else {
                    format!("pm install-existing --user 0 {pkg}")
                }
            }
            // am force-stop nic nie wypisuje - kod wyjścia decyduje
            AppAction::ForceStop => format!("am force-stop {pkg} && echo Success"),
            AppAction::Clear => format!("pm clear {pkg}"),
        }
    }

    fn succeeded(self, out: &str) -> bool {
        let o = out.to_lowercase();
        if o.contains("failure") || o.contains("exception") || o.contains("error:") {
            return false;
        }
        match self {
            AppAction::Disable => o.contains("disabled"),
            AppAction::Enable => o.contains("enabled"),
            AppAction::Restore => o.contains("installed for user") || o.contains("success"),
            _ => o.contains("success"),
        }
    }
}

/// Zamienia surowy komunikat `pm`/`am` na czytelny opis (PL / EN).
pub fn explain_failure(action: AppAction, raw: &str, sdk: u32) -> String {
    let o = raw.to_lowercase();
    let hint = if o.contains("delete_failed_internal_error") {
        if action == AppAction::UninstallFull {
            "Aplikacja systemowa - pełne usunięcie wymaga roota. Użyj „Usuń dla użytkownika 0” / system app: full uninstall needs root, use “uninstall for user 0”"
        } else {
            "Błąd wewnętrzny instalatora pakietów / package manager internal error"
        }
    } else if o.contains("delete_failed_device_policy_manager") {
        "Aplikacja jest administratorem urządzenia - wyłącz ją w Ustawienia > Zabezpieczenia > Administratorzy / app is a device admin"
    } else if o.contains("delete_failed_user_restricted") || o.contains("delete_failed_owner_blocked") {
        "Usuwanie zablokowane przez politykę urządzenia / blocked by device policy"
    } else if o.contains("not installed for 0") || o.contains("not installed for user") {
        "Pakiet jest już usunięty dla użytkownika 0 / already removed for user 0"
    } else if o.contains("unknown package") || o.contains("doesn't exist") || o.contains("not found") {
        "Nieznany pakiet (mógł zostać już usunięty) / unknown package"
    } else if o.contains("cannot change component state")
        || o.contains("cannot disable")
        || o.contains("protected package")
    {
        "Producent blokuje zmianę stanu tego pakietu z poziomu powłoki / OEM blocks changing this package"
    } else if o.contains("securityexception") || o.contains("permission denial") || o.contains("not allowed") {
        "Brak uprawnień powłoki ADB dla tej operacji (wymaga roota) / shell lacks permission"
    } else if o.contains("unknown option") || o.contains("unknown command") || o.contains("usage:") {
        return format!(
            "Komenda nieobsługiwana na tym Androidzie (SDK {sdk}) / command not supported on this Android (SDK {sdk}). {}",
            raw.lines().next().unwrap_or("").trim()
        );
    } else if raw.trim().is_empty() {
        "Brak odpowiedzi od urządzenia / no response"
    } else {
        return raw.trim().to_string();
    };
    format!("{hint} [{}]", raw.lines().next().unwrap_or("").trim())
}

pub async fn run_action(
    adb: Arc<Adb>,
    serial: String,
    packages: Vec<String>,
    action: AppAction,
    emitter: Emitter,
    cancel: CancelToken,
) {
    let total = packages.len() as u64;
    let sdk = adb.sdk(&serial).await.unwrap_or(0);
    let tracker = Tracker::new(total, 0);
    let ticker = tracker.start(emitter.clone());
    let mut ok = 0u64;
    let mut fails = Vec::new();

    for pkg in &packages {
        if cancel.is_cancelled() {
            break;
        }
        tracker.set_current(pkg.clone());

        let pre_error = if !is_valid_package(pkg) {
            Some("nieprawidłowa nazwa pakietu / invalid package".to_string())
        } else if sdk != 0 && sdk < action.min_sdk() {
            Some(format!(
                "Komenda wymaga Androida z SDK >= {} (urządzenie: SDK {sdk}) / requires SDK >= {}",
                action.min_sdk(),
                action.min_sdk()
            ))
        } else {
            None
        };

        let (success, msg) = match pre_error {
            Some(e) => (false, e),
            None => match adb.shell(Some(&serial), &action.command(pkg, sdk), 60).await {
                Ok(o) => {
                    let text = format!("{} {}", o.stdout.trim(), o.stderr.trim()).trim().to_string();
                    if action.succeeded(&text) {
                        (true, text)
                    } else {
                        (false, explain_failure(action, &text, sdk))
                    }
                }
                Err(e) => (false, e.to_string()),
            },
        };

        if success {
            ok += 1;
            emitter.ok(format!("{pkg}: {msg}"));
        } else {
            emitter.error(format!("{pkg}: {msg}"));
            fails.push(FailedItem {
                path: pkg.clone(),
                error: msg.clone(),
            });
        }
        emitter.item(pkg, success, msg);
        tracker.item_done();
    }
    tracker.shutdown(ticker).await;
    emitter.finished(ok, 0, total, fails, cancel.is_cancelled());
}

// --------------------------------------------------------------------------- //
// Ładowanie listy + nazw/ikon ze strumieniowym postępem (zakładka Aplikacje)
// --------------------------------------------------------------------------- //

/// Zdarzenia ładowania listy aplikacji -> GUI (pasek postępu nad listą).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum AppsLoadEvent {
    /// Etap: "list" (pm list packages) | "details" (nazwy/ikony z APK)
    Stage { stage: String },
    /// Lista pakietów gotowa (pokazujemy ją od razu, szczegóły dochodzą w tle).
    List { apps: Vec<AppEntry>, sdk: u32 },
    /// Szczegóły jednej aplikacji (nazwa, wersja, ikona).
    Details { details: AppDetails },
    Progress { done: u64, total: u64, current: Option<String> },
    Finished { cancelled: bool, failed: u64 },
    Error { message: String },
}

pub async fn run_load(
    adb: Arc<Adb>,
    serial: String,
    lang: String,
    cache_dir: PathBuf,
    channel: tauri::ipc::Channel<AppsLoadEvent>,
    cancel: CancelToken,
) {
    let send = |e: AppsLoadEvent| {
        let _ = channel.send(e);
    };

    send(AppsLoadEvent::Stage { stage: "list".into() });
    let sdk = adb.sdk(&serial).await.unwrap_or(0);
    let apps = match list(&adb, &serial).await {
        Ok(a) => a,
        Err(e) => {
            send(AppsLoadEvent::Error { message: e.to_string() });
            send(AppsLoadEvent::Finished { cancelled: false, failed: 0 });
            return;
        }
    };
    send(AppsLoadEvent::List { apps: apps.clone(), sdk });
    if cancel.is_cancelled() {
        send(AppsLoadEvent::Finished { cancelled: true, failed: 0 });
        return;
    }

    // kolejność: najpierw aplikacje użytkownika (to je serwisant widzi na start)
    let mut queue: Vec<(bool, String, String)> = apps
        .iter()
        .filter_map(|a| a.apk_path.clone().map(|p| (a.system, a.package.clone(), p)))
        .collect();
    // sortowanie stabilne: false (użytkownika) przed true (systemowe)
    queue.sort_by_key(|(system, _, _)| *system);
    let queue: Vec<(String, String)> = queue.into_iter().map(|(_, pkg, p)| (pkg, p)).collect();

    let total = queue.len() as u64;
    send(AppsLoadEvent::Stage { stage: "details".into() });
    send(AppsLoadEvent::Progress { done: 0, total, current: None });

    let queue = Arc::new(queue);
    let next = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let done = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let failed = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let channel = Arc::new(channel);

    // 3 równoległe odczyty - kompromis między szybkością a obciążeniem starych urządzeń
    let mut handles = Vec::new();
    for _ in 0..3 {
        let (queue, next, done, failed, channel) =
            (queue.clone(), next.clone(), done.clone(), failed.clone(), channel.clone());
        let (adb, serial, lang, cache_dir, cancel) =
            (adb.clone(), serial.clone(), lang.clone(), cache_dir.clone(), cancel.clone());
        handles.push(tokio::spawn(async move {
            use std::sync::atomic::Ordering;
            loop {
                if cancel.is_cancelled() {
                    break;
                }
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some((pkg, apk)) = queue.get(i) else { break };
                let det = tokio::select! {
                    r = details(adb.clone(), &serial, pkg, apk, &lang, &cache_dir) => r,
                    _ = cancel.cancelled() => break,
                };
                let det = det.unwrap_or_else(|_| {
                    failed.fetch_add(1, Ordering::SeqCst);
                    AppDetails {
                        package: pkg.clone(),
                        ..Default::default()
                    }
                });
                let _ = channel.send(AppsLoadEvent::Details { details: det });
                let d = done.fetch_add(1, Ordering::SeqCst) + 1;
                let _ = channel.send(AppsLoadEvent::Progress {
                    done: d,
                    total,
                    current: Some(pkg.clone()),
                });
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    let _ = channel.send(AppsLoadEvent::Finished {
        cancelled: cancel.is_cancelled(),
        failed: failed.load(std::sync::atomic::Ordering::SeqCst),
    });
}

// --------------------------------------------------------------------------- //
// Ekstrakcja APK (port ApkExtractThread)
// --------------------------------------------------------------------------- //

pub(crate) async fn pm_path(adb: &Adb, serial: &str, pkg: &str) -> std::result::Result<Vec<String>, String> {
    let r = adb
        .shell(Some(serial), &format!("pm path {pkg}"), 20)
        .await
        .map_err(|e| e.to_string())?;
    let paths: Vec<String> = r
        .stdout
        .lines()
        .filter_map(|l| l.trim().strip_prefix("package:").map(str::to_string))
        .collect();
    if paths.is_empty() {
        let e = r.err_text();
        return Err(if e.is_empty() {
            "Nie znaleziono plików APK (aplikacja mogła zostać odinstalowana) / no APK found".into()
        } else {
            e
        });
    }
    Ok(paths)
}

pub async fn run_extract(
    adb: Arc<Adb>,
    serial: String,
    packages: Vec<String>,
    out_dir: String,
    emitter: Emitter,
    cancel: CancelToken,
) {
    let total = packages.len() as u64;
    let tracker = Tracker::new(total, 0);
    let ticker = tracker.start(emitter.clone());
    let mut ok = 0u64;
    let mut fails: Vec<FailedItem> = Vec::new();

    for (i, pkg) in packages.iter().enumerate() {
        if cancel.is_cancelled() {
            emitter.warn("Anulowano wyciąganie APK / cancelled.");
            break;
        }
        let n = i + 1;
        tracker.set_current(pkg.clone());
        if !is_valid_package(pkg) {
            fails.push(FailedItem { path: pkg.clone(), error: "invalid package".into() });
            tracker.item_done();
            continue;
        }
        emitter.info(format!("[{n}/{total}] {pkg}: szukam plików APK / locating..."));
        let paths = match pm_path(&adb, &serial, pkg).await {
            Ok(p) => p,
            Err(e) => {
                emitter.error(format!("[{n}/{total}] {pkg}: {e}"));
                fails.push(FailedItem { path: pkg.clone(), error: e });
                tracker.item_done();
                continue;
            }
        };
        let pkg_dir = PathBuf::from(&out_dir).join(sanitize_filename(&pkg.replace(' ', "_")));
        if let Err(e) = tokio::fs::create_dir_all(&pkg_dir).await {
            emitter.error(format!("[{n}/{total}] {pkg}: mkdir: {e}"));
            fails.push(FailedItem { path: pkg.clone(), error: e.to_string() });
            tracker.item_done();
            continue;
        }

        let mut all_ok = true;
        for remote in &paths {
            let fname = remote.rsplit('/').next().unwrap_or("base.apk");
            let local = pkg_dir.join(sanitize_filename(fname));
            let local_s = local.to_string_lossy().to_string();
            match adb.run_c(Some(&serial), &["pull", remote, &local_s], 600, &cancel).await {
                Ok(o) if o.ok() && local.exists() => {}
                Ok(o) => {
                    all_ok = false;
                    let e = o.err_text();
                    emitter.error(format!("[{n}/{total}] {pkg}: {remote}: {e}"));
                    fails.push(FailedItem { path: pkg.clone(), error: format!("{remote}: {e}") });
                }
                Err(AdbError::Cancelled) => {
                    all_ok = false;
                    let _ = tokio::fs::remove_file(&local).await;
                    break;
                }
                Err(e) => {
                    all_ok = false;
                    emitter.error(format!("[{n}/{total}] {pkg}: {remote}: {e}"));
                    fails.push(FailedItem { path: pkg.clone(), error: e.to_string() });
                }
            }
        }
        if all_ok {
            ok += 1;
            emitter.ok(format!("[{n}/{total}] {pkg}: OK ({} plik(ów) APK)", paths.len()));
            emitter.item(pkg, true, pkg_dir.to_string_lossy());
        } else {
            emitter.item(pkg, false, "extract failed");
        }
        tracker.item_done();
    }
    tracker.shutdown(ticker).await;
    emitter.finished(ok, 0, total, fails, cancel.is_cancelled());
}

// --------------------------------------------------------------------------- //
// Instalacja APK (port ApkInstallThread)
// --------------------------------------------------------------------------- //

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct InstallOptions {
    /// -d: pozwól na downgrade
    #[serde(default)]
    pub downgrade: bool,
    /// -g: nadaj wszystkie uprawnienia z manifestu
    #[serde(default)]
    pub grant_all: bool,
}

/// Grupy instalacyjne: (nazwa, pliki). Plik .apk = osobna aplikacja;
/// folder = jego .apk to jedna aplikacja (split) + podfoldery po jednej aplikacji.
fn discover_groups(paths: &[String]) -> (Vec<(String, Vec<PathBuf>)>, Vec<String>) {
    let mut groups = Vec::new();
    let mut warnings = Vec::new();
    let apks_in = |dir: &Path| -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.is_file()
                            && p.extension().map(|e| e.eq_ignore_ascii_case("apk")).unwrap_or(false)
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    };

    for p in paths {
        let path = Path::new(p);
        let name = path
            .file_stem()
            .or_else(|| path.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "apk".into());
        if path.is_file() {
            if path.extension().map(|e| e.eq_ignore_ascii_case("apk")).unwrap_or(false) {
                groups.push((name, vec![path.to_path_buf()]));
            } else {
                warnings.push(format!("Pomijam {p} - to nie jest .apk / not an apk"));
            }
        } else if path.is_dir() {
            let top = apks_in(path);
            if !top.is_empty() {
                groups.push((name, top));
            }
            let mut subs: Vec<PathBuf> = std::fs::read_dir(path)
                .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
                .unwrap_or_default();
            subs.sort();
            for s in subs {
                let apks = apks_in(&s);
                if !apks.is_empty() {
                    let n = s.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
                    groups.push((n, apks));
                }
            }
        } else {
            warnings.push(format!("Nie znaleziono {p} / not found"));
        }
    }
    (groups, warnings)
}

pub async fn run_install(
    adb: Arc<Adb>,
    serial: String,
    paths: Vec<String>,
    opts: InstallOptions,
    emitter: Emitter,
    cancel: CancelToken,
) {
    let (groups, warnings) = match tokio::task::spawn_blocking(move || discover_groups(&paths)).await {
        Ok(v) => v,
        Err(e) => {
            emitter.fatal(e.to_string());
            return;
        }
    };
    for w in warnings {
        emitter.warn(w);
    }
    let total = groups.len() as u64;
    if total == 0 {
        emitter.warn("Nie znaleziono plików .apk / no .apk files found.");
        emitter.finished(0, 0, 0, vec![], false);
        return;
    }

    let bytes_total: u64 = groups
        .iter()
        .flat_map(|(_, f)| f.iter())
        .filter_map(|p| std::fs::metadata(p).ok().map(|m| m.len()))
        .sum();
    let tracker = Tracker::new(total, bytes_total);
    let ticker = tracker.start(emitter.clone());
    let mut ok = 0u64;
    let mut fails = Vec::new();

    for (i, (name, files)) in groups.iter().enumerate() {
        if cancel.is_cancelled() {
            emitter.warn("Anulowano instalację / cancelled.");
            break;
        }
        let n = i + 1;
        tracker.set_current(name.clone());
        emitter.info(format!("[{n}/{total}] Instaluję / installing {name} ({} plik(ów))...", files.len()));

        let mut args: Vec<String> = vec![if files.len() == 1 { "install" } else { "install-multiple" }.into(), "-r".into()];
        if opts.downgrade {
            args.push("-d".into());
        }
        if opts.grant_all {
            args.push("-g".into());
        }
        args.extend(files.iter().map(|p| p.to_string_lossy().to_string()));
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();

        let group_bytes: u64 = files.iter().filter_map(|p| std::fs::metadata(p).ok().map(|m| m.len())).sum();
        let res = adb.run_c(Some(&serial), &argv, 900, &cancel).await;
        tracker.add_bytes(group_bytes);
        match res {
            Ok(o) if o.ok() && o.stdout.contains("Success") => {
                ok += 1;
                emitter.ok(format!("[{n}/{total}] {name}: zainstalowano / installed OK"));
                emitter.item(name, true, "Success");
            }
            Err(AdbError::Cancelled) => break,
            Ok(o) => {
                let e = format!("{} {}", o.stdout.trim(), o.stderr.trim()).trim().to_string();
                emitter.error(format!("[{n}/{total}] {name}: {e}"));
                emitter.item(name, false, e.clone());
                fails.push(FailedItem { path: name.clone(), error: e });
            }
            Err(e) => {
                emitter.error(format!("[{n}/{total}] {name}: {e}"));
                emitter.item(name, false, e.to_string());
                fails.push(FailedItem { path: name.clone(), error: e.to_string() });
            }
        }
        tracker.item_done();
    }
    tracker.shutdown(ticker).await;
    emitter.finished(ok, 0, total, fails, cancel.is_cancelled());
}
