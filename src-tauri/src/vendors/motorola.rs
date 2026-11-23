//! Motorola: automatyczny flasher na podstawie flashfile.xml / servicefile.xml.
//!
//! Bezpieczeństwo:
//! - przed startem: wszystkie pliki muszą istnieć, opcjonalnie weryfikacja MD5
//!   (sumy z XML) - uszkodzony obraz nie trafi do urządzenia;
//! - kroki wykonywane po kolei przez `fastboot`; anulowanie działa TYLKO
//!   między krokami - nigdy nie przerywamy zapisu partycji w połowie;
//! - błąd `flash`/`erase` zatrzymuje proces (błąd `getvar`/`oem` = ostrzeżenie).

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use md5::{Digest, Md5};
use serde::Deserialize;

use crate::adb::events::{Emitter, FailedItem};
use crate::adb::progress::Tracker;
use crate::adb::Adb;
use crate::error::{AdbError, Result};
use crate::parsers::moto_xml::{self, MotoFirmware, StepOp};
use crate::utils::CancelToken;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlashOptions {
    pub dir: String,
    /// konkretny plik XML (flashfile.xml / servicefile.xml)
    pub xml_file: Option<String>,
    /// pomiń kroki kasujące dane (erase userdata/metadata/cache)
    #[serde(default)]
    pub keep_data: bool,
    #[serde(default = "yes")]
    pub verify_md5: bool,
    #[serde(default)]
    pub reboot_after: bool,
}

fn yes() -> bool {
    true
}

pub fn load(dir: &str, xml_file: Option<&str>) -> Result<MotoFirmware> {
    moto_xml::load(&PathBuf::from(dir), xml_file)
}

/// MD5 pliku (blokujące, w spawn_blocking) z raportem bajtów do trackera.
fn md5_file(path: &std::path::Path, tracker: &Tracker, cancel: &CancelToken) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Md5::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if cancel.is_cancelled() {
            return Err(AdbError::Cancelled);
        }
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        tracker.add_bytes(n as u64);
    }
    Ok(crate::utils::download::hex(&h.finalize()))
}

pub async fn run_flash(adb: Arc<Adb>, serial: String, opts: FlashOptions, emitter: Emitter, cancel: CancelToken) {
    match flash_impl(adb, serial, opts, &emitter, &cancel).await {
        Ok((ok, total, fails)) => emitter.finished(ok, 0, total, fails, cancel.is_cancelled()),
        Err(AdbError::Cancelled) => {
            emitter.warn("Anulowano przed rozpoczęciem flashowania / cancelled before flashing.");
            emitter.finished(0, 0, 0, vec![], true);
        }
        Err(e) => emitter.fatal(e.to_string()),
    }
}

