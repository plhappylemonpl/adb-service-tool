//! Uruchamianie narzędzi Java (apktool, uber-apk-signer) w osobnych,
//! odizolowanych procesach: własny katalog roboczy i `java.io.tmpdir` w
//! przestrzeni roboczej, limit pamięci, timeout, natychmiastowe zabicie przy
//! anulowaniu. Wyjście jest strumieniowane do loga (z ograniczeniem zalewu
//! ostrzeżeń), a ogon zachowany do czytelnego opisu błędu.
//!
//! Tu też `Stepper` - płynny, wieloetapowy pasek postępu całego łańcucha.

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::adb::events::{Emitter, TransferEvent};
use crate::adb::manager::hide_console;
use crate::error::{AdbError, Result};
use crate::utils::CancelToken;

use super::toolchain::Toolchain;

// --------------------------------------------------------------------------- //
// Pasek postępu etapów
// --------------------------------------------------------------------------- //

/// Etap łańcucha: klucz (tłumaczony w GUI) + waga w pasku.
#[derive(Debug, Clone, Copy)]
pub struct Step {
    pub key: &'static str,
    pub weight: u32,
}

pub const fn step(key: &'static str, weight: u32) -> Step {
    Step { key, weight }
}

struct StepState {
    steps: Vec<Step>,
    idx: usize,
    sub: f64,
    cap: f64,
    detail: String,
}

/// Postęp ważony etapami. Ticker co 200 ms "dociąga" pasek do kolejnego
/// kamienia milowego (zgłaszanego z wyjścia narzędzia), więc pasek płynie
/// nawet gdy apktool długo milczy. Zdarzenie `Progress`:
/// done/total = promile, current = "klucz_etapu|szczegół".
pub struct Stepper {
    st: Mutex<StepState>,
    emitter: Emitter,
    stop: CancellationToken,
}

impl Stepper {
    pub fn new(steps: Vec<Step>, emitter: Emitter) -> Arc<Self> {
        Arc::new(Self {
            st: Mutex::new(StepState { steps, idx: 0, sub: 0.0, cap: 0.0, detail: String::new() }),
            emitter,
            stop: CancellationToken::new(),
        })
    }

    pub fn start(self: &Arc<Self>) -> JoinHandle<()> {
        let me = self.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(200));
            loop {
                tokio::select! {
                    _ = me.stop.cancelled() => break,
                    _ = iv.tick() => {
                        if let Ok(mut s) = me.st.lock() {
                            // asymptotyczne zbliżanie do limitu bieżącej fazy
                            if s.sub < s.cap {
                                s.sub += (s.cap - s.sub) * 0.035;
                            }
                        }
                        me.emit();
                    }
                }
            }
        })
    }

    pub async fn shutdown(&self, h: JoinHandle<()>) {
        self.stop.cancel();
        let _ = h.await;
        self.emit();
    }

    fn emit(&self) {
        let Ok(s) = self.st.lock() else { return };
        let total: u32 = s.steps.iter().map(|x| x.weight).sum::<u32>().max(1);
        let before: u32 = s.steps.iter().take(s.idx).map(|x| x.weight).sum();
        let cur = s.steps.get(s.idx).map(|x| x.weight).unwrap_or(0) as f64;
        let permille = (((before as f64 + cur * s.sub.clamp(0.0, 1.0)) / total as f64) * 1000.0) as u64;
        let key = s.steps.get(s.idx).map(|x| x.key).unwrap_or("done");
        self.emitter.send(TransferEvent::Progress {
            done: permille.min(1000),
            total: 1000,
            bytes_done: 0,
            bytes_total: 0,
            current: Some(format!("{key}|{}", s.detail)),
        });
    }

    /// Przechodzi do etapu `key` (etapy pominięte są liczone jako zrobione).
    pub fn begin(&self, key: &str, detail: impl Into<String>) {
        if let Ok(mut s) = self.st.lock() {
            if let Some(i) = s.steps.iter().position(|x| x.key == key) {
                s.idx = i;
            }
            s.sub = 0.0;
            s.cap = 0.3;
            s.detail = detail.into();
        }
        self.emit();
    }

    /// Kamień milowy w bieżącym etapie (0..1); `next` = do ilu wolno "dopłynąć".
    pub fn milestone(&self, at: f64, next: f64) {
        if let Ok(mut s) = self.st.lock() {
            s.sub = s.sub.max(at);
            s.cap = s.cap.max(next).max(s.sub);
        }
    }

    /// Dokładny postęp (np. bajty pobrane / rozmiar).
    pub fn exact(&self, frac: f64, detail: Option<String>) {
        if let Ok(mut s) = self.st.lock() {
            s.sub = frac.clamp(0.0, 1.0);
            s.cap = s.sub;
            if let Some(d) = detail {
                s.detail = d;
            }
        }
    }

    pub fn finish_step(&self) {
        if let Ok(mut s) = self.st.lock() {
            s.sub = 1.0;
            s.cap = 1.0;
        }
        self.emit();
    }

    pub fn complete(&self) {
        if let Ok(mut s) = self.st.lock() {
            s.idx = s.steps.len();
            s.sub = 0.0;
            s.cap = 0.0;
            s.detail.clear();
        }
        self.emit();
    }

    pub fn current_key(&self) -> String {
        self.st
            .lock()
            .ok()
            .and_then(|s| s.steps.get(s.idx).map(|x| x.key.to_string()))
            .unwrap_or_default()
    }
}

