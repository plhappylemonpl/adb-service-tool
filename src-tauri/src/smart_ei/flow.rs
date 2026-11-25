//! Przebieg zadania Smart EI (eksport / import, ADB / Wi-Fi).
//!
//! 1. połączenie: ADB - wgranie agenta + `adb reverse` + automatyczne parowanie;
//!    Wi-Fi - ekran z kodami QR, czekanie na parowanie PINem,
//! 2. `hello` od agenta (model, wolne miejsce, rozmiary, zainstalowane pakiety),
//! 3. podsumowanie w GUI (szacowany rozmiar / wolne miejsce / wybór modułów i aplikacji),
//! 4. praca: agent (kontakty, SMS, tapeta, lista aplikacji, w Wi-Fi także pliki i APK)
//!    równolegle z komputerem (ADB: pliki, APK, ustawienia, zrzut ekranu),
//! 5. import: kolejka Sklepu Play, raport, sprzątanie (ADB: `adb reverse --remove`,
//!    przywrócenie domyślnej aplikacji SMS, odinstalowanie agenta).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::apps::{apk_file_name, parse_package_names, parse_pm_list, ApkFile, AppRecord, AppSource, AppsFile};
use super::bridge::{queue_json, AgentEvent, Bridge, Role};
use super::manifest::{folder_name, DeviceDesc, Manifest, ModuleEntry, MANIFEST};
use super::queue::{Action, PlayQueue};
use super::settings::{parse_volume, PortableSettings, STREAMS, SYSTEM_KEYS};
use super::vendor::{build_import_list, family_of};
use super::{category_paths, net, Control, ControlMsg, Modules, Selection, SessionInfo, Sessions, StartOptions};
use super::{DOWNLOAD_EXCLUDE, FILE_CATEGORIES};
use crate::adb::backup::{self, AgentCtx, AgentLease};
use crate::adb::events::{Emitter, FailedItem, LogLevel, TransferEvent};
use crate::adb::pull::{human_bytes, run_pull, PullOptions, SelectedItem};
use crate::adb::push::{run_push, PushOptions};
use crate::adb::root::RootCache;
use crate::adb::Adb;
use crate::error::{AdbError, Result};
use crate::utils::{is_valid_package, shell_quote, CancelToken};

const AGENT_ACTIVITY: &str = ".SmartEiActivity";
const SDCARD: &str = "/sdcard";
const APK_FALLBACK_DIR: &str = "/sdcard/Download/SmartEI_APKs";
/// ADB: agent łączy się sam przez `adb reverse` - brak parowania w tym czasie = awaria / blokada.
const PAIR_TIMEOUT_ADB: Duration = Duration::from_secs(30);
/// ADB: po tym czasie bez parowania - jednorazowe ponowne uruchomienie aktywności.
const RELAUNCH_AFTER: Duration = Duration::from_secs(12);
const PAIR_TIMEOUT_WIFI: Duration = Duration::from_secs(45 * 60);
/// Od parowania do `hello` (agent liczy rozmiary, listę aplikacji; w Wi-Fi może prosić o dostęp do plików).
const HELLO_TIMEOUT_ADB: Duration = Duration::from_secs(3 * 60);
const HELLO_TIMEOUT_WIFI: Duration = Duration::from_secs(15 * 60);
const AGENT_PKG: &str = crate::adb::backup::AGENT_PKG;
const REVIEW_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// Agent wysyła `/ping` co 8 s. Cisza dłuższa niż limit = proces agenta zabity / rozłączony.
const AGENT_SILENCE: Duration = Duration::from_secs(30);
/// ... w krokach z oknami systemu (zgody, import VCF, rola SMS, instalator APK).
const AGENT_SILENCE_INTERACTIVE: Duration = Duration::from_secs(120);
const QUEUE_POLL: Duration = Duration::from_secs(2);

/// Sesja przygotowana przez komendę (serwer już działa - GUI pokazuje QR / PIN).
pub struct Prepared {
    pub info: SessionInfo,
    session: Session,
}

struct Session {
    role: Role,
    serial: Option<String>,
    /// Eksport: folder bazowy (folder kopii powstaje po `hello`); import: folder kopii.
    base: PathBuf,
    resumed: bool,
    modules: Modules,
    manifest: Option<Manifest>,
    bridge: Bridge,
    events: mpsc::UnboundedReceiver<AgentEvent>,
    ctl: mpsc::UnboundedReceiver<ControlMsg>,
}

/// Zależności z warstwy aplikacji.
pub struct Env {
    pub adb: Option<Arc<Adb>>,
    pub agent: Option<AgentCtx>,
    pub root: Option<RootCache>,
    pub sessions: Sessions,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct Hello {
    device: DeviceDesc,
    installed: Vec<String>,
    free_bytes: u64,
    sizes: HashMap<String, u64>,
    agent_version: String,
}

fn err(m: impl Into<String>) -> AdbError {
    AdbError::Other(m.into())
}

// --------------------------------------------------------------------------- //
// Przygotowanie (komenda smartei_start)
// --------------------------------------------------------------------------- //

pub async fn prepare(opts: StartOptions, agent_apk: Option<PathBuf>) -> Result<(Prepared, Arc<Control>)> {
    let role = match opts.role.as_str() {
        "export" => Role::Export,
        "import" => Role::Import,
        r => return Err(AdbError::InvalidArg(format!("role: {r}"))),
    };
    let adb_mode = match opts.connection.as_str() {
        "adb" => true,
        "wifi" => false,
        c => return Err(AdbError::InvalidArg(format!("connection: {c}"))),
    };
    let serial = opts.serial.clone().filter(|s| !s.trim().is_empty());
    if adb_mode && serial.is_none() {
        return Err(AdbError::InvalidArg("Wybierz urządzenie ADB / select an ADB device".into()));
    }
    let modules = opts.modules.clone().sanitized();
    let base = PathBuf::from(opts.dir.trim());
    if opts.dir.trim().is_empty() || !base.is_dir() {
        return Err(AdbError::InvalidArg(format!("Folder nie istnieje / folder not found: {}", opts.dir)));
    }

    let (manifest, resumed) = match role {
        Role::Import => (Some(Manifest::load(&base)?), false),
        Role::Export => {
            if !modules.any() {
                return Err(AdbError::InvalidArg("Nie wybrano nic do eksportu / nothing selected".into()));
            }
            // wskazany istniejący folder SmartEI_* = wznowienie przerwanej kopii
            let resume = base.join(MANIFEST).is_file()
                && base.file_name().is_some_and(|n| n.to_string_lossy().starts_with("SmartEI_"));
            if resume {
                let m: Manifest = serde_json::from_str(&std::fs::read_to_string(base.join(MANIFEST))?)
                    .map_err(|e| AdbError::InvalidArg(format!("manifest.json: {e}")))?;
                m.validate()?;
                (Some(m), true)
            } else {
                (None, false)
            }
        }
    };

    let ip: std::net::IpAddr = if adb_mode {
        std::net::Ipv4Addr::LOCALHOST.into()
    } else {
        let ifs = net::interfaces();
        match opts.bind_ip.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(want) => {
                let ip: std::net::IpAddr = want.parse().map_err(|_| AdbError::InvalidArg(format!("IP: {want}")))?;
                if !ifs.iter().any(|i| i.ip == want) || !net::peer_allowed(ip) || ip.is_loopback() {
                    return Err(AdbError::InvalidArg(format!("{want} nie jest adresem w sieci lokalnej / not a LAN address")));
                }
                ip
            }
            None => ifs
                .iter()
                .find(|i| i.recommended)
                .and_then(|i| i.ip.parse().ok())
                .ok_or_else(|| err("Brak połączenia z siecią lokalną (Wi-Fi/LAN) / no LAN interface"))?,
        }
    };

    let (bridge, events) = Bridge::start(ip, role, adb_mode, base.clone(), agent_apk.clone())
        .await
        .map_err(|e| err(format!("Nie można uruchomić serwera Smart EI / cannot start bridge: {e}")))?;
    let (tx, ctl) = mpsc::unbounded_channel();
    let port = bridge.addr.port();
    let pin = bridge.shared.pin.clone();
    let host = ip.to_string();
    let pair_url = format!("http://{host}:{port}/p#k={pin}");
    let apk_url = format!("http://{host}:{port}/agent.apk");
    let session_id = format!("sei-{}", &net::new_token()[..12]);
    let info = SessionInfo {
        session_id,
        job_id: String::new(),
        role: role.as_str().into(),
        connection: opts.connection.clone(),
        dir: base.to_string_lossy().into(),
        ip: host,
        port,
        pin,
        pair_qr: if adb_mode { String::new() } else { net::qr_svg(&pair_url).unwrap_or_default() },
        apk_qr: if adb_mode { String::new() } else { net::qr_svg(&apk_url).unwrap_or_default() },
        pair_url,
        apk_url,
        agent_apk: agent_apk.map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
    };
    let session = Session { role, serial, base, resumed, modules, manifest, bridge, events, ctl };
    Ok((Prepared { info, session }, Arc::new(Control { tx })))
}

// --------------------------------------------------------------------------- //
// Zadanie
// --------------------------------------------------------------------------- //

pub async fn run(p: Prepared, env: Env, em: Emitter, cancel: CancelToken) {
    let id = p.info.session_id.clone();
    let mut sess = p.session;
    let mut lease: Option<AgentLease> = None;
    let res = run_inner(&mut sess, &env, &mut lease, &em, &cancel).await;

    // agent widzi anulowanie / koniec przy następnym zapytaniu
    {
        let mut g = sess.bridge.shared.lock();
        if res.is_err() {
            g.cancelled = true;
        }
        g.job["state"] = json!(if res.is_ok() { "done" } else { "cancel" });
    }
    em.item("stage:cleanup", true, "");
    tokio::time::sleep(Duration::from_millis(2500)).await;
    sess.bridge.stop();

    let mut warnings = Vec::new();
    if let (Some(adb), Some(serial)) = (&env.adb, &sess.serial) {
        let port = sess.bridge.addr.port().to_string();
        let _ = adb.run(Some(serial), &["reverse", "--remove", &format!("tcp:{port}")], 10).await;
        if let (Some(ctx), Some(l)) = (&env.agent, lease.take()) {
            warnings = backup::agent_release(ctx, l, &em).await;
        }
    }
    env.sessions.lock().await.remove(&id);

    match res {
        Ok(summary) => {
            em.item("result", true, summary.to_string());
            let failures: Vec<FailedItem> = summary["warnings"]
                .as_array()
                .map(|a| a.iter().filter_map(|w| w.as_str()).map(|w| FailedItem { path: "smartei".into(), error: w.into() }).collect())
                .unwrap_or_default();
            let failures: Vec<FailedItem> =
                failures.into_iter().chain(warnings.into_iter().map(|w| FailedItem { path: "cleanup".into(), error: w })).collect();
            em.ok("Smart EI: zakończono / finished.");
            em.finished(1, 0, 1, failures, false);
        }
        Err(AdbError::Cancelled) => {
            em.warn("Anulowano - pliki już zapisane zostają, można wznowić / cancelled, resume possible.");
            em.finished(0, 1, 1, vec![], true);
        }
        Err(e) => {
            let msg = match e {
                AdbError::Other(m) => m,
                o => o.to_string(),
            };
            em.error(msg.clone());
            em.item("stage:failed", false, msg.clone());
            em.finished(0, 0, 1, vec![FailedItem { path: "smartei".into(), error: msg }], false);
        }
    }
}

