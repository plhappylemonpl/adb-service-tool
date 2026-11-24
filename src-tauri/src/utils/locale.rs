//! Locale systemu dla automatycznego wyboru języka GUI.
//!
//! WebKitGTK (Linux) potrafi zgłaszać `navigator.language = en-US` niezależnie
//! od ustawień systemu, więc frontend pyta backend. Kolejność jak w glibc:
//! `LANGUAGE` (lista z `:`), `LC_ALL`, `LC_MESSAGES`, `LANG`; potem `sys-locale`
//! (Windows: `GetUserPreferredUILanguages`, macOS: CFLocale).

/// Locale z listy zmiennych środowiskowych (testowalne: podajemy funkcję odczytu).
pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |v: &str| {
        let v = v.trim();
        if v.is_empty() || v == "C" || v == "POSIX" || v.starts_with("C.") {
            return;
        }
        if !out.iter().any(|x| x == v) {
            out.push(v.to_string());
        }
    };
    // LANGUAGE ma sens tylko, gdy locale nie jest "C" (zachowanie gettext)
    let lc_all = get("LC_ALL").unwrap_or_default();
    let messages = get("LC_MESSAGES").unwrap_or_default();
    let lang = get("LANG").unwrap_or_default();
    let effective = [&lc_all, &messages, &lang].into_iter().find(|v| !v.is_empty()).cloned().unwrap_or_default();
    if !(effective == "C" || effective == "POSIX" || effective.starts_with("C.")) {
        if let Some(list) = get("LANGUAGE") {
            for l in list.split(':') {
                push(l);
            }
        }
    }
    for v in [&lc_all, &messages, &lang] {
        push(v);
    }
    out
}

/// Preferowane locale systemu, od najważniejszego.
pub fn system_locales() -> Vec<String> {
    let mut out = if cfg!(unix) { from_env(|k| std::env::var(k).ok()) } else { Vec::new() };
    for l in sys_locale::get_locales() {
        if !out.contains(&l) {
            out.push(l);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: std::collections::HashMap<String, String> =
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn reads_env_in_gettext_order() {
        assert_eq!(
            from_env(env(&[("LANGUAGE", "nl_BE:nl"), ("LANG", "nl_BE.UTF-8")])),
            ["nl_BE", "nl", "nl_BE.UTF-8"]
        );
        assert_eq!(from_env(env(&[("LC_ALL", "cs_CZ.UTF-8"), ("LANG", "en_US.UTF-8")])), ["cs_CZ.UTF-8", "en_US.UTF-8"]);
        // LANGUAGE ignorowane przy locale "C"
        assert!(from_env(env(&[("LANGUAGE", "de"), ("LANG", "C")])).is_empty());
        assert!(from_env(env(&[])).is_empty());
    }
}
