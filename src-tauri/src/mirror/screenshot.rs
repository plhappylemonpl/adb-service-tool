//! Natywny zrzut ekranu: `adb exec-out screencap -p` -> bajty PNG w Rust ->
//! plik na dysku + data:URL do natychmiastowego podglądu w GUI.

use std::path::Path;

use base64::Engine as _;
use serde::Serialize;

use crate::adb::Adb;
use crate::error::{AdbError, Result};
use crate::utils::sanitize_filename;

const PNG_SIG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
const REMOTE_TMP: &str = "/data/local/tmp/adbst_screen.png";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Screenshot {
    pub path: String,
    pub data_url: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    pub taken_at: String,
}

/// Wycina PNG z bufora - część urządzeń dopisuje ostrzeżenia linkera przed danymi.
fn extract_png(buf: &[u8]) -> Option<&[u8]> {
    let window = buf.len().min(4096);
    let start = buf[..window].windows(PNG_SIG.len()).position(|w| w == PNG_SIG)?;
    Some(&buf[start..])
}

fn png_size(png: &[u8]) -> (u32, u32) {
    // IHDR: szerokość/wysokość big-endian na offsetach 16 i 20
    let be = |o: usize| {
        png.get(o..o + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
            .unwrap_or(0)
    };
    (be(16), be(20))
}

pub async fn take(adb: &Adb, serial: &str, out_dir: &Path) -> Result<Screenshot> {
    // 1. szybka ścieżka: strumień bajtów prosto z exec-out (Android 5+)
    let mut png: Option<Vec<u8>> = match adb.exec_out(serial, "screencap -p", 30).await {
        Ok(buf) => extract_png(&buf).map(<[u8]>::to_vec),
        Err(_) => None,
    };

    // 2. fallback: zapis na urządzeniu + adb pull (stare / nietypowe urządzenia)
    if png.is_none() {
        let tmp_local = out_dir.join(".screencap_tmp.png");
        tokio::fs::create_dir_all(out_dir).await?;
        let o = adb
            .shell(Some(serial), &format!("screencap -p {REMOTE_TMP}"), 30)
            .await?;
        if !o.ok() {
            return Err(AdbError::Command(format!("screencap: {}", o.err_text())));
        }
        let local_s = tmp_local.to_string_lossy().to_string();
        let p = adb.run(Some(serial), &["pull", REMOTE_TMP, &local_s], 60).await?;
        let _ = adb.shell(Some(serial), &format!("rm -f {REMOTE_TMP}"), 10).await;
        if !p.ok() {
            return Err(AdbError::Command(format!("pull: {}", p.err_text())));
        }
        let buf = tokio::fs::read(&tmp_local).await?;
        let _ = tokio::fs::remove_file(&tmp_local).await;
        png = extract_png(&buf).map(<[u8]>::to_vec);
    }

    let png = png.ok_or_else(|| {
        AdbError::Parse(
            "urządzenie nie zwróciło obrazu PNG (ekran zablokowany przez DRM/secure flag?) / no PNG returned".into(),
        )
    })?;

    let (width, height) = png_size(&png);
    let now = chrono::Local::now();
    let name = format!(
        "screenshot_{}_{}.png",
        sanitize_filename(serial).replace([' ', '.'], "_"),
        now.format("%Y%m%d_%H%M%S")
    );
    tokio::fs::create_dir_all(out_dir).await?;
    let path = out_dir.join(name);
    tokio::fs::write(&path, &png).await?;

    Ok(Screenshot {
        path: path.to_string_lossy().to_string(),
        data_url: format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&png)
        ),
        width,
        height,
        bytes: png.len() as u64,
        taken_at: now.format("%H:%M:%S").to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_png_after_garbage() {
        let mut v = b"WARNING: linker: something\n".to_vec();
        v.extend_from_slice(PNG_SIG);
        v.extend_from_slice(&[0, 0, 0, 13, b'I', b'H', b'D', b'R', 0, 0, 4, 56, 0, 0, 9, 96]);
        let p = extract_png(&v).unwrap();
        assert_eq!(png_size(p), (1080, 2400));
    }
}
