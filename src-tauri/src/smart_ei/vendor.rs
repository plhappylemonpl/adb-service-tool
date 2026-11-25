//! Smart Vendor Filter - przy przenosinach między producentami aplikacje producenta
//! źródłowego (MIUI, One UI, EMUI, ColorOS...) są domyślnie odznaczone: na innym
//! telefonie zwykle nie działają albo dublują systemowe odpowiedniki.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::apps::{AppRecord, AppSource};

/// Rodzina producenta -> prefiksy jego pakietów.
pub const VENDOR_PREFIXES: &[(&str, &[&str])] = &[
    ("xiaomi", &["com.mi.", "com.xiaomi.", "com.miui."]),
    ("samsung", &["com.samsung.", "com.sec."]),
    ("huawei", &["com.huawei.", "com.hihonor."]),
    ("oppo", &["com.coloros.", "com.oppo.", "com.heytap.", "com.oplus."]),
    ("vivo", &["com.vivo.", "com.bbk.", "com.iqoo."]),
    ("motorola", &["com.motorola."]),
];

/// `ro.product.manufacturer` / marka -> rodzina (Redmi/POCO = Xiaomi, Honor = Huawei, realme/OnePlus = OPPO...).
pub fn family_of(manufacturer: &str) -> String {
    let m = manufacturer.trim().to_lowercase();
    let fam = match m.as_str() {
        "xiaomi" | "redmi" | "poco" | "blackshark" => "xiaomi",
        "samsung" => "samsung",
        "huawei" | "honor" => "huawei",
        "oppo" | "realme" | "oneplus" => "oppo",
        "vivo" | "iqoo" => "vivo",
        "motorola" | "lenovo" => "motorola",
        _ => return m,
    };
    fam.to_string()
}

