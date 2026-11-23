//! Integracja z pulpitem Linuksa (KDE Plasma, XFCE, MATE, GNOME, Cinnamon, LXQt...).
//!
//! Skąd brak ikony na pasku zadań?
//! - Na **Wayland** (KDE Plasma 6 domyślnie) okno NIE wysyła własnej ikony.
//!   Kompozytor szuka pliku `<app_id>.desktop` i bierze z niego `Icon=`.
//!   Bez pasującego pliku `.desktop` KDE pokazuje generyczną ikonę "W".
//! - Na **X11** (XFCE, MATE, starsze KDE) pasek używa ikony okna
//!   (`_NET_WM_ICON`) albo dopasowuje okno do `.desktop` po `WM_CLASS`.
//!
//! Rozwiązanie:
//! 1. `early_init()` (przed startem GTK): stały identyfikator aplikacji
//!    `com.drapco.adbservicetool` jako prgname = Wayland app_id oraz X11
//!    WM_CLASS ("com.drapco.adbservicetool", "Com.drapco.adbservicetool").
//!    KDE dopasowuje klasę bez rozróżniania wielkości liter, a na X11 i tak
//!    dostaje ikonę okna (_NET_WM_ICON) ustawianą w `setup`.
//! 2. `integrate()` (w tle po starcie): zapisuje do `~/.local/share`
//!    `applications/com.drapco.adbservicetool.desktop` (z `StartupWMClass`)
//!    oraz ikony hicolor 32..512 px, potem odświeża bazy (KDE: kbuildsycoca,
//!    reszta: update-desktop-database / gtk-update-icon-cache).
//!    Zapis tylko gdy coś się zmieniło - kolejne starty nic nie robią.
//!    Jeśli aplikacja jest zainstalowana z paczki .deb/.rpm, plik użytkownika
//!    dostaje `NoDisplay=true` (bez duplikatu w menu, ikona dalej działa).
//!
//! Wyłączenie: zmienna środowiskowa `ADBST_NO_DESKTOP_INTEGRATION=1`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Musi być identyczny z `identifier` w tauri.conf.json (pilnuje tego test).
pub const APP_ID: &str = "com.drapco.adbservicetool";
const APP_NAME: &str = "ADB Service Tool";

const ICONS: &[(u32, &[u8])] = &[
    (32, include_bytes!("../icons/32x32.png")),
    (64, include_bytes!("../icons/64x64.png")),
    (128, include_bytes!("../icons/128x128.png")),
    (256, include_bytes!("../icons/128x128@2x.png")),
    (512, include_bytes!("../icons/icon.png")),
];

/// Wywołać PRZED inicjalizacją GTK (na początku `run()`).
pub fn early_init() {
    glib::set_prgname(Some(APP_ID));
    glib::set_application_name(APP_NAME);
}

fn data_home() -> Option<PathBuf> {
    match std::env::var_os("XDG_DATA_HOME") {
        Some(p) if !p.is_empty() => Some(PathBuf::from(p)),
        _ => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")),
    }
}

fn data_dirs() -> Vec<PathBuf> {
    let v = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let v = if v.trim().is_empty() {
        "/usr/local/share:/usr/share".to_string()
    } else {
        v
    };
    v.split(':').filter(|s| !s.is_empty()).map(PathBuf::from).collect()
}

