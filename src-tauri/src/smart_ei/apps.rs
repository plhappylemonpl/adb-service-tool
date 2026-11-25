//! Aplikacje w kopii Smart EI: klasyfikacja źródła instalacji i `apps.json`.
//!
//! - `play_store` - instalator `com.android.vending` (Sklep Google Play): APK NIE jest
//!   domyślnie eksportowany, na nowym telefonie aplikacja trafia do kolejki Sklepu Play
//!   (aktualna wersja, licencja i dane konta zostają po stronie Google),
//! - `sideload` - inny instalator albo brak ("Spoza Sklepu Play / Własne APK"): APK
//!   (base + wszystkie splity) może trafić do `apks/<pakiet>/`.

use serde::{Deserialize, Serialize};

use crate::utils::is_valid_package;

/// Instalatory traktowane jako Sklep Google Play.
pub const PLAY_INSTALLERS: &[&str] = &["com.android.vending", "com.google.android.feedback"];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppSource {
    PlayStore,
    Sideload,
}

impl AppSource {
    pub fn label(self) -> &'static str {
        match self {
            AppSource::PlayStore => "Google Play",
            AppSource::Sideload => "Spoza Sklepu Play / Własne APK",
        }
    }
}

/// Źródło po nazwie pakietu instalatora (`InstallSourceInfo` / `getInstallerPackageName` / `pm list packages -i`).
pub fn classify(installer: Option<&str>) -> AppSource {
    match installer.map(str::trim) {
        Some(i) if PLAY_INSTALLERS.contains(&i) => AppSource::PlayStore,
        _ => AppSource::Sideload,
    }
}

/// Czy aplikacja należy do kopii: aplikacje użytkownika oraz zaktualizowane systemowe
/// (FLAG_SYSTEM bez FLAG_UPDATED_SYSTEM_APP pomijamy - są w obrazie systemu).
pub fn should_include(system: bool, updated_system: bool) -> bool {
    !system || updated_system
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ApkFile {
    pub name: String,
    pub size: u64,
}

/// Wpis z agenta (PackageManager) - surowe dane przed klasyfikacją.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentApp {
    pub package: String,
    pub label: String,
    pub version_name: String,
    pub version_code: i64,
    pub installer: Option<String>,
    pub system: bool,
    pub updated_system: bool,
    pub size: u64,
    /// Ścieżki APK na urządzeniu (sourceDir + splitSourceDirs).
    pub apks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AppRecord {
    pub package: String,
    pub label: String,
    #[serde(default)]
    pub version_name: String,
    #[serde(default)]
    pub version_code: i64,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub installer: Option<String>,
    pub source: AppSource,
    #[serde(default)]
    pub source_label: String,
    #[serde(default)]
    pub updated_system: bool,
    /// Ścieżki APK na urządzeniu źródłowym (tylko informacyjnie, do pobrania przez ADB).
    #[serde(default)]
    pub device_apks: Vec<String>,
    /// APK zapisane w kopii (`apks/<pakiet>/<nazwa>`); puste = brak pliku w kopii.
    #[serde(default)]
    pub apk_files: Vec<ApkFile>,
}

impl AppRecord {
    pub fn from_agent(a: AgentApp) -> Option<Self> {
        if !is_valid_package(&a.package) || !should_include(a.system, a.updated_system) {
            return None;
        }
        let source = classify(a.installer.as_deref());
        let label = a.label.trim().chars().take(200).collect::<String>();
        Some(Self {
            label: if label.is_empty() { a.package.clone() } else { label },
            package: a.package,
            version_name: a.version_name.chars().take(100).collect(),
            version_code: a.version_code,
            size: a.size,
            installer: a.installer.filter(|i| is_valid_package(i)),
            source,
            source_label: source.label().into(),
            updated_system: a.updated_system,
            device_apks: a.apks.into_iter().filter(|p| p.starts_with('/') && p.ends_with(".apk")).take(64).collect(),
            apk_files: vec![],
        })
    }

    pub fn has_apk(&self) -> bool {
        !self.apk_files.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AppsFile {
    pub format_version: u32,
    pub apps: Vec<AppRecord>,
}

/// Normalizacja listy z agenta: filtr, deduplikacja, sortowanie po nazwie.
pub fn normalize(raw: Vec<AgentApp>) -> Vec<AppRecord> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<AppRecord> = raw
        .into_iter()
        .filter_map(AppRecord::from_agent)
        .filter(|a| seen.insert(a.package.clone()))
        .collect();
    out.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()).then(a.package.cmp(&b.package)));
    out
}