// --------------------------------------------------------------------------- //
// Proces Java
// --------------------------------------------------------------------------- //

/// Kamienie milowe apktool (fragment linii -> (osiągnięty, następny limit)).
const MILESTONES: &[(&str, f64, f64)] = &[
    // dekompilacja
    ("Loading resource table...", 0.05, 0.35),
    ("Decoding file-resources", 0.35, 0.55),
    ("Decoding values", 0.55, 0.75),
    ("Decoding AndroidManifest.xml", 0.75, 0.85),
    ("Copying raw classes", 0.85, 0.9),
    ("Copying assets and libs", 0.9, 0.95),
    ("Copying original files", 0.95, 0.98),
    // budowanie
    ("Checking whether resources", 0.05, 0.15),
    ("Building resources with aapt2", 0.15, 0.7),
    ("Copying raw", 0.7, 0.8),
    ("Copying libs", 0.8, 0.9),
    ("Building apk file", 0.9, 0.97),
    ("Importing", 0.92, 0.98),
    // uber-apk-signer
    ("zipalign", 0.2, 0.5),
    ("sign", 0.5, 0.85),
    ("verify", 0.85, 0.98),
];

pub struct ToolRun {
    pub status: i32,
    /// ostatnie linie wyjścia (do opisu błędu)
    pub tail: Vec<String>,
}

impl ToolRun {
    pub fn ok(&self) -> bool {
        self.status == 0
    }
    pub fn tail_text(&self) -> String {
        self.tail.join("\n")
    }
}

const TAIL: usize = 80;
const MAX_WARN_LINES: usize = 25;