/// Czy aplikacja ma już systemowy plik .desktop (instalacja z .deb/.rpm)?
fn system_desktop_installed() -> bool {
    let marker = format!("StartupWMClass={APP_ID}");
    data_dirs().into_iter().any(|d| {
        let apps = d.join("applications");
        if apps.join(format!("{APP_ID}.desktop")).is_file() {
            return true;
        }
        std::fs::read_dir(&apps)
            .map(|rd| {
                rd.flatten().any(|e| {
                    let p = e.path();
                    p.extension().map(|x| x == "desktop").unwrap_or(false)
                        && std::fs::read_to_string(&p)
                            .map(|s| s.lines().any(|l| l.trim() == marker))
                            .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    })
}

/// Cytowanie argumentu `Exec=` wg Desktop Entry Specification.
fn quote_exec(path: &str) -> String {
    let needs = path
        .chars()
        .any(|c| c.is_whitespace() || "\"'\\><~|&;$*?#()`".contains(c));
    if !needs {
        return path.to_string();
    }
    let mut s = String::from("\"");
    for c in path.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('"');
    s
}

fn desktop_entry(exec: &str, hidden: bool) -> String {
    let mut s = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={APP_NAME}\n\
         GenericName=Android Service Tool\n\
         GenericName[pl]=Narzędzie serwisowe Android\n\
         Comment=ADB / Fastboot / MTP service tool (Drapco)\n\
         Comment[pl]=Serwisowe narzędzie ADB / Fastboot / MTP (Drapco)\n\
         Exec={exec}\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         StartupNotify=true\n\
         StartupWMClass={APP_ID}\n\
         Categories=Utility;Development;\n\
         Keywords=adb;fastboot;android;scrcpy;mtp;debloat;\n\
         X-GNOME-UsesNotifications=true\n"
    );
    if hidden {
        s.push_str("NoDisplay=true\n");
    }
    s
}

/// Zapisuje plik tylko, gdy treść się różni. Zwraca true przy zmianie.
fn write_if_changed(path: &Path, data: &[u8]) -> std::io::Result<bool> {
    if std::fs::read(path).map(|old| old == data).unwrap_or(false) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // zapis atomowy: tmp + rename (menu KDE nie złapie pół-pliku)
    let tmp = path.with_extension("tmp-adbst");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

/// Uruchamia narzędzie odświeżające, jeśli istnieje (bez czekania na wynik).
fn run_if_present(bin: &str, args: &[&str]) {
    if let Ok(p) = which::which(bin) {
        let _ = Command::new(p)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

/// Instaluje/aktualizuje wpis .desktop i ikony użytkownika. Zwraca true, gdy coś zmieniono.
pub fn integrate() -> std::io::Result<bool> {
    if std::env::var_os("ADBST_NO_DESKTOP_INTEGRATION").is_some() {
        return Ok(false);
    }
    let Some(data) = data_home() else {
        return Ok(false);
    };

    // AppImage: uruchamiamy plik .AppImage, nie tymczasowy punkt montowania
    let exec_path = match std::env::var("APPIMAGE") {
        Ok(p) if !p.is_empty() => p,
        _ => std::env::current_exe()?.to_string_lossy().to_string(),
    };

    let hicolor = data.join("icons/hicolor");
    let mut changed = false;
    for (size, bytes) in ICONS {
        let p = hicolor.join(format!("{size}x{size}/apps/{APP_ID}.png"));
        changed |= write_if_changed(&p, bytes)?;
    }

    let apps = data.join("applications");
    let entry = desktop_entry(&quote_exec(&exec_path), system_desktop_installed());
    changed |= write_if_changed(&apps.join(format!("{APP_ID}.desktop")), entry.as_bytes())?;

    if changed {
        let apps_s = apps.to_string_lossy().to_string();
        run_if_present("update-desktop-database", &["-q", &apps_s]);
        // cache ikon odświeżamy tylko, gdy już istnieje - nowy (a potem
        // nieaktualny) cache mógłby ukryć ikony innych aplikacji
        if hicolor.join("icon-theme.cache").is_file() {
            let h = hicolor.to_string_lossy().to_string();
            run_if_present("gtk-update-icon-cache", &["-q", "-f", "-t", &h]);
        }
        // KDE Plasma: przebudowa bazy KService (menu, ikony w pasku zadań)
        if which::which("kbuildsycoca6").is_ok() {
            run_if_present("kbuildsycoca6", &[]);
        } else {
            run_if_present("kbuildsycoca5", &[]);
        }
        // XFCE / MATE / LXDE (xdg-utils)
        run_if_present("xdg-desktop-menu", &["forceupdate"]);
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_id_matches_tauri_identifier() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(conf["identifier"].as_str(), Some(APP_ID));
    }

    #[test]
    fn exec_quoting() {
        assert_eq!(quote_exec("/opt/adb/adb-service-tool"), "/opt/adb/adb-service-tool");
        assert_eq!(
            quote_exec("/home/l/My Apps/a$b"),
            "\"/home/l/My Apps/a\\$b\""
        );
    }

    #[test]
    fn entry_has_wmclass() {
        let e = desktop_entry("/x", true);
        assert!(e.contains("StartupWMClass=com.drapco.adbservicetool"));
        assert!(e.contains("Icon=com.drapco.adbservicetool"));
        assert!(e.ends_with("NoDisplay=true\n"));
    }

    #[test]
    fn icons_have_expected_sizes() {
        for (size, bytes) in ICONS {
            let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
            assert_eq!(w, *size, "icon {size}px has width {w}");
        }
    }
}