/// Rodzina producenta, do której należy pakiet (None = aplikacja niezależna).
pub fn vendor_of_package(pkg: &str) -> Option<&'static str> {
    VENDOR_PREFIXES
        .iter()
        .find(|(_, prefixes)| prefixes.iter().any(|p| pkg.starts_with(p)))
        .map(|(f, _)| *f)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SkipReason {
    /// "Aplikacja producenta źródłowego (zalecane pominięcie)"
    SourceVendor,
    /// "Już zainstalowana"
    AlreadyInstalled,
    /// Aplikacja spoza Sklepu Play bez APK w kopii - nie ma czego zainstalować.
    NoApk,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImportApp {
    pub package: String,
    pub label: String,
    pub source: AppSource,
    pub version_name: String,
    pub size: u64,
    pub has_apk: bool,
    /// Domyślne zaznaczenie na ekranie podsumowania.
    pub selected: bool,
    pub reason: Option<SkipReason>,
    /// Pakiet producenta (dla etykiety także wtedy, gdy producent ten sam).
    pub vendor: Option<String>,
}

/// Lista aplikacji do ekranu podsumowania importu z domyślnymi zaznaczeniami.
pub fn build_import_list(
    apps: &[AppRecord],
    source_manufacturer: &str,
    target_manufacturer: &str,
    installed: &HashSet<String>,
) -> Vec<ImportApp> {
    let src = family_of(source_manufacturer);
    let dst = family_of(target_manufacturer);
    let cross_vendor = !dst.is_empty() && src != dst;
    apps.iter()
        .map(|a| {
            let vendor = vendor_of_package(&a.package);
            let has_apk = a.has_apk();
            let reason = if installed.contains(&a.package) {
                Some(SkipReason::AlreadyInstalled)
            } else if cross_vendor && vendor.is_some_and(|v| v != dst) {
                Some(SkipReason::SourceVendor)
            } else if a.source == AppSource::Sideload && !has_apk {
                Some(SkipReason::NoApk)
            } else {
                None
            };
            ImportApp {
                package: a.package.clone(),
                label: a.label.clone(),
                source: a.source,
                version_name: a.version_name.clone(),
                size: a.size,
                has_apk,
                selected: reason.is_none(),
                reason,
                vendor: vendor.map(Into::into),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smart_ei::apps::ApkFile;

    fn app(pkg: &str, source: AppSource, apk: bool) -> AppRecord {
        AppRecord {
            package: pkg.into(),
            label: pkg.into(),
            version_name: "1".into(),
            version_code: 1,
            size: 10,
            installer: None,
            source,
            source_label: source.label().into(),
            updated_system: false,
            device_apks: vec![],
            apk_files: if apk { vec![ApkFile { name: "base.apk".into(), size: 10 }] } else { vec![] },
        }
    }

    #[test]
    fn families_and_prefixes() {
        assert_eq!(family_of("Xiaomi"), "xiaomi");
        assert_eq!(family_of("POCO"), "xiaomi");
        assert_eq!(family_of("OnePlus"), "oppo");
        assert_eq!(family_of("HONOR"), "huawei");
        assert_eq!(family_of("Google"), "google");
        assert_eq!(vendor_of_package("com.miui.gallery"), Some("xiaomi"));
        assert_eq!(vendor_of_package("com.mi.health"), Some("xiaomi"));
        assert_eq!(vendor_of_package("com.xiaomi.smarthome"), Some("xiaomi"));
        assert_eq!(vendor_of_package("com.sec.android.app.sbrowser"), Some("samsung"));
        assert_eq!(vendor_of_package("com.samsung.android.app.notes"), Some("samsung"));
        assert_eq!(vendor_of_package("com.huawei.health"), Some("huawei"));
        assert_eq!(vendor_of_package("com.coloros.weather2"), Some("oppo"));
        assert_eq!(vendor_of_package("com.vivo.browser"), Some("vivo"));
        assert_eq!(vendor_of_package("com.motorola.camera3"), Some("motorola"));
        // podobne, ale NIE producenta
        assert_eq!(vendor_of_package("com.microsoft.teams"), None);
        assert_eq!(vendor_of_package("com.mixplorer"), None);
        assert_eq!(vendor_of_package("com.secret.app"), None);
    }

    #[test]
    fn cross_vendor_filter() {
        let apps = vec![
            app("com.miui.notes", AppSource::Sideload, true),
            app("com.xiaomi.smarthome", AppSource::PlayStore, false),
            app("com.whatsapp", AppSource::PlayStore, false),
            app("org.fdroid.fdroid", AppSource::Sideload, true),
            app("org.nopackage", AppSource::Sideload, false),
            app("com.spotify.music", AppSource::PlayStore, false),
        ];
        let installed: HashSet<String> = ["com.spotify.music".to_string()].into();
        let l = build_import_list(&apps, "Xiaomi", "samsung", &installed);
        let by = |p: &str| l.iter().find(|a| a.package == p).unwrap().clone();
        assert_eq!(by("com.miui.notes").reason, Some(SkipReason::SourceVendor));
        assert!(!by("com.miui.notes").selected);
        assert_eq!(by("com.xiaomi.smarthome").reason, Some(SkipReason::SourceVendor));
        assert!(by("com.whatsapp").selected);
        assert!(by("org.fdroid.fdroid").selected);
        assert_eq!(by("org.nopackage").reason, Some(SkipReason::NoApk));
        assert_eq!(by("com.spotify.music").reason, Some(SkipReason::AlreadyInstalled));
        assert!(!by("com.spotify.music").selected);

        // ten sam producent (Redmi -> POCO = Xiaomi): aplikacje producenta zostają zaznaczone
        let same = build_import_list(&apps, "Redmi", "POCO", &HashSet::new());
        assert!(same.iter().find(|a| a.package == "com.miui.notes").unwrap().selected);
        assert_eq!(same.iter().find(|a| a.package == "com.miui.notes").unwrap().vendor.as_deref(), Some("xiaomi"));

        // na Samsunga aplikacje Samsunga z kopii nie są "obce" (np. kopia z Xiaomi z Samsung Notes)
        let s = build_import_list(&[app("com.samsung.android.app.notes", AppSource::PlayStore, false)], "Xiaomi", "Samsung", &HashSet::new());
        assert!(s[0].selected);
    }
}
