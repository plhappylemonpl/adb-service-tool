//! Edycja zdekodowanego (tekstowego) `AndroidManifest.xml` z apktool.
//!
//! Parsujemy `roxmltree` (dokładne pozycje bajtowe atrybutów) i robimy
//! punktowe podmiany/wstawki w oryginalnym tekście - formatowanie, komentarze
//! i kolejność atrybutów zostają nietknięte (diff = tylko nasze zmiany).
//!
//! Obsługiwane modyfikacje (legalne, deweloperskie):
//! - `android:debuggable="true"` na `<application>`
//! - własna etykieta aplikacji (`android:label`)
//! - klon: nowa nazwa pakietu. Sam `package` zmienia aapt2 przez
//!   `renameManifestPackage` w apktool.yml (poprawnie kwalifikuje względne
//!   nazwy klas). Tu przepisujemy to, czego aapt2 nie ruszy, a co blokuje
//!   instalację obok oryginału: `android:authorities` providerów, własne
//!   uprawnienia (`<permission>` + odwołania) i `taskAffinity`.

use std::collections::HashMap;
use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::error::{AdbError, Result};

pub const ANDROID_NS: &str = "http://schemas.android.com/apk/res/android";

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ManifestInfo {
    pub package: String,
    pub debuggable: bool,
    pub label: Option<String>,
    pub application_class: Option<String>,
    pub activities: usize,
    pub services: usize,
    pub receivers: usize,
    pub providers: Vec<String>,
    pub declared_permissions: Vec<String>,
    pub uses_permissions: usize,
    pub has_split_attr: bool,
    // z apktool.yml
    pub version_name: Option<String>,
    pub version_code: Option<String>,
    pub min_sdk: Option<String>,
    pub target_sdk: Option<String>,
    pub rename_package: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct PatchOptions {
    pub debuggable: bool,
    /// Nowa nazwa pakietu (klon). None/pusta = bez zmian.
    pub new_package: Option<String>,
    /// Nowa etykieta (tekst). None/pusta = bez zmian.
    pub new_label: Option<String>,
}

fn doc(xml: &str) -> Result<roxmltree::Document<'_>> {
    roxmltree::Document::parse(xml).map_err(|e| AdbError::Parse(format!("AndroidManifest.xml: {e}")))
}

fn android_attr<'a>(n: &roxmltree::Node<'a, 'a>, name: &str) -> Option<&'a str> {
    n.attribute((ANDROID_NS, name))
}

/// Poprawna nazwa pakietu Androida: min. 2 segmenty, [A-Za-z][A-Za-z0-9_]*.
pub fn valid_package_name(p: &str) -> bool {
    let segs: Vec<&str> = p.split('.').collect();
    segs.len() >= 2
        && p.len() <= 200
        && segs.iter().all(|s| {
            let mut c = s.chars();
            matches!(c.next(), Some(f) if f.is_ascii_alphabetic())
                && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
        })
}

pub fn inspect(xml: &str, yml: Option<&str>) -> Result<ManifestInfo> {
    let d = doc(xml)?;
    let root = d.root_element();
    if root.tag_name().name() != "manifest" {
        return Err(AdbError::Parse("brak <manifest> / not a manifest".into()));
    }
    let mut info = ManifestInfo {
        package: root.attribute("package").unwrap_or("").to_string(),
        has_split_attr: root.attribute("split").is_some(),
        ..Default::default()
    };
    for n in root.descendants().filter(|n| n.is_element()) {
        match n.tag_name().name() {
            "application" => {
                info.debuggable = android_attr(&n, "debuggable") == Some("true");
                info.label = android_attr(&n, "label").map(str::to_string);
                info.application_class = android_attr(&n, "name").map(str::to_string);
            }
            "activity" | "activity-alias" => info.activities += 1,
            "service" => info.services += 1,
            "receiver" => info.receivers += 1,
            "provider" => {
                if let Some(a) = android_attr(&n, "authorities") {
                    info.providers.push(a.to_string());
                }
            }
            "permission" => {
                if let Some(a) = android_attr(&n, "name") {
                    info.declared_permissions.push(a.to_string());
                }
            }
            "uses-permission" | "uses-permission-sdk-23" => info.uses_permissions += 1,
            _ => {}
        }
    }
    if let Some(y) = yml {
        info.version_name = yml_value(y, "versionName");
        info.version_code = yml_value(y, "versionCode");
        info.min_sdk = yml_value(y, "minSdkVersion");
        info.target_sdk = yml_value(y, "targetSdkVersion");
        info.rename_package = yml_value(y, "renameManifestPackage");
    }
    Ok(info)
}