fn stage(em: &Emitter, key: &str, detail: impl Into<String>) {
    em.item(format!("stage:{key}"), true, detail);
}

async fn run_inner(
    sess: &mut Session,
    env: &Env,
    lease: &mut Option<AgentLease>,
    em: &Emitter,
    cancel: &CancelToken,
) -> Result<Value> {
    let port = sess.bridge.addr.port();
    let mut watch: Option<AdbWatch> = None;

    // --- 1. połączenie
    if let (Some(adb), Some(serial), Some(ctx)) = (&env.adb, &sess.serial, &env.agent) {
        stage(em, "deploy", "");
        let (r, l) = backup::agent_acquire(ctx, em, cancel).await;
        let sdk = l.sdk();
        *lease = Some(l);
        r?;
        if sdk > 0 && sdk < 21 {
            return Err(err("Android 5.0+ wymagany / required"));
        }
        let fwd = format!("tcp:{port}");
        let o = adb.run_c(Some(serial), &["reverse", &fwd, &fwd], 15, cancel).await?;
        if !o.ok() {
            return Err(err(format!("adb reverse: {} / port forwarding failed", o.err_text())));
        }
        if sess.role == Role::Import {
            if let Some(l) = lease.as_mut() {
                backup::agent_watch_sms(ctx, l).await;
            }
        }
        stage(em, "launch", "");
        // czas urządzenia przed startem - do odczytu ewentualnego crasha z bufora `crash`
        let since = sh(adb, serial, "date '+%m-%d %H:%M:%S.000'", 10).await.trim().to_string();
        let extras: Vec<String> = [
            "--es", "host", "127.0.0.1", "--ei", "port", &port.to_string(), "--es", "pin", &sess.bridge.shared.pin,
            "--es", "role", sess.role.as_str(), "--ez", "adb", "true",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        backup::agent_start(ctx, AGENT_ACTIVITY, &extras, cancel).await?;
        em.ok("Agent uruchomiony - łączy się z komputerem przez USB / agent started.");
        watch = Some(AdbWatch { adb: adb.clone(), serial: serial.clone(), since, extras });
    } else {
        stage(em, "pair", "");
        em.info("Czekam na parowanie agenta przez Wi-Fi (kod QR albo IP + PIN) / waiting for Wi-Fi pairing...");
    }

    // --- 2. parowanie + hello
    let hello = wait_hello(sess, env.agent.as_ref(), watch.as_ref(), em, cancel).await?;
    em.item("hello", true, json!({
        "device": hello.device, "freeBytes": hello.free_bytes, "sizes": hello.sizes, "agentVersion": hello.agent_version,
    }).to_string());
    em.ok(format!(
        "Połączono z: {} (Android {}, API {}) / paired.",
        hello.device.label(),
        hello.device.android_version,
        hello.device.sdk
    ));

    match sess.role {
        Role::Export => export(sess, env, &hello, em, cancel).await,
        Role::Import => import(sess, env, &hello, em, cancel).await,
    }
}

/// Nadzór nad agentem uruchomionym przez ADB (crash, brak procesu, brak połączenia).
struct AdbWatch {
    adb: Arc<Adb>,
    serial: String,
    /// Czas urządzenia tuż przed `am start` (`logcat -T`).
    since: String,
    extras: Vec<String>,
}

/// Wyciąg z bufora `crash` dla agenta: wyjątek + "Caused by" (None = brak crasha agenta).
pub fn crash_summary(logcat: &str) -> Option<String> {
    let lines: Vec<&str> = logcat.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.contains("FATAL EXCEPTION") && lines.iter().any(|x| x.contains(&format!("Process: {AGENT_PKG}"))))?;
    let body: Vec<String> = lines[start..]
        .iter()
        .map(|l| l.split_once("AndroidRuntime:").map(|(_, r)| r.trim()).unwrap_or(l.trim()).to_string())
        .filter(|l| !l.is_empty() && !l.starts_with("at ") && !l.starts_with("..."))
        .filter(|l| !l.starts_with("FATAL EXCEPTION") && !l.starts_with("Process:"))
        .take(4)
        .collect();
    Some(if body.is_empty() { "FATAL EXCEPTION".into() } else { body.join(" | ") })
}

async fn agent_running(adb: &Adb, serial: &str) -> Option<bool> {
    let o = adb.shell(Some(serial), &format!("pidof {AGENT_PKG} || ps -A 2>/dev/null | grep {AGENT_PKG} || ps | grep {AGENT_PKG}"), 10).await.ok()?;
    Some(!o.stdout.trim().is_empty())
}

async fn wait_hello(
    sess: &mut Session,
    agent: Option<&AgentCtx>,
    watch: Option<&AdbWatch>,
    em: &Emitter,
    cancel: &CancelToken,
) -> Result<Hello> {
    let started = Instant::now();
    let mut paired_at: Option<Instant> = None;
    let mut relaunched = false;
    let mut not_running = 0u32;
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        // limity: najpierw parowanie, potem hello
        match paired_at {
            None => {
                let limit = if watch.is_some() { PAIR_TIMEOUT_ADB } else { PAIR_TIMEOUT_WIFI };
                if started.elapsed() >= limit {
                    return Err(err(if watch.is_some() {
                        format!(
                            "Agent nie połączył się z komputerem w {} s. Odblokuj ekran telefonu i spróbuj ponownie; \
                             na Xiaomi/MIUI zezwól aplikacji na „Wyświetlanie wyskakujących okien w tle” / \
                             the agent did not connect within {} s - unlock the phone and retry",
                            PAIR_TIMEOUT_ADB.as_secs(),
                            PAIR_TIMEOUT_ADB.as_secs()
                        )
                    } else {
                        "Agent nie połączył się w wyznaczonym czasie / agent did not connect in time".into()
                    }));
                }
            }
            Some(t) => {
                let limit = if watch.is_some() { HELLO_TIMEOUT_ADB } else { HELLO_TIMEOUT_WIFI };
                if t.elapsed() >= limit {
                    return Err(err("Agent połączył się, ale nie przysłał danych urządzenia - przerwano / agent sent no hello"));
                }
            }
        }
        tokio::select! {
            ev = sess.events.recv() => match ev {
                Some(AgentEvent::Paired(dev)) => {
                    paired_at = Some(Instant::now());
                    stage(em, "paired", dev.to_string());
                    em.ok("Agent sparowany / agent paired.");
                }
                Some(AgentEvent::Hello(v)) => {
                    let h: Hello = serde_json::from_value(v).unwrap_or_default();
                    return Ok(h);
                }
                Some(AgentEvent::Log { level, message }) => log(em, &level, message),
                Some(AgentEvent::Error(e)) => return Err(err(format!("Agent: {e}"))),
                Some(_) => {}
                None => return Err(err("bridge closed")),
            },
            _ = tick.tick() => {
                let Some(w) = watch else { continue };
                // 1. crash agenta (bufor crash logcat od chwili uruchomienia)
                if !w.since.is_empty() {
                    let cmd = format!("logcat -b crash -d -v brief -T {} 2>/dev/null", shell_quote(&w.since));
                    if let Some(c) = crash_summary(&sh(&w.adb, &w.serial, &cmd, 10).await) {
                        return Err(err(format!(
                            "Agent na telefonie uległ awarii (crash) - przerwano, można spróbować ponownie / agent crashed: {c}"
                        )));
                    }
                }
                if paired_at.is_some() {
                    continue;
                }
                // 2. proces agenta zniknął (zamknięty / zabity przez system)
                if started.elapsed() >= Duration::from_secs(6) {
                    match agent_running(&w.adb, &w.serial).await {
                        Some(false) => not_running += 1,
                        _ => not_running = 0,
                    }
                    if not_running >= 3 && relaunched {
                        return Err(err(
                            "Agent nie działa na telefonie (proces zakończony) - przerwano, można spróbować ponownie / agent process is not running",
                        ));
                    }
                }
                // 3. jednorazowe ponowne uruchomienie (np. ekran był zablokowany)
                if !relaunched && (started.elapsed() >= RELAUNCH_AFTER || not_running >= 2) {
                    relaunched = true;
                    not_running = 0;
                    if let Some(ctx) = agent {
                        em.warn("Agent jeszcze się nie połączył - uruchamiam go ponownie / relaunching agent...");
                        let _ = backup::agent_start(ctx, AGENT_ACTIVITY, &w.extras, cancel).await;
                    }
                }
            }
            _ = cancel.cancelled() => return Err(AdbError::Cancelled),
        }
    }
}

async fn wait_proceed(sess: &mut Session, em: &Emitter, cancel: &CancelToken) -> Result<Selection> {
    stage(em, "review", "");
    let started = Instant::now();
    let live = Liveness::new(&sess.bridge.shared);
    let mut tick = liveness_tick();
    loop {
        let left = REVIEW_TIMEOUT
            .checked_sub(started.elapsed())
            .ok_or_else(|| err("Brak decyzji na ekranie podsumowania - przerwano / review timed out"))?;
        tokio::select! {
            c = sess.ctl.recv() => match c {
                Some(ControlMsg::Proceed(s)) => return Ok(s),
                Some(_) => {}
                None => return Err(AdbError::Cancelled),
            },
            ev = sess.events.recv() => match ev {
                Some(AgentEvent::Log { level, message }) => log(em, &level, message),
                Some(AgentEvent::Error(e)) => return Err(err(format!("Agent: {e}"))),
                Some(_) => {}
                None => return Err(err("bridge closed")),
            },
            _ = tokio::time::sleep(left) => {}
            _ = tick.tick() => live.check()?,
            _ = cancel.cancelled() => return Err(AdbError::Cancelled),
        }
    }
}

fn log(em: &Emitter, level: &str, message: String) {
    let lv = match level {
        "ok" => LogLevel::Ok,
        "warn" => LogLevel::Warn,
        "error" => LogLevel::Error,
        _ => LogLevel::Info,
    };
    em.log(lv, format!("[Agent] {message}"));
}

/// Nadzór "czy agent żyje": znacznik ostatniego zapytania z mostka + tryb kroku interaktywnego.
struct Liveness {
    shared: Arc<super::bridge::Shared>,
    interactive: bool,
}

impl Liveness {
    fn new(shared: &Arc<super::bridge::Shared>) -> Self {
        Self { shared: shared.clone(), interactive: false }
    }

    fn on_stage(&mut self, s: &str) {
        match s {
            "interactive" => self.interactive = true,
            "interactive_end" => self.interactive = false,
            _ => {}
        }
    }

    fn silent_for(&self) -> Option<Duration> {
        self.shared.lock().last_seen.map(|t| t.elapsed())
    }

