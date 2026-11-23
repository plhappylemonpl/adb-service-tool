//! Enterprise & Provisioning: komendy wdrożeniowe ADB / Fastboot / MTP do
//! masowego przygotowania urządzeń (Zero-Touch, flota serwisowa).
//!
//! Wszystko zwraca `Result` i czytelny `Output` - GUI pokazuje dokładne
//! wyjście polecenia zamiast wyjątków. Komendy destrukcyjne (unlock, erase,
//! skip-setup) są oznaczone i wymagają potwierdzenia po stronie GUI.

use serde::Deserialize;

use crate::adb::manager::Output;
use crate::adb::Adb;
use crate::error::{AdbError, Result};
use crate::utils::shell_quote;

// --------------------------------------------------------------------------- //
// ADB (Zero-Touch Setup)
// --------------------------------------------------------------------------- //

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AdbAction {
    /// Otwiera menu diagnostyczne (sekretny kod), np. 4636 => testing.
    /// Realizacja: broadcast SECRET_CODE (działa niezależnie od dialera).
    SecretCode { code: String },
    /// Otwiera URL w przeglądarce urządzenia (np. pobranie profilu MDM).
    OpenUrl { url: String },
    /// Pomija kreator pierwszego uruchomienia (urządzenia testowe).
    SkipSetupWizard,
    /// Otwiera ekran ustawień języka (zmiana locale bez roota).
    LocaleSettings,
    /// Ustawia locale przez persist.sys.locale (WYMAGA ROOTA) + restart UI.
    SetLocaleRoot { locale: String },
    /// `am start -a <action>` z opcjonalnym `-d <data>`.
    StartAction { action: String, data: Option<String> },
    /// `am start -n <pkg/.Activity>`.
    StartActivity { component: String },
    /// Dowolne `am ...` (walidowane: musi zaczynać się od start/broadcast/startservice).
    AmRaw { args: String },
}

fn valid_code(c: &str) -> bool {
    !c.is_empty() && c.len() <= 32 && c.chars().all(|x| x.is_ascii_alphanumeric())
}

/// Dozwolone znaki URL (bez spacji, cudzysłowów, backticków, `$`).
fn valid_url(u: &str) -> bool {
    (u.starts_with("http://") || u.starts_with("https://") || u.starts_with("otg://") || u.starts_with("intent://"))
        && u.len() <= 2048
        && !u.chars().any(|c| c.is_control() || c == '"' || c == '`' || c == '$' || c == '\'' || c == ' ')
}

fn valid_component(c: &str) -> bool {
    !c.is_empty()
        && c.len() <= 256
        && c.chars().all(|x| x.is_ascii_alphanumeric() || "._/".contains(x))
        && c.contains('/')
}

fn valid_action(a: &str) -> bool {
    !a.is_empty() && a.len() <= 128 && a.chars().all(|x| x.is_ascii_alphanumeric() || x == '.' || x == '_')
}

