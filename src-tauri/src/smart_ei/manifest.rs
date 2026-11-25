//! `manifest.json` kopii Smart EI - opis źródła, lista modułów, sumy kontrolne.
//!
//! Struktura folderu kopii (`SmartEI_<Model>_<Data>/`):
//! ```text
//! manifest.json            <- ten plik (zapisywany na starcie z complete=false, na końcu complete=true)
//! files/                   <- DCIM, Pictures, Movies, Music, Download, Documents, WhatsApp... (ścieżki względem /sdcard)
//! contacts_sms/            <- contacts.vcf, sms.xml (agent)
//! wallpaper/               <- home.png, lock.png (jeśli system pozwolił), homescreen_ref.png (zrzut ekranu, ADB)
//! settings.json            <- tylko przenośne, bezpieczne ustawienia
//! apps.json                <- lista aplikacji użytkownika ze źródłem instalacji
//! apks/<pakiet>/*.apk      <- APK (wszystkie splity) aplikacji spoza Sklepu Play
//! ```

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::adb::pull::sanitize_for_ntfs;
use crate::error::{AdbError, Result};

pub const FORMAT: &str = "smartei";
pub const FORMAT_VERSION: u32 = 1;
pub const MANIFEST: &str = "manifest.json";
/// Najwyższe katalogi/pliki, które mogą powstać w folderze kopii.
pub const TOP_LEVEL: &[&str] = &["files", "contacts_sms", "wallpaper", "apks", "settings.json", "apps.json"];

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct DeviceDesc {
    pub manufacturer: String,
    pub brand: String,
    pub model: String,
    pub device: String,
    pub sdk: u32,
    pub android_version: String,
}