    fn check(&self) -> Result<()> {
        let limit = if self.interactive { AGENT_SILENCE_INTERACTIVE } else { AGENT_SILENCE };
        match self.silent_for() {
            Some(d) if d > limit => Err(err(format!(
                "Agent przestał odpowiadać od {} s (proces zamknięty przez system, ekran zablokowany albo rozłączenie) - \
                 przerwano; zapisane dane zostają, można spróbować ponownie / agent stopped responding",
                d.as_secs()
            ))),
            _ => Ok(()),
        }
    }
}

fn liveness_tick() -> tokio::time::Interval {
    let mut t = tokio::time::interval(Duration::from_secs(2));
    t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    t
}

fn forward_progress(em: &Emitter, v: &Value) {
    let g = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    em.send(TransferEvent::Progress {
        done: g("done"),
        total: g("total"),
        bytes_done: g("bytesDone"),
        bytes_total: g("bytesTotal"),
        current: v.get("current").and_then(|x| x.as_str()).map(|s| s.chars().take(300).collect()),
    });
    if let Some(ph) = v.get("phase").and_then(|x| x.as_str()) {
        em.item(format!("phase:{}", ph.chars().take(32).collect::<String>()), true, "");
    }
}

/// Emitter pod-zadania: przekazuje log i postęp, zatrzymuje Finished/Fatal dla siebie.
#[derive(Default, Clone, Debug)]
struct SubResult {
    ok: u64,
    skipped: u64,
    total: u64,
    failures: usize,
    fatal: Option<String>,
    cancelled: bool,
}

fn sub_emitter(parent: &Emitter, phase: &str) -> (Emitter, Arc<StdMutex<SubResult>>) {
    let res = Arc::new(StdMutex::new(SubResult::default()));
    let (p, r) = (parent.clone(), res.clone());
    let phase = phase.to_string();
    let em = Emitter::callback(move |e| match e {
        TransferEvent::Finished { ok, skipped, total, failures, cancelled } => {
            let mut g = r.lock().unwrap_or_else(|x| x.into_inner());
            g.ok += ok;
            g.skipped += skipped;
            g.total += total;
            g.failures += failures.len();
            g.cancelled |= cancelled;
            for f in failures.iter().take(20) {
                p.warn(format!("{phase}: {} - {}", f.path, f.error));
            }
        }
        TransferEvent::Fatal { message } => {
            p.warn(format!("{phase}: {message}"));
            r.lock().unwrap_or_else(|x| x.into_inner()).fatal = Some(message);
        }
        TransferEvent::Item { .. } => {}
        other => p.send(other),
    });
    (em, res)
}

fn take(r: &Arc<StdMutex<SubResult>>) -> SubResult {
    r.lock().unwrap_or_else(|x| x.into_inner()).clone()
}

async fn sh(adb: &Adb, serial: &str, cmd: &str, t: u64) -> String {
    adb.shell(Some(serial), cmd, t).await.map(|o| o.stdout).unwrap_or_default()
}

fn dir_size(p: &Path) -> (u64, u64) {
    let (mut n, mut b) = (0u64, 0u64);
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let name = e.file_name().to_string_lossy().to_string();
            if crate::adb::pull::is_junk_path(&name, true) {
                continue;
            }
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                if name.ends_with(".part") {
                    continue;
                }
                n += 1;
                b += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    (n, b)
}

fn free_space(p: &Path) -> u64 {
    fs4::available_space(p).unwrap_or(0)
}

// --------------------------------------------------------------------------- //
// EKSPORT
// --------------------------------------------------------------------------- //

#[derive(Default)]
struct ExportState {
    apps: Option<Vec<AppRecord>>,
    settings: PortableSettings,
    modules: Vec<ModuleEntry>,
    uploaded_files: u64,
    uploaded_bytes: u64,
    agent_error: Option<String>,
    agent_notes: Vec<String>,
    agent_done: bool,
}

async fn estimate_export(env: &Env, serial: Option<&str>, hello: &Hello, m: &Modules) -> (u64, BTreeMap<String, u64>) {
    let mut sizes: BTreeMap<String, u64> = BTreeMap::new();
    for cat in &m.files {
        let mut bytes = hello.sizes.get(cat).copied().unwrap_or(0);
        if let (Some(adb), Some(serial)) = (&env.adb, serial) {
            let mut total = 0u64;
            for p in category_paths(cat) {
                let out = sh(adb, serial, &format!("du -sk {} 2>/dev/null", shell_quote(&format!("{SDCARD}/{p}"))), 120).await;
                total += out.split_whitespace().next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) * 1024;
            }
            if total > 0 {
                bytes = total;
            }
        }
        sizes.insert(cat.clone(), bytes);
    }
    if m.apks {
        sizes.insert("apks".into(), hello.sizes.get("apks").copied().unwrap_or(0));
    }
    for k in ["contacts", "sms", "wallpaper"] {
        let on = match k {
            "contacts" => m.contacts,
            "sms" => m.sms,
            _ => m.wallpaper,
        };
        if on {
            sizes.insert(k.into(), hello.sizes.get(k).copied().unwrap_or(0));
        }
    }
    (sizes.values().sum(), sizes)
}

async fn export(sess: &mut Session, env: &Env, hello: &Hello, em: &Emitter, cancel: &CancelToken) -> Result<Value> {
    let adb_mode = sess.serial.is_some();
    let serial = sess.serial.clone();

    // --- podsumowanie: rozmiar vs wolne miejsce
    let (estimate, sizes) = estimate_export(env, serial.as_deref(), hello, &sess.modules).await;
    let free = free_space(&sess.base);
    let need = estimate + estimate / 20 + 200 * 1024 * 1024;
    em.item(
        "review",
        true,
        json!({
            "role": "export", "estimate": estimate, "sizes": sizes, "free": free, "enough": free == 0 || free >= need,
            "resumed": sess.resumed, "modules": sess.modules, "source": hello.device,
        })
        .to_string(),
    );
    let sel = wait_proceed(sess, em, cancel).await?;
    let mut modules = sel.modules.sanitized();
    if !modules.any() {
        modules = sess.modules.clone();
    }
    let (estimate, _) = estimate_export(env, serial.as_deref(), hello, &modules).await;
    let need = estimate + estimate / 20 + 200 * 1024 * 1024;
    if free > 0 && free < need {
        return Err(err(format!(
            "Za mało miejsca na dysku: potrzeba ok. {}, wolne {} / not enough disk space",
            human_bytes(need),
            human_bytes(free)
        )));
    }

    // --- folder kopii + manifest (complete=false)
    let dir = if sess.resumed {
        sess.base.clone()
    } else {
        let d = sess.base.join(folder_name(&hello.device.model, chrono::Local::now()));
        std::fs::create_dir_all(&d)?;
        d
    };
    let mut manifest = match sess.manifest.take() {
        Some(mut m) => {
            m.complete = false;
            m.source = hello.device.clone();
            m
        }
        None => Manifest::new(hello.device.clone(), if adb_mode { "adb" } else { "wifi" }),
    };
    manifest.save(&dir)?;
    stage(em, "export", dir.to_string_lossy());
    em.info(format!("Folder kopii / backup folder: {}", dir.display()));

    // --- start agenta
    {
        let mut g = sess.bridge.shared.lock();
        g.dir = dir.clone();
        g.agent_sends_apks = modules.apks && !adb_mode;
        let files: Vec<Value> = if adb_mode {
            vec![]
        } else {
            modules.files.iter().map(|k| json!({ "key": k, "paths": category_paths(k) })).collect()
        };
        g.job = json!({
            "state": "go", "role": "export", "adb": adb_mode,
            "modules": {
                "contacts": modules.contacts, "sms": modules.sms, "wallpaper": modules.wallpaper,
                "settings": modules.settings, "apps": modules.apps, "apks": modules.apks && !adb_mode, "files": files,
            },
            "exclude": DOWNLOAD_EXCLUDE,
        });
    }

    let mut st = ExportState::default();
    let pc = pc_export(env, serial.as_deref(), &dir, &modules, em, cancel);
    let ag = agent_loop_export(&mut sess.events, Liveness::new(&sess.bridge.shared), &mut st, em, cancel);
    let (pc_res, ag_res) = tokio::join!(pc, ag);
    if cancel.is_cancelled() {
        manifest.notes.push("Przerwano - można wznowić wskazując ten folder / interrupted, resume by selecting this folder".into());
        let _ = manifest.save(&dir);
        return Err(AdbError::Cancelled);
    }
    let mut warnings: Vec<String> = Vec::new();
    if let Err(e) = ag_res {
        warnings.push(format!("Agent: {e}"));
    }
    if let Some(e) = &st.agent_error {
        warnings.push(format!("Agent: {e}"));
    }
    let pc_out = pc_res;
    warnings.extend(pc_out.warnings.iter().cloned());
    warnings.extend(st.agent_notes.iter().cloned());

    // --- APK aplikacji spoza Sklepu Play (ADB: adb pull wszystkich splitów)
    let mut apps = st.apps.take().unwrap_or_default();
    if apps.is_empty() && modules.apps {
        // agent nie przysłał listy (np. przerwany) - lista z `pm list packages -f -i -3` (bez nazw)
        if let (Some(adb), Some(serial)) = (&env.adb, &serial) {
            let out = sh(adb, serial, "pm list packages -f -i -3", 30).await;
            apps = parse_pm_list(&out)
                .into_iter()
                .filter_map(|(package, path, installer)| {
                    AppRecord::from_agent(super::apps::AgentApp {
                        label: package.clone(),
                        package,
                        installer,
                        apks: vec![path],
                        ..Default::default()
                    })
                })
                .collect();
            if !apps.is_empty() {
                warnings.push("Lista aplikacji z pm (bez nazw) - agent jej nie przysłał / app list from pm".into());
            }
        }
    }
    if modules.apks && adb_mode {
        if let (Some(adb), Some(serial)) = (&env.adb, &serial) {
            pull_apks(adb, serial, &dir, &apps, em, cancel).await;
        }
    }
    if cancel.is_cancelled() {
        let _ = manifest.save(&dir);
        return Err(AdbError::Cancelled);
    }

    // --- zrzut ekranu głównego (po pracy agenta - przycisk HOME go nie przerwie)
    if modules.homescreen && adb_mode {
        if let (Some(adb), Some(serial)) = (&env.adb, &serial) {
            match homescreen(adb, serial, &dir).await {
                Ok(()) => em.ok("Zapisano wallpaper/homescreen_ref.png (układ ekranu - podgląd) / home screen captured."),
                Err(e) => warnings.push(format!("homescreen_ref.png: {e}")),
            }
        }
    }

    // --- apps.json
    if modules.apps {
        for a in apps.iter_mut() {
            a.apk_files = scan_apks(&dir, &a.package);
        }
        let with_apk = apps.iter().filter(|a| a.has_apk()).count() as u64;
        let apk_bytes: u64 = apps.iter().flat_map(|a| a.apk_files.iter()).map(|f| f.size).sum();
        let play = apps.iter().filter(|a| a.source == AppSource::PlayStore).count();
        let file = AppsFile { format_version: 1, apps };
        std::fs::write(dir.join("apps.json"), serde_json::to_vec_pretty(&file).map_err(|e| err(e.to_string()))?)?;
        manifest.set_module(ModuleEntry {
            name: "apps".into(),
            ok: !file.apps.is_empty(),
            items: file.apps.len() as u64,
            bytes: apk_bytes,
            note: format!("play_store: {play}, sideload: {}, apk: {with_apk}", file.apps.len() - play),
        });
        em.ok(format!("Aplikacje: {} (Google Play: {play}, APK w kopii: {with_apk}) / apps saved.", file.apps.len()));
    }

    // --- settings.json (ADB ma pierwszeństwo przed odczytem agenta)
    if modules.settings {
        let mut s = st.settings.clone();
        s.merge(&pc_out.settings);
        s.format_version = 1;
        if !s.is_empty() {
            std::fs::write(dir.join("settings.json"), serde_json::to_vec_pretty(&s).map_err(|e| err(e.to_string()))?)?;
        }
        manifest.set_module(ModuleEntry {
            name: "settings".into(),
            ok: !s.is_empty(),
            items: s.count() as u64,
            bytes: 0,
            note: String::new(),
        });
    }

    // --- moduły od agenta (kontakty, SMS, tapeta, pliki w Wi-Fi)
    for m in st.modules.drain(..) {
        manifest.set_module(m);
    }
    if !modules.files.is_empty() {
        let (n, b) = dir_size(&dir.join("files"));
        let note = if adb_mode {
            format!("adb pull: ok {}, pominięte {}, błędy {}", pc_out.files.ok, pc_out.files.skipped, pc_out.files.failures)
        } else {
            format!("wifi: przesłano {} plików ({})", st.uploaded_files, human_bytes(st.uploaded_bytes))
        };
        manifest.set_module(ModuleEntry {
            name: "files".into(),
            ok: pc_out.files.fatal.is_none() && pc_out.files.failures == 0,
            items: n,
            bytes: b,
            note,
        });
    }
    if modules.wallpaper && manifest.module("wallpaper").is_none() {
        manifest.set_module(ModuleEntry {
            name: "wallpaper".into(),
            ok: dir.join("wallpaper/home.png").is_file(),
            note: "agent nie zgłosił wyniku / no agent report".into(),
            ..Default::default()
        });
    }
    if !st.agent_done {
        warnings.push("Agent nie potwierdził zakończenia - część modułów może być niepełna / agent did not finish".into());
    }

    // --- sumy kontrolne + complete
    stage(em, "finalize", "");
    em.info("Liczę sumy kontrolne / computing checksums...");
    let d2 = dir.clone();
    let mut m2 = manifest.clone();
    manifest = tokio::task::spawn_blocking(move || {
        let _ = m2.compute_checksums(&d2);
        m2
    })
    .await
    .unwrap_or(manifest);
    manifest.notes.extend(warnings.iter().cloned());
    manifest.complete = true;
    manifest.completed = Some(chrono::Local::now().to_rfc3339());
    manifest.save(&dir)?;
    let (n, b) = dir_size(&dir);
    em.ok(format!("Kopia Smart EI gotowa: {n} plików, {} -> {} / backup complete.", human_bytes(b), dir.display()));
    Ok(json!({ "role": "export", "dir": dir, "modules": manifest.modules, "files": n, "bytes": b, "warnings": warnings }))
}

