//! Kontakty i SMS przez aplikację pomocniczą "Drapco Backup Agent" - kopia i przywracanie.
//!
//! `content query` jest na nowszych Androidach blokowane przez producentów,
//! więc eksport/import robi mała aplikacja na urządzeniu - zawsze za zgodą
//! użytkownika (systemowe okna uprawnień + przyciski w agencie).
//!
//! KOPIA (każdy krok zwraca `Result`):
//!  1. deploy - `Agent.apk` z zasobów aplikacji (albo kopia wbudowana w binarkę)
//!     -> `adb install -r -t`; gdy instalacja przez USB jest zablokowana
//!     (np. Xiaomi "Instaluj przez USB" wyłączone) -> plan B: `adb push` do
//!     `/sdcard/Download/Agent.apk` i ręczna instalacja przez użytkownika,
//!  2. launch - wybudzenie ekranu + `am start -n com.drapco.agent/.MainActivity --es session <id>`,
//!  3. wait - co 2 s `ls /sdcard/Download/Backup/<id>/` aż pojawi się `done.json`
//!     (lub `error.json`); agent zapisuje znacznik OSTATNI, po zamknięciu plików danych,
//!  4. pull - `adb pull` plików wymienionych w znaczniku + weryfikacja rozmiarów,
//!  5. cleanup - ZAWSZE (także po błędzie / anulowaniu): agent usuwa swoje pliki
//!     z MediaStore (`am broadcast ... CleanupReceiver`), `rm -rf` folderu sesji,
//!     `adb uninstall com.drapco.agent`.
//!
//! PRZYWRACANIE:
//!  1. deploy (jak wyżej) + nadanie agentowi odczytu plików
//!     (Android 11+: `appops set ... MANAGE_EXTERNAL_STORAGE allow`, 6-10: `pm grant READ_EXTERNAL_STORAGE`),
//!  2. push - `contacts.vcf` i `sms.xml` do `/sdcard/Download/Restore/<id>/` + weryfikacja rozmiarów,
//!  3. launch - `am start ... --es session <id> --es mode restore`,
//!  4. wait - aż agent zapisze `restore_done.json` (użytkownik kliknął "Gotowe"),
//!  5. cleanup - ZAWSZE: przywrócenie poprzedniej domyślnej aplikacji SMS (jeśli agent
//!     nadal nią jest), usunięcie plików z urządzenia, odinstalowanie agenta.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

use crate::adb::events::{Emitter, FailedItem};
use crate::adb::pull::check_pull_output;
use crate::adb::Adb;
use crate::error::{AdbError, Result};
use crate::utils::{sanitize_filename, shell_quote, CancelToken};

pub const AGENT_PKG: &str = "com.drapco.agent";
const AGENT_ACTIVITY: &str = "com.drapco.agent/.MainActivity";
const AGENT_RECEIVER: &str = "com.drapco.agent/.CleanupReceiver";
const REMOTE_BASE: &str = "/sdcard/Download/Backup";
const RESTORE_BASE: &str = "/sdcard/Download/Restore";
/// Plan B instalacji: plik dla menedżera plików na urządzeniu.
pub const MANUAL_APK: &str = "/sdcard/Download/Agent.apk";
const POLL_EVERY: Duration = Duration::from_secs(2);
const MANUAL_INSTALL_LIMIT: Duration = Duration::from_secs(15 * 60);
const MIN_SDK: u32 = 21;
const ROLE_SMS: &str = "android.app.role.SMS";

/// Kopia agenta wbudowana w binarkę - działa także bez pliku w zasobach.
/// (cfg `agent_apk` ustawia build.rs, gdy plik istnieje w czasie kompilacji)
#[cfg(agent_apk)]
static EMBEDDED_APK: &[u8] = include_bytes!("../../resources/agent/Agent.apk");
#[cfg(not(agent_apk))]
static EMBEDDED_APK: &[u8] = &[];