/// Prosty odczyt `klucz: wartość` z apktool.yml (bez pełnego parsera YAML).
pub fn yml_value(yml: &str, key: &str) -> Option<String> {
    yml.lines().find_map(|l| {
        let t = l.trim();
        t.strip_prefix(&format!("{key}:"))
            .map(|v| v.trim().trim_matches('\'').trim_matches('"').to_string())
            .filter(|v| !v.is_empty() && v != "null")
    })
}

/// Ustawia `renameManifestPackage` w apktool.yml (dodaje w sekcji packageInfo, gdy brak).
pub fn yml_set_rename(yml: &str, new_package: Option<&str>) -> String {
    let value = new_package.unwrap_or("null");
    let mut out = Vec::new();
    let mut done = false;
    for l in yml.lines() {
        if l.trim_start().starts_with("renameManifestPackage:") {
            let indent = &l[..l.len() - l.trim_start().len()];
            out.push(format!("{indent}renameManifestPackage: {value}"));
            done = true;
        } else {
            out.push(l.to_string());
        }
    }
    if !done {
        let mut res = Vec::new();
        let mut inserted = false;
        for l in out {
            let is_pkg = l.trim() == "packageInfo:";
            res.push(l);
            if is_pkg && !inserted {
                res.push(format!("  renameManifestPackage: {value}"));
                inserted = true;
            }
        }
        if !inserted {
            res.push("packageInfo:".into());
            res.push(format!("  renameManifestPackage: {value}"));
        }
        out = res;
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// Escapowanie wartości atrybutu XML.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Zamienia prefiks pakietu: "orig" / "orig.x" -> "new" / "new.x".
fn swap_prefix(value: &str, orig: &str, new: &str) -> Option<String> {
    if value == orig {
        Some(new.to_string())
    } else {
        value.strip_prefix(&format!("{orig}.")).map(|rest| format!("{new}.{rest}"))
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchResult {
    pub manifest: String,
    pub changes: Vec<String>,
}

/// Nakłada modyfikacje na tekst manifestu. Zwraca nowy tekst + opis zmian.
pub fn patch(xml: &str, opts: &PatchOptions) -> Result<PatchResult> {
    let d = doc(xml)?;
    let root = d.root_element();
    let orig = root
        .attribute("package")
        .ok_or_else(|| AdbError::Parse("manifest bez atrybutu package".into()))?
        .to_string();
    let prefix = root
        .namespaces()
        .find(|ns| ns.uri() == ANDROID_NS)
        .and_then(|ns| ns.name())
        .unwrap_or("android")
        .to_string();

    let app = root
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "application");

    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    let mut changes = Vec::new();

    // wstawia/podmienia atrybut android:<name> na elemencie
    let set_attr = |node: &roxmltree::Node, name: &str, value: &str, edits: &mut Vec<(Range<usize>, String)>| {
        if let Some(a) = node.attributes().find(|a| a.namespace() == Some(ANDROID_NS) && a.name() == name) {
            edits.push((a.range_value(), xml_escape(value)));
        } else {
            // pozycja tuż za nazwą tagu (`<application|`), niezależnie od prefiksu
            let start = node.range().start;
            let pos = xml[start..]
                .char_indices()
                .skip(1)
                .find(|(_, c)| c.is_whitespace() || *c == '>' || *c == '/')
                .map(|(i, _)| start + i)
                .unwrap_or(start + 1);
            edits.push((pos..pos, format!(" {prefix}:{name}=\"{}\"", xml_escape(value))));
        }
    };

    // --- debuggable ---
    if opts.debuggable {
        let a = app.ok_or_else(|| AdbError::Parse("manifest bez <application>".into()))?;
        if android_attr(&a, "debuggable") != Some("true") {
            set_attr(&a, "debuggable", "true", &mut edits);
            changes.push("android:debuggable=\"true\"".into());
        }
    }

    // --- etykieta ---
    if let Some(label) = opts.new_label.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let a = app.ok_or_else(|| AdbError::Parse("manifest bez <application>".into()))?;
        set_attr(&a, "label", label, &mut edits);
        changes.push(format!("android:label=\"{label}\""));
    }

    // --- klon (nowy pakiet): wartości, których aapt2 nie przepisze ---
    if let Some(newp) = opts.new_package.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if !valid_package_name(newp) {
            return Err(AdbError::InvalidArg(format!("niepoprawna nazwa pakietu / invalid package: {newp}")));
        }
        if newp == orig {
            return Err(AdbError::InvalidArg("nowa nazwa pakietu jest taka sama jak oryginalna".into()));
        }

        // własne uprawnienia zadeklarowane przez aplikację -> mapa stara->nowa
        let mut perm_map: HashMap<String, String> = HashMap::new();
        for n in root.descendants().filter(|n| n.has_tag_name("permission") || n.has_tag_name("permission-tree") || n.has_tag_name("permission-group")) {
            if let Some(name) = android_attr(&n, "name") {
                let new_name = swap_prefix(name, &orig, newp).unwrap_or_else(|| format!("{newp}.{name}"));
                perm_map.insert(name.to_string(), new_name);
            }
        }

        let mut renamed_auth = 0;
        let mut renamed_perm = 0;
        let mut renamed_aff = 0;
        for n in root.descendants().filter(|n| n.is_element()) {
            for a in n.attributes().filter(|a| a.namespace() == Some(ANDROID_NS)) {
                let v = a.value();
                let new_val = match a.name() {
                    "authorities" if n.has_tag_name("provider") => {
                        // każde authority musi być unikalne w systemie
                        let parts: Vec<String> = v
                            .split(';')
                            .map(|p| swap_prefix(p, &orig, newp).unwrap_or_else(|| format!("{newp}.{p}")))
                            .collect();
                        renamed_auth += 1;
                        Some(parts.join(";"))
                    }
                    "name" if matches!(n.tag_name().name(), "permission" | "permission-tree" | "permission-group" | "uses-permission" | "uses-permission-sdk-23") => {
                        perm_map.get(v).cloned().inspect(|_| renamed_perm += 1)
                    }
                    "permission" | "readPermission" | "writePermission" => perm_map.get(v).cloned().inspect(|_| renamed_perm += 1),
                    "taskAffinity" => swap_prefix(v, &orig, newp).inspect(|_| renamed_aff += 1),
                    _ => None,
                };
                if let Some(nv) = new_val {
                    edits.push((a.range_value(), xml_escape(&nv)));
                }
            }
        }
        changes.push(format!("pakiet / package: {orig} -> {newp} (apktool.yml renameManifestPackage)"));
        if renamed_auth > 0 {
            changes.push(format!("authorities providerów: {renamed_auth}"));
        }
        if renamed_perm > 0 {
            changes.push(format!("własne uprawnienia / custom permissions: {renamed_perm}"));
        }
        if renamed_aff > 0 {
            changes.push(format!("taskAffinity: {renamed_aff}"));
        }
    }

    // zastosuj od końca (pozycje wcześniejszych edycji się nie przesuwają)
    edits.sort_by_key(|e| std::cmp::Reverse(e.0.start));
    let mut out = xml.to_string();
    for (r, text) in edits {
        out.replace_range(r, &text);
    }
    // kontrola: wynik musi się parsować
    doc(&out)?;
    Ok(PatchResult { manifest: out, changes })
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="com.example.app">
    <permission android:name="com.example.app.permission.C2D_MESSAGE" android:protectionLevel="signature"/>
    <uses-permission android:name="com.example.app.permission.C2D_MESSAGE"/>
    <uses-permission android:name="android.permission.INTERNET"/>
    <application android:label="@string/app_name" android:name=".App" android:icon="@mipmap/ic_launcher">
        <activity android:name=".MainActivity" android:taskAffinity="com.example.app.main"/>
        <provider android:name="androidx.core.content.FileProvider" android:authorities="com.example.app.fileprovider;com.facebook.app.FacebookContentProvider123" android:exported="false"/>
        <receiver android:name=".Push" android:permission="com.example.app.permission.C2D_MESSAGE"/>
    </application>
</manifest>"#;

    #[test]
    fn inspects() {
        let i = inspect(M, Some("versionInfo:\n  versionCode: 12\n  versionName: 1.2.3\nsdkInfo:\n  minSdkVersion: 21\n")).unwrap();
        assert_eq!(i.package, "com.example.app");
        assert!(!i.debuggable);
        assert_eq!(i.activities, 1);
        assert_eq!(i.providers.len(), 1);
        assert_eq!(i.version_name.as_deref(), Some("1.2.3"));
        assert_eq!(i.min_sdk.as_deref(), Some("21"));
    }

    #[test]
    fn injects_debuggable() {
        let r = patch(M, &PatchOptions { debuggable: true, ..Default::default() }).unwrap();
        assert!(r.manifest.contains("<application android:debuggable=\"true\" android:label="));
        assert!(inspect(&r.manifest, None).unwrap().debuggable);
        // ponownie - bez zmian
        let r2 = patch(&r.manifest, &PatchOptions { debuggable: true, ..Default::default() }).unwrap();
        assert!(r2.changes.is_empty());
        assert_eq!(r2.manifest, r.manifest);
        // false -> true
        let m3 = M.replace("<application ", "<application android:debuggable=\"false\" ");
        let r3 = patch(&m3, &PatchOptions { debuggable: true, ..Default::default() }).unwrap();
        assert!(r3.manifest.contains("android:debuggable=\"true\""));
        assert!(!r3.manifest.contains("debuggable=\"false\""));
    }

    #[test]
    fn clones_package() {
        let r = patch(
            M,
            &PatchOptions {
                new_package: Some("com.example.app.clone".into()),
                new_label: Some("Klon & Co".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let m = &r.manifest;
        // package zostaje (zmienia aapt2), względne nazwy klas zostają
        assert!(m.contains("package=\"com.example.app\""));
        assert!(m.contains("android:name=\".MainActivity\""));
        assert!(m.contains("android:authorities=\"com.example.app.clone.fileprovider;com.example.app.clone.com.facebook.app.FacebookContentProvider123\""));
        assert!(m.contains("<permission android:name=\"com.example.app.clone.permission.C2D_MESSAGE\""));
        assert!(m.contains("<uses-permission android:name=\"com.example.app.clone.permission.C2D_MESSAGE\""));
        assert!(m.contains("android:permission=\"com.example.app.clone.permission.C2D_MESSAGE\""));
        assert!(m.contains("android:name=\"android.permission.INTERNET\""));
        assert!(m.contains("android:taskAffinity=\"com.example.app.clone.main\""));
        assert!(m.contains("android:label=\"Klon &amp; Co\""));
    }

    #[test]
    fn rejects_bad_package() {
        assert!(patch(M, &PatchOptions { new_package: Some("bad".into()), ..Default::default() }).is_err());
        assert!(patch(M, &PatchOptions { new_package: Some("com.example.app".into()), ..Default::default() }).is_err());
        assert!(valid_package_name("pl.drapco.test_2"));
        assert!(!valid_package_name("pl.1drapco"));
    }

    #[test]
    fn yml_rename() {
        let y = "packageInfo:\n  forcedPackageId: 127\n  renameManifestPackage: null\nversionInfo:\n  versionCode: 1\n";
        let y2 = yml_set_rename(y, Some("a.b.c"));
        assert!(y2.contains("  renameManifestPackage: a.b.c"));
        assert_eq!(yml_value(&y2, "renameManifestPackage").as_deref(), Some("a.b.c"));
        let y3 = yml_set_rename("version: 2\npackageInfo:\n  forcedPackageId: 127\n", Some("x.y"));
        assert!(y3.contains("packageInfo:\n  renameManifestPackage: x.y"));
        assert_eq!(yml_value(&yml_set_rename(&y2, None), "renameManifestPackage"), None);
    }
}
