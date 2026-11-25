//! Zdarzenia strumieniowane z backendu do GUI przez `tauri::ipc::Channel`.
//! Odpowiednik Qt-owych sygnałów `log_msg` / `progress` / `finished_ok`.

use serde::Serialize;

/// Poziom wpisu w logu - steruje kolorem w GUI.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Info,
    Ok,
    Warn,
    Error,
}

/// Jedno zdarzenie zadania. Frontend dostaje je na żywo przez Channel.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TransferEvent {
    /// Wpis do loga operacji.
    Log { level: LogLevel, message: String },
    /// Postęp: pliki `done`/`total` oraz bajty (płynny pasek + prędkość/ETA).
    Progress {
        done: u64,
        total: u64,
        bytes_done: u64,
        bytes_total: u64,
        current: Option<String>,
    },
    /// Wynik pojedynczego elementu (np. pakietu przy debloat).
    Item { id: String, ok: bool, message: String },
    /// Zakończenie całej operacji (może zawierać błędy per-element).
    Finished {
        ok: u64,
        skipped: u64,
        total: u64,
        failures: Vec<FailedItem>,
        cancelled: bool,
    },
    /// Błąd krytyczny przerywający całą operację.
    Fatal { message: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedItem {
    pub path: String,
    pub error: String,
}

/// Emituje zdarzenia do kanału, ignorując błąd kanału
/// (jeśli front się rozłączył, backend i tak dokończy robotę).
#[derive(Clone)]
pub struct Emitter {
    sink: Sink,
}

type Callback = std::sync::Arc<dyn Fn(TransferEvent) + Send + Sync>;

#[derive(Clone)]
enum Sink {
    Channel(tauri::ipc::Channel<TransferEvent>),
    /// Zdarzenia pod-zadania przekazywane do zadania nadrzędnego (np. Smart EI:
    /// `run_pull` w środku eksportu - jego Finished nie może zamknąć całego zadania w GUI).
    Callback(Callback),
}

impl Emitter {
    pub fn new(channel: tauri::ipc::Channel<TransferEvent>) -> Self {
        Self { sink: Sink::Channel(channel) }
    }

    /// Emitter bez odbiorcy (operacje synchroniczne, testy).
    pub fn silent() -> Self {
        Self::callback(|_| {})
    }

    /// Emitter wywołujący funkcję dla każdego zdarzenia.
    pub fn callback(f: impl Fn(TransferEvent) + Send + Sync + 'static) -> Self {
        Self { sink: Sink::Callback(std::sync::Arc::new(f)) }
    }

    pub fn send(&self, e: TransferEvent) {
        match &self.sink {
            Sink::Channel(c) => {
                let _ = c.send(e);
            }
            Sink::Callback(f) => f(e),
        }
    }

    pub fn log(&self, level: LogLevel, message: impl Into<String>) {
        self.send(TransferEvent::Log {
            level,
            message: message.into(),
        });
    }

    pub fn info(&self, m: impl Into<String>) {
        self.log(LogLevel::Info, m);
    }
    pub fn ok(&self, m: impl Into<String>) {
        self.log(LogLevel::Ok, m);
    }
    pub fn warn(&self, m: impl Into<String>) {
        self.log(LogLevel::Warn, m);
    }
    pub fn error(&self, m: impl Into<String>) {
        self.log(LogLevel::Error, m);
    }

    pub fn item(&self, id: impl Into<String>, ok: bool, message: impl Into<String>) {
        self.send(TransferEvent::Item {
            id: id.into(),
            ok,
            message: message.into(),
        });
    }

    pub fn finished(&self, ok: u64, skipped: u64, total: u64, failures: Vec<FailedItem>, cancelled: bool) {
        self.send(TransferEvent::Finished {
            ok,
            skipped,
            total,
            failures,
            cancelled,
        });
    }

    pub fn fatal(&self, message: impl Into<String>) {
        self.send(TransferEvent::Fatal {
            message: message.into(),
        });
    }
}
