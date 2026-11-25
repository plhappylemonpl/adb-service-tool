//! Globalny stan aplikacji trzymany przez Tauri (`State`).

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::adb::Adb;
use crate::hardware::logcat::LogcatSession;
use crate::utils::CancelToken;

pub type Jobs = Arc<Mutex<HashMap<String, CancelToken>>>;

pub struct AppState {
    /// Uchwyt do binarek. None jeśli `adb` nie znaleziono.
    pub adb: Mutex<Option<Arc<Adb>>>,
    /// Rejestr aktywnych zadań -> token anulowania (przycisk Anuluj w GUI).
    pub jobs: Jobs,
    /// Aktywne sesje Live Logcat (jobId -> filtr/bufor)
    pub logcat: Arc<Mutex<HashMap<String, Arc<LogcatSession>>>>,
    /// Wykryty tryb root per urządzenie (serial -> tryb).
    pub root: crate::adb::root::RootCache,
    /// Przycisk "Gotowe" (ręczna instalacja agenta) per urządzenie -> czekające zadanie.
    pub agent_confirm: Arc<Mutex<HashMap<String, Arc<tokio::sync::Notify>>>>,
    /// Sesje Smart EI (sessionId -> sterowanie z GUI).
    pub smartei: crate::smart_ei::Sessions,
}

impl AppState {
    pub fn new() -> Self {
        // Lokalizacja binarek jest szybka (which) - robimy ją synchronicznie przy starcie.
        let adb = Adb::locate().ok().map(Arc::new);
        Self {
            adb: Mutex::new(adb),
            jobs: Arc::new(Mutex::new(HashMap::new())),
            logcat: Arc::new(Mutex::new(HashMap::new())),
            root: Arc::new(Mutex::new(HashMap::new())),
            agent_confirm: Arc::new(Mutex::new(HashMap::new())),
            smartei: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Zwraca uchwyt Adb lub czytelny błąd, jeśli brak binarki.
    pub async fn adb(&self) -> crate::error::Result<Arc<Adb>> {
        self.adb
            .lock()
            .await
            .clone()
            .ok_or_else(|| crate::error::AdbError::BinaryNotFound("adb".into()))
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