/// Uruchamia `java -jar <jar> args...` w osobnym procesie.
#[allow(clippy::too_many_arguments)]
pub async fn run_jar(
    tc: &Toolchain,
    jar: &Path,
    args: Vec<OsString>,
    work_dir: &Path,
    emitter: &Emitter,
    stepper: Option<&Stepper>,
    cancel: &CancelToken,
    timeout_s: u64,
) -> Result<ToolRun> {
    let tmp = work_dir.join("tmp");
    tokio::fs::create_dir_all(&tmp).await?;
    let mut cmd = Command::new(&tc.java);
    hide_console(&mut cmd);
    cmd.arg("-Xmx2048m")
        .arg("-Djava.awt.headless=true")
        .arg("-Duser.language=en")
        .arg("-Duser.country=US")
        .arg("-Dfile.encoding=UTF-8")
        .arg(format!("-Djava.io.tmpdir={}", tmp.display()))
        .arg("-jar")
        .arg(jar)
        .args(args)
        .current_dir(work_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| AdbError::BinaryNotFound(format!("java ({}): {e}", tc.java.display())))?;

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut l = BufReader::new(out).lines();
            while let Ok(Some(line)) = l.next_line().await {
                let _ = tx.send(line);
            }
        });
    }
    if let Some(err) = child.stderr.take() {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut l = BufReader::new(err).lines();
            while let Ok(Some(line)) = l.next_line().await {
                let _ = tx.send(line);
            }
        });
    }
    drop(tx);

    let mut tail: Vec<String> = Vec::new();
    let mut warns = 0usize;
    let deadline = tokio::time::sleep(Duration::from_secs(timeout_s.max(10)));
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            line = rx.recv() => {
                let Some(line) = line else { break };
                let t = line.trim_end().to_string();
                if t.is_empty() || t.starts_with("Picked up JAVA_TOOL_OPTIONS") || t.starts_with("Picked up _JAVA_OPTIONS") {
                    continue;
                }
                if let Some(s) = stepper {
                    let low = t.to_lowercase();
                    for (needle, at, next) in MILESTONES {
                        if t.contains(needle) || (needle.chars().all(|c| c.is_lowercase()) && low.contains(needle)) {
                            s.milestone(*at, *next);
                        }
                    }
                }
                if t.starts_with("W:") || t.contains("warning:") {
                    warns += 1;
                    if warns <= MAX_WARN_LINES {
                        emitter.warn(t.clone());
                    } else if warns == MAX_WARN_LINES + 1 {
                        emitter.warn("... (kolejne ostrzeżenia pominięte w logu / further warnings suppressed)");
                    }
                } else if t.starts_with("E:") || t.starts_with("S:") || t.contains("Exception") || t.contains("error:") {
                    emitter.error(t.clone());
                } else {
                    emitter.info(t.clone());
                }
                tail.push(t);
                if tail.len() > TAIL {
                    tail.remove(0);
                }
            }
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                return Err(AdbError::Cancelled);
            }
            _ = &mut deadline => {
                let _ = child.kill().await;
                return Err(AdbError::Timeout(timeout_s));
            }
        }
    }
    let status = tokio::select! {
        s = child.wait() => s.map_err(|e| AdbError::Io(e.to_string()))?,
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            return Err(AdbError::Cancelled);
        }
    };
    if warns > MAX_WARN_LINES {
        emitter.warn(format!("Łącznie ostrzeżeń / total warnings: {warns}"));
    }
    Ok(ToolRun { status: status.code().unwrap_or(-1), tail })
}

// --------------------------------------------------------------------------- //
// Czytelne komunikaty błędów
// --------------------------------------------------------------------------- //