/// Wspólny kontekst zadania agenta.
pub struct AgentCtx {
    pub adb: Arc<Adb>,
    pub serial: String,
    /// `resources/agent/Agent.apk` z katalogu zasobów aplikacji.
    pub resource_apk: Option<PathBuf>,
    pub cache_dir: PathBuf,
    /// Przycisk "Gotowe" w oknie ręcznej instalacji (komenda `agent_install_confirm`).
    pub confirm: Arc<Notify>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupOptions {
    /// Folder na komputerze (powstanie w nim podfolder <model>_<data>).
    pub dest_dir: String,
    /// Ile minut czekać na kliknięcie "Wykonaj kopię" na urządzeniu.
    #[serde(default = "default_wait")]
    pub wait_minutes: u64,
}

fn default_wait() -> u64 {
    10
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreOptions {
    /// Folder kopii na komputerze (contacts.vcf / sms.xml, np. "Kontakty_SMS_<model>_<data>").
    pub source_dir: String,
    /// Ile minut czekać na "Gotowe" w agencie (przywracanie tysięcy SMS trwa).
    #[serde(default = "default_restore_wait")]
    pub wait_minutes: u64,
}

fn default_restore_wait() -> u64 {
    30
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupResult {
    pub dir: String,
    pub contacts: u64,
    pub sms: u64,
    pub contacts_denied: bool,
    pub sms_denied: bool,
    pub files: Vec<String>,
    pub device: String,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RestoreResult {
    pub contacts_file: bool,
    pub contacts_cards: u64,
    /// "intent" (aplikacja Kontakty), "direct" (import agenta) albo "none".
    pub contacts_method: String,
    /// Liczba zaimportowanych kontaktów (tylko "direct"; -1 = nieznana).
    pub contacts_imported: i64,
    pub sms_file: bool,
    pub sms_total: u64,
    pub sms_inserted: u64,
    pub sms_skipped: u64,
    pub sms_failed: u64,
    pub mms_skipped: u64,
    pub sms_restored: bool,
    /// Agent był domyślną aplikacją SMS w trakcie przywracania.
    pub default_sms_changed: bool,
    /// Po zakończeniu domyślna aplikacja SMS to znów poprzednia (przez użytkownika lub komputer).
    pub default_sms_restored: bool,
    /// Nazwa pakietu poprzedniej aplikacji SMS.
    pub previous_sms_app: String,
    pub incoming_sms_saved: u64,
    pub incoming_sms_lost: u64,
    pub incoming_mms: u64,
    pub manual_install: bool,
    pub device: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MarkerFile {
    name: String,
    size: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DoneMarker {
    #[serde(default)]
    session: String,
    #[serde(default)]
    contacts: u64,
    #[serde(default)]
    sms: u64,
    #[serde(default)]
    contacts_denied: bool,
    #[serde(default)]
    sms_denied: bool,
    #[serde(default)]
    files: Vec<MarkerFile>,
    #[serde(default)]
    device: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RestoreMarker {
    session: String,
    contacts_file: bool,
    contacts_cards: u64,
    contacts_method: String,
    contacts_imported: i64,
    sms_file: bool,
    sms_total: u64,
    sms_inserted: u64,
    sms_skipped: u64,
    sms_failed: u64,
    mms_skipped: u64,
    sms_restored: bool,
    default_sms_changed: bool,
    default_sms_restored: bool,
    previous_sms_app: String,
    incoming_sms_saved: u64,
    incoming_sms_lost: u64,
    incoming_mms: u64,
    device: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ErrorMarker {
    #[serde(default)]
    error: String,
}

/// Stan do sprzątania - wypełniany w trakcie, sprzątany zawsze.
#[derive(Default)]
struct Trace {
    installed: bool,
    /// Agent.apk wgrany do Download/ (plan B) - do usunięcia.
    pushed_apk: bool,
    /// Folder sesji na urządzeniu (Backup/<id> albo Restore/<id>) + sesja dla CleanupReceiver.
    remote_dir: Option<String>,
    remote_base: &'static str,
    session: Option<String>,
    sdk: u32,
    /// Domyślna aplikacja SMS sprzed przywracania (None = nie dotyczy / nieznana).
    prev_sms: Option<String>,
    watch_sms: bool,
}

/// Wynik sprzątania.
struct CleanupReport {
    warnings: Vec<String>,
    /// Some(true/false) - czy po sprzątaniu domyślna aplikacja SMS nie jest agentem.
    sms_ok: Option<bool>,
}

fn stage(emitter: &Emitter, key: &str, detail: impl Into<String>) {
    emitter.item(format!("stage:{key}"), true, detail);
}

fn new_session() -> String {
    format!("adbst-{}", chrono::Local::now().format("%Y%m%d-%H%M%S-%3f"))
}

fn finish_err(emitter: &Emitter, e: AdbError, label: &str) {
    match e {
        AdbError::Cancelled => {
            emitter.warn("Anulowano - agent odinstalowany, pliki tymczasowe usunięte / cancelled.");
            emitter.finished(0, 1, 1, vec![], true);
        }
        e => {
            let msg = match e {
                AdbError::Other(m) => m,
                other => other.to_string(),
            };
            emitter.error(msg.clone());
            emitter.item("stage:failed", false, msg.clone());
            emitter.finished(0, 0, 1, vec![FailedItem { path: label.into(), error: msg }], false);
        }
    }
}

fn warn_items(w: Vec<String>) -> Vec<FailedItem> {
    w.into_iter().map(|w| FailedItem { path: "cleanup".into(), error: w }).collect()
}

// --------------------------------------------------------------------------- //
// Agent dla innych modułów (Smart EI): wgranie -> praca -> sprzątanie ZAWSZE
// --------------------------------------------------------------------------- //

/// Agent wgrany na czas zadania; `agent_release` sprząta (SMS, Agent.apk, odinstalowanie).
#[derive(Default)]
pub(crate) struct AgentLease {
    trace: Trace,
}

impl AgentLease {
    pub(crate) fn sdk(&self) -> u32 {
        self.trace.sdk
    }
}

/// Wgrywa agenta (ADB albo plan B: ręczna instalacja z Download/). Lease zwracany zawsze -
/// także po błędzie trzeba po nim posprzątać.
pub(crate) async fn agent_acquire(ctx: &AgentCtx, emitter: &Emitter, cancel: &CancelToken) -> (Result<u32>, AgentLease) {
    let mut lease = AgentLease::default();
    let r = deploy(ctx, emitter, cancel, &mut lease.trace).await;
    if let Ok(sdk) = r {
        grant_file_access(&ctx.adb, &ctx.serial, sdk, emitter).await;
    }
    (r, lease)
}

/// Zapamiętuje domyślną aplikację SMS (przywracanie SMS przez agenta zmienia ją na chwilę).
pub(crate) async fn agent_watch_sms(ctx: &AgentCtx, lease: &mut AgentLease) {
    lease.trace.watch_sms = true;
    lease.trace.prev_sms = current_sms_app(&ctx.adb, &ctx.serial, lease.trace.sdk)
        .await
        .filter(|p| p != AGENT_PKG);
}

/// `am start -n com.drapco.agent/<activity>` z dodatkami (wartości przekazywane jako osobne argumenty).
pub(crate) async fn agent_start(ctx: &AgentCtx, activity: &str, extras: &[String], cancel: &CancelToken) -> Result<()> {
    let (adb, serial) = (&*ctx.adb, ctx.serial.as_str());
    let _ = adb.shell(Some(serial), "input keyevent KEYCODE_WAKEUP", 10).await;
    let comp = format!("{AGENT_PKG}/{activity}");
    let mut args: Vec<&str> = vec!["shell", "am", "start", "-n", &comp];
    args.extend(extras.iter().map(String::as_str));
    let o = adb.run_c(Some(serial), &args, 30, cancel).await?;
    let txt = format!("{}\n{}", o.stdout, o.stderr);
    if !o.ok() || txt.contains("Error:") || txt.contains("Exception") {
        return Err(AdbError::Other(format!(
            "Nie udało się uruchomić agenta na urządzeniu / failed to start agent: {}",
            txt.trim()
        )));
    }
    Ok(())
}

/// Sprzątanie po agencie (bez anulowania). Zwraca ostrzeżenia dla raportu.
pub(crate) async fn agent_release(ctx: &AgentCtx, lease: AgentLease, emitter: &Emitter) -> Vec<String> {
    cleanup(ctx, &lease.trace, emitter).await.warnings
}

/// APK agenta (zasoby albo kopia wbudowana) - np. do pobrania przez Wi-Fi.
pub(crate) async fn agent_apk_path(resource_apk: Option<PathBuf>, cache_dir: &Path) -> Result<PathBuf> {
    resolve_apk(resource_apk, cache_dir, &Emitter::silent()).await
}

// --------------------------------------------------------------------------- //
// Kopia
// --------------------------------------------------------------------------- //

pub async fn run_backup(ctx: AgentCtx, opts: BackupOptions, emitter: Emitter, cancel: CancelToken) {
    let mut trace = Trace { remote_base: REMOTE_BASE, ..Default::default() };
    let res = backup_flow(&ctx, &opts, &emitter, &cancel, &mut trace).await;

    // sprzątanie ZAWSZE - prywatność ponad wszystko (także po błędzie / anulowaniu)
    stage(&emitter, "cleanup", "");
    let report = cleanup(&ctx, &trace, &emitter).await;

    match res {
        Ok(r) => {
            emitter.item("result", true, serde_json::to_string(&r).unwrap_or_default());
            emitter.ok(format!(
                "Kopia gotowa: {} kontaktów, {} SMS -> {} / backup done.",
                r.contacts, r.sms, r.dir
            ));
            emitter.finished(1, 0, 1, warn_items(report.warnings), false);
        }
        Err(e) => finish_err(&emitter, e, "backup"),
    }
}

async fn backup_flow(
    ctx: &AgentCtx,
    opts: &BackupOptions,
    emitter: &Emitter,
    cancel: &CancelToken,
    trace: &mut Trace,
) -> Result<BackupResult> {
    let (adb, serial) = (&*ctx.adb, ctx.serial.as_str());

    // --- 1. wgranie agenta
    deploy(ctx, emitter, cancel, trace).await?;

    // --- 2. uruchomienie
    let session = new_session();
    let remote_dir = format!("{REMOTE_BASE}/{session}");
    trace.session = Some(session.clone());
    trace.remote_dir = Some(remote_dir.clone());
    stage(emitter, "launch", session.clone());
    launch(ctx, &session, &[], cancel).await?;
    emitter.ok("Agent uruchomiony na urządzeniu / agent started.");

    // --- 3. czekanie na użytkownika (polling co 2 s)
    stage(emitter, "wait", remote_dir.clone());
    emitter.info(
        "Odblokuj ekran urządzenia, zaakceptuj uprawnienia (Kontakty, SMS) i kliknij „Wykonaj kopię” / \
         unlock the device, allow permissions and tap the backup button.",
    );
    let raw = wait_for_marker(
        adb,
        serial,
        &remote_dir,
        ("done.json", "error.json"),
        opts.wait_minutes,
        "kliknięcie „Wykonaj kopię”",
        emitter,
        cancel,
    )
    .await?;
    let marker: DoneMarker =
        serde_json::from_str(&raw).map_err(|e| AdbError::Other(format!("done.json nieczytelny / unreadable: {e}")))?;
    if marker.session != session && !marker.session.is_empty() {
        return Err(AdbError::Other(format!(
            "Znacznik z innej sesji ({}) - przerwano / session mismatch",
            marker.session
        )));
    }
    emitter.ok(format!(
        "Agent zakończył eksport: {} kontaktów, {} SMS / export finished on device.",
        marker.contacts, marker.sms
    ));
    if marker.contacts_denied {
        emitter.warn("Użytkownik nie zezwolił na dostęp do kontaktów - skopiowano tylko SMS / contacts permission denied.");
    }
    if marker.sms_denied {
        emitter.warn("Użytkownik nie zezwolił na dostęp do SMS - skopiowano tylko kontakty / SMS permission denied.");
    }

    // --- 4. pobranie + weryfikacja
    stage(emitter, "pull", "");
    let model = model_of(adb, serial).await;
    let device = marker.device.clone().filter(|d| !d.trim().is_empty()).unwrap_or_else(|| model.clone());
    let folder = sanitize_filename(&format!(
        "Kontakty_SMS_{}_{}",
        if model.is_empty() { serial } else { &model },
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ))
    .replace(' ', "_");
    let local_dir = PathBuf::from(&opts.dest_dir).join(folder);
    tokio::fs::create_dir_all(&local_dir).await?;

    let mut files = Vec::new();
    for f in &marker.files {
        if cancel.is_cancelled() {
            return Err(AdbError::Cancelled);
        }
        if f.name.is_empty() || f.name.contains('/') || f.name.contains('\\') || f.name.starts_with('.') {
            return Err(AdbError::Other(format!("Nieprawidłowa nazwa pliku w znaczniku: '{}'", f.name)));
        }
        let remote = format!("{remote_dir}/{}", f.name);
        let local = local_dir.join(sanitize_filename(&f.name));
        let local_s = local.to_string_lossy().to_string();
        let o = adb.run_c(Some(serial), &["pull", &remote, &local_s], 900, cancel).await?;
        check_pull_output(&o, &remote, &local_s).map_err(|e| AdbError::Other(format!("{}: {e}", f.name)))?;
        let len = tokio::fs::metadata(&local).await.map(|m| m.len()).map_err(|_| {
            AdbError::Other(format!("{}: adb zgłosił sukces, ale pliku nie ma na dysku / missing after pull", f.name))
        })?;
        if len != f.size {
            return Err(AdbError::Other(format!(
                "{}: rozmiar {len} B, agent zapisał {} B - plik niekompletny / size mismatch",
                f.name, f.size
            )));
        }
        emitter.ok(format!("Pobrano / pulled {} ({len} B)", f.name));
        files.push(local.to_string_lossy().to_string());
    }
    // opis kopii obok danych
    let _ = tokio::fs::write(local_dir.join("backup_info.json"), raw.as_bytes()).await;

    Ok(BackupResult {
        dir: local_dir.to_string_lossy().to_string(),
        contacts: marker.contacts,
        sms: marker.sms,
        contacts_denied: marker.contacts_denied,
        sms_denied: marker.sms_denied,
        files,
        device,
    })
}

// --------------------------------------------------------------------------- //
// Przywracanie
// --------------------------------------------------------------------------- //

/// Pliki kopii w folderze na komputerze: `contacts.vcf` / `sms.xml`
/// (albo pierwszy `*.vcf` / `*.xml`, np. z innych programów do kopii).
pub fn find_restore_files(dir: &Path) -> Result<(Option<PathBuf>, Option<PathBuf>)> {
    if !dir.is_dir() {
        return Err(AdbError::InvalidArg(format!("Folder nie istnieje / not a folder: {}", dir.display())));
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    entries.sort();
    let pick = |preferred: &str, ext: &str| -> Option<PathBuf> {
        let exact = dir.join(preferred);
        if exact.is_file() {
            return Some(exact);
        }
        entries
            .iter()
            .find(|p| {
                let n = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                n.ends_with(ext) && !n.starts_with('.')
            })
            .cloned()
    };
    let vcf = pick("contacts.vcf", ".vcf");
    let xml = pick("sms.xml", ".xml");
    if vcf.is_none() && xml.is_none() {
        return Err(AdbError::InvalidArg(format!(
            "W folderze {} nie ma pliku kontaktów (.vcf) ani SMS (.xml) / no backup files found",
            dir.display()
        )));
    }
    Ok((vcf, xml))
}

pub async fn run_restore(ctx: AgentCtx, opts: RestoreOptions, emitter: Emitter, cancel: CancelToken) {
    let mut trace = Trace { remote_base: RESTORE_BASE, ..Default::default() };
    let res = restore_flow(&ctx, &opts, &emitter, &cancel, &mut trace).await;

    stage(&emitter, "cleanup", "");
    let report = cleanup(&ctx, &trace, &emitter).await;

    match res {
        Ok(mut r) => {
            if let Some(ok) = report.sms_ok {
                r.default_sms_restored = ok;
            }
            emitter.item("result", true, serde_json::to_string(&r).unwrap_or_default());
            emitter.ok(format!(
                "Przywracanie zakończone: SMS {} nowych ({} pominiętych duplikatów), kontakty: {} / restore done.",
                r.sms_inserted, r.sms_skipped, r.contacts_cards
            ));
            emitter.finished(1, 0, 1, warn_items(report.warnings), false);
        }
        Err(e) => finish_err(&emitter, e, "restore"),
    }
}

async fn restore_flow(
    ctx: &AgentCtx,
    opts: &RestoreOptions,
    emitter: &Emitter,
    cancel: &CancelToken,
    trace: &mut Trace,
) -> Result<RestoreResult> {
    let (adb, serial) = (&*ctx.adb, ctx.serial.as_str());
    let (vcf, xml) = find_restore_files(Path::new(&opts.source_dir))?;
    for p in vcf.iter().chain(xml.iter()) {
        let len = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        emitter.info(format!("Plik kopii / backup file: {} ({len} B)", p.display()));
    }

    // --- 1. agent + dostęp do plików
    let sdk = deploy(ctx, emitter, cancel, trace).await?;
    grant_file_access(adb, serial, sdk, emitter).await;

    // --- 2. wypchnięcie kopii
    stage(emitter, "push", "");
    let session = new_session();
    let remote_dir = format!("{RESTORE_BASE}/{session}");
    trace.session = Some(session.clone());
    trace.remote_dir = Some(remote_dir.clone());
    let mk = adb
        .run_c(Some(serial), &["shell", &format!("mkdir -p {}", shell_quote(&remote_dir))], 20, cancel)
        .await?;
    if !mk.ok() {
        return Err(AdbError::Other(format!("Nie można utworzyć {remote_dir}: {}", mk.err_text())));
    }
    if let Some(v) = &vcf {
        push_verified(adb, serial, v, &format!("{remote_dir}/contacts.vcf"), emitter, cancel).await?;
    }
    if let Some(x) = &xml {
        push_verified(adb, serial, x, &format!("{remote_dir}/sms.xml"), emitter, cancel).await?;
    }

    // domyślna aplikacja SMS PRZED startem agenta - komputer przywróci ją przy sprzątaniu
    if xml.is_some() {
        trace.watch_sms = true;
        trace.prev_sms = current_sms_app(adb, serial, sdk).await.filter(|p| p != AGENT_PKG);
        match &trace.prev_sms {
            Some(p) => emitter.info(format!("Domyślna aplikacja SMS / default SMS app: {p}")),
            None => emitter.warn("Nie udało się odczytać domyślnej aplikacji SMS - po zakończeniu sprawdź ją ręcznie."),
        }
    }

    // --- 3. uruchomienie w trybie przywracania
    stage(emitter, "launch", session.clone());
    launch(ctx, &session, &["--es", "mode", "restore"], cancel).await?;
    emitter.ok("Agent uruchomiony w trybie przywracania / agent started (restore).");

    // --- 4. czekanie na "Gotowe"
    stage(emitter, "wait", remote_dir.clone());
    emitter.info(
        "Na urządzeniu: „Przywróć kontakty”, „Przywróć SMS” (agent poprosi o rolę domyślnej aplikacji SMS), \
         potem przywróć swoją aplikację SMS i kliknij „Gotowe” / follow the steps on the device, then tap Done.",
    );
    let raw = wait_for_marker(
        adb,
        serial,
        &remote_dir,
        ("restore_done.json", "restore_error.json"),
        opts.wait_minutes,
        "„Gotowe” w agencie",
        emitter,
        cancel,
    )
    .await?;
    let m: RestoreMarker = serde_json::from_str(&raw)
        .map_err(|e| AdbError::Other(format!("restore_done.json nieczytelny / unreadable: {e}")))?;
    if !m.session.is_empty() && m.session != session {
        return Err(AdbError::Other(format!("Znacznik z innej sesji ({}) / session mismatch", m.session)));
    }
    if trace.prev_sms.is_none() && !m.previous_sms_app.is_empty() && m.previous_sms_app != AGENT_PKG {
        trace.prev_sms = Some(m.previous_sms_app.clone());
    }

    // raport
    match m.contacts_method.as_str() {
        "intent" => emitter.ok(format!(
            "Kontakty: {} wizytówek przekazano do aplikacji Kontakty (import w telefonie) / contacts handed to the Contacts app.",
            m.contacts_cards
        )),
        "direct" => emitter.ok(format!(
            "Kontakty: zaimportowano {} z {} / contacts imported.",
            m.contacts_imported, m.contacts_cards
        )),
        _ if m.contacts_file => emitter.warn("Kontaktów nie przywrócono (nie kliknięto „Przywróć kontakty”) / contacts not restored."),
        _ => {}
    }
    if m.sms_restored {
        emitter.ok(format!(
            "SMS: {} nowych, {} duplikatów pominięto, {} błędów (z {}) / SMS restored.",
            m.sms_inserted, m.sms_skipped, m.sms_failed, m.sms_total
        ));
        if m.sms_failed > 0 {
            emitter.warn(format!("{} wiadomości nie dało się zapisać / {} SMS failed.", m.sms_failed, m.sms_failed));
        }
    } else if m.sms_file {
        emitter.warn("SMS-ów nie przywrócono (brak zgody na domyślną aplikację SMS albo nie kliknięto „Przywróć SMS”).");
    }
    if m.mms_skipped > 0 {
        emitter.warn(format!("Pominięto {} MMS (nieobsługiwane) / MMS skipped.", m.mms_skipped));
    }
    if m.incoming_sms_saved > 0 {
        emitter.info(format!(
            "W trakcie przywracania przyszło {} SMS - zapisane w skrzynce / incoming SMS saved.",
            m.incoming_sms_saved
        ));
    }
    if m.incoming_sms_lost > 0 || m.incoming_mms > 0 {
        emitter.warn(format!(
            "W trakcie przywracania: {} SMS nie zapisano, {} MMS nie pobrano - poproś nadawców o ponowne wysłanie.",
            m.incoming_sms_lost, m.incoming_mms
        ));
    }

    Ok(RestoreResult {
        contacts_file: m.contacts_file,
        contacts_cards: m.contacts_cards,
        contacts_method: if m.contacts_method.is_empty() { "none".into() } else { m.contacts_method },
        contacts_imported: m.contacts_imported,
        sms_file: m.sms_file,
        sms_total: m.sms_total,
        sms_inserted: m.sms_inserted,
        sms_skipped: m.sms_skipped,
        sms_failed: m.sms_failed,
        mms_skipped: m.mms_skipped,
        sms_restored: m.sms_restored,
        default_sms_changed: m.default_sms_changed,
        default_sms_restored: m.default_sms_restored,
        previous_sms_app: trace.prev_sms.clone().unwrap_or(m.previous_sms_app),
        incoming_sms_saved: m.incoming_sms_saved,
        incoming_sms_lost: m.incoming_sms_lost,
        incoming_mms: m.incoming_mms,
        manual_install: trace.pushed_apk,
        device: m.device.filter(|d| !d.trim().is_empty()).unwrap_or(model_of(adb, serial).await),
    })
}

/// Odczyt plików w Download/Restore/ przez agenta. Błąd nie przerywa - agent poprosi sam.
async fn grant_file_access(adb: &Adb, serial: &str, sdk: u32, emitter: &Emitter) {
    let cmds: Vec<String> = if sdk >= 30 {
        vec![format!("appops set {AGENT_PKG} MANAGE_EXTERNAL_STORAGE allow")]
    } else if sdk >= 23 {
        let mut v = vec![format!("pm grant {AGENT_PKG} android.permission.READ_EXTERNAL_STORAGE")];
        if sdk <= 28 {
            v.push(format!("pm grant {AGENT_PKG} android.permission.WRITE_EXTERNAL_STORAGE"));
        }
        v
    } else {
        vec![] // Android 5.x: uprawnienia nadawane przy instalacji
    };
    for c in cmds {
        match adb.shell(Some(serial), &c, 15).await {
            Ok(o) if o.ok() && !o.stdout.contains("Exception") && !o.stderr.contains("Exception") => {
                emitter.info(format!("Dostęp do plików nadany / files access granted ({c})"));
            }
            Ok(o) => emitter.warn(format!(
                "Nie udało się nadać dostępu do plików ({}) - agent poprosi o niego na ekranie urządzenia.",
                o.err_text()
            )),
            Err(e) => emitter.warn(format!("{c}: {e}")),
        }
    }
}

/// `adb push` + kontrola rozmiaru na urządzeniu (bez fałszywych sukcesów).
async fn push_verified(
    adb: &Adb,
    serial: &str,
    local: &Path,
    remote: &str,
    emitter: &Emitter,
    cancel: &CancelToken,
) -> Result<()> {
    let len = tokio::fs::metadata(local).await?.len();
    let local_s = local.to_string_lossy().to_string();
    let o = adb.run_c(Some(serial), &["push", &local_s, remote], 900, cancel).await?;
    let txt = format!("{}\n{}", o.stdout, o.stderr).to_lowercase();
    if !o.ok() || txt.contains("error:") || txt.contains("failed to copy") || txt.contains("permission denied") {
        return Err(AdbError::Other(format!(
            "adb push {} -> {remote}: {} / push failed",
            local.display(),
            o.err_text()
        )));
    }
    let remote_len = remote_size(adb, serial, remote).await;
    if remote_len != Some(len) {
        return Err(AdbError::Other(format!(
            "{remote}: na urządzeniu {} B, plik ma {len} B - przesłanie niekompletne / size mismatch",
            remote_len.map(|v| v.to_string()).unwrap_or_else(|| "?".into())
        )));
    }
    emitter.ok(format!("Wysłano / pushed {} -> {remote} ({len} B)", local.display()));
    Ok(())
}

async fn remote_size(adb: &Adb, serial: &str, remote: &str) -> Option<u64> {
    let q = shell_quote(remote);
    let o = adb.shell(Some(serial), &format!("stat -c %s {q} 2>/dev/null || wc -c < {q}"), 15).await.ok()?;
    o.stdout.split_whitespace().next()?.parse().ok()
}

// --------------------------------------------------------------------------- //
// Domyślna aplikacja SMS
// --------------------------------------------------------------------------- //

fn valid_pkg(p: &str) -> bool {
    !p.is_empty() && p.len() < 256 && p.contains('.') && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
}

/// Aktualna domyślna aplikacja SMS (Android 10+: rola, starsze: ustawienie secure).
async fn current_sms_app(adb: &Adb, serial: &str, sdk: u32) -> Option<String> {
    if sdk >= 29 {
        if let Ok(o) = adb.shell(Some(serial), &format!("cmd role get-role-holders {ROLE_SMS}"), 10).await {
            if let Some(p) = o.stdout.lines().map(str::trim).find(|l| valid_pkg(l)) {
                return Some(p.to_string());
            }
        }
    }
    let o = adb.shell(Some(serial), "settings get secure sms_default_application", 10).await.ok()?;
    let p = o.stdout.trim();
    valid_pkg(p).then(|| p.to_string())
}

async fn set_sms_app(adb: &Adb, serial: &str, sdk: u32, pkg: &str) -> bool {
    if !valid_pkg(pkg) {
        return false;
    }
    let cmd = if sdk >= 29 {
        format!("cmd role add-role-holder {ROLE_SMS} {pkg}")
    } else {
        format!("settings put secure sms_default_application {pkg}")
    };
    let _ = adb.shell(Some(serial), &cmd, 15).await;
    current_sms_app(adb, serial, sdk).await.as_deref() == Some(pkg)
}

// --------------------------------------------------------------------------- //
// Wspólne: wgranie, uruchomienie, czekanie, sprzątanie
// --------------------------------------------------------------------------- //

async fn model_of(adb: &Adb, serial: &str) -> String {
    adb.shell(Some(serial), "getprop ro.product.model", 8)
        .await
        .map(|o| o.stdout.trim().to_string())
        .unwrap_or_default()
}

/// Etap "deploy": instalacja przez ADB, a gdy jest zablokowana - plan B (ręczna instalacja).
/// Zwraca API urządzenia.
async fn deploy(ctx: &AgentCtx, emitter: &Emitter, cancel: &CancelToken, trace: &mut Trace) -> Result<u32> {
    let (adb, serial) = (&*ctx.adb, ctx.serial.as_str());
    stage(emitter, "deploy", "");
    let sdk = adb.sdk(serial).await.unwrap_or(0);
    trace.sdk = sdk;
    if sdk > 0 && sdk < MIN_SDK {
        return Err(AdbError::Other(format!(
            "Agent wymaga Androida 5.0+ (API {MIN_SDK}), urządzenie ma API {sdk} / Android too old"
        )));
    }
    let apk = resolve_apk(ctx.resource_apk.clone(), &ctx.cache_dir, emitter).await?;
    match install_agent(adb, serial, &apk, emitter, cancel, trace).await? {
        Install::Done => Ok(sdk),
        Install::Blocked(reason) => {
            manual_install(ctx, &apk, &reason, emitter, cancel, trace).await?;
            Ok(sdk)
        }
    }
}

/// APK z zasobów aplikacji (podmienialny po przebudowaniu agenta) albo kopia wbudowana.
async fn resolve_apk(resource_apk: Option<PathBuf>, cache_dir: &Path, emitter: &Emitter) -> Result<PathBuf> {
    if let Some(p) = resource_apk.filter(|p| p.is_file()) {
        let bytes = tokio::fs::read(&p).await?;
        if bytes.starts_with(b"PK\x03\x04") {
            emitter.info(format!("Agent.apk z zasobów: {} ({} B, sha256 {})", p.display(), bytes.len(), short_sha(&bytes)));
            return Ok(p);
        }
        emitter.warn(format!("{} nie jest poprawnym APK - używam kopii wbudowanej", p.display()));
    }
    if !EMBEDDED_APK.starts_with(b"PK\x03\x04") {
        return Err(AdbError::Other(
            "Brak Agent.apk - aplikacja została zbudowana bez agenta. Zbuduj go (./build.sh agent lub build.bat agent) \
             i przebuduj aplikację / agent apk missing"
                .into(),
        ));
    }
    tokio::fs::create_dir_all(cache_dir).await?;
    let p = cache_dir.join("Agent.apk");
    tokio::fs::write(&p, EMBEDDED_APK).await?;
    emitter.info(format!("Agent.apk (wbudowany, {} B, sha256 {})", EMBEDDED_APK.len(), short_sha(EMBEDDED_APK)));
    Ok(p)
}

fn short_sha(b: &[u8]) -> String {
    let h = Sha256::digest(b);
    h.iter().take(6).map(|x| format!("{x:02x}")).collect()
}

enum Install {
    Done,
    /// Instalacja przez USB odrzucona - czytelny powód (plan B ma sens).
    Blocked(String),
}

/// Błędy, przy których ręczna instalacja też nic nie da (zły/niezgodny plik, brak miejsca).
fn manual_pointless(txt: &str) -> bool {
    [
        "INSTALL_FAILED_OLDER_SDK",
        "INSTALL_PARSE_FAILED",
        "INSTALL_FAILED_INVALID_APK",
        "INSTALL_FAILED_INSUFFICIENT_STORAGE",
        "INSTALL_FAILED_NO_MATCHING_ABIS",
        "-124",
    ]
    .iter()
    .any(|k| txt.contains(k))
}

async fn install_agent(
    adb: &Adb,
    serial: &str,
    apk: &Path,
    emitter: &Emitter,
    cancel: &CancelToken,
    trace: &mut Trace,
) -> Result<Install> {
    let apk_s = apk.to_string_lossy().to_string();
    let mut last = String::new();
    for attempt in 0..2 {
        emitter.info("adb install -r -t Agent.apk ...");
        let o = adb.run_c(Some(serial), &["install", "-r", "-t", &apk_s], 180, cancel).await?;
        let txt = format!("{}\n{}", o.stdout.trim(), o.stderr.trim());
        if o.ok() && o.stdout.contains("Success") {
            trace.installed = true;
            emitter.ok("Agent zainstalowany / agent installed.");
            return Ok(Install::Done);
        }
        // stara wersja agenta z innym podpisem -> usuń i spróbuj ponownie
        if attempt == 0 && txt.contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE") {
            emitter.warn("Na urządzeniu jest stara wersja agenta - odinstalowuję / removing old agent.");
            let _ = adb.run(Some(serial), &["uninstall", AGENT_PKG], 60).await;
            continue;
        }
        last = txt;
        break;
    }
    let explained = crate::devtools::runner::explain_install(&last, false);
    if manual_pointless(&last) {
        return Err(AdbError::Other(explained));
    }
    Ok(Install::Blocked(explained))
}

async fn agent_installed(adb: &Adb, serial: &str) -> bool {
    adb.shell(Some(serial), &format!("pm path {AGENT_PKG}"), 10)
        .await
        .map(|o| o.stdout.contains("package:"))
        .unwrap_or(false)
}

/// Plan B: `adb push Agent.apk /sdcard/Download/Agent.apk`, okno w GUI i czekanie,
/// aż użytkownik zainstaluje plik menedżerem plików (sprawdzane co 2 s `pm path`).
async fn manual_install(
    ctx: &AgentCtx,
    apk: &Path,
    reason: &str,
    emitter: &Emitter,
    cancel: &CancelToken,
    trace: &mut Trace,
) -> Result<()> {
    let (adb, serial) = (&*ctx.adb, ctx.serial.as_str());
    emitter.warn(format!(
        "Instalacja przez USB zablokowana / USB install blocked: {reason}\nPlan B: wysyłam Agent.apk do folderu Pobrane."
    ));
    trace.pushed_apk = true;
    push_verified(adb, serial, apk, MANUAL_APK, emitter, cancel).await.map_err(|e| {
        AdbError::Other(format!(
            "Instalacja przez USB zablokowana ({reason}), a wysłanie Agent.apk do Download też się nie udało: {e}"
        ))
    })?;
    emitter.item("manual_install", true, reason);
    emitter.info(format!(
        "Otwórz menedżer plików na urządzeniu -> Pobrane (Download) -> Agent.apk -> Zainstaluj, potem kliknij Gotowe. \
         Czekam do {} min / install {MANUAL_APK} manually.",
        MANUAL_INSTALL_LIMIT.as_secs() / 60
    ));

    let started = Instant::now();
    loop {
        if agent_installed(adb, serial).await {
            trace.installed = true;
            emitter.item("manual_install_done", true, "");
            emitter.ok("Agent zainstalowany ręcznie / agent installed manually.");
            return Ok(());
        }
        if started.elapsed() >= MANUAL_INSTALL_LIMIT {
            return Err(AdbError::Other(format!(
                "Agent nie został zainstalowany w ciągu {} min - przerwano / manual install timed out",
                MANUAL_INSTALL_LIMIT.as_secs() / 60
            )));
        }
        tokio::select! {
            _ = tokio::time::sleep(POLL_EVERY) => {}
            _ = ctx.confirm.notified() => {
                if agent_installed(adb, serial).await {
                    continue;
                }
                emitter.item("manual_install_pending", false, "");
                emitter.warn(
                    "Agent jeszcze nie jest zainstalowany. Dokończ instalację Agent.apk na urządzeniu \
                     (zezwól menedżerowi plików na instalowanie aplikacji) / not installed yet.",
                );
            }
            _ = cancel.cancelled() => return Err(AdbError::Cancelled),
        }
    }
}

async fn launch(ctx: &AgentCtx, session: &str, extra: &[&str], cancel: &CancelToken) -> Result<()> {
    let (adb, serial) = (&*ctx.adb, ctx.serial.as_str());
    let _ = adb.shell(Some(serial), "input keyevent KEYCODE_WAKEUP", 10).await;
    let mut args = vec!["shell", "am", "start", "-n", AGENT_ACTIVITY, "--es", "session", session];
    args.extend_from_slice(extra);
    let o = adb.run_c(Some(serial), &args, 30, cancel).await?;
    let txt = format!("{}\n{}", o.stdout, o.stderr);
    if !o.ok() || txt.contains("Error:") || txt.contains("Exception") {
        return Err(AdbError::Other(format!(
            "Nie udało się uruchomić agenta na urządzeniu / failed to start agent: {}",
            txt.trim()
        )));
    }
    Ok(())
}

/// Polling co 2 s. Kończy się na znaczniku sukcesu (Ok - treść JSON),
/// znaczniku błędu (Err) albo po czasie.
#[allow(clippy::too_many_arguments)]
async fn wait_for_marker(
    adb: &Adb,
    serial: &str,
    remote_dir: &str,
    (done_name, err_name): (&str, &str),
    wait_minutes: u64,
    waiting_for: &str,
    emitter: &Emitter,
    cancel: &CancelToken,
) -> Result<String> {
    let limit = Duration::from_secs(wait_minutes.clamp(1, 120) * 60);
    let started = Instant::now();
    let mut failures = 0u32;
    let mut last_note = Instant::now();
    let ls = format!("ls -1 {} 2>/dev/null", shell_quote(remote_dir));
    loop {
        if cancel.is_cancelled() {
            return Err(AdbError::Cancelled);
        }
        match adb.shell(Some(serial), &ls, 10).await {
            Ok(o) => {
                failures = 0;
                let names: Vec<&str> = o.stdout.lines().map(str::trim).collect();
                if names.contains(&err_name) {
                    let e = read_remote(adb, serial, &format!("{remote_dir}/{err_name}")).await?;
                    let m: ErrorMarker = serde_json::from_str(&e).unwrap_or(ErrorMarker { error: e.trim().to_string() });
                    return Err(AdbError::Other(format!("Agent zgłosił błąd / agent error: {}", m.error)));
                }
                if names.contains(&done_name) {
                    return read_remote(adb, serial, &format!("{remote_dir}/{done_name}")).await;
                }
            }
            Err(e) => {
                failures += 1;
                // ~30 s bez odpowiedzi = urządzenie odłączone
                if failures >= 15 {
                    return Err(AdbError::Other(format!(
                        "Utracono połączenie z urządzeniem podczas oczekiwania / device lost: {e}"
                    )));
                }
            }
        }
        if started.elapsed() >= limit {
            return Err(AdbError::Other(format!(
                "Przekroczono czas oczekiwania ({wait_minutes} min) na {waiting_for} - przerwano / timed out waiting for the user"
            )));
        }
        if last_note.elapsed() >= Duration::from_secs(30) {
            last_note = Instant::now();
            emitter.info(format!(
                "Czekam na {waiting_for} na urządzeniu... ({} s) / waiting...",
                started.elapsed().as_secs()
            ));
        }
        tokio::select! {
            _ = tokio::time::sleep(POLL_EVERY) => {}
            _ = cancel.cancelled() => return Err(AdbError::Cancelled),
        }
    }
}

async fn read_remote(adb: &Adb, serial: &str, path: &str) -> Result<String> {
    let o = adb.shell(Some(serial), &format!("cat {}", shell_quote(path)), 15).await?;
    Ok(o.stdout)
}

/// Sprzątanie (bez anulowania - musi się wykonać).
async fn cleanup(ctx: &AgentCtx, trace: &Trace, emitter: &Emitter) -> CleanupReport {
    let (adb, serial) = (&*ctx.adb, ctx.serial.as_str());
    let mut warnings = Vec::new();
    let mut sms_ok = None;

    // plan B: użytkownik mógł zainstalować agenta już po przerwaniu czekania
    let installed = trace.installed || (trace.pushed_apk && agent_installed(adb, serial).await);

    // 0. domyślna aplikacja SMS - agent nie może nią zostać
    if trace.watch_sms && installed {
        let current = current_sms_app(adb, serial, trace.sdk).await;
        if current.as_deref() == Some(AGENT_PKG) {
            match trace.prev_sms.as_deref() {
                Some(prev) if set_sms_app(adb, serial, trace.sdk, prev).await => {
                    emitter.ok(format!("Przywrócono domyślną aplikację SMS / default SMS app restored: {prev}"));
                    sms_ok = Some(true);
                }
                prev => {
                    let w = format!(
                        "Nie udało się automatycznie przywrócić domyślnej aplikacji SMS{} - po odinstalowaniu agenta \
                         system wybierze aplikację sam; sprawdź Ustawienia -> Aplikacje domyślne -> SMS.",
                        prev.map(|p| format!(" ({p})")).unwrap_or_default()
                    );
                    emitter.warn(w.clone());
                    warnings.push(w);
                    sms_ok = Some(false);
                }
            }
        } else {
            sms_ok = Some(true);
        }
    }

    if let (Some(session), Some(remote_dir)) = (&trace.session, &trace.remote_dir) {
        // 1. agent usuwa swoje pliki razem z wpisami MediaStore
        if installed {
            match adb
                .run(
                    Some(serial),
                    &["shell", "am", "broadcast", "-n", AGENT_RECEIVER, "--es", "session", session],
                    20,
                )
                .await
            {
                Ok(o) => {
                    if let Some(n) = o.stdout.split("deleted=").nth(1).and_then(|s| s.split('"').next()) {
                        emitter.info(format!("Agent usunął {n} plików z MediaStore / agent removed files."));
                    }
                }
                Err(e) => emitter.warn(format!("cleanup broadcast: {e}")),
            }
        }
        // 2. folder sesji (pliki mogły zostać, np. agent zatrzymany)
        let _ = adb.shell(Some(serial), &format!("rm -rf {}", shell_quote(remote_dir)), 30).await;
        let _ = adb
            .shell(Some(serial), &format!("rmdir {} 2>/dev/null", shell_quote(trace.remote_base)), 10)
            .await;
        let left = adb
            .shell(Some(serial), &format!("ls {} 2>/dev/null", shell_quote(remote_dir)), 10)
            .await
            .map(|o| o.stdout.trim().to_string())
            .unwrap_or_default();
        if left.is_empty() {
            emitter.ok("Pliki tymczasowe usunięte z urządzenia / device copies removed.");
        } else {
            let w = format!("Nie udało się usunąć wszystkich plików z {remote_dir} - usuń je ręcznie");
            emitter.warn(w.clone());
            warnings.push(w);
        }
    }

    // 3. Agent.apk z planu B
    if trace.pushed_apk {
        let _ = adb.shell(Some(serial), &format!("rm -f {}", shell_quote(MANUAL_APK)), 15).await;
        if remote_size(adb, serial, MANUAL_APK).await.is_some() {
            let w = format!("Nie udało się usunąć {MANUAL_APK} - usuń plik ręcznie");
            emitter.warn(w.clone());
            warnings.push(w);
        } else {
            emitter.info(format!("Usunięto {MANUAL_APK}"));
        }
    }

    // 4. odinstalowanie agenta
    if installed {
        match adb.run(Some(serial), &["uninstall", AGENT_PKG], 60).await {
            Ok(o) if o.stdout.contains("Success") => emitter.ok("Agent odinstalowany / agent uninstalled."),
            Ok(o) => {
                let w = format!(
                    "Nie udało się odinstalować agenta ({}) - usuń „Drapco Backup Agent” ręcznie",
                    o.err_text()
                );
                emitter.warn(w.clone());
                warnings.push(w);
            }
            Err(e) => {
                let w = format!("Nie udało się odinstalować agenta: {e} - usuń „Drapco Backup Agent” ręcznie");
                emitter.warn(w.clone());
                warnings.push(w);
            }
        }
    }
    CleanupReport { warnings, sms_ok }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(agent_apk)]
    #[test]
    fn embedded_apk_is_valid() {
        assert!(EMBEDDED_APK.starts_with(b"PK\x03\x04"));
        assert!(EMBEDDED_APK.len() > 10_000);
    }

    #[test]
    fn parses_markers() {
        let d: DoneMarker = serde_json::from_str(
            r#"{"status":"ok","session":"adbst-1","contacts":12,"sms":340,"contactsDenied":false,"smsDenied":true,
               "files":[{"name":"contacts.vcf","size":4096}],"device":"samsung SM-A525F","sdk":34}"#,
        )
        .unwrap();
        assert_eq!(d.contacts, 12);
        assert!(d.sms_denied);
        assert_eq!(d.files[0].size, 4096);
        let e: ErrorMarker = serde_json::from_str(r#"{"status":"error","error":"Brak uprawnień"}"#).unwrap();
        assert_eq!(e.error, "Brak uprawnień");
        let r: RestoreMarker = serde_json::from_str(
            r#"{"status":"ok","session":"s","contactsCards":3,"contactsMethod":"intent","contactsImported":-1,
               "smsTotal":10,"smsInserted":8,"smsSkipped":2,"smsRestored":true,"previousSmsApp":"com.x.sms","extra":1}"#,
        )
        .unwrap();
        assert_eq!((r.sms_inserted, r.sms_skipped, r.contacts_imported), (8, 2, -1));
        assert_eq!(r.previous_sms_app, "com.x.sms");
    }

    #[test]
    fn install_errors_classified() {
        assert!(manual_pointless("Failure [INSTALL_FAILED_OLDER_SDK: Requires newer sdk version #21]"));
        assert!(manual_pointless("Failure [-124: Failed parse during installPackageLI]"));
        assert!(!manual_pointless("Failure [INSTALL_FAILED_USER_RESTRICTED: Install canceled by user]"));
        assert!(!manual_pointless("Failure [INSTALL_FAILED_VERIFICATION_FAILURE]"));
        assert!(valid_pkg("com.google.android.apps.messaging"));
        assert!(!valid_pkg("null") && !valid_pkg("a b.c") && !valid_pkg("x;rm -rf"));
    }

    #[test]
    fn finds_restore_files() {
        let d = std::env::temp_dir().join(format!("adbst-rf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert!(find_restore_files(&d).is_err());
        std::fs::write(d.join("export.VCF"), "BEGIN:VCARD").unwrap();
        std::fs::write(d.join("backup_info.json"), "{}").unwrap();
        let (v, x) = find_restore_files(&d).unwrap();
        assert!(v.unwrap().ends_with("export.VCF") && x.is_none());
        std::fs::write(d.join("a.xml"), "<smses/>").unwrap();
        std::fs::write(d.join("sms.xml"), "<smses/>").unwrap();
        let (_, x) = find_restore_files(&d).unwrap();
        assert!(x.unwrap().ends_with("sms.xml"));
        let _ = std::fs::remove_dir_all(&d);
    }

    type Events = Arc<std::sync::Mutex<Vec<String>>>;

    fn channel() -> (Events, Emitter) {
        let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let ev = events.clone();
        let ch = tauri::ipc::Channel::new(move |body| {
            if let tauri::ipc::InvokeResponseBody::Json(s) = body {
                ev.lock().unwrap().push(s);
            }
            Ok(())
        });
        (events, Emitter::new(ch))
    }

    fn parsed(events: &Events) -> Vec<serde_json::Value> {
        events.lock().unwrap().iter().filter_map(|s| serde_json::from_str(s).ok()).collect()
    }

    /// Atrapa `adb` (sh): pliki "urządzenia" w <base>/sdcard, rola SMS w <base>/role.
    #[cfg(unix)]
    fn fake_adb(base: &Path, install_reply: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(base.join("sdcard/Download")).unwrap();
        std::fs::write(base.join("role"), "com.google.android.apps.messaging\n").unwrap();
        let fake = base.join("adb");
        std::fs::write(
            &fake,
            format!(
                r#"#!/bin/sh
B="{base}"
echo "$*" >> "$B/calls.log"
[ "$1" = "-s" ] && shift 2
cmd="$1"; shift
case "$cmd" in
  install) {install_reply} ;;
  uninstall) rm -f "$B/installed"; echo "Success" ;;
  pull) r=$(echo "$1" | sed "s#^/sdcard#$B/sdcard#"); cp "$r" "$2" || exit 1; n=$(wc -c < "$2" | tr -d ' '); echo "$1: 1 file pulled, 0 skipped. ($n bytes in 0.001s)" ;;
  push) r=$(echo "$2" | sed "s#^/sdcard#$B/sdcard#"); mkdir -p "$(dirname "$r")"; cp "$1" "$r" || exit 1; echo "$1: 1 file pushed, 0 skipped.";
        case "$2" in *Agent.apk) ( sleep 3; touch "$B/installed" ) >/dev/null 2>&1 & ;; esac ;;
  shell)
    c=$(echo "$*" | sed "s#/sdcard#$B/sdcard#g")
    case "$c" in
      "getprop ro.build.version.sdk") echo 34 ;;
      "getprop ro.product.model") echo "SM-A525F" ;;
      "input keyevent"*) ;;
      "pm path com.drapco.agent") [ -f "$B/installed" ] && echo "package:/data/app/com.drapco.agent/base.apk" ;;
      "appops set"*|"pm grant"*) ;;
      "cmd role get-role-holders"*) cat "$B/role" ;;
      "cmd role add-role-holder"*) echo "$c" | awk '{{print $NF}}' > "$B/role" ;;
      "am start"*"mode restore"*)
        s=$(echo "$c" | sed 's/.*--es session \([^ ]*\).*/\1/')
        d="$B/sdcard/Download/Restore/$s"
        ( sleep 2; echo com.drapco.agent > "$B/role";
          printf '{{"status":"ok","session":"%s","contactsFile":true,"contactsCards":2,"contactsMethod":"intent","contactsImported":-1,"smsFile":true,"smsTotal":3,"smsInserted":3,"smsRestored":true,"defaultSmsChanged":true,"defaultSmsRestored":false,"previousSmsApp":"com.google.android.apps.messaging","device":"samsung SM-A525F"}}' "$s" > "$d/restore_done.json" ) >/dev/null 2>&1 &
        echo "Starting: Intent {{ cmp=com.drapco.agent/.MainActivity }}" ;;
      "am start"*)
        s=$(echo "$c" | sed 's/.*--es session //')
        d="$B/sdcard/Download/Backup/$s"
        ( sleep 3; mkdir -p "$d"; printf 'BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Jan\r\nEND:VCARD\r\n' > "$d/contacts.vcf";
          printf '<smses count="0"></smses>\n' > "$d/sms.xml";
          cs=$(wc -c < "$d/contacts.vcf" | tr -d ' '); ss=$(wc -c < "$d/sms.xml" | tr -d ' ');
          printf '{{"status":"ok","session":"%s","contacts":1,"sms":0,"contactsDenied":false,"smsDenied":false,"files":[{{"name":"contacts.vcf","size":%s}},{{"name":"sms.xml","size":%s}}],"device":"samsung SM-A525F"}}' "$s" "$cs" "$ss" > "$d/done.json" ) >/dev/null 2>&1 &
        echo "Starting: Intent {{ cmp=com.drapco.agent/.MainActivity }}" ;;
      "am broadcast"*) echo 'Broadcast completed: result=0, data="deleted=3"' ;;
      *) exec sh -c "$c" ;;
    esac ;;
