//! Pomocnicze funkcje do bezpiecznego budowania poleceń powłoki i nazw plików.
//! Port odpowiedników z `adb_pull_all_gui.py` (`shell_quote`, `_sanitize`,
//! `_remote_join`, `_posix_dirname`).

/// Bezpiecznie cytuje ścieżkę do wstawienia w polecenie `adb shell` (POSIX sh).
/// Zamyka w pojedyncze apostrofy i escapuje zawarte apostrofy wg wzorca '\'' .
pub fn shell_quote(path: &str) -> String {
    let escaped = path.replace('\'', "'\\''");
    format!("'{}'", escaped)
}

/// Czyści nazwę pliku ze znaków nielegalnych na Windows (i problematycznych).
/// Odpowiednik Pythonowego `_sanitize`: bad = '<>:"\|?*'
pub fn sanitize_filename(name: &str) -> String {
    const BAD: &[char] = &['<', '>', ':', '"', '\\', '|', '?', '*'];
    let cleaned: String = name
        .chars()
        .map(|c| if BAD.contains(&c) || c.is_control() { '_' } else { c })
        .collect();
    if cleaned.is_empty() {
        "_".to_string()
    } else {
        cleaned
    }
}

/// Łączy ścieżkę bazową i względną po stronie urządzenia (zawsze `/`).
pub fn remote_join(base: &str, rel: &str) -> String {
    let base = base.trim_end_matches('/');
    let rel = rel.trim_start_matches('/');
    format!("{base}/{rel}")
}

/// `dirname` dla ścieżek POSIX (urządzenie).
pub fn posix_dirname(path: &str) -> String {
    let p = path.trim_end_matches('/');
    match p.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => p[..i].to_string(),
        None => String::new(),
    }
}

/// Nazwa pakietu Androida: tylko [A-Za-z0-9._] - chroni przed wstrzyknięciem
/// poleceń, bo nazwa trafia do `adb shell pm ...`.
pub fn is_valid_package(pkg: &str) -> bool {
    !pkg.is_empty()
        && pkg.len() <= 255
        && pkg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_plain() {
        assert_eq!(shell_quote("/sdcard/DCIM"), "'/sdcard/DCIM'");
    }

    #[test]
    fn quotes_with_apostrophe() {
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn sanitizes_bad_chars() {
        assert_eq!(sanitize_filename("a:b*c?"), "a_b_c_");
        assert_eq!(sanitize_filename(""), "_");
    }

    #[test]
    fn joins_and_dirnames() {
        assert_eq!(remote_join("/sdcard/", "/DCIM/a.jpg"), "/sdcard/DCIM/a.jpg");
        assert_eq!(posix_dirname("/sdcard/DCIM/a.jpg"), "/sdcard/DCIM");
        assert_eq!(posix_dirname("/a"), "/");
    }

    #[test]
    fn validates_packages() {
        assert!(is_valid_package("com.sec.android.app.sbrowser"));
        assert!(!is_valid_package("com.x; rm -rf /"));
        assert!(!is_valid_package(""));
    }
}
