//! Narzędzia zewnętrzne APK DevTools: Java (JRE), apktool, uber-apk-signer,
//! klucz debug. Brakujące są pobierane automatycznie do folderu aplikacji -
//! z PRZYPIĘTYCH wersji i weryfikacją SHA-256 (nie "latest" z sieci), żeby
//! łańcuch narzędzi był powtarzalny i odporny na podmianę plików.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::Serialize;
use tokio::process::Command;

use crate::adb::events::Emitter;
use crate::adb::manager::{hide_console, run_raw};
use crate::error::{AdbError, Result};
use crate::utils::{download, CancelToken};

struct Pinned {
    url: &'static str,
    sha256: &'static str,
    file: &'static str,
}

const APKTOOL: Pinned = Pinned {
    url: "https://github.com/iBotPeaches/Apktool/releases/download/v2.11.1/apktool_2.11.1.jar",
    sha256: "56d59c524fc764263ba8d345754d8daf55b1887818b15cd3b594f555d249e2db",
    file: "apktool_2.11.1.jar",
};

const SIGNER: Pinned = Pinned {
    url: "https://github.com/patrickfav/uber-apk-signer/releases/download/v1.3.0/uber-apk-signer-1.3.0.jar",
    sha256: "e1299fd6fcf4da527dd53735b56127e8ea922a321128123b9c32d619bba1d835",
    file: "uber-apk-signer-1.3.0.jar",
};

/// Eclipse Temurin JRE 21 (Adoptium) dla bieżącej platformy.
fn pinned_jre() -> Option<Pinned> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some(Pinned {
            url: "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.5%2B11/OpenJDK21U-jre_x64_linux_hotspot_21.0.5_11.tar.gz",
            sha256: "553dda64b3b1c3c16f8afe402377ffebe64fb4a1721a46ed426a91fd18185e62",
            file: "jre21-linux-x64.tar.gz",
        }),
        ("linux", "aarch64") => Some(Pinned {
            url: "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.5%2B11/OpenJDK21U-jre_aarch64_linux_hotspot_21.0.5_11.tar.gz",
            sha256: "e4d02c33aeaf8e1148c1c505e129a709c5bc1889e855d4fb4f001b1780db42b4",
            file: "jre21-linux-aarch64.tar.gz",
        }),
        ("windows", "x86_64") => Some(Pinned {
            url: "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.5%2B11/OpenJDK21U-jre_x64_windows_hotspot_21.0.5_11.zip",
            sha256: "1749b36cfac273cee11802bf3e90caada5062de6a3fef1a3814c0568b25fd654",
            file: "jre21-windows-x64.zip",
        }),
        _ => None,
    }
}

pub const KS_ALIAS: &str = "androiddebugkey";
pub const KS_PASS: &str = "android";

#[derive(Debug, Clone)]
pub struct Toolchain {
    pub java: PathBuf,
    pub keytool: Option<PathBuf>,
    pub apktool: PathBuf,
    pub signer: PathBuf,
    pub keystore: PathBuf,
    pub frameworks: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolchainStatus {
    pub java: Option<String>,
    pub java_version: Option<String>,
    pub java_bundled: bool,
    pub apktool: Option<String>,
    pub signer: Option<String>,
    pub keystore: bool,
    pub tools_dir: String,
    pub can_download_java: bool,
    pub ready: bool,
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn find_file(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut dirs = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.is_file() && p.file_name().map(|n| n == name).unwrap_or(false) {
            return Some(p);
        }
        if p.is_dir() {
            dirs.push(p);
        }
    }
    if depth == 0 {
        return None;
    }
    dirs.into_iter().find_map(|d| find_file(&d, name, depth - 1))
}

/// Java: JAVA_HOME, PATH, potem JRE pobrane przez aplikację. Zwraca (ścieżka, czy_pobrane).
fn find_java(tools: &Path) -> Vec<(PathBuf, bool)> {
    let mut v = Vec::new();
    if let Ok(home) = std::env::var("JAVA_HOME") {
        let p = PathBuf::from(home).join("bin").join(exe("java"));
        if p.is_file() {
            v.push((p, false));
        }
    }
    if let Ok(p) = which::which("java") {
        v.push((p, false));
    }
    if let Some(p) = find_file(&tools.join("jre"), &exe("java"), 4) {
        v.push((p, true));
    }
    v
}

/// `java -version` -> (major, opis). 1.8 -> 8.
async fn java_version(java: &Path) -> Option<(u32, String)> {
    let mut cmd = Command::new(java);
    hide_console(&mut cmd);
    cmd.arg("-version");
    let out = run_raw(cmd, 20, None).await.ok()?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let line = text.lines().find(|l| l.contains(" version \""))?.trim().to_string();
    let ver = line.split('"').nth(1)?;
    let mut parts = ver.split(['.', '_', '-', '+']);
    let first: u32 = parts.next()?.parse().ok()?;
    let major = if first == 1 { parts.next()?.parse().ok()? } else { first };
    Some((major, line))
}

fn find_jar(tools: &Path, prefix: &str) -> Option<PathBuf> {
    let mut jars: Vec<PathBuf> = std::fs::read_dir(tools)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            n.starts_with(prefix) && n.ends_with(".jar")
        })
        .collect();
    jars.sort();
    jars.pop()
}