esac
"#,
                base = base.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        fake
    }

    #[cfg(unix)]
    fn ctx_for(base: &Path, fake: PathBuf) -> AgentCtx {
        // atrapa APK (prawdziwy Agent.apk nie jest potrzebny - instalację udaje skrypt)
        let apk = base.join("Agent.apk");
        std::fs::write(&apk, b"PK\x03\x04fake-agent-apk").unwrap();
        AgentCtx {
            adb: Arc::new(Adb { adb: fake, fastboot: None }),
            serial: "SERIAL".into(),
            resource_apk: Some(apk),
            cache_dir: base.join("cache"),
            confirm: Arc::new(Notify::new()),
        }
    }

    /// E2E z atrapą `adb`: instalacja -> start -> "użytkownik klika" po 3 s ->
    /// polling -> pobranie + weryfikacja -> sprzątanie + odinstalowanie.
    #[cfg(unix)]
    #[tokio::test]
    async fn e2e_fake_agent_flow() {
        let base = std::env::temp_dir().join(format!("adbst-backup-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let fake = fake_adb(&base, r#"touch "$B/installed"; echo "Performing Streamed Install"; echo "Success""#);
        let (events, em) = channel();
        let opts = BackupOptions { dest_dir: base.join("out").to_string_lossy().to_string(), wait_minutes: 1 };
        run_backup(ctx_for(&base, fake), opts, em, CancelToken::new()).await;

        let evs = parsed(&events);
        let fin = evs.iter().find(|v| v["kind"] == "finished").expect("finished");
        assert_eq!(fin["ok"], 1, "{evs:#?}");
        let result = evs.iter().find(|v| v["kind"] == "item" && v["id"] == "result").expect("result");
        let r: serde_json::Value = serde_json::from_str(result["message"].as_str().unwrap()).unwrap();
        let dir = PathBuf::from(r["dir"].as_str().unwrap());
        assert!(std::fs::read_to_string(dir.join("contacts.vcf")).unwrap().contains("FN:Jan"));
        assert!(dir.join("sms.xml").is_file() && dir.join("backup_info.json").is_file());
        for st in ["stage:deploy", "stage:launch", "stage:wait", "stage:pull", "stage:cleanup"] {
            assert!(evs.iter().any(|v| v["id"] == st), "brak etapu {st}");
        }
        assert!(!evs.iter().any(|v| v["id"] == "manual_install"));
        // sprzątanie: folder sesji usunięty, agent odinstalowany
        let calls = std::fs::read_to_string(base.join("calls.log")).unwrap();
        assert!(calls.contains("uninstall com.drapco.agent"), "{calls}");
        assert!(calls.contains("CleanupReceiver"));
        let left: Vec<_> = std::fs::read_dir(base.join("sdcard/Download")).unwrap().collect();
        assert!(left.is_empty(), "na urządzeniu zostały pliki: {left:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// E2E przywracania: instalacja przez USB zablokowana -> plan B (push do Download,
    /// "użytkownik instaluje" po 3 s) -> push kopii -> start w trybie restore ->
    /// agent zostaje domyślną aplikacją SMS -> komputer przywraca poprzednią,
    /// usuwa pliki i Agent.apk, odinstalowuje agenta.
    #[cfg(unix)]
    #[tokio::test]
    async fn e2e_restore_with_manual_install() {
        let base = std::env::temp_dir().join(format!("adbst-restore-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let fake = fake_adb(
            &base,
            r#"echo "Performing Streamed Install"; echo "adb: failed to install Agent.apk: Failure [INSTALL_FAILED_USER_RESTRICTED: Install canceled by user]" >&2; exit 1"#,
        );
        let src = base.join("kopia");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("contacts.vcf"), "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Jan\r\nEND:VCARD\r\n").unwrap();
        std::fs::write(src.join("sms.xml"), "<smses count=\"1\"><sms address=\"1\" date=\"1\" type=\"1\" body=\"x\"/></smses>")
            .unwrap();

        let (events, em) = channel();
        let ctx = ctx_for(&base, fake);
        // klik "Gotowe" w GUI zanim agent jest zainstalowany -> ostrzeżenie, dalej czekamy
        let confirm = ctx.confirm.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(800)).await;
            confirm.notify_one();
        });
        let opts = RestoreOptions { source_dir: src.to_string_lossy().to_string(), wait_minutes: 1 };
        run_restore(ctx, opts, em, CancelToken::new()).await;

        let evs = parsed(&events);
        let fin = evs.iter().find(|v| v["kind"] == "finished").expect("finished");
        assert_eq!(fin["ok"], 1, "{evs:#?}");
        for id in ["manual_install", "manual_install_done", "stage:push", "stage:launch", "stage:wait", "stage:cleanup"] {
            assert!(evs.iter().any(|v| v["id"] == id), "brak {id}: {evs:#?}");
        }
        assert!(evs.iter().any(|v| v["id"] == "manual_install_pending"), "{evs:#?}");
        let result = evs.iter().find(|v| v["id"] == "result").expect("result");
        let r: serde_json::Value = serde_json::from_str(result["message"].as_str().unwrap()).unwrap();
        assert_eq!(r["smsInserted"], 3);
        assert_eq!(r["manualInstall"], true);
        assert_eq!(r["defaultSmsRestored"], true, "{r}");
        assert_eq!(r["previousSmsApp"], "com.google.android.apps.messaging");
        // rola SMS wróciła do poprzedniej aplikacji
        assert_eq!(std::fs::read_to_string(base.join("role")).unwrap().trim(), "com.google.android.apps.messaging");
        let calls = std::fs::read_to_string(base.join("calls.log")).unwrap();
        assert!(calls.contains("push") && calls.contains("/sdcard/Download/Agent.apk"), "{calls}");
        assert!(calls.contains("appops set com.drapco.agent MANAGE_EXTERNAL_STORAGE allow"));
        assert!(calls.contains("--es mode restore"));
        assert!(calls.contains("uninstall com.drapco.agent"));
        // urządzenie posprzątane: brak Restore/<sesja> i Agent.apk
        let left: Vec<_> = std::fs::read_dir(base.join("sdcard/Download")).unwrap().collect();
        assert!(left.is_empty(), "na urządzeniu zostały pliki: {left:?}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