async fn flash_impl(
    adb: Arc<Adb>,
    serial: String,
    opts: FlashOptions,
    emitter: &Emitter,
    cancel: &CancelToken,
) -> Result<(u64, u64, Vec<FailedItem>)> {
    let fw = {
        let (d, x) = (opts.dir.clone(), opts.xml_file.clone());
        tokio::task::spawn_blocking(move || load(&d, x.as_deref())).await??
    };
    let dir = PathBuf::from(&fw.dir);
    if !fw.missing_files.is_empty() {
        return Err(AdbError::Other(format!(
            "Brak plików firmware / missing files: {}",
            fw.missing_files.join(", ")
        )));
    }
    adb.fastboot_path()?;
    emitter.info(format!(
        "{} • {} • {} ({} kroków)",
        fw.phone_model.as_deref().unwrap_or("?"),
        fw.software_version.as_deref().unwrap_or("?"),
        fw.xml_file,
        fw.steps.len()
    ));

    // urządzenie w fastboot?
    let dev = adb.fastboot(Some(&serial), &["getvar", "product"], 10).await?;
    let dev_text = format!("{}{}", dev.stderr, dev.stdout);
    if !dev.ok() && !dev_text.contains("product") {
        return Err(AdbError::Other(format!(
            "Urządzenie {serial} nie odpowiada w trybie Fastboot / not in fastboot: {}",
            dev_text.trim()
        )));
    }
    if let (Some(model), Some(product)) = (
        fw.phone_model.as_deref(),
        dev_text
            .lines()
            .find_map(|l| l.trim().trim_start_matches("(bootloader)").trim().strip_prefix("product:").map(|v| v.trim().to_string())),
    ) {
        if !model.to_lowercase().starts_with(&product.to_lowercase()) && !product.to_lowercase().starts_with(&model.to_lowercase()) {
            emitter.warn(format!(
                "UWAGA: firmware dla „{model}”, urządzenie zgłasza „{product}” - sprawdź zgodność / model mismatch"
            ));
        }
    }

    // 1. weryfikacja MD5
    if opts.verify_md5 {
        let to_check: Vec<(String, String, u64)> = fw
            .steps
            .iter()
            .filter(|s| s.op == StepOp::Flash)
            .filter_map(|s| Some((s.filename.clone()?, s.md5.clone()?, s.size.unwrap_or(0))))
            .collect();
        if !to_check.is_empty() {
            emitter.info(format!("Weryfikacja MD5: {} plików / verifying...", to_check.len()));
            let tracker = Tracker::new(to_check.len() as u64, to_check.iter().map(|c| c.2).sum());
            let ticker = tracker.start(emitter.clone());
            let mut bad = Vec::new();
            for (file, expected, _) in &to_check {
                tracker.set_current(format!("MD5 {file}"));
                let p = dir.join(file);
                let (t2, c2) = (tracker.clone(), cancel.clone());
                let got = tokio::task::spawn_blocking(move || md5_file(&p, &t2, &c2)).await?;
                let got = match got {
                    Ok(g) => g,
                    Err(e) => {
                        tracker.shutdown(ticker).await;
                        return Err(e);
                    }
                };
                tracker.item_done();
                if !got.eq_ignore_ascii_case(expected) {
                    bad.push(format!("{file} (XML {expected}, plik {got})"));
                }
            }
            tracker.shutdown(ticker).await;
            if !bad.is_empty() {
                return Err(AdbError::Other(format!(
                    "Niezgodne sumy MD5 - flashowanie wstrzymane / MD5 mismatch: {}",
                    bad.join("; ")
                )));
            }
            emitter.ok("MD5 OK - wszystkie obrazy zgodne z XML / all images verified.");
        }
    }

    // 2. kroki
    let steps: Vec<_> = fw
        .steps
        .iter()
        .filter(|s| !(opts.keep_data && s.wipes_data))
        .cloned()
        .collect();
    let skipped_wipes = fw.steps.len() - steps.len();
    if skipped_wipes > 0 {
        emitter.info(format!("Zachowanie danych: pomijam {skipped_wipes} krok(ów) erase / keeping user data."));
    }
    let total = steps.len() as u64;
    let bytes_total: u64 = steps.iter().filter(|s| s.op == StepOp::Flash).filter_map(|s| s.size).sum();
    let tracker = Tracker::new(total, bytes_total);
    let ticker = tracker.start(emitter.clone());
    let mut ok = 0u64;
    let mut fails = Vec::new();

    for (n, step) in steps.iter().enumerate() {
        if cancel.is_cancelled() {
            emitter.warn(format!(
                "Zatrzymano po {n}/{total} krokach (między krokami - bezpiecznie) / stopped between steps."
            ));
            break;
        }
        let label = format!("[{}/{total}] {}", n + 1, step.command);
        tracker.set_current(step.command.clone());
        let Some(args) = step.fastboot_args(&dir) else {
            emitter.warn(format!("{label} - pomijam nieobsługiwaną operację / skipping"));
            tracker.item_done();
            continue;
        };
        emitter.info(format!("{label} ..."));
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let timeout = if step.op == StepOp::Flash { 1200 } else { 120 };
        // celowo BEZ tokenu anulowania - nie przerywamy zapisu partycji
        let res = adb.fastboot(Some(&serial), &argv, timeout).await;
        if step.op == StepOp::Flash {
            tracker.add_bytes(step.size.unwrap_or(0));
        }
        tracker.item_done();
        let critical = matches!(step.op, StepOp::Flash | StepOp::Erase);
        match res {
            Ok(o) if o.ok() => {
                ok += 1;
                emitter.ok(format!("{label} OK"));
            }
            Ok(o) => {
                let err = o.err_text().lines().rfind(|l| !l.trim().is_empty()).unwrap_or("").to_string();
                fails.push(FailedItem {
                    path: step.command.clone(),
                    error: err.clone(),
                });
                if critical {
                    emitter.error(format!("{label} BŁĄD: {err} - przerywam / aborting"));
                    break;
                }
                emitter.warn(format!("{label}: {err} (niekrytyczne / non-critical)"));
            }
            Err(e) => {
                fails.push(FailedItem {
                    path: step.command.clone(),
                    error: e.to_string(),
                });
                emitter.error(format!("{label} BŁĄD: {e} - przerywam / aborting"));
                break;
            }
        }
    }
    tracker.shutdown(ticker).await;

    let all_ok = ok == total && fails.is_empty();
    if all_ok {
        emitter.ok(format!("Flashowanie zakończone: {ok}/{total} kroków OK / flashing complete."));
        if opts.reboot_after {
            let _ = adb.fastboot(Some(&serial), &["reboot"], 30).await;
            emitter.info("fastboot reboot");
        }
    } else if !cancel.is_cancelled() {
        emitter.error("Flashowanie NIE zakończone - nie restartuj urządzenia, popraw przyczynę i uruchom ponownie / flashing incomplete.");
    }
    Ok((ok, total, fails))
}