fn bcp47(l: &str) -> bool {
    !l.is_empty()
        && l.len() <= 16
        && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl AdbAction {
    fn command(&self) -> Result<String> {
        Ok(match self {
            AdbAction::SecretCode { code } => {
                if !valid_code(code) {
                    return Err(AdbError::InvalidArg("kod musi być alfanumeryczny / code must be alphanumeric".into()));
                }
                format!("am broadcast -a android.provider.Telephony.SECRET_CODE -d android_secret_code://{code}")
            }
            AdbAction::OpenUrl { url } => {
                if !valid_url(url) {
                    return Err(AdbError::InvalidArg("nieprawidłowy URL (http/https, bez spacji) / invalid URL".into()));
                }
                format!("am start -a android.intent.action.VIEW -d {}", shell_quote(url))
            }
            AdbAction::SkipSetupWizard => {
                // user_setup_complete + device_provisioned - pomija kreatora
                "settings put secure user_setup_complete 1; settings put global device_provisioned 1; \
                 am start -a com.android.setupwizard.EXIT 2>/dev/null; echo done"
                    .to_string()
            }
            AdbAction::LocaleSettings => "am start -a android.settings.LOCALE_SETTINGS".to_string(),
            AdbAction::SetLocaleRoot { locale } => {
                if !bcp47(locale) {
                    return Err(AdbError::InvalidArg("locale np. en-US / invalid locale".into()));
                }
                // wymaga roota: ustaw properkę i zrestartuj powłokę systemową
                format!("setprop persist.sys.locale {locale}; setprop ctl.restart zygote; echo 'locale={locale}'")
            }
            AdbAction::StartAction { action, data } => {
                if !valid_action(action) {
                    return Err(AdbError::InvalidArg("nieprawidłowa akcja intentu / invalid action".into()));
                }
                match data {
                    Some(d) if !d.is_empty() => format!("am start -a {action} -d {}", shell_quote(d)),
                    _ => format!("am start -a {action}"),
                }
            }
            AdbAction::StartActivity { component } => {
                if !valid_component(component) {
                    return Err(AdbError::InvalidArg("komponent w formacie pkg/.Activity / invalid component".into()));
                }
                format!("am start -n {component}")
            }
            AdbAction::AmRaw { args } => {
                let a = args.trim();
                let verb = a.split_whitespace().next().unwrap_or("");
                if !matches!(verb, "start" | "broadcast" | "startservice" | "start-foreground-service") {
                    return Err(AdbError::InvalidArg(
                        "dozwolone tylko am start/broadcast/startservice / only am start|broadcast|startservice".into(),
                    ));
                }
                if a.contains('\n') || a.contains('`') || a.contains('$') {
                    return Err(AdbError::InvalidArg("niedozwolone znaki / illegal characters".into()));
                }
                format!("am {a}")
            }
        })
    }
}

pub async fn adb_action(adb: &Adb, serial: &str, action: AdbAction) -> Result<Output> {
    let cmd = action.command()?;
    adb.shell(Some(serial), &cmd, 30).await
}

// --------------------------------------------------------------------------- //
// Fastboot (OEM & Persistent Data)
// --------------------------------------------------------------------------- //

/// Allowlista podkomend fastboot dostępnych z panelu (bez dowolnego wejścia).
pub const FASTBOOT_COMMANDS: &[(&str, &[&str], bool)] = &[
    // (id, argv, destrukcyjne)
    ("deviceInfo", &["oem", "device-info"], false),
    ("getVarAll", &["getvar", "all"], false),
    ("rebootRecovery", &["oem", "reboot-recovery"], false),
    ("rebootBootloader", &["reboot-bootloader"], false),
    ("rebootFastbootd", &["reboot", "fastboot"], false),
    ("rebootDownload", &["reboot", "download"], false),
    ("oemEdl", &["oem", "edl"], true),
    ("rebootEdl", &["reboot", "edl"], true),
    ("eraseConfig", &["erase", "config"], true),
    ("erasePersistent", &["erase", "persistent"], true),
    ("eraseMetadata", &["erase", "metadata"], true),
    ("eraseCarrier", &["erase", "carrier"], true),
    ("oemUnlock", &["oem", "unlock"], true),
    ("flashingUnlock", &["flashing", "unlock"], true),
    ("oemLock", &["oem", "lock"], true),
    ("flashingLock", &["flashing", "lock"], true),
];

pub async fn fastboot_command(adb: &Adb, serial: &str, id: &str) -> Result<Output> {
    let (_, argv, _) = FASTBOOT_COMMANDS
        .iter()
        .find(|(cid, _, _)| *cid == id)
        .ok_or_else(|| AdbError::InvalidArg(format!("nieznana komenda fastboot / unknown: {id}")))?;
    adb.fastboot(Some(serial), argv, 60).await
}

// --------------------------------------------------------------------------- //
// MTP (inicjalizacja przed ADB) - best effort
// --------------------------------------------------------------------------- //

use crate::hardware::usb::{UsbDevice, UsbKind};
use crate::hardware::usb_transport::UsbLink;

/// Próba restartu urządzenia z trybu MTP standardową operacją PTP ResetDevice
/// (0x1010). UWAGA: większość urządzeń Android tego NIE wspiera - pewny
/// restart do Recovery/Download bez ADB to kombinacja klawiszy. Funkcja
/// zwraca czytelny komunikat zamiast udawać sukces.
pub async fn mtp_reset() -> Result<String> {
    let dev: Option<UsbDevice> = tokio::task::spawn_blocking(crate::hardware::usb::scan)
        .await?
        .into_iter()
        .find(|d| d.kind == UsbKind::Mtp);
    let Some(dev) = dev else {
        return Err(AdbError::Other("Nie wykryto urządzenia MTP / no MTP device".into()));
    };
    let vid = u16::from_str_radix(&dev.vid, 16).unwrap_or(0);
    let pid = u16::from_str_radix(&dev.pid, 16).unwrap_or(0);
    let link = UsbLink::open_vid_pid(vid, pid)?;

    // Kontener PTP: [u32 len][u16 typ=1 (command)][u16 op=0x1010 ResetDevice][u32 transactionId]
    let mut pkt = Vec::with_capacity(12);
    pkt.extend_from_slice(&12u32.to_le_bytes());
    pkt.extend_from_slice(&1u16.to_le_bytes());
    pkt.extend_from_slice(&0x1010u16.to_le_bytes());
    pkt.extend_from_slice(&1u32.to_le_bytes());
    link.send(pkt, 5).await?;
    // odpowiedź (jeśli przyjdzie) - nie wszystkie urządzenia odpowiadają
    let _ = link.recv(64, 3).await;
    Ok(format!(
        "Wysłano PTP ResetDevice do {}:{} ({}). Jeśli urządzenie się nie zresetowało, użyj kombinacji klawiszy - standardowy MTP Androida zwykle nie obsługuje restartu / sent PTP ResetDevice (best effort).",
        dev.vid,
        dev.pid,
        dev.product.clone().unwrap_or_default()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_code_command() {
        let c = AdbAction::SecretCode { code: "4636".into() }.command().unwrap();
        assert!(c.contains("SECRET_CODE"));
        assert!(c.contains("android_secret_code://4636"));
        assert!(AdbAction::SecretCode { code: "a; rm -rf /".into() }.command().is_err());
    }

    #[test]
    fn url_validation() {
        assert!(AdbAction::OpenUrl { url: "https://mdm.example.com/enroll".into() }.command().is_ok());
        assert!(AdbAction::OpenUrl { url: "https://x.com/`id`".into() }.command().is_err());
        assert!(AdbAction::OpenUrl { url: "file:///etc/passwd".into() }.command().is_err());
    }

    #[test]
    fn component_and_action() {
        assert!(AdbAction::StartActivity { component: "com.android.settings/.Settings".into() }.command().is_ok());
        assert!(AdbAction::StartActivity { component: "no-slash".into() }.command().is_err());
        assert!(AdbAction::AmRaw { args: "start -a X".into() }.command().is_ok());
        assert!(AdbAction::AmRaw { args: "shell rm -rf /".into() }.command().is_err());
    }

    #[test]
    fn fastboot_allowlist() {
        assert!(FASTBOOT_COMMANDS.iter().any(|(id, _, d)| *id == "oemUnlock" && *d));
        assert!(FASTBOOT_COMMANDS.iter().any(|(id, _, d)| *id == "deviceInfo" && !*d));
    }
}
