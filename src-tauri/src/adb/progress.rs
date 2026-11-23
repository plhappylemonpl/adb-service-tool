//! Licznik postępu współdzielony przez wątki robocze + "ticker", który
//! co 150 ms wysyła migawkę do GUI. Dzięki temu pasek jest płynny, a kanał
//! IPC nie jest zalewany tysiącami zdarzeń przy małych plikach.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::adb::events::{Emitter, TransferEvent};

pub struct Tracker {
    done: AtomicU64,
    total: AtomicU64,
    bytes_done: AtomicU64,
    bytes_total: AtomicU64,
    current: Mutex<Option<String>>,
    /// rośnie przy każdej zmianie `current` - ticker wysyła migawkę także wtedy,
    /// gdy zmienił się tylko opis (np. "SUPER (47%)"), a liczniki stoją
    rev: AtomicU64,
    stop: CancellationToken,
}

impl Tracker {
    pub fn new(total: u64, bytes_total: u64) -> Arc<Self> {
        Arc::new(Self {
            done: AtomicU64::new(0),
            total: AtomicU64::new(total),
            bytes_done: AtomicU64::new(0),
            bytes_total: AtomicU64::new(bytes_total),
            current: Mutex::new(None),
            rev: AtomicU64::new(0),
            stop: CancellationToken::new(),
        })
    }

    pub fn add_bytes(&self, n: u64) {
        if n > 0 {
            self.bytes_done.fetch_add(n, Ordering::Relaxed);
        }
    }

    /// Zwiększa licznik ukończonych elementów, zwraca nową wartość.
    pub fn item_done(&self) -> u64 {
        self.done.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn set_current(&self, s: impl Into<String>) {
        let s = s.into();
        if let Ok(mut g) = self.current.lock() {
            if g.as_deref() != Some(s.as_str()) {
                *g = Some(s);
                self.rev.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Ustawia (nie dodaje) liczbę przetworzonych bajtów - dla postępu liczonego z zewnątrz.
    pub fn set_bytes(&self, n: u64) {
        self.bytes_done.store(n, Ordering::Relaxed);
    }

    /// Ustawia liczbę ukończonych elementów.
    pub fn set_done(&self, n: u64) {
        self.done.store(n, Ordering::SeqCst);
    }

    fn snapshot(&self) -> TransferEvent {
        TransferEvent::Progress {
            done: self.done.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            bytes_done: self
                .bytes_done
                .load(Ordering::Relaxed)
                .min(self.bytes_total.load(Ordering::Relaxed)),
            bytes_total: self.bytes_total.load(Ordering::Relaxed),
            current: self.current.lock().ok().and_then(|g| g.clone()),
        }
    }

    /// Startuje ticker wysyłający migawki do GUI (tylko gdy coś się zmieniło).
    pub fn start(self: &Arc<Self>, emitter: Emitter) -> JoinHandle<()> {
        let me = self.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(150));
            let mut last = (u64::MAX, u64::MAX, u64::MAX);
            loop {
                tokio::select! {
                    _ = iv.tick() => {}
                    _ = me.stop.cancelled() => break,
                }
                let cur = (
                    me.done.load(Ordering::Relaxed),
                    me.bytes_done.load(Ordering::Relaxed),
                    me.rev.load(Ordering::Relaxed),
                );
                if cur != last {
                    emitter.send(me.snapshot());
                    last = cur;
                }
            }
            // migawka końcowa - pasek zawsze dochodzi do stanu faktycznego
            emitter.send(me.snapshot());
        })
    }

    /// Zatrzymuje ticker i czeka na migawkę końcową.
    pub async fn shutdown(&self, handle: JoinHandle<()>) {
        self.stop.cancel();
        let _ = handle.await;
    }
}
