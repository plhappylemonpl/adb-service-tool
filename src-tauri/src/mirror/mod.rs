//! Zakładka "Zrzut / Mirroring":
//! - `scrcpy`     : wykrywanie / pobieranie (Windows) / uruchamianie scrcpy z flagami
//! - `screenshot` : natywny zrzut ekranu przez `adb exec-out screencap -p`

pub mod scrcpy;
pub mod screenshot;