/// Zamienia surowe wyjście apktool/aapt2/javy na zrozumiały komunikat (PL / EN).
pub fn explain(tool: &str, tail: &str) -> String {
    let has = |s: &str| tail.contains(s);
    let first_line = |pred: &dyn Fn(&str) -> bool| -> Option<String> {
        tail.lines().map(str::trim).find(|l| pred(l)).map(|l| {
            let mut s = l.to_string();
            if s.len() > 300 {
                s.truncate(300);
                s.push('…');
            }
            s
        })
    };

    if has("UnsupportedClassVersionError") {
        return "Za stara wersja Javy dla tego narzędzia - usuń starą Javę z PATH lub ustaw JAVA_HOME na Java 17+ \
                (aplikacja może pobrać JRE 21) / Java too old - use Java 17+."
            .into();
    }
    if has("OutOfMemoryError") {
        return "Za mało pamięci dla Javy (APK jest bardzo duży). Zamknij inne programy i spróbuj ponownie / \
                Java ran out of memory."
            .into();
    }
    if has("No space left on device") {
        return "Brak miejsca na dysku (przestrzeń robocza w cache aplikacji) / no space left on device.".into();
    }
    if has("CantFindFrameworkResException") || has("Could not find framework resources") || has("framework resources for package of id") {
        let id = tail
            .split("package of id:")
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .unwrap_or("?")
            .trim_matches(|c: char| !c.is_ascii_digit());
        return format!(
            "APK korzysta z frameworku producenta (pakiet zasobów id {id}), którego brak. Podłącz urządzenie, z którego \
             pochodzi aplikacja (framework zostanie pobrany automatycznie) albo zaimportuj plik framework-res.apk / \
             vendor framework (id {id}) missing - connect the source device or import framework-res.apk."
        );
    }
    if has("ZipException") || has("zip END header not found") || has("not a valid zip") || has("Invalid CEN header") {
        return "Plik nie jest poprawnym APK (uszkodzone archiwum ZIP) / not a valid APK (corrupt ZIP).".into();
    }
    if has("Could not decode arsc") || (has("ResTable") && has("Exception")) || has("Invalid chunk") {
        return "Tablica zasobów (resources.arsc) jest zaciemniona lub uszkodzona - apktool nie potrafi jej \
                zdekodować. Ta aplikacja nie nadaje się do edycji zasobów / resources.arsc obfuscated or corrupt."
            .into();
    }
    if (has("could not exec") || has("aapt2")) && (has("Permission denied") || has("cannot execute") || has("error=13")) {
        return "Nie udało się uruchomić aapt2 (z paczki apktool). Na Linuksie sprawdź, czy katalog cache aplikacji \
                nie jest zamontowany z noexec / failed to execute aapt2."
            .into();
    }
    if tool == "build" {
        if let Some(l) = first_line(&|l: &str| l.contains("error:") && !l.starts_with("W:")) {
            return format!(
                "Błąd kompilacji zasobów (aapt2). Jeśli podmieniałeś grafiki - sprawdź format (np. 9-patch .9.png \
                 wymaga ramki 1 px) lub przywróć oryginał. Szczegóły / resource compile error: {l}"
            );
        }
    }
    if tool == "sign" {
        if has("Keystore was tampered") || has("password was incorrect") {
            return "Klucz debug jest uszkodzony - usuń debug.keystore z folderu narzędzi (zostanie wygenerowany \
                    ponownie) / debug keystore broken."
                .into();
        }
        if has("zipalign") && (has("fail") || has("error")) {
            return "zipalign zakończył się błędem / zipalign failed.".into();
        }
    }
    let detail = first_line(&|l: &str| l.contains("Exception") || l.contains("error") || l.contains("Error") || l.starts_with("E:"))
        .or_else(|| tail.lines().rev().find(|l| !l.trim().is_empty()).map(str::to_string))
        .unwrap_or_else(|| "brak szczegółów / no details".into());
    let what = match tool {
        "decode" => "Dekompilacja nie powiodła się / decompilation failed",
        "build" => "Budowanie APK nie powiodło się / build failed",
        "sign" => "Podpisywanie nie powiodło się / signing failed",
        "framework" => "Import frameworku nie powiódł się / framework import failed",
        _ => "Narzędzie zakończyło się błędem / tool failed",
    };
    format!("{what}: {detail}")
}