impl DeviceDesc {
    pub fn label(&self) -> String {
        let m = self.manufacturer.trim();
        let model = self.model.trim();
        if model.to_lowercase().starts_with(&m.to_lowercase()) || m.is_empty() {
            model.to_string()
        } else {
            format!("{m} {model}")
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ModuleEntry {
    /// files | contacts | sms | wallpaper | settings | apps
    pub name: String,
    pub ok: bool,
    pub items: u64,
    pub bytes: u64,
    /// Np. "Android 13+: system nie udostępnia tapety aplikacjom".
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileSum {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub format: String,
    pub format_version: u32,
    pub tool: String,
    pub created: String,
    #[serde(default)]
    pub completed: Option<String>,
    #[serde(default)]
    pub complete: bool,
    /// adb | wifi
    pub connection: String,
    pub source: DeviceDesc,
    #[serde(default)]
    pub modules: Vec<ModuleEntry>,
    /// SHA-256 plików danych (bez multimediów z files/ - tylko ich liczba i rozmiar w module).
    #[serde(default)]
    pub checksums: Vec<FileSum>,
    #[serde(default)]
    pub notes: Vec<String>,
}

impl Manifest {
    pub fn new(source: DeviceDesc, connection: &str) -> Self {
        Self {
            format: FORMAT.into(),
            format_version: FORMAT_VERSION,
            tool: format!("ADB Service Tool {}", env!("CARGO_PKG_VERSION")),
            created: chrono::Local::now().to_rfc3339(),
            completed: None,
            complete: false,
            connection: connection.into(),
            source,
            modules: vec![],
            checksums: vec![],
            notes: vec![],
        }
    }

    /// Dodaje albo zastępuje wpis modułu (wznowienie nadpisuje stary wynik).
    pub fn set_module(&mut self, m: ModuleEntry) {
        match self.modules.iter_mut().find(|x| x.name == m.name) {
            Some(x) => *x = m,
            None => self.modules.push(m),
        }
    }

    pub fn module(&self, name: &str) -> Option<&ModuleEntry> {
        self.modules.iter().find(|m| m.name == name)
    }

    pub fn validate(&self) -> Result<()> {
        if self.format != FORMAT {
            return Err(AdbError::InvalidArg("To nie jest kopia Smart EI (format) / not a Smart EI backup".into()));
        }
        if self.format_version == 0 || self.format_version > FORMAT_VERSION {
            return Err(AdbError::InvalidArg(format!(
                "Kopia w nowszym formacie ({}) - zaktualizuj program / backup format too new",
                self.format_version
            )));
        }
        for c in &self.checksums {
            safe_rel_path(&c.path).ok_or_else(|| AdbError::InvalidArg(format!("Nieprawidłowa ścieżka: {}", c.path)))?;
        }
        Ok(())
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let txt = std::fs::read_to_string(dir.join(MANIFEST))
            .map_err(|e| AdbError::InvalidArg(format!("Brak manifest.json w {} / {e}", dir.display())))?;
        let m: Manifest = serde_json::from_str(&txt)
            .map_err(|e| AdbError::InvalidArg(format!("manifest.json nieczytelny / unreadable: {e}")))?;
        m.validate()?;
        Ok(m)
    }

    /// Zapis atomowy (plik tymczasowy + zmiana nazwy) - przerwany zapis nie psuje kopii.
    pub fn save(&self, dir: &Path) -> Result<()> {
        let tmp = dir.join(".manifest.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(|e| AdbError::Other(e.to_string()))?)?;
        std::fs::rename(&tmp, dir.join(MANIFEST))?;
        Ok(())
    }

    /// Sumy kontrolne plików danych (wszystko poza files/).
    pub fn compute_checksums(&mut self, dir: &Path) -> Result<()> {
        let mut out = Vec::new();
        for top in TOP_LEVEL.iter().filter(|t| **t != "files") {
            let p = dir.join(top);
            if p.is_file() {
                out.push(sum_file(dir, &p)?);
            } else if p.is_dir() {
                let mut stack = vec![p];
                while let Some(d) = stack.pop() {
                    let mut entries: Vec<_> = std::fs::read_dir(&d)?.flatten().map(|e| e.path()).collect();
                    entries.sort();
                    for e in entries {
                        if e.is_dir() {
                            stack.push(e);
                        } else if e.is_file() && !is_temp(&e) {
                            out.push(sum_file(dir, &e)?);
                        }
                    }
                }
            }
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        self.checksums = out;
        Ok(())
    }

    /// Weryfikacja sum (import). Zwraca listę problemów (pusta = OK).
    pub fn verify(&self, dir: &Path) -> Vec<String> {
        let mut bad = Vec::new();
        for c in &self.checksums {
            let Some(rel) = safe_rel_path(&c.path) else {
                bad.push(c.path.clone());
                continue;
            };
            match sum_file(dir, &dir.join(rel)) {
                Ok(s) if s.size == c.size && s.sha256 == c.sha256 => {}
                Ok(_) => bad.push(format!("{} (suma/rozmiar / checksum mismatch)", c.path)),
                Err(_) => bad.push(format!("{} (brak pliku / missing)", c.path)),
            }
        }
        bad
    }
}

fn is_temp(p: &Path) -> bool {
    p.file_name()
        .map(|n| {
            let n = n.to_string_lossy();
            n.ends_with(".part") || n.starts_with(".manifest")
        })
        .unwrap_or(false)
}

fn sum_file(root: &Path, p: &Path) -> Result<FileSum> {
    use std::io::Read;
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut size = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        h.update(&buf[..n]);
    }
    let rel = p.strip_prefix(root).unwrap_or(p);
    let path = rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect::<Vec<_>>().join("/");
    Ok(FileSum { path, size, sha256: crate::utils::download::hex(&h.finalize()) })
}

/// Nazwa folderu kopii: `SmartEI_<Model>_<RRRRMMDD-GGMMSS>` (bez znaków zakazanych w NTFS).
pub fn folder_name(model: &str, now: chrono::DateTime<chrono::Local>) -> String {
    let m: String = model
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
        .collect();
    let m = m.trim_matches(['_', '.']).to_string();
    format!("SmartEI_{}_{}", if m.is_empty() { "Android".into() } else { m }, now.format("%Y%m%d-%H%M%S"))
}

/// Ścieżka względna z agenta / manifestu -> bezpieczna ścieżka w folderze kopii.
/// Odrzuca ścieżki bezwzględne, `..`, puste i spoza dozwolonych katalogów; nazwy
/// składników są oczyszczane jak przy pobieraniu (NTFS-friendly).
pub fn safe_rel_path(rel: &str) -> Option<PathBuf> {
    let rel = rel.trim();
    if rel.is_empty() || rel.starts_with('/') || rel.starts_with('\\') || rel.contains('\0') || rel.len() > 4096 {
        return None;
    }
    let parts: Vec<&str> = rel.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() || !TOP_LEVEL.contains(&parts[0]) {
        return None;
    }
    let mut pb = PathBuf::new();
    for p in parts {
        if p == "." || p == ".." || p.contains(':') && pb.as_os_str().is_empty() {
            return None;
        }
        let clean = sanitize_for_ntfs(p);
        if clean.is_empty() || clean == "." || clean == ".." {
            return None;
        }
        pb.push(clean);
    }
    // ostatnia kontrola - żadnych prefiksów dysku ani skoków w górę
    if pb.components().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    Some(pb)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src() -> DeviceDesc {
        DeviceDesc {
            manufacturer: "Xiaomi".into(),
            brand: "Redmi".into(),
            model: "Redmi Note 12".into(),
            device: "tapas".into(),
            sdk: 34,
            android_version: "14".into(),
        }
    }

    #[test]
    fn manifest_structure_roundtrip() {
        let mut m = Manifest::new(src(), "adb");
        m.set_module(ModuleEntry { name: "files".into(), ok: true, items: 10, bytes: 1000, note: String::new() });
        m.set_module(ModuleEntry { name: "files".into(), ok: true, items: 12, bytes: 1200, note: String::new() });
        m.set_module(ModuleEntry { name: "wallpaper".into(), ok: false, items: 0, bytes: 0, note: "android13".into() });
        let v = serde_json::to_value(&m).unwrap();
        // pola w camelCase, wersja formatu, źródło i moduły
        assert_eq!(v["format"], "smartei");
        assert_eq!(v["formatVersion"], 1);
        assert_eq!(v["source"]["manufacturer"], "Xiaomi");
        assert_eq!(v["source"]["androidVersion"], "14");
        assert_eq!(v["modules"].as_array().unwrap().len(), 2, "set_module zastępuje wpis o tej samej nazwie");
        assert_eq!(v["modules"][0]["items"], 12);
        assert!(v["checksums"].is_array());
        assert_eq!(v["complete"], false);
        let back: Manifest = serde_json::from_value(v).unwrap();
        assert_eq!(back, m);
        assert!(back.validate().is_ok());
    }

    #[test]
    fn manifest_rejects_foreign_or_newer() {
        let mut m = Manifest::new(src(), "wifi");
        m.format = "other".into();
        assert!(m.validate().is_err());
        let mut m = Manifest::new(src(), "wifi");
        m.format_version = FORMAT_VERSION + 1;
        assert!(m.validate().is_err());
        let mut m = Manifest::new(src(), "wifi");
        m.checksums.push(FileSum { path: "../etc/passwd".into(), size: 1, sha256: "x".into() });
        assert!(m.validate().is_err());
    }

    #[test]
    fn checksums_save_load_verify() {
        let dir = std::env::temp_dir().join(format!("smartei-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("contacts_sms")).unwrap();
        std::fs::create_dir_all(dir.join("files/DCIM")).unwrap();
        std::fs::write(dir.join("contacts_sms/contacts.vcf"), b"BEGIN:VCARD\r\nEND:VCARD\r\n").unwrap();
        std::fs::write(dir.join("contacts_sms/sms.xml.part"), b"partial").unwrap();
        std::fs::write(dir.join("files/DCIM/a.jpg"), b"jpg").unwrap();
        std::fs::write(dir.join("apps.json"), b"{}").unwrap();
        let mut m = Manifest::new(src(), "adb");
        m.compute_checksums(&dir).unwrap();
        let paths: Vec<_> = m.checksums.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, vec!["apps.json", "contacts_sms/contacts.vcf"], "bez files/ i plików .part");
        m.save(&dir).unwrap();
        let l = Manifest::load(&dir).unwrap();
        assert_eq!(l.checksums, m.checksums);
        assert!(l.verify(&dir).is_empty());
        std::fs::write(dir.join("apps.json"), b"{ }").unwrap();
        assert_eq!(l.verify(&dir).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folder_and_paths() {
        let now = chrono::Local::now();
        let f = folder_name("Redmi Note 12 / Pro", now);
        assert!(f.starts_with("SmartEI_Redmi_Note_12___Pro_"), "{f}");
        assert!(!f.contains('/') && !f.contains(' '));
        assert!(folder_name("  ", now).starts_with("SmartEI_Android_"));

        assert_eq!(safe_rel_path("files/DCIM/Camera/a.jpg"), Some(PathBuf::from("files/DCIM/Camera/a.jpg")));
        assert_eq!(safe_rel_path("files/Pictures/x:y?.png"), Some(PathBuf::from("files/Pictures/x_y_.png")));
        assert!(safe_rel_path("apks/com.a/base.apk").is_some());
        assert!(safe_rel_path("../x").is_none());
        assert!(safe_rel_path("files/../../x").is_none());
        assert!(safe_rel_path("/etc/passwd").is_none());
        assert!(safe_rel_path("C:\\Windows\\x").is_none());
        assert!(safe_rel_path("manifest.json").is_none(), "manifestu agent nie nadpisze");
        assert!(safe_rel_path("other/x").is_none());
        assert!(safe_rel_path("").is_none());
    }
}