async fn usable_java(tools: &Path) -> Option<(PathBuf, bool, String)> {
    for (p, bundled) in find_java(tools) {
        if let Some((major, line)) = java_version(&p).await {
            if major >= 8 {
                return Some((p, bundled, line));
            }
        }
    }
    None
}

pub async fn status(tools: &Path) -> ToolchainStatus {
    let java = usable_java(tools).await;
    let apktool = find_jar(tools, "apktool");
    let signer = find_jar(tools, "uber-apk-signer");
    let keystore = tools.join("debug.keystore").is_file();
    ToolchainStatus {
        ready: java.is_some() && apktool.is_some() && signer.is_some(),
        java: java.as_ref().map(|j| j.0.display().to_string()),
        java_version: java.as_ref().map(|j| j.2.clone()),
        java_bundled: java.as_ref().map(|j| j.1).unwrap_or(false),
        apktool: apktool.map(|p| p.display().to_string()),
        signer: signer.map(|p| p.display().to_string()),
        keystore,
        tools_dir: tools.display().to_string(),
        can_download_java: pinned_jre().is_some(),
    }
}

/// Toolchain bez pobierania (gdy wszystko już jest) - do szybkich operacji.
pub async fn existing(tools: &Path) -> Option<Toolchain> {
    let (java, _, _) = usable_java(tools).await?;
    let keytool = java
        .parent()
        .map(|b| b.join(exe("keytool")))
        .filter(|p| p.is_file())
        .or_else(|| which::which("keytool").ok());
    Some(Toolchain {
        apktool: find_jar(tools, "apktool")?,
        signer: find_jar(tools, "uber-apk-signer")?,
        keystore: tools.join("debug.keystore"),
        frameworks: tools.join("framework"),
        java,
        keytool,
    })
}

/// Rozpakowuje archiwum JRE (.tar.gz na Linuksie, .zip na Windows).
fn extract_archive(archive: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let name = archive.to_string_lossy().to_lowercase();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let f = std::fs::File::open(archive)?;
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(f));
        ar.set_preserve_permissions(true);
        // unpack() odrzuca ścieżki wychodzące poza `dest`
        ar.unpack(dest).map_err(|e| AdbError::Parse(format!("tar: {e}")))?;
    } else {
        let f = std::fs::File::open(archive)?;
        let mut z = zip::ZipArchive::new(f).map_err(|e| AdbError::Parse(format!("zip: {e}")))?;
        for i in 0..z.len() {
            let mut entry = z.by_index(i).map_err(|e| AdbError::Parse(format!("zip: {e}")))?;
            let Some(rel) = entry.enclosed_name() else { continue };
            let out = dest.join(rel);
            if entry.is_dir() {
                std::fs::create_dir_all(&out)?;
                continue;
            }
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p)?;
            }
            let mut w = std::fs::File::create(&out)?;
            std::io::copy(&mut entry, &mut w)?;
        }
    }
    Ok(())
}

async fn fetch_pinned(p: &Pinned, dest: &Path, emitter: &Emitter, cancel: &CancelToken) -> Result<()> {
    emitter.info(format!("Pobieram / downloading {} ...", p.file));
    let tmp = dest.with_extension("part");
    download::download(p.url, &tmp, Some(p.sha256), emitter, cancel).await?;
    tokio::fs::rename(&tmp, dest).await?;
    emitter.ok(format!("{} OK (SHA-256 zgodny / verified)", p.file));
    Ok(())
}