/// Instalacja: znane kody błędów `pm install` -> wskazówka.
pub fn explain_install(raw: &str, clone: bool) -> String {
    // Android 11+ (targetSdk 30+): resources.arsc skompresowany / niewyrównany -> Failure [-124]
    if raw.contains("resources.arsc") && (raw.contains("-124") || raw.contains("uncompressed") || raw.contains("aligned")) {
        return "APK spakowany niezgodnie z Androidem 11+: resources.arsc musi być bez kompresji i wyrównany do 4 B. \
                Przebuduj APK (Agent: ./build.sh agent / build.bat agent) / resources.arsc compressed or unaligned (-124)"
            .to_string();
    }
    let code = raw
        .split(|c: char| c == '[' || c == ']' || c.is_whitespace() || c == ':')
        .find(|w| w.starts_with("INSTALL_"))
        .unwrap_or("")
        .to_string();
    let hint = match code.as_str() {
        "INSTALL_FAILED_UPDATE_INCOMPATIBLE" => {
            if clone {
                "Na urządzeniu jest już pakiet o tej nazwie z innym podpisem (np. wcześniejszy klon z innego komputera). \
                 Odinstaluj go lub zmień nazwę klonu."
            } else {
                "Oryginalna aplikacja ma inny podpis niż przebudowana. Zaznacz 'Odinstaluj oryginał przed instalacją' \
                 (UWAGA: usuwa dane aplikacji) albo użyj klonowania (zmiana nazwy pakietu) - wtedy obie wersje działają obok siebie."
            }
        }
        "INSTALL_FAILED_CONFLICTING_PROVIDER" => {
            "Konflikt dostawcy treści (authorities) z inną aplikacją - użyj innej nazwy pakietu klonu."
        }
        "INSTALL_FAILED_DUPLICATE_PERMISSION" => {
            "Własne uprawnienie aplikacji jest już zadeklarowane przez inną aplikację (oryginał) z innym podpisem."
        }
        "INSTALL_FAILED_VERSION_DOWNGRADE" => "Na urządzeniu jest nowsza wersja tej aplikacji.",
        "INSTALL_FAILED_INSUFFICIENT_STORAGE" => "Za mało miejsca na urządzeniu.",
        "INSTALL_PARSE_FAILED_NO_CERTIFICATES" | "INSTALL_PARSE_FAILED_INCONSISTENT_CERTIFICATES" => {
            "APK nie jest poprawnie podpisany (wszystkie części split muszą mieć ten sam podpis)."
        }
        "INSTALL_FAILED_MISSING_SPLIT" => "Brakuje części split APK - zainstaluj komplet.",
        "INSTALL_FAILED_OLDER_SDK" => "Android na urządzeniu jest za stary dla tej aplikacji (minSdk).",
        "INSTALL_FAILED_TEST_ONLY" => "Aplikacja oznaczona jako testOnly - wymaga flagi -t.",
        "INSTALL_FAILED_INVALID_APK" => "Nieprawidłowy APK (np. zła nazwa pakietu lub uszkodzone natywne biblioteki).",
        "INSTALL_FAILED_USER_RESTRICTED" => "Instalacja zablokowana na urządzeniu - zezwól na instalację przez USB (MIUI/HyperOS: 'Install via USB').",
        _ => "",
    };
    let raw = raw.trim();
    if hint.is_empty() {
        format!("Instalacja nie powiodła się / install failed: {raw}")
    } else {
        format!("{hint} ({code})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explains_framework() {
        let m = explain(
            "decode",
            "brut.androlib.exceptions.CantFindFrameworkResException: Could not find framework resources for package of id: 2. You must install proper framework files",
        );
        assert!(m.contains("id 2"), "{m}");
    }

    #[test]
    fn explains_aapt2() {
        let m = explain("build", "I: Building resources with aapt2...\nW: res/drawable/x.9.png: error: 9-patch malformed.\nbrut.androlib.exceptions.AndrolibException: brut.common.BrutException: could not exec (exit code = 1)");
        assert!(m.contains("aapt2") || m.contains("9-patch"), "{m}");
    }

    #[test]
    fn explains_arsc_124() {
        let m = explain_install(
            "Failure [-124: Failed parse during installPackageLI: Targeting R+ (version 30 and above) requires the resources.arsc of installed APKs to be stored uncompressed and aligned on a 4-byte boundary]",
            false,
        );
        assert!(m.contains("resources.arsc") && m.contains("-124"), "{m}");
    }

    #[test]
    fn explains_install() {
        let m = explain_install("Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE: Package x signatures do not match]", false);
        assert!(m.contains("UPDATE_INCOMPATIBLE") && m.contains("podpis"));
        assert!(explain_install("Failure [WEIRD]", false).contains("WEIRD"));
    }
}
