//! Quick Reboot - restart do wybranego trybu, zarówno z ADB jak i z Fastboot.

use crate::adb::Adb;
use crate::error::{AdbError, Result};

pub async fn reboot(adb: &Adb, serial: &str, mode: &str, via_fastboot: bool) -> Result<()> {
    let out = if via_fastboot {
        let args: &[&str] = match mode {
            "system" => &["reboot"],
            "bootloader" | "fastboot" => &["reboot", "bootloader"],
            "fastbootd" => &["reboot", "fastboot"],
            "recovery" => &["reboot", "recovery"],
            "edl" => &["oem", "edl"],
            "poweroff" => &["oem", "poweroff"],
            "download" => {
                return Err(AdbError::InvalidArg(
                    "Download Mode niedostępny z Fastboot (Samsung nie ma fastboot) / not available from fastboot".into(),
                ))
            }
            other => return Err(AdbError::InvalidArg(format!("tryb / mode: {other}"))),
        };
        adb.fastboot(Some(serial), args, 20).await?
    } else {
        let args: &[&str] = match mode {
            "system" => &["reboot"],
            "recovery" => &["reboot", "recovery"],
            // Download Mode (Samsung Odin)
            "download" => &["reboot", "download"],
            // bootloader = klasyczny fastboot
            "bootloader" | "fastboot" => &["reboot", "bootloader"],
            // fastbootd (userspace fastboot, Android 10+ z dynamic partitions)
            "fastbootd" => &["reboot", "fastboot"],
            "sideload" => &["reboot", "sideload"],
            // Qualcomm Emergency Download (9008) - działa tylko na części urządzeń
            "edl" => &["reboot", "edl"],
            "poweroff" => &["shell", "reboot -p"],
            other => return Err(AdbError::InvalidArg(format!("tryb / mode: {other}"))),
        };
        adb.run(Some(serial), args, 20).await?
    };

    // fastboot pisze "OKAY"/"Finished" na stderr - liczy się kod wyjścia
    if out.ok() {
        Ok(())
    } else {
        Err(AdbError::Command(out.err_text()))
    }
}
