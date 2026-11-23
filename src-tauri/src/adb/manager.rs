//! Lokalizacja binarek `adb`/`fastboot` i asynchroniczny runner poleceń.
//!
//! KLUCZOWE: żadne wywołanie nie blokuje wątku GUI. Wszystko leci przez
//! `tokio::process::Command` z twardym timeoutem i (opcjonalnie) tokenem
//! anulowania - anulowanie / timeout ZABIJA proces (kill_on_drop).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;

use crate::error::{AdbError, Result};
use crate::utils::CancelToken;

/// Wynik pojedynczego wywołania procesu (tekstowy).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Output {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.status == 0
    }

    /// stderr, a gdy pusty - stdout (adb/pm bywają niekonsekwentne).
    pub fn err_text(&self) -> String {
        let e = self.stderr.trim();
        if e.is_empty() {
            self.stdout.trim().to_string()
        } else {
            e.to_string()
        }
    }
}

/// Wynik surowy (binarny stdout - np. `adb exec-out dd ...`).
#[derive(Debug)]
pub struct RawOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl From<RawOutput> for Output {
    fn from(r: RawOutput) -> Self {
        Output {
            status: r.status,
            stdout: String::from_utf8_lossy(&r.stdout).to_string(),
            stderr: String::from_utf8_lossy(&r.stderr).to_string(),
        }
    }
}

/// Uchwyt do binarek. Tworzony raz, trzymany w stanie aplikacji.
#[derive(Debug, Clone)]
pub struct Adb {
    pub adb: PathBuf,
    pub fastboot: Option<PathBuf>,
}

impl Adb {
    /// Znajduje `adb` (wymagane) i `fastboot` (opcjonalne).
    /// Najpierw PATH (przez `which`), potem typowe lokalizacje platform.
    pub fn locate() -> Result<Self> {
        let adb = locate_binary("adb").ok_or_else(|| AdbError::BinaryNotFound("adb".into()))?;
        // fastboot zwykle leży obok adb (platform-tools)
        let fastboot = sibling(&adb, "fastboot").or_else(|| locate_binary("fastboot"));
        Ok(Self { adb, fastboot })
    }

    /// Ręcznie wskazana ścieżka do `adb` (np. platform-tools w nietypowym folderze).
    pub fn from_path(adb: PathBuf) -> Result<Self> {
        if !adb.is_file() {
            return Err(AdbError::BinaryNotFound(adb.to_string_lossy().to_string()));
        }
        let fastboot = sibling(&adb, "fastboot").or_else(|| locate_binary("fastboot"));
        Ok(Self { adb, fastboot })
    }

    pub fn fastboot_path(&self) -> Result<&Path> {
        self.fastboot
            .as_deref()
            .ok_or_else(|| AdbError::BinaryNotFound("fastboot".into()))
    }

    /// Gotowe polecenie `adb [-s serial] args...` (np. do strumieniowego czytania wyjścia).
    pub fn command(&self, serial: Option<&str>, args: &[&str]) -> Command {
        self.adb_cmd(serial, args)
    }

    fn adb_cmd(&self, serial: Option<&str>, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.adb);
        hide_console(&mut cmd);
        if let Some(s) = serial {
            cmd.arg("-s").arg(s);
        }
        cmd.args(args);
        cmd
    }

    /// Surowe wywołanie `adb <args...>` z timeoutem (sekundy).
    pub async fn run(&self, serial: Option<&str>, args: &[&str], timeout_s: u64) -> Result<Output> {
        run_raw(self.adb_cmd(serial, args), timeout_s, None).await.map(Into::into)
    }

    /// Jak `run`, ale przerywalne tokenem (anulowanie zabija proces adb).
    pub async fn run_c(
        &self,
        serial: Option<&str>,
        args: &[&str],
        timeout_s: u64,
        cancel: &CancelToken,
    ) -> Result<Output> {
        run_raw(self.adb_cmd(serial, args), timeout_s, Some(cancel))
            .await
            .map(Into::into)
    }

    /// `adb [-s serial] shell <command>` - `command` to JEDEN string.
    pub async fn shell(&self, serial: Option<&str>, command: &str, timeout_s: u64) -> Result<Output> {
        self.run(serial, &["shell", command], timeout_s).await
    }

    /// Poziom API urządzenia (`ro.build.version.sdk`), np. 27 dla Androida 8.1.
    /// Używane do doboru komend zgodnych ze starszymi Androidami.
    pub async fn sdk(&self, serial: &str) -> Result<u32> {
        let o = self.shell(Some(serial), "getprop ro.build.version.sdk", 8).await?;
        o.stdout
            .trim()
            .parse()
            .map_err(|_| AdbError::Parse(format!("SDK: '{}'", o.stdout.trim())))
    }

    /// `adb exec-out <command>` - binarny stdout bez konwersji CRLF (pty).
    pub async fn exec_out(&self, serial: &str, command: &str, timeout_s: u64) -> Result<Vec<u8>> {
        let r = run_raw(self.adb_cmd(Some(serial), &["exec-out", command]), timeout_s, None).await?;
        if r.status != 0 && r.stdout.is_empty() {
            return Err(AdbError::Command(String::from_utf8_lossy(&r.stderr).trim().to_string()));
        }
        Ok(r.stdout)
    }

    /// `fastboot [-s serial] <args...>` z timeoutem.
    pub async fn fastboot(&self, serial: Option<&str>, args: &[&str], timeout_s: u64) -> Result<Output> {
        let path = self.fastboot_path()?;
        let mut cmd = Command::new(path);
        hide_console(&mut cmd);
        if let Some(s) = serial {
            cmd.arg("-s").arg(s);
        }
        cmd.args(args);
        run_raw(cmd, timeout_s, None).await.map(Into::into)
    }
}

