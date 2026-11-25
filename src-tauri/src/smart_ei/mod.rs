//! Smart EI (Smart Export Import) - przenosiny telefonu krok po kroku.
//!
//! Eksport (telefon -> PC) i import (PC -> telefon) w dwóch trybach połączenia:
//! - **ADB** (domyślny): agent instalowany automatycznie (plan B: Download/ + ręczna instalacja),
//!   rozmowa agent <-> komputer przez `adb reverse` (127.0.0.1), pliki i APK przez szybkie,
//!   weryfikowane `adb pull/push/install`, ustawienia przez `settings`, zrzut ekranu głównego,
//! - **Bez ADB** (Smart Wi-Fi Bridge): komputer uruchamia tymczasowy serwer w sieci lokalnej
//!   (losowy port + PIN + jednorazowy token), agent paruje się kodem QR albo ręcznie i sam
//!   przesyła/odbiera wszystko przez Wi-Fi z kontrolą rozmiarów.
//!
//! Własna architektura na publicznym API Androida (PackageManager, ContactsContract,
//! Telephony, WallpaperManager, PackageInstaller, MediaStore) - bez kodu producentów.

pub mod apps;
pub mod bridge;
pub mod flow;
pub mod manifest;
pub mod net;
pub mod queue;
pub mod settings;
pub mod vendor;

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Mutex};

/// Kategorie plików: klucz -> ścieżki względem /sdcard (pierwsza istniejąca/wszystkie istniejące).
pub const FILE_CATEGORIES: &[(&str, &[&str])] = &[
    ("dcim", &["DCIM"]),
    ("pictures", &["Pictures"]),
    ("movies", &["Movies"]),
    ("music", &["Music"]),
    ("download", &["Download"]),
    ("documents", &["Documents"]),
    ("whatsapp", &["Android/media/com.whatsapp/WhatsApp/Media", "WhatsApp/Media"]),
];

/// W Download/ pomijamy pliki robocze agenta i samego Smart EI.
pub const DOWNLOAD_EXCLUDE: &[&str] = &["Backup", "Restore", "SmartEI_APKs", "Agent.apk"];

pub fn category_paths(key: &str) -> &'static [&'static str] {
    FILE_CATEGORIES.iter().find(|(k, _)| *k == key).map(|(_, p)| *p).unwrap_or(&[])
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Modules {
    /// Klucze z [`FILE_CATEGORIES`].
    pub files: Vec<String>,
    pub contacts: bool,
    pub sms: bool,
    pub wallpaper: bool,
    pub settings: bool,
    pub apps: bool,
    /// Eksport APK aplikacji spoza Sklepu Play.
    pub apks: bool,
    /// Zrzut ekranu głównego (`homescreen_ref.png`, tylko ADB).
    pub homescreen: bool,
}

impl Modules {
    pub fn sanitized(mut self) -> Self {
        self.files.retain(|f| FILE_CATEGORIES.iter().any(|(k, _)| k == f));
        self.files.dedup();
        if !self.apps {
            self.apks = false;
        }
        self
    }
    pub fn any(&self) -> bool {
        !self.files.is_empty() || self.contacts || self.sms || self.wallpaper || self.settings || self.apps
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartOptions {
    /// export | import
    pub role: String,
    /// adb | wifi
    pub connection: String,
    #[serde(default)]
    pub serial: Option<String>,
    /// Eksport: folder docelowy (albo istniejący folder SmartEI_* do wznowienia). Import: folder kopii.
    pub dir: String,
    #[serde(default)]
    pub modules: Modules,
    /// Wi-Fi: adres IP interfejsu (wybór użytkownika); domyślnie rekomendowany.
    #[serde(default)]
    pub bind_ip: Option<String>,
}

/// Wybór użytkownika na ekranie podsumowania.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Selection {
    pub modules: Modules,
    /// Import: zaznaczone pakiety.
    pub packages: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: String,
    pub job_id: String,
    pub role: String,
    pub connection: String,
    pub dir: String,
    pub ip: String,
    pub port: u16,
    pub pin: String,
    /// QR parowania: `http://IP:PORT/p#k=PIN`
    pub pair_url: String,
    pub pair_qr: String,
    pub apk_url: String,
    pub apk_qr: String,
    pub agent_apk: String,
}

pub enum ControlMsg {
    Proceed(Selection),
    Queue(queue::Action),
}

/// Uchwyt sesji w stanie aplikacji (przyciski GUI: Start po podsumowaniu, Dalej/Pomiń w kolejce).
pub struct Control {
    pub tx: mpsc::UnboundedSender<ControlMsg>,
}

pub type Sessions = Arc<Mutex<HashMap<String, Arc<Control>>>>;