/// Parser `pm list packages -f -i [-3]`:
/// `package:/data/app/~~X==/com.a-Y==/base.apk=com.a  installer=com.android.vending`
/// Zwraca (pakiet, ścieżka base.apk, instalator).
pub fn parse_pm_list(out: &str) -> Vec<(String, String, Option<String>)> {
    let mut v = Vec::new();
    for line in out.lines() {
        let Some(rest) = line.trim().strip_prefix("package:") else { continue };
        let (main, installer) = match rest.split_once("  installer=").or_else(|| rest.split_once(" installer=")) {
            Some((m, i)) => (m.trim(), Some(i.trim().to_string()).filter(|i| i != "null" && !i.is_empty())),
            None => (rest.trim(), None),
        };
        // ścieżka może zawierać '=' (np. "~~abc==") - pakiet to część po OSTATNIM '='
        let Some((path, pkg)) = main.rsplit_once('=') else { continue };
        if is_valid_package(pkg) {
            v.push((pkg.to_string(), path.to_string(), installer));
        }
    }
    v
}

/// Lista pakietów z `pm list packages` (instalowane na urządzeniu docelowym).
pub fn parse_package_names(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| l.trim().strip_prefix("package:"))
        .map(|p| p.split_whitespace().next().unwrap_or("").to_string())
        .filter(|p| is_valid_package(p))
        .collect()
}

/// Bezpieczna nazwa pliku APK w `apks/<pakiet>/` (z ścieżki urządzenia).
pub fn apk_file_name(device_path: &str) -> Option<String> {
    let name = device_path.rsplit('/').next()?;
    let ok = !name.is_empty()
        && name.len() <= 200
        && name.ends_with(".apk")
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    ok.then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(pkg: &str, installer: Option<&str>, system: bool, updated: bool) -> AgentApp {
        AgentApp {
            package: pkg.into(),
            label: format!("L {pkg}"),
            installer: installer.map(Into::into),
            system,
            updated_system: updated,
            apks: vec![format!("/data/app/{pkg}-1/base.apk"), format!("/data/app/{pkg}-1/split_config.arm64_v8a.apk")],
            ..Default::default()
        }
    }

    #[test]
    fn classification_play_vs_sideload() {
        assert_eq!(classify(Some("com.android.vending")), AppSource::PlayStore);
        assert_eq!(classify(Some(" com.android.vending ")), AppSource::PlayStore);
        assert_eq!(classify(Some("com.google.android.feedback")), AppSource::PlayStore);
        assert_eq!(classify(Some("com.sec.android.app.samsungapps")), AppSource::Sideload);
        assert_eq!(classify(Some("com.google.android.packageinstaller")), AppSource::Sideload);
        assert_eq!(classify(None), AppSource::Sideload);
        assert_eq!(AppSource::Sideload.label(), "Spoza Sklepu Play / Własne APK");
        assert_eq!(serde_json::to_string(&AppSource::PlayStore).unwrap(), "\"play_store\"");
        assert_eq!(serde_json::to_string(&AppSource::Sideload).unwrap(), "\"sideload\"");
    }

    #[test]
    fn system_apps_skipped_unless_updated() {
        let list = normalize(vec![
            raw("com.whatsapp", Some("com.android.vending"), false, false),
            raw("com.android.settings", None, true, false),
            raw("com.google.android.youtube", Some("com.android.vending"), true, true),
            raw("org.fdroid.fdroid", Some("com.google.android.packageinstaller"), false, false),
            raw("com.whatsapp", Some("com.android.vending"), false, false),
            raw("bad pkg;rm", None, false, false),
        ]);
        let pk: Vec<_> = list.iter().map(|a| (a.package.as_str(), a.source)).collect();
        assert_eq!(
            pk,
            vec![
                ("com.google.android.youtube", AppSource::PlayStore),
                ("com.whatsapp", AppSource::PlayStore),
                ("org.fdroid.fdroid", AppSource::Sideload),
            ]
        );
        assert_eq!(list[2].device_apks.len(), 2);
        assert!(!list[2].has_apk());
    }

    #[test]
    fn pm_list_parsing() {
        let out = "package:/data/app/~~a1==/com.whatsapp-b2==/base.apk=com.whatsapp  installer=com.android.vending\n\
                   package:/data/app/org.fdroid.fdroid-1/base.apk=org.fdroid.fdroid  installer=null\n\
                   package:/system/app/Foo/Foo.apk=com.foo\n\
                   garbage\n";
        let v = parse_pm_list(out);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].0, "com.whatsapp");
        assert_eq!(v[0].1, "/data/app/~~a1==/com.whatsapp-b2==/base.apk");
        assert_eq!(v[0].2.as_deref(), Some("com.android.vending"));
        assert_eq!(v[1].2, None);
        assert_eq!(v[2], ("com.foo".into(), "/system/app/Foo/Foo.apk".into(), None));
        assert_eq!(parse_package_names("package:com.a\npackage:com.b uid:1\nx"), vec!["com.a", "com.b"]);
        assert_eq!(apk_file_name("/data/app/x/split_config.pl.apk").as_deref(), Some("split_config.pl.apk"));
        assert_eq!(apk_file_name("/data/app/x/base.apk").as_deref(), Some("base.apk"));
        assert!(apk_file_name("/data/app/x/../evil").is_none());
        assert!(apk_file_name("/data/app/x/a b.apk").is_none());
    }
}