/// Uruchamia komendę, zbiera stdout/stderr, egzekwuje timeout i anulowanie.
/// Porzucenie future (timeout/anulowanie) zabija proces dzięki `kill_on_drop`.
pub async fn run_raw(mut cmd: Command, timeout_s: u64, cancel: Option<&CancelToken>) -> Result<RawOutput> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AdbError::BinaryNotFound("adb/fastboot".into())
        } else {
            AdbError::Io(e.to_string())
        }
    })?;

    let timed = timeout(Duration::from_secs(timeout_s.max(1)), child.wait_with_output());
    let res = match cancel {
        Some(c) => {
            tokio::select! {
                r = timed => r,
                _ = c.cancelled() => return Err(AdbError::Cancelled),
            }
        }
        None => timed.await,
    };

    match res {
        Ok(Ok(out)) => Ok(RawOutput {
            status: out.status.code().unwrap_or(-1),
            stdout: out.stdout,
            stderr: out.stderr,
        }),
        Ok(Err(e)) => Err(AdbError::Io(e.to_string())),
        Err(_) => Err(AdbError::Timeout(timeout_s)),
    }
}

/// Na Windows ukrywa okno konsoli przy każdym wywołaniu procesu.
#[cfg(windows)]
pub fn hide_console(cmd: &mut Command) {
    // tokio::process::Command ma creation_flags jako metodę własną (Windows)
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub fn hide_console(_cmd: &mut Command) {}

fn exe_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// `fastboot` w tym samym katalogu co `adb`.
fn sibling(adb: &Path, name: &str) -> Option<PathBuf> {
    let p = adb.parent()?.join(exe_name(name));
    p.is_file().then_some(p)
}

/// Szuka binarki w PATH, potem w typowych miejscach dla danego systemu,
/// na końcu obok pliku wykonywalnego aplikacji (przenośna paczka z platform-tools).
fn locate_binary(name: &str) -> Option<PathBuf> {
    if let Ok(p) = which::which(name) {
        return Some(p);
    }

    let exe = exe_name(name);
    let mut candidates: Vec<PathBuf> = Vec::new();

    // obok exe aplikacji: ./platform-tools/adb(.exe) lub ./adb(.exe)
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            candidates.push(dir.join("platform-tools").join(&exe));
            candidates.push(dir.join(&exe));
        }
    }

    #[cfg(windows)]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            candidates.push(PathBuf::from(&local).join("Android\\Sdk\\platform-tools").join(&exe));
            candidates.push(
                PathBuf::from(&local)
                    .join("Microsoft\\WinGet\\Links")
                    .join(&exe),
            );
        }
        if let Ok(profile) = std::env::var("USERPROFILE") {
            candidates.push(PathBuf::from(&profile).join("platform-tools").join(&exe));
            candidates.push(PathBuf::from(&profile).join("scoop\\shims").join(&exe));
        }
        candidates.push(PathBuf::from("C:\\platform-tools").join(&exe));
        candidates.push(PathBuf::from("C:\\adb").join(&exe));
        candidates.push(PathBuf::from("C:\\Android\\platform-tools").join(&exe));
        if let Ok(pf) = std::env::var("ProgramFiles") {
            candidates.push(PathBuf::from(pf).join("platform-tools").join(&exe));
        }
        candidates.push(PathBuf::from("C:\\ProgramData\\chocolatey\\bin").join(&exe));
    }

    #[cfg(not(windows))]
    {
        for base in [
            "/usr/bin",
            "/usr/local/bin",
            "/opt/android-sdk/platform-tools",
            "/opt/platform-tools",
            "/usr/lib/android-sdk/platform-tools",
        ] {
            candidates.push(PathBuf::from(base).join(&exe));
        }
        if let Ok(home) = std::env::var("HOME") {
            candidates.push(PathBuf::from(&home).join("Android/Sdk/platform-tools").join(&exe));
            candidates.push(PathBuf::from(&home).join("platform-tools").join(&exe));
            candidates.push(PathBuf::from(&home).join(".local/bin").join(&exe));
        }
    }

    candidates.into_iter().find(|p| p.is_file())
}
