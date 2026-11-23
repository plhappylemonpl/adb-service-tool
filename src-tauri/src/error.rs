//! Jednolity typ błędu dla całego backendu.
//! Unified error type for the whole backend.
//!
//! Każdy wariant jest dwujęzyczny (PL / EN) - komunikaty trafiają wprost do
//! loga w GUI, więc muszą być zrozumiałe dla serwisanta.

use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum AdbError {
    #[error("Nie znaleziono binarki '{0}' w systemie (sprawdź PATH) / binary '{0}' not found in PATH")]
    BinaryNotFound(String),

    #[error("Przekroczono czas oczekiwania ({0}s) - urządzenie mogło się odłączyć / timeout after {0}s")]
    Timeout(u64),

    #[error("Brak uprawnień do odczytu: {0} / permission denied: {0}")]
    NoPermission(String),

    #[error("Błąd we/wy: {0} / io error: {0}")]
    Io(String),

    #[error("Polecenie zakończyło się błędem: {0} / command failed: {0}")]
    Command(String),

    #[error("Błąd parsowania: {0} / parse error: {0}")]
    Parse(String),

    #[error("Anulowano / cancelled")]
    Cancelled,

    #[error("Nieprawidłowy argument: {0} / invalid argument: {0}")]
    InvalidArg(String),

    #[error("{0}")]
    Other(String),
}

impl From<std::io::Error> for AdbError {
    fn from(e: std::io::Error) -> Self {
        AdbError::Io(e.to_string())
    }
}

impl From<tokio::task::JoinError> for AdbError {
    fn from(e: tokio::task::JoinError) -> Self {
        AdbError::Other(format!("Błąd zadania / task error: {e}"))
    }
}

/// Tauri wymaga, by błąd zwracany z `#[command]` był Serialize.
/// Serializujemy jako czytelny string (dwujęzyczny).
/// UWAGA: pełna ścieżka `std::result::Result`, bo niżej definiujemy własny alias `Result<T>`.
impl Serialize for AdbError {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

pub type Result<T> = std::result::Result<T, AdbError>;