async fn agent_loop_export(
    events: &mut mpsc::UnboundedReceiver<AgentEvent>,
    mut live: Liveness,
    st: &mut ExportState,
    em: &Emitter,
    cancel: &CancelToken,
) -> Result<()> {
    let mut tick = liveness_tick();
    loop {
        tokio::select! {
            ev = events.recv() => match ev {
                Some(AgentEvent::Apps(a)) => {
                    em.info(format!("Agent: lista aplikacji ({}) / app list received", a.len()));
                    st.apps = Some(a);
                }
                Some(AgentEvent::Settings(v)) => st.settings = PortableSettings::sanitize(&v),
                Some(AgentEvent::Module(m)) => {
                    let txt = format!("{}: {} ({}) {}", m.name, if m.ok { "OK" } else { "-" }, m.items, m.note);
                    if m.ok { em.ok(txt) } else { em.warn(txt) }
                    st.modules.retain(|x| x.name != m.name);
                    st.modules.push(m);
                }
                Some(AgentEvent::Uploaded { path, size }) => {
                    if path.starts_with("files/") {
                        st.uploaded_files += 1;
                        st.uploaded_bytes += size;
                    }
                }
                Some(AgentEvent::Progress(v)) => forward_progress(em, &v),
                Some(AgentEvent::Log { level, message }) => log(em, &level, message),
                Some(AgentEvent::Stage(s)) => {
                    live.on_stage(&s);
                    stage(em, &format!("agent-{s}"), "");
                }
                Some(AgentEvent::Done(v)) => {
                    st.agent_done = true;
                    if let Some(notes) = v.get("notes").and_then(|n| n.as_array()) {
                        st.agent_notes.extend(notes.iter().filter_map(|n| n.as_str()).map(|n| n.chars().take(500).collect::<String>()));
                    }
                    em.ok("Agent zakończył swoją część / agent finished.");
                    return Ok(());
                }
                Some(AgentEvent::Error(e)) => {
                    st.agent_error = Some(e);
                    return Ok(());
                }
                Some(_) => {}
                None => return Err(err("bridge closed")),
            },
            _ = tick.tick() => live.check()?,
            _ = cancel.cancelled() => return Err(AdbError::Cancelled),
        }
    }
}

#[derive(Default)]
struct PcExport {
    files: SubResult,
    settings: PortableSettings,
    warnings: Vec<String>,
}

async fn pc_export(
    env: &Env,
    serial: Option<&str>,
    dir: &Path,
    m: &Modules,
    em: &Emitter,
    cancel: &CancelToken,
) -> PcExport {
    let mut out = PcExport::default();
    let (Some(adb), Some(serial)) = (&env.adb, serial) else { return out };

    if m.settings {
        out.settings = read_settings_adb(adb, serial).await;
        em.info(format!("Ustawienia odczytane przez ADB: {} / settings read.", out.settings.count()));
    }

    // pliki: osobny pull dla każdej ścieżki - ścieżki względem /sdcard zachowane w files/
    let (sub, res) = sub_emitter(em, "files");
    for cat in &m.files {
        for rel in category_paths(cat) {
            if cancel.is_cancelled() {
                return out;
            }
            let remote = format!("{SDCARD}/{rel}");
            let exists = sh(adb, serial, &format!("[ -d {} ] && echo Y", shell_quote(&remote)), 15).await;
            if !exists.contains('Y') {
                continue;
            }
            let items: Vec<SelectedItem> = if *cat == "download" {
                let ls = sh(adb, serial, &format!("ls -1p {} 2>/dev/null", shell_quote(&remote)), 30).await;
                ls.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !DOWNLOAD_EXCLUDE.contains(&l.trim_end_matches('/')))
                    .map(|l| SelectedItem {
                        path: format!("{remote}/{}", l.trim_end_matches('/')),
                        is_file: !l.ends_with('/'),
                    })
                    .collect()
            } else {
                vec![SelectedItem { path: remote.clone(), is_file: false }]
            };
            if items.is_empty() {
                continue;
            }
            let parent = rel.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
            let local_root = if *cat == "download" {
                dir.join("files").join(rel)
            } else if parent.is_empty() {
                dir.join("files")
            } else {
                dir.join("files").join(parent)
            };
            let _ = std::fs::create_dir_all(&local_root);
            em.item("phase:files", true, rel.to_string());
            em.info(format!("Pobieram / pulling {remote} ..."));
            let opts = PullOptions {
                items,
                local_root: local_root.to_string_lossy().into(),
                workers: 4,
                timeout_seconds: 900,
                resume: true,
                skip_existing: true,
                preserve_time: true,
                ntfs_safe: true,
                use_root: false,
                exclude_junk: true,
            };
            run_pull(adb.clone(), serial.to_string(), opts, env.root.clone(), sub.clone(), cancel.clone()).await;
        }
    }
    out.files = take(&res);
    if out.files.failures > 0 {
        out.warnings.push(format!("Pliki: {} nie udało się pobrać / {} files failed", out.files.failures, out.files.failures));
    }
    out
}

async fn read_settings_adb(adb: &Adb, serial: &str) -> PortableSettings {
    let mut raw = json!({ "system": {}, "volumes": {} });
    for k in SYSTEM_KEYS {
        let v = sh(adb, serial, &format!("settings get system {k}"), 10).await;
        let v = v.trim();
        if !v.is_empty() && v != "null" {
            raw["system"][*k] = json!(v);
        }
    }
    let night = sh(adb, serial, "cmd uimode night", 10).await;
    if !night.trim().is_empty() {
        raw["uiNightMode"] = json!(night.trim());
    }
    for (name, stream) in STREAMS {
        if let Some(v) = parse_volume(&sh(adb, serial, &format!("cmd media_session volume --stream {stream} --get"), 10).await) {
            raw["volumes"][*name] = json!({ "value": v.value, "max": v.max });
        }
    }
    PortableSettings::sanitize(&raw)
}

fn scan_apks(dir: &Path, pkg: &str) -> Vec<ApkFile> {
    let d = dir.join("apks").join(pkg);
    let mut v: Vec<ApkFile> = std::fs::read_dir(&d)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    let m = e.metadata().ok()?;
                    (m.is_file() && name.ends_with(".apk")).then_some(ApkFile { name, size: m.len() })
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| (a.name != "base.apk", &a.name).cmp(&(b.name != "base.apk", &b.name)));
    v
}

async fn pull_apks(adb: &Adb, serial: &str, dir: &Path, apps: &[AppRecord], em: &Emitter, cancel: &CancelToken) {
    let side: Vec<&AppRecord> = apps.iter().filter(|a| a.source == AppSource::Sideload).collect();
    if side.is_empty() {
        return;
    }
    stage(em, "apks", "");
    let total = side.len() as u64;
    for (i, a) in side.iter().enumerate() {
        if cancel.is_cancelled() {
            return;
        }
        em.send(TransferEvent::Progress {
            done: i as u64,
            total,
            bytes_done: 0,
            bytes_total: 0,
            current: Some(format!("APK: {}", a.label)),
        });
        if !is_valid_package(&a.package) {
            continue;
        }
        let out = sh(adb, serial, &format!("pm path {}", a.package), 20).await;
        let paths: Vec<String> = out.lines().filter_map(|l| l.trim().strip_prefix("package:")).map(String::from).collect();
        let target = dir.join("apks").join(&a.package);
        let _ = std::fs::create_dir_all(&target);
        let mut ok = true;
        for p in &paths {
            let Some(name) = apk_file_name(p) else { continue };
            let local = target.join(&name);
            let remote_size: u64 = sh(adb, serial, &format!("stat -c %s {}", shell_quote(p)), 10)
                .await
                .trim()
                .parse()
                .unwrap_or(0);
            if remote_size > 0 && std::fs::metadata(&local).map(|m| m.len()) .ok() == Some(remote_size) {
                continue;
            }
            let ls = local.to_string_lossy().to_string();
            let r = adb.run_c(Some(serial), &["pull", p, &ls], 600, cancel).await;
            let got = std::fs::metadata(&local).map(|m| m.len()).unwrap_or(0);
            if !matches!(r, Ok(ref o) if o.ok()) || (remote_size > 0 && got != remote_size) {
                ok = false;
                let _ = std::fs::remove_file(&local);
            }
        }
        if ok && !paths.is_empty() {
            em.ok(format!("APK: {} ({} plików) / apk saved", a.label, paths.len()));
        } else {
            em.warn(format!("APK: {} - nie udało się pobrać / apk pull failed", a.package));
        }
    }
    em.send(TransferEvent::Progress { done: total, total, bytes_done: 0, bytes_total: 0, current: None });
}