/// Upewnia się, że wszystkie narzędzia są dostępne (pobiera brakujące).
pub async fn ensure(tools: &Path, emitter: &Emitter, cancel: &CancelToken) -> Result<Toolchain> {
    tokio::fs::create_dir_all(tools).await?;

    // 1. Java
    let java = match usable_java(tools).await {
        Some((p, _, line)) => {
            emitter.info(format!("Java: {line}"));
            p
        }
        None => {
            let jre = pinned_jre().ok_or_else(|| {
                AdbError::BinaryNotFound(
                    "java (zainstaluj Java 8+ - brak paczki JRE do pobrania dla tej platformy)".into(),
                )
            })?;
            emitter.warn("Brak Java 8+ - pobieram Eclipse Temurin JRE 21 (~50 MB) / downloading JRE...");
            let archive = tools.join(jre.file);
            if !archive.is_file() {
                fetch_pinned(&jre, &archive, emitter, cancel).await?;
            }
            emitter.info("Rozpakowuję JRE / extracting...");
            let (a, d) = (archive.clone(), tools.join("jre"));
            tokio::task::spawn_blocking(move || extract_archive(&a, &d)).await??;
            let _ = tokio::fs::remove_file(&archive).await;
            usable_java(tools)
                .await
                .map(|j| j.0)
                .ok_or_else(|| AdbError::BinaryNotFound("java (pobrane JRE nie działa / bundled JRE failed)".into()))?
        }
    };
    let keytool = java
        .parent()
        .map(|b| b.join(exe("keytool")))
        .filter(|p| p.is_file())
        .or_else(|| which::which("keytool").ok());

    // 2. apktool + uber-apk-signer
    let apktool = match find_jar(tools, "apktool") {
        Some(p) => p,
        None => {
            let d = tools.join(APKTOOL.file);
            fetch_pinned(&APKTOOL, &d, emitter, cancel).await?;
            d
        }
    };
    let signer = match find_jar(tools, "uber-apk-signer") {
        Some(p) => p,
        None => {
            let d = tools.join(SIGNER.file);
            fetch_pinned(&SIGNER, &d, emitter, cancel).await?;
            d
        }
    };

    let tc = Toolchain {
        java,
        keytool,
        apktool,
        signer,
        keystore: tools.join("debug.keystore"),
        frameworks: tools.join("framework"),
    };
    ensure_keystore(&tc, emitter).await?;
    Ok(tc)
}

/// Generuje (raz) klucz debug - ten sam dla wszystkich przebudowanych APK,
/// więc kolejne wersje klonu instalują się jako aktualizacja.
async fn ensure_keystore(tc: &Toolchain, emitter: &Emitter) -> Result<()> {
    if tc.keystore.is_file() {
        return Ok(());
    }
    let keytool = tc
        .keytool
        .as_ref()
        .ok_or_else(|| AdbError::BinaryNotFound("keytool (część JDK/JRE)".into()))?;
    emitter.info("Generuję klucz debug (debug.keystore, RSA 2048) / generating debug key...");
    let mut cmd = Command::new(keytool);
    hide_console(&mut cmd);
    cmd.args([
        "-genkeypair",
        "-v",
        "-storetype",
        "JKS",
        "-keystore",
    ])
    .arg(&tc.keystore)
    .args([
        "-storepass",
        KS_PASS,
        "-keypass",
        KS_PASS,
        "-alias",
        KS_ALIAS,
        "-keyalg",
        "RSA",
        "-keysize",
        "2048",
        "-validity",
        "10000",
        "-dname",
        "CN=Android Debug,O=Drapco ADB Service Tool,C=PL",
    ])
    .stdin(Stdio::null());
    let out = run_raw(cmd, 60, None).await?;
    if out.status != 0 || !tc.keystore.is_file() {
        return Err(AdbError::Command(format!(
            "keytool: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    emitter.ok("Klucz debug utworzony / debug key created.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_newest_jar() {
        let d = std::env::temp_dir().join(format!("adbst-jar-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("apktool_2.9.0.jar"), b"x").unwrap();
        std::fs::write(d.join("apktool_2.11.1.jar"), b"x").unwrap();
        std::fs::write(d.join("other.jar"), b"x").unwrap();
        let j = find_jar(&d, "apktool").unwrap();
        // sortowanie leksykalne: "2.9.0" > "2.11.1" - dlatego trzymamy jedną wersję w folderze
        assert!(j.file_name().unwrap().to_string_lossy().starts_with("apktool_"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
