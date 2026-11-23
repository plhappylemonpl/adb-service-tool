//! Moduł ADB/Fastboot - cała logika komunikacji z urządzeniem.
//!
//! - `manager`  : lokalizacja binarek + asynchroniczny, nieblokujący runner.
//! - `device`   : detekcja urządzeń (ADB/Fastboot/MTP) + odczyt informacji.
//! - `tree`     : leniwe listowanie drzewa katalogów (port DirListThread).
//! - `pull`     : pobieranie plików z resume (port PullThread).
//! - `push`     : wysyłanie plików z resume (port PushThread).
//! - `backup`   : kontakty + SMS przez aplikację pomocniczą (Drapco Backup Agent).
//! - `apps`     : menadżer aplikacji, ikony, debloat, extract/install APK.
//! - `power`    : szybki restart (Recovery/Download/Fastboot/Fastbootd/EDL).
//! - `events`   : zdarzenia strumieniowane do GUI (log + progress).
//! - `progress` : licznik postępu z tickerem (płynny pasek).
//! - `root`     : wykrywanie roota (adbd / su) i eskalacja poleceń.
//! - `scan`     : skaner drzewa plików (pliki, puste foldery, dowiązania bez pętli).
//! - `wireless` : ADB przez Wi-Fi (parowanie kodem, tcpip, connect/disconnect).

pub mod apps;
pub mod backup;
pub mod device;
pub mod events;
pub mod manager;
pub mod power;
pub mod progress;
pub mod pull;
pub mod push;
pub mod root;
pub mod scan;
pub mod tree;
pub mod wireless;

pub use manager::Adb;