async fn homescreen(adb: &Adb, serial: &str, dir: &Path) -> Result<()> {
    let _ = adb.shell(Some(serial), "input keyevent KEYCODE_HOME", 10).await;
    tokio::time::sleep(Duration::from_millis(1800)).await;
    let wd = dir.join("wallpaper");
    std::fs::create_dir_all(&wd)?;
    let shot = crate::mirror::screenshot::take(adb, serial, &wd).await?;
    std::fs::rename(&shot.path, wd.join("homescreen_ref.png"))?;
    Ok(())
}

// --------------------------------------------------------------------------- //
// IMPORT
// --------------------------------------------------------------------------- //

struct FileEntry {
    rel: String,
    target: String,
    size: u64,
    cat: String,
}

/// Pliki z `files/` z kategorią (po ścieżce względem /sdcard).
fn index_files(dir: &Path) -> Vec<FileEntry> {
    let root = dir.join("files");
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            let name = e.file_name().to_string_lossy().to_string();
            // .thumbnails, checkpoint pobierania, .adbst-partial - nie przywracamy na telefon
            if crate::adb::pull::is_junk_path(&name, true) {
                continue;
            }
            if ft.is_dir() {
                stack.push(p);
                continue;
            }
            if !ft.is_file() || name.ends_with(".part") {
                continue;
            }
            let Ok(rel) = p.strip_prefix(&root) else { continue };
            let target = rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect::<Vec<_>>().join("/");
            let cat = FILE_CATEGORIES
                .iter()
                .find(|(_, paths)| paths.iter().any(|pp| target.starts_with(&format!("{pp}/"))))
                .map(|(k, _)| k.to_string())
                .unwrap_or_else(|| "other".into());
            out.push(FileEntry {
                rel: format!("files/{target}"),
                target,
                size: e.metadata().map(|m| m.len()).unwrap_or(0),
                cat,
            });
        }
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

