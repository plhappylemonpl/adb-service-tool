//! Wspólne pobieranie plików przez HTTPS (rustls) z postępem bajtowym,
//! anulowaniem i sumą SHA-256 (do logu / weryfikacji).

use std::path::Path;

use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::adb::events::Emitter;
use crate::adb::progress::Tracker;
use crate::error::{AdbError, Result};
use crate::utils::CancelToken;

pub fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("adb-service-tool/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| AdbError::Io(e.to_string()))
}

/// Pobiera `url` do `dest`. Tylko HTTPS. Zwraca SHA-256 (hex).
/// Gdy `expected_sha256` podany - niezgodność usuwa plik i zwraca błąd.
pub async fn download(
    url: &str,
    dest: &Path,
    expected_sha256: Option<&str>,
    emitter: &Emitter,
    cancel: &CancelToken,
) -> Result<String> {
    if !url.starts_with("https://") {
        return Err(AdbError::InvalidArg(format!("dozwolone tylko HTTPS / https only: {url}")));
    }
    let mut resp = client()?
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| AdbError::Io(format!("download {url}: {e}")))?;
    let size = resp.content_length().unwrap_or(0);
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let tracker = Tracker::new(1, size);
    tracker.set_current(name);
    let ticker = tracker.start(emitter.clone());

    let res: Result<String> = async {
        let mut file = tokio::fs::File::create(dest).await?;
        let mut hasher = Sha256::new();
        loop {
            let chunk = tokio::select! {
                c = resp.chunk() => c.map_err(|e| AdbError::Io(format!("download: {e}")))?,
                _ = cancel.cancelled() => return Err(AdbError::Cancelled),
            };
            let Some(bytes) = chunk else { break };
            hasher.update(&bytes);
            file.write_all(&bytes).await?;
            tracker.add_bytes(bytes.len() as u64);
        }
        file.flush().await?;
        Ok(hex(&hasher.finalize()))
    }
    .await;
    tracker.item_done();
    tracker.shutdown(ticker).await;

    match res {
        Ok(sha) => {
            if let Some(exp) = expected_sha256.map(str::trim).filter(|s| !s.is_empty()) {
                if !exp.eq_ignore_ascii_case(&sha) {
                    let _ = tokio::fs::remove_file(dest).await;
                    return Err(AdbError::Other(format!(
                        "Suma SHA-256 nie zgadza się (oczekiwano {exp}, jest {sha}) - plik usunięty / checksum mismatch"
                    )));
                }
            }
            Ok(sha)
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(dest).await;
            Err(e)
        }
    }
}

/// Pobiera mały plik tekstowy do pamięci (np. reguły udev).
pub async fn fetch_text(url: &str, max_bytes: usize) -> Result<String> {
    let bytes = client()?
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| AdbError::Io(format!("{url}: {e}")))?
        .bytes()
        .await
        .map_err(|e| AdbError::Io(e.to_string()))?;
    if bytes.len() > max_bytes {
        return Err(AdbError::Other(format!("plik za duży / too large: {} B", bytes.len())));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