async fn import(sess: &mut Session, env: &Env, hello: &Hello, em: &Emitter, cancel: &CancelToken) -> Result<Value> {
    let adb_mode = sess.serial.is_some();
    let serial = sess.serial.clone();
    let dir = sess.base.clone();
    let man = sess.manifest.clone().ok_or_else(|| err("manifest"))?;

    // --- integralność kopii
    let d2 = dir.clone();
    let m2 = man.clone();
    let bad = tokio::task::spawn_blocking(move || m2.verify(&d2)).await.unwrap_or_default();
    for b in &bad {
        em.warn(format!("Kopia: {b}"));
    }
    if !man.complete {
        em.warn("Kopia nie została ukończona (przerwany eksport) - przywrócę to, co jest / backup incomplete.");
    }

    let apps: Vec<AppRecord> = std::fs::read(dir.join("apps.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<AppsFile>(&b).ok())
        .map(|f| f.apps)
        .unwrap_or_default();
    let settings = std::fs::read(dir.join("settings.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .map(|v| PortableSettings::sanitize(&v))
        .unwrap_or_default();
    let mut installed: HashSet<String> = hello.installed.iter().cloned().collect();
    if let (Some(adb), Some(serial)) = (&env.adb, &serial) {
        installed.extend(parse_package_names(&sh(adb, serial, "pm list packages", 30).await));
    }
    let list = build_import_list(&apps, &man.source.manufacturer, &hello.device.manufacturer, &installed);
    let files = index_files(&dir);
    let mut cats: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for f in &files {
        let e = cats.entry(f.cat.clone()).or_default();
        e.0 += 1;
        e.1 += f.size;
    }
    let vcf = dir.join("contacts_sms/contacts.vcf");
    let xml = dir.join("contacts_sms/sms.xml");
    let home = dir.join("wallpaper/home.png");
    let lock = dir.join("wallpaper/lock.png");
    let cross = family_of(&man.source.manufacturer) != family_of(&hello.device.manufacturer);
    em.item(
        "review",
        true,
        json!({
            "role": "import",
            "source": man.source, "target": hello.device, "crossVendor": cross,
            "created": man.created, "complete": man.complete, "problems": bad,
            "targetFree": hello.free_bytes,
            "available": {
                "contacts": vcf.is_file(), "sms": xml.is_file(),
                "wallpaper": home.is_file() || lock.is_file(),
                "settings": !settings.is_empty(), "settingsCount": settings.count(),
                "files": cats.iter().map(|(k, (n, b))| json!({"key": k, "count": n, "bytes": b})).collect::<Vec<_>>(),
                "apps": !list.is_empty(),
            },
            "apps": list,
        })
        .to_string(),
    );

    let sel = wait_proceed(sess, em, cancel).await?;
    let modules = sel.modules.sanitized();
    let chosen: HashSet<&str> = sel.packages.iter().map(String::as_str).collect();
    let selected_apps: Vec<&AppRecord> = apps
        .iter()
        .filter(|a| modules.apps && chosen.contains(a.package.as_str()))
        .collect();
    let sideload: Vec<&AppRecord> =
        selected_apps.iter().copied().filter(|a| a.source == AppSource::Sideload && a.has_apk()).collect();
    let play: Vec<&AppRecord> = selected_apps.iter().copied().filter(|a| a.source == AppSource::PlayStore).collect();
    let sel_files: Vec<&FileEntry> =
        files.iter().filter(|f| modules.files.contains(&f.cat) || (f.cat == "other" && !modules.files.is_empty())).collect();

    let need: u64 = sel_files.iter().map(|f| f.size).sum::<u64>()
        + sideload.iter().flat_map(|a| a.apk_files.iter()).map(|f| f.size * 2).sum::<u64>();
    if hello.free_bytes > 0 && need + 300 * 1024 * 1024 > hello.free_bytes {
        return Err(err(format!(
            "Za mało miejsca w telefonie: potrzeba ok. {}, wolne {} / not enough space on the phone",
            human_bytes(need + 300 * 1024 * 1024),
            human_bytes(hello.free_bytes)
        )));
    }

    // --- plan dla agenta
    let mut allowed: HashSet<String> = HashSet::new();
    let rel_if = |on: bool, p: &Path, rel: &str, allowed: &mut HashSet<String>| -> Value {
        if on && p.is_file() {
            allowed.insert(rel.to_string());
            json!(rel)
        } else {
            Value::Null
        }
    };
    let contacts = rel_if(modules.contacts, &vcf, "contacts_sms/contacts.vcf", &mut allowed);
    let sms = rel_if(modules.sms, &xml, "contacts_sms/sms.xml", &mut allowed);
    let wp_home = rel_if(modules.wallpaper, &home, "wallpaper/home.png", &mut allowed);
    let wp_lock = rel_if(modules.wallpaper, &lock, "wallpaper/lock.png", &mut allowed);
    let files_json: Vec<Value> = sel_files
        .iter()
        .map(|f| {
            if !adb_mode {
                allowed.insert(f.rel.clone());
            }
            json!({ "path": f.rel, "size": f.size, "target": f.target })
        })
        .collect();
    let apks_json: Vec<Value> = if adb_mode {
        vec![]
    } else {
        sideload
            .iter()
            .map(|a| {
                let fl: Vec<Value> = a
                    .apk_files
                    .iter()
                    .map(|f| {
                        let rel = format!("apks/{}/{}", a.package, f.name);
                        allowed.insert(rel.clone());
                        json!({ "path": rel, "size": f.size })
                    })
                    .collect();
                json!({ "package": a.package, "label": a.label, "files": fl })
            })
            .collect()
    };
    let play_json: Vec<Value> = play.iter().map(|a| json!({ "package": a.package, "label": a.label })).collect();
    let plan = json!({
        "adb": adb_mode,
        "contacts": contacts, "sms": sms,
        "wallpaper": if wp_home.is_null() && wp_lock.is_null() { Value::Null } else { json!({ "home": wp_home, "lock": wp_lock }) },
        "settings": if modules.settings && !adb_mode && !settings.is_empty() { serde_json::to_value(&settings).unwrap_or(Value::Null) } else { Value::Null },
        "download": !adb_mode,
        "files": files_json,
        "apks": apks_json,
        "play": play_json,
    });
    {
        let mut g = sess.bridge.shared.lock();
        g.plan = Some(plan);
        g.allowed_files = allowed;
        g.job = json!({ "state": "go", "role": "import", "adb": adb_mode, "pcDone": !adb_mode, "agentInstall": [] });
    }
    stage(em, "import", "");

    // --- równolegle: komputer (ADB) + agent
    let shared = sess.bridge.shared.clone();
    let pc = async {
        let r = pc_import(env, serial.as_deref(), &dir, &modules, &settings, &sel_files, &sideload, em, cancel).await;
        // po długim push/instalacji ekran mógł zgasnąć: wybudzenie + agent na pierwszy plan, bo
        // okna systemu (import VCF, prośba o rolę SMS) nie otwierają się z tła
        if let (Some(adb), Some(serial)) = (&env.adb, serial.as_deref()) {
            bring_agent_to_front(adb, serial).await;
        }
        let mut g = shared.lock();
        g.job["pcDone"] = json!(true);
        g.job["agentInstall"] = json!(r.agent_install);
        r
    };
    let mut report: Vec<ModuleEntry> = Vec::new();
    let ag = agent_loop_until(&mut sess.events, Liveness::new(&shared), &mut report, "queue", em, cancel);
    let (pc_out, ag_res) = tokio::join!(pc, ag);
    if cancel.is_cancelled() {
        return Err(AdbError::Cancelled);
    }
    let mut warnings = pc_out.warnings.clone();
    // agent zamilkł / zgłosił błąd przed kolejką: czytelny błąd zamiast czekania
    ag_res?;

    // --- kolejka Sklepu Play
    let mut queue_summary = json!({ "total": 0 });
    if !play.is_empty() {
        stage(em, "queue", "");
        let q = PlayQueue::new(play.iter().map(|a| (a.package.clone(), a.label.clone())));
        sess.bridge.shared.lock().queue = Some(q);
        run_queue(sess, env, &serial, &installed, em, cancel).await?;
        let g = sess.bridge.shared.lock();
        if let Some(q) = g.queue.as_ref() {
            use super::queue::ItemStatus as S;
            queue_summary = json!({
                "total": q.items.len(), "installed": q.count(S::Installed), "skipped": q.count(S::Skipped), "advanced": q.count(S::Advanced),
            });
        }
    }

    // --- koniec: agent pokazuje podsumowanie, użytkownik klika "Zakończ"
    {
        let mut g = sess.bridge.shared.lock();
        g.job["state"] = json!("finish");
        g.job["queue"] = queue_summary.clone();
    }
    stage(em, "finish", "");
    // agent może być zamrożony w tle (Sklep Play) - dłuższa cisza = kończymy bez jego potwierdzenia
    let mut finish_live = Liveness::new(&sess.bridge.shared);
    finish_live.interactive = true;
    let fin = tokio::time::timeout(
        Duration::from_secs(10 * 60),
        agent_loop_until(&mut sess.events, finish_live, &mut report, "", em, cancel),
    )
    .await;
    match fin {
        Ok(Ok(())) => {}
        Ok(Err(AdbError::Cancelled)) => return Err(AdbError::Cancelled),
        Ok(Err(e)) => warnings.push(format!("Agent nie potwierdził zakończenia ({e}) - dane zostały już przywrócone")),
        Err(_) => warnings.push("Agent nie potwierdził zakończenia w 10 min - dane zostały już przywrócone".into()),
    }
    if cancel.is_cancelled() {
        return Err(AdbError::Cancelled);
    }

    if let (Some(adb), Some(serial)) = (&env.adb, &serial) {
        if !pc_out.agent_install.is_empty() {
            let _ = adb.shell(Some(serial), &format!("rm -rf {}", shell_quote(APK_FALLBACK_DIR)), 30).await;
        }
    }
    report.push(ModuleEntry {
        name: "files".into(),
        ok: pc_out.files.failures == 0 && pc_out.files.fatal.is_none(),
        items: if adb_mode { pc_out.files.ok + pc_out.files.skipped } else { sel_files.len() as u64 },
        bytes: sel_files.iter().map(|f| f.size).sum(),
        note: String::new(),
    });
    report.push(ModuleEntry {
        name: "apks".into(),
        ok: pc_out.apk_failed == 0,
        items: pc_out.apk_installed,
        bytes: 0,
        note: format!("fallback: {}", pc_out.agent_install.len()),
    });
    em.ok("Import Smart EI zakończony / import finished.");
    Ok(json!({ "role": "import", "dir": dir, "modules": report, "queue": queue_summary, "warnings": warnings }))
}

/// Zdarzenia agenta aż do etapu `until` (pusty = do `/done`).
async fn agent_loop_until(
    events: &mut mpsc::UnboundedReceiver<AgentEvent>,
    mut live: Liveness,
    report: &mut Vec<ModuleEntry>,
    until: &str,
    em: &Emitter,
    cancel: &CancelToken,
) -> Result<()> {
    let mut tick = liveness_tick();
    loop {
        tokio::select! {
            ev = events.recv() => match ev {
                Some(AgentEvent::Module(m)) => {
                    let txt = format!("{}: {} ({}) {}", m.name, if m.ok { "OK" } else { "-" }, m.items, m.note);
                    if m.ok { em.ok(txt) } else { em.warn(txt) }
                    report.retain(|x| x.name != m.name);
                    report.push(m);
                }
                Some(AgentEvent::Progress(v)) => forward_progress(em, &v),
                Some(AgentEvent::Log { level, message }) => log(em, &level, message),
                Some(AgentEvent::Stage(s)) => {
                    live.on_stage(&s);
                    stage(em, &format!("agent-{s}"), "");
                    if !until.is_empty() && s == until {
                        return Ok(());
                    }
                }
                Some(AgentEvent::Done(_)) => return Ok(()),
                Some(AgentEvent::Error(e)) => return Err(err(e)),
                Some(_) => {}
                None => return Err(err("bridge closed")),
            },
            _ = tick.tick() => live.check()?,
            _ = cancel.cancelled() => return Err(AdbError::Cancelled),
        }
    }
}

#[derive(Default, Clone)]
struct PcImport {
    files: SubResult,
    apk_installed: u64,
    apk_failed: u64,
    /// APK wysłane do Download/SmartEI_APKs - instaluje je agent (instalacja przez USB zablokowana).
    agent_install: Vec<Value>,
    warnings: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
async fn pc_import(
    env: &Env,
    serial: Option<&str>,
    dir: &Path,
    m: &Modules,
    settings: &PortableSettings,
    files: &[&FileEntry],
    sideload: &[&AppRecord],
    em: &Emitter,
    cancel: &CancelToken,
) -> PcImport {
    let mut out = PcImport::default();
    let (Some(adb), Some(serial)) = (&env.adb, serial) else { return out };

    // ustawienia
    if m.settings && !settings.is_empty() {
        let mut tmax = BTreeMap::new();
        for (name, stream) in STREAMS {
            if let Some(v) = parse_volume(&sh(adb, serial, &format!("cmd media_session volume --stream {stream} --get"), 10).await) {
                tmax.insert(name.to_string(), v.max);
            }
        }
        let mut applied = 0;
        for c in settings.adb_commands(&tmax) {
            if adb.shell(Some(serial), &c, 15).await.map(|o| o.ok()).unwrap_or(false) {
                applied += 1;
            }
        }
        em.ok(format!("Ustawienia przywrócone: {applied} / settings applied."));
    }

    // pliki: push katalogów najwyższego poziomu z files/ (zachowane ścieżki względem /sdcard)
    if !files.is_empty() {
        stage(em, "files", "");
        let mut roots: Vec<(PathBuf, String)> = Vec::new();
        for f in files {
            // katalog kategorii (np. DCIM, Android/media/com.whatsapp/WhatsApp/Media) albo plik w Download/
            let rel = FILE_CATEGORIES
                .iter()
                .flat_map(|(_, p)| p.iter())
                .find(|p| f.target.starts_with(&format!("{p}/")))
                .map(|p| p.to_string())
                .unwrap_or_else(|| f.target.split('/').next().unwrap_or("").to_string());
            if rel.is_empty() {
                continue;
            }
            let local = dir.join("files").join(&rel);
            if !roots.iter().any(|(l, _)| *l == local) {
                let parent = rel.rsplit_once('/').map(|(p, _)| format!("{SDCARD}/{p}")).unwrap_or_else(|| SDCARD.into());
                roots.push((local, parent));
            }
        }
        let (sub, res) = sub_emitter(em, "files");
        for (local, remote_parent) in roots {
            if cancel.is_cancelled() {
                break;
            }
            em.info(format!("Wysyłam / pushing {} -> {remote_parent}", local.display()));
            let _ = adb.shell(Some(serial), &format!("mkdir -p {}", shell_quote(&remote_parent)), 15).await;
            let opts = PushOptions {
                items: vec![local.to_string_lossy().into()],
                remote_dest: remote_parent,
                workers: 4,
                timeout_seconds: 900,
                skip_existing: true,
                exclude_junk: true,
            };
            run_push(adb.clone(), serial.to_string(), opts, sub.clone(), cancel.clone()).await;
        }
        out.files = take(&res);
        if out.files.failures > 0 {
            out.warnings.push(format!("Pliki: {} nie udało się wysłać / files failed", out.files.failures));
        }
    }

    // APK spoza Sklepu Play: adb install / install-multiple, plan B: push + instalacja przez agenta
    if !sideload.is_empty() {
        stage(em, "apks", "");
        let total = sideload.len() as u64;
        for (i, a) in sideload.iter().enumerate() {
            if cancel.is_cancelled() {
                break;
            }
            em.send(TransferEvent::Progress {
                done: i as u64,
                total,
                bytes_done: 0,
                bytes_total: 0,
                current: Some(format!("APK: {}", a.label)),
            });
            let local: Vec<String> = a
                .apk_files
                .iter()
                .map(|f| dir.join("apks").join(&a.package).join(&f.name))
                .filter(|p| p.is_file())
                .map(|p| p.to_string_lossy().to_string())
                .collect();
            if local.is_empty() {
                continue;
            }
            let mut args: Vec<&str> = vec![if local.len() > 1 { "install-multiple" } else { "install" }, "-r"];
            args.extend(local.iter().map(String::as_str));
            let r = adb.run_c(Some(serial), &args, 600, cancel).await;
            let ok = matches!(&r, Ok(o) if o.stdout.contains("Success"));
            if ok {
                out.apk_installed += 1;
                em.ok(format!("Zainstalowano / installed: {}", a.label));
                continue;
            }
            let why = match &r {
                Ok(o) => crate::devtools::runner::explain_install(&format!("{}\n{}", o.stdout, o.stderr), false),
                Err(e) => e.to_string(),
            };
            em.warn(format!("{}: instalacja przez USB nieudana ({why}) - plan B: agent / install via agent", a.label));
            // plan B
            let remote_dir = format!("{APK_FALLBACK_DIR}/{}", a.package);
            let _ = adb.shell(Some(serial), &format!("mkdir -p {}", shell_quote(&remote_dir)), 15).await;
            let mut remote_files = Vec::new();
            let mut pushed = true;
            for f in &a.apk_files {
                let lp = dir.join("apks").join(&a.package).join(&f.name);
                let rp = format!("{remote_dir}/{}", f.name);
                let o = adb.run_c(Some(serial), &["push", &lp.to_string_lossy(), &rp], 600, cancel).await;
                if !matches!(o, Ok(ref o) if o.ok()) {
                    pushed = false;
                    break;
                }
                remote_files.push(json!({ "path": rp, "size": f.size }));
            }
            if pushed {
                out.agent_install.push(json!({ "package": a.package, "label": a.label, "files": remote_files }));
            } else {
                out.apk_failed += 1;
            }
        }
        em.send(TransferEvent::Progress { done: total, total, bytes_done: 0, bytes_total: 0, current: None });
    }
    out
}

/// Wybudza ekran, zdejmuje blokadę (jeśli bez zabezpieczeń) i wyciąga Smart EI agenta na pierwszy plan.
async fn bring_agent_to_front(adb: &Adb, serial: &str) {
    for c in ["input keyevent KEYCODE_WAKEUP", "wm dismiss-keyguard"] {
        let _ = adb.shell(Some(serial), c, 10).await;
    }
    let _ = adb.shell(Some(serial), &format!("am start -n {AGENT_PKG}/{AGENT_ACTIVITY}"), 15).await;
}

async fn run_queue(
    sess: &mut Session,
    env: &Env,
    serial: &Option<String>,
    installed_before: &HashSet<String>,
    em: &Emitter,
    cancel: &CancelToken,
) -> Result<()> {
    let shared = sess.bridge.shared.clone();
    let adb_ctx = match (&env.adb, serial) {
        (Some(a), Some(s)) => Some((a.clone(), s.clone())),
        _ => None,
    };
    // pozycje zainstalowane w międzyczasie (np. przez przywracanie innej aplikacji)
    {
        let mut g = shared.lock();
        if let Some(q) = g.queue.as_mut() {
            let pk: Vec<String> = q.items.iter().map(|i| i.package.clone()).collect();
            for p in pk.iter().filter(|p| installed_before.contains(*p)) {
                q.mark_installed(p);
            }
        }
    }
    let mut last_rev = u64::MAX;
    loop {
        // otwarcie bieżącej pozycji (ADB robi to komputer, w Wi-Fi - agent)
        let (to_open, snapshot, finished, current) = {
            let mut g = shared.lock();
            let q = g.queue.as_mut().ok_or_else(|| err("queue"))?;
            let to_open = if adb_ctx.is_some() { q.take_to_open() } else { None };
            (to_open, queue_json(Some(q)), q.finished(), q.current().map(|c| c.package.clone()))
        };
        let rev = snapshot["rev"].as_u64().unwrap_or(0);
        if rev != last_rev {
            last_rev = rev;
            em.item("queue", true, snapshot.to_string());
            em.send(TransferEvent::Progress {
                done: snapshot["position"].as_u64().unwrap_or(0).saturating_sub(u64::from(!finished)),
                total: snapshot["total"].as_u64().unwrap_or(0),
                bytes_done: 0,
                bytes_total: 0,
                current: snapshot["current"]["label"].as_str().map(String::from),
            });
        }
        if finished {
            return Ok(());
        }
        if let (Some(pkg), Some((adb, s))) = (&to_open, &adb_ctx) {
            let url = format!("market://details?id={pkg}");
            let o = adb.shell(Some(s), &format!("am start -a android.intent.action.VIEW -d {}", shell_quote(&url)), 15).await;
            let failed = match &o {
                Ok(o) => o.stdout.contains("Error") || o.stderr.contains("Error"),
                Err(_) => true,
            };
            if failed {
                let web = format!("https://play.google.com/store/apps/details?id={pkg}");
                let _ = adb.shell(Some(s), &format!("am start -a android.intent.action.VIEW -d {}", shell_quote(&web)), 15).await;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(QUEUE_POLL) => {
                if let (Some(pkg), Some((adb, s))) = (&current, &adb_ctx) {
                    if is_valid_package(pkg) && sh(adb, s, &format!("pm path {pkg}"), 10).await.contains("package:") {
                        if let Some(q) = shared.lock().queue.as_mut() {
                            q.mark_installed(pkg);
                        }
                        em.ok(format!("Zainstalowano ze Sklepu Play / installed: {pkg}"));
                    }
                }
            }
            c = sess.ctl.recv() => {
                if let Some(ControlMsg::Queue(a)) = c {
                    if let Some(q) = shared.lock().queue.as_mut() {
                        q.act(a);
                    }
                }
            }
            ev = sess.events.recv() => match ev {
                Some(AgentEvent::Log { level, message }) => log(em, &level, message),
                Some(AgentEvent::Queue) | Some(_) => {}
                None => return Err(err("bridge closed")),
            },
            _ = cancel.cancelled() => return Err(AdbError::Cancelled),
        }
    }
}

/// Akcja przycisków w GUI (Dalej / Pomiń).
pub fn parse_action(a: &str) -> Option<Action> {
    match a {
        "next" => Some(Action::Next),
        "skip" => Some(Action::Skip),
        _ => None,
    }
}

/// Podgląd folderu kopii (krok 1 importu).
pub fn inspect(dir: &Path) -> Result<Value> {
    let m = Manifest::load(dir)?;
    let apps = std::fs::read(dir.join("apps.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<AppsFile>(&b).ok())
        .map(|f| f.apps.len())
        .unwrap_or(0);
    Ok(json!({ "manifest": m, "apps": apps }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_index_categories() {
        let dir = std::env::temp_dir().join(format!("smartei-flow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for p in [
            "files/DCIM/Camera/a.jpg",
            "files/Android/media/com.whatsapp/WhatsApp/Media/WhatsApp Images/b.jpg",
            "files/Download/doc.pdf",
            "files/Music/x.mp3",
        ] {
            let f = dir.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, b"123").unwrap();
        }
        std::fs::write(dir.join("files/DCIM/Camera/c.jpg.part"), b"1").unwrap();
        let idx = index_files(&dir);
        let cats: Vec<(&str, &str)> = idx.iter().map(|f| (f.target.as_str(), f.cat.as_str())).collect();
        assert_eq!(
            cats,
            vec![
                ("Android/media/com.whatsapp/WhatsApp/Media/WhatsApp Images/b.jpg", "whatsapp"),
                ("DCIM/Camera/a.jpg", "dcim"),
                ("Download/doc.pdf", "download"),
                ("Music/x.mp3", "music"),
            ]
        );
        assert_eq!(idx[1].rel, "files/DCIM/Camera/a.jpg");
        assert_eq!(dir_size(&dir.join("files")), (4, 12));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------------------------ //
    // E2E: przebieg Wi-Fi z symulowanym agentem (HTTP jak w SmartEiClient)
    // ------------------------------------------------------------------ //

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn http(addr: std::net::SocketAddr, method: &str, path: &str, token: &str, body: &[u8]) -> (u16, Vec<u8>) {
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: x\r\nX-SmartEI-Token: {token}\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        s.write_all(head.as_bytes()).await.unwrap();
        s.write_all(body).await.unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).await.unwrap();
        let code: u16 = String::from_utf8_lossy(&out[9..12]).parse().unwrap();
        let pos = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        (code, out[pos..].to_vec())
    }

    async fn jget(addr: std::net::SocketAddr, path: &str, t: &str) -> Value {
        serde_json::from_slice(&http(addr, "GET", path, t, b"").await.1).unwrap_or(Value::Null)
    }

    async fn jpost(addr: std::net::SocketAddr, path: &str, t: &str, v: Value) -> Value {
        serde_json::from_slice(&http(addr, "POST", path, t, v.to_string().as_bytes()).await.1).unwrap_or(Value::Null)
    }

    struct Harness {
        addr: std::net::SocketAddr,
        tx: mpsc::UnboundedSender<ControlMsg>,
        events: Arc<StdMutex<Vec<TransferEvent>>>,
        job: tokio::task::JoinHandle<()>,
    }

    async fn harness(role: Role, base: PathBuf, modules: Modules, manifest: Option<Manifest>) -> Harness {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (bridge, events) = Bridge::with_listener(l, role, false, base.clone(), None, "246810".into());
        let addr = bridge.addr;
        let (tx, ctl) = mpsc::unbounded_channel();
        let session = Session { role, serial: None, base, resumed: false, modules, manifest, bridge, events, ctl };
        let info = SessionInfo {
            session_id: "sei-test".into(),
            job_id: String::new(),
            role: role.as_str().into(),
            connection: "wifi".into(),
            dir: String::new(),
            ip: "127.0.0.1".into(),
            port: addr.port(),
            pin: "246810".into(),
            pair_url: String::new(),
            pair_qr: String::new(),
            apk_url: String::new(),
            apk_qr: String::new(),
            agent_apk: String::new(),
        };
        let log = Arc::new(StdMutex::new(Vec::new()));
        let l2 = log.clone();
        let em = Emitter::callback(move |e| l2.lock().unwrap().push(e));
        let env = Env { adb: None, agent: None, root: None, sessions: Arc::new(tokio::sync::Mutex::new(HashMap::new())) };
        let job = tokio::spawn(run(Prepared { info, session }, env, em, CancelToken::new()));
        Harness { addr, tx, events: log, job }
    }

    fn item(events: &Arc<StdMutex<Vec<TransferEvent>>>, id: &str) -> Option<Value> {
        events.lock().unwrap().iter().rev().find_map(|e| match e {
            TransferEvent::Item { id: i, message, .. } if i == id => serde_json::from_str(message).ok(),
            _ => None,
        })
    }

    async fn wait_item(events: &Arc<StdMutex<Vec<TransferEvent>>>, id: &str) -> Value {
        for _ in 0..200 {
            if let Some(v) = item(events, id) {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("brak zdarzenia {id}: {:?}", events.lock().unwrap());
    }

    fn device(manufacturer: &str, model: &str) -> Value {
        json!({ "manufacturer": manufacturer, "brand": manufacturer, "model": model, "device": "x", "sdk": 34, "androidVersion": "14" })
    }

    #[tokio::test]
    async fn e2e_wifi_export() {
        let base = std::env::temp_dir().join(format!("smartei-e2e-exp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let modules = Modules { files: vec!["dcim".into()], contacts: true, apps: true, apks: true, settings: true, ..Default::default() };
        let h = harness(Role::Export, base.clone(), modules.clone(), None).await;
        let a = h.addr;

        // agent: parowanie + hello
        let r = jpost(a, "/pair", "", json!({ "pin": "246810" })).await;
        let t = r["token"].as_str().unwrap().to_string();
        jpost(a, "/hello", &t, json!({ "device": device("Xiaomi", "Redmi Note 12"), "freeBytes": 1u64 << 34, "sizes": { "dcim": 7 } })).await;
        let review = wait_item(&h.events, "review").await;
        assert_eq!(review["estimate"], 7);
        assert_eq!(jget(a, "/job", &t).await["state"], "wait");
        // przed "Rozpocznij" nic się nie zapisze
        assert_eq!(http(a, "PUT", "/export/file?path=files/x.jpg&size=1", &t, b"x").await.0, 400);

        // GUI: Rozpocznij
        h.tx.send(ControlMsg::Proceed(Selection { modules: modules.clone(), packages: vec![] })).unwrap();
        let mut job = Value::Null;
        for _ in 0..100 {
            job = jget(a, "/job", &t).await;
            if job["state"] == "go" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(job["modules"]["files"][0]["paths"][0], "DCIM");
        assert_eq!(job["modules"]["apks"], true);

        // agent wysyła dane
        let vcf = b"BEGIN:VCARD\r\nFN:Jan\r\nEND:VCARD\r\n";
        assert_eq!(http(a, "PUT", &format!("/export/file?path=contacts_sms/contacts.vcf&size={}", vcf.len()), &t, vcf).await.0, 200);
        jpost(a, "/export/module", &t, json!({ "name": "contacts", "ok": true, "items": 1 })).await;
        assert_eq!(http(a, "PUT", "/export/file?path=files/DCIM/Camera/a.jpg&size=7", &t, b"JPEGJPG").await.0, 200);
        let resp = jpost(a, "/export/apps", &t, json!({ "apps": [
            { "package": "com.whatsapp", "label": "WhatsApp", "installer": "com.android.vending", "versionName": "2.24" },
            { "package": "org.fdroid.fdroid", "label": "F-Droid", "installer": null },
            { "package": "com.miui.notes", "label": "Notes", "installer": null },
        ] })).await;
        assert_eq!(resp["apks"], json!(["org.fdroid.fdroid", "com.miui.notes"]));
        assert_eq!(http(a, "PUT", "/export/file?path=apks/org.fdroid.fdroid/base.apk&size=4", &t, b"PK\x03\x04").await.0, 200);
        jpost(a, "/export/settings", &t, json!({ "system": { "font_scale": 1.1, "adb_enabled": 1 }, "uiNightMode": 2 })).await;
        jpost(a, "/done", &t, json!({ "notes": [] })).await;
        tokio::time::timeout(Duration::from_secs(30), h.job).await.unwrap().unwrap();

        // wynik: folder SmartEI_* z kompletnym manifestem
        let dir = std::fs::read_dir(&base).unwrap().flatten().map(|e| e.path()).find(|p| p.is_dir()).unwrap();
        assert!(dir.file_name().unwrap().to_string_lossy().starts_with("SmartEI_Redmi_Note_12_"));
        let m = Manifest::load(&dir).unwrap();
        assert!(m.complete);
        assert_eq!(m.connection, "wifi");
        assert_eq!(m.source.manufacturer, "Xiaomi");
        assert_eq!(m.module("contacts").unwrap().items, 1);
        assert_eq!(m.module("files").unwrap().items, 1);
        assert_eq!(m.module("apps").unwrap().items, 3);
        assert!(m.checksums.iter().any(|c| c.path == "apks/org.fdroid.fdroid/base.apk"));
        assert!(m.verify(&dir).is_empty());
        let apps: AppsFile = serde_json::from_slice(&std::fs::read(dir.join("apps.json")).unwrap()).unwrap();
        let fd = apps.apps.iter().find(|x| x.package == "org.fdroid.fdroid").unwrap();
        assert_eq!(fd.apk_files, vec![ApkFile { name: "base.apk".into(), size: 4 }]);
        assert_eq!(apps.apps.iter().find(|x| x.package == "com.whatsapp").unwrap().source, AppSource::PlayStore);
        let st: Value = serde_json::from_slice(&std::fs::read(dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!(st["system"]["font_scale"], "1.1");
        assert!(st["system"].get("adb_enabled").is_none());
        assert_eq!(st["uiNightMode"], "yes");
        let fin = h.events.lock().unwrap().iter().any(|e| matches!(e, TransferEvent::Finished { ok: 1, cancelled: false, .. }));
        assert!(fin);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn e2e_wifi_import_with_vendor_filter_and_queue() {
        // kopia z Xiaomi
        let dir = std::env::temp_dir().join(format!("smartei-e2e-imp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (p, b) in [
            ("contacts_sms/contacts.vcf", &b"VCARD"[..]),
            ("files/DCIM/Camera/a.jpg", &b"JPEGJPG"[..]),
            ("files/Music/m.mp3", &b"MP3"[..]),
            ("apks/org.fdroid.fdroid/base.apk", &b"PK\x03\x04"[..]),
            ("apks/com.miui.notes/base.apk", &b"PK\x03\x04"[..]),
        ] {
            let f = dir.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, b).unwrap();
        }
        let rec = |pkg: &str, src: AppSource, apk: bool| AppRecord {
            package: pkg.into(),
            label: pkg.into(),
            version_name: String::new(),
            version_code: 0,
            size: 4,
            installer: None,
            source: src,
            source_label: src.label().into(),
            updated_system: false,
            device_apks: vec![],
            apk_files: if apk { vec![ApkFile { name: "base.apk".into(), size: 4 }] } else { vec![] },
        };
        let apps = AppsFile {
            format_version: 1,
            apps: vec![
                rec("com.whatsapp", AppSource::PlayStore, false),
                rec("com.spotify.music", AppSource::PlayStore, false),
                rec("com.instagram.android", AppSource::PlayStore, false),
                rec("org.fdroid.fdroid", AppSource::Sideload, true),
                rec("com.miui.notes", AppSource::Sideload, true),
            ],
        };
        std::fs::write(dir.join("apps.json"), serde_json::to_vec(&apps).unwrap()).unwrap();
        std::fs::write(dir.join("settings.json"), br#"{"system":{"time_12_24":"24"}}"#).unwrap();
        let mut man = Manifest::new(serde_json::from_value(device("Xiaomi", "Redmi Note 12")).unwrap(), "adb");
        man.compute_checksums(&dir).unwrap();
        man.complete = true;
        man.save(&dir).unwrap();

        let h = harness(Role::Import, dir.clone(), Modules::default(), Some(Manifest::load(&dir).unwrap())).await;
        let a = h.addr;
        let t = jpost(a, "/pair", "", json!({ "pin": "246810" })).await["token"].as_str().unwrap().to_string();
        jpost(a, "/hello", &t, json!({ "device": device("samsung", "SM-S921B"), "installed": ["com.spotify.music"], "freeBytes": 1u64 << 34 })).await;
        let review = wait_item(&h.events, "review").await;
        assert_eq!(review["crossVendor"], true);
        let list = review["apps"].as_array().unwrap();
        let by = |p: &str| list.iter().find(|x| x["package"] == p).unwrap().clone();
        assert_eq!(by("com.miui.notes")["reason"], "sourceVendor");
        assert_eq!(by("com.miui.notes")["selected"], false);
        assert_eq!(by("com.spotify.music")["reason"], "alreadyInstalled");
        assert_eq!(by("org.fdroid.fdroid")["selected"], true);
        assert_eq!(review["available"]["contacts"], true);

        // GUI: domyślne zaznaczenia (bez Music)
        let packages: Vec<String> =
            list.iter().filter(|x| x["selected"] == true).map(|x| x["package"].as_str().unwrap().to_string()).collect();
        let modules = Modules { files: vec!["dcim".into()], contacts: true, settings: true, apps: true, ..Default::default() };
        h.tx.send(ControlMsg::Proceed(Selection { modules, packages })).unwrap();
        let mut plan = Value::Null;
        for _ in 0..100 {
            let (c, b) = http(a, "GET", "/import/plan", &t, b"").await;
            if c == 200 {
                plan = serde_json::from_slice(&b).unwrap();
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(plan["download"], true);
        assert_eq!(plan["contacts"], "contacts_sms/contacts.vcf");
        assert_eq!(plan["files"].as_array().unwrap().len(), 1, "tylko DCIM");
        assert_eq!(plan["files"][0]["target"], "DCIM/Camera/a.jpg");
        assert_eq!(plan["apks"][0]["package"], "org.fdroid.fdroid");
        assert_eq!(plan["apks"].as_array().unwrap().len(), 1, "aplikacja MIUI pominięta");
        assert_eq!(plan["settings"]["system"]["time_12_24"], "24");
        let play: Vec<&str> = plan["play"].as_array().unwrap().iter().map(|p| p["package"].as_str().unwrap()).collect();
        assert_eq!(play, vec!["com.whatsapp", "com.instagram.android"]);

        // agent pobiera tylko pliki z planu
        let (c, b) = http(a, "GET", "/import/file?path=files/DCIM/Camera/a.jpg", &t, b"").await;
        assert_eq!((c, b.as_slice()), (200, b"JPEGJPG".as_slice()));
        assert_eq!(http(a, "GET", "/import/file?path=files/Music/m.mp3", &t, b"").await.0, 403);
        assert_eq!(http(a, "GET", "/import/file?path=apks/com.miui.notes/base.apk", &t, b"").await.0, 403);
        jpost(a, "/export/module", &t, json!({ "name": "contacts", "ok": true, "items": 1, "note": "intent" })).await;
        jpost(a, "/stage", &t, json!({ "stage": "queue" })).await;

        // kolejka Sklepu Play: 1 / 2 -> zainstalowano, 2 / 2 -> Pomiń z GUI
        let mut q = Value::Null;
        for _ in 0..100 {
            q = jget(a, "/queue", &t).await;
            if q["active"] == true {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!((q["position"].clone(), q["total"].clone()), (json!(1), json!(2)));
        let q = jpost(a, "/queue/installed", &t, json!({ "package": "com.whatsapp" })).await;
        assert_eq!(q["current"]["package"], "com.instagram.android");
        h.tx.send(ControlMsg::Queue(Action::Skip)).unwrap();
        let mut job = Value::Null;
        for _ in 0..200 {
            job = jget(a, "/job", &t).await;
            if job["state"] == "finish" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(job["state"], "finish");
        assert_eq!(job["queue"]["installed"], 1);
        assert_eq!(job["queue"]["skipped"], 1);
        jpost(a, "/done", &t, json!({})).await;
        tokio::time::timeout(Duration::from_secs(30), h.job).await.unwrap().unwrap();
        let res = item(&h.events, "result").unwrap();
        assert_eq!(res["role"], "import");
        assert_eq!(res["queue"]["total"], 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn liveness_detects_dead_agent() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (b, _rx) = Bridge::with_listener(l, Role::Import, true, std::env::temp_dir(), None, "111111".into());
        let mut live = Liveness::new(&b.shared);
        assert!(live.check().is_ok(), "przed pierwszym kontaktem nie ma ciszy");
        b.shared.lock().last_seen = Instant::now().checked_sub(Duration::from_secs(45));
        assert!(live.check().is_err(), "45 s ciszy poza krokiem interaktywnym = błąd");
        live.on_stage("interactive");
        assert!(live.check().is_ok(), "okna systemu: limit 120 s");
        b.shared.lock().last_seen = Instant::now().checked_sub(Duration::from_secs(130));
        assert!(live.check().is_err());
        live.on_stage("interactive_end");
        b.shared.lock().last_seen = Some(Instant::now());
        assert!(live.check().is_ok());
    }

    #[test]
    fn junk_not_indexed() {
        let dir = std::env::temp_dir().join(format!("smartei-junk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for p in ["files/DCIM/a.jpg", "files/DCIM/.thumbnails/1.jpg", "files/Download/.adb_pull_progress.jsonl", "files/Download/doc.pdf"] {
            let f = dir.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, b"x").unwrap();
        }
        let t: Vec<String> = index_files(&dir).into_iter().map(|f| f.target).collect();
        assert_eq!(t, vec!["DCIM/a.jpg", "Download/doc.pdf"]);
        assert_eq!(dir_size(&dir.join("files")).0, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crash_detection() {
        let log = "--------- beginning of crash\n\
            11-24 16:05:38.362  4363  4363 E AndroidRuntime: FATAL EXCEPTION: main\n\
            11-24 16:05:38.362  4363  4363 E AndroidRuntime: Process: com.drapco.agent, PID: 4363\n\
            11-24 16:05:38.362  4363  4363 E AndroidRuntime: java.lang.RuntimeException: Unable to instantiate activity ComponentInfo{com.drapco.agent/com.drapco.agent.SmartEiActivity}\n\
            11-24 16:05:38.362  4363  4363 E AndroidRuntime:        at android.app.ActivityThread.performLaunchActivity(ActivityThread.java:4482)\n\
            11-24 16:05:38.362  4363  4363 E AndroidRuntime: Caused by: java.lang.ClassNotFoundException: Didn't find class \"com.drapco.agent.SmartEiActivity\"\n";
        let c = crash_summary(log).unwrap();
        assert!(c.contains("Unable to instantiate activity"), "{c}");
        assert!(c.contains("ClassNotFoundException"), "{c}");
        assert!(!c.contains(" at "));
        // crash innej aplikacji nie przerywa
        assert!(crash_summary("E AndroidRuntime: FATAL EXCEPTION: main\nE AndroidRuntime: Process: com.other.app, PID: 1").is_none());
        assert!(crash_summary("").is_none());
    }

    #[test]
    fn queue_actions() {
        assert_eq!(parse_action("next"), Some(Action::Next));
        assert_eq!(parse_action("skip"), Some(Action::Skip));
        assert_eq!(parse_action("x"), None);
    }
}
