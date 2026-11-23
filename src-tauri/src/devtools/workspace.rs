//! Przestrzeń robocza jednego APK (folder w cache aplikacji):
//!
//! ```text
//! <root>/<id>/
//!   meta.json              opis (pakiet, źródło, zapisane opcje edycji, ostatni build)
//!   orig/                  pobrane / wskazane pliki APK (base.apk + split_*.apk)
//!   src/                   wynik `apktool d` (res/, assets/, AndroidManifest.xml...)
//!   manifest.orig.xml      manifest zaraz po dekompilacji (edycje zawsze od oryginału)
//!   apktool.orig.yml
//!   backup/<rel>           oryginały podmienionych zasobów
//!   out/                   zbudowane i podpisane APK
//!   tmp/                   java.io.tmpdir procesów Java
//! ```
//!
//! Wszystkie ścieżki zasobów od frontendu przechodzą przez `resolve_rel`
//! (tylko `res/` i `assets/`, bez `..`, bez ścieżek absolutnych, bez wyjścia
//! symlinkiem poza `src/`).

use std::path::{Component, Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::error::{AdbError, Result};

use super::manifest::{self, ManifestInfo, PatchOptions};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BuildInfo {
    pub at: i64,
    pub package: String,
    pub files: Vec<String>,
    pub changes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct WorkspaceMeta {
    pub id: String,
    pub package: String,
    pub label: Option<String>,
    pub version_name: Option<String>,
    /// "device" | "file"
    pub source_kind: String,
    /// serial urządzenia lub ścieżka pliku
    pub source: String,
    pub created: i64,
    /// nazwy plików w orig/ - base.apk pierwszy
    pub apks: Vec<String>,
    pub size: u64,
    pub decoded: bool,
    pub patch: PatchOptions,
    pub replaced: Vec<String>,
    pub last_build: Option<BuildInfo>,
}

impl WorkspaceMeta {
    pub fn is_split(&self) -> bool {
        self.apks.len() > 1
    }
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub dir: PathBuf,
}

impl Workspace {
    pub fn new_id(package: &str) -> String {
        let clean: String = package
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' { c } else { '_' })
            .take(80)
            .collect();
        format!("{clean}-{}", chrono::Local::now().format("%Y%m%d-%H%M%S-%3f"))
    }

    /// Otwiera istniejącą przestrzeń. `id` walidowane (bez separatorów ścieżki).
    pub fn open(root: &Path, id: &str) -> Result<Self> {
        if id.is_empty()
            || id.len() > 160
            || id.starts_with('.')
            || !id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Err(AdbError::InvalidArg(format!("workspace id: {id}")));
        }
        let dir = root.join(id);
        if !dir.join("meta.json").is_file() {
            return Err(AdbError::InvalidArg(format!(
                "Przestrzeń robocza nie istnieje (usunięta?) / workspace not found: {id}"
            )));
        }
        Ok(Self { dir })
    }

    pub fn create(root: &Path, id: &str) -> Result<Self> {
        let dir = root.join(id);
        std::fs::create_dir_all(dir.join("orig"))?;
        Ok(Self { dir })
    }

    pub fn orig(&self) -> PathBuf {
        self.dir.join("orig")
    }
    pub fn src(&self) -> PathBuf {
        self.dir.join("src")
    }
    pub fn out(&self) -> PathBuf {
        self.dir.join("out")
    }
    pub fn backup(&self) -> PathBuf {
        self.dir.join("backup")
    }

    pub fn load(&self) -> Result<WorkspaceMeta> {
        let s = std::fs::read_to_string(self.dir.join("meta.json"))?;
        serde_json::from_str(&s).map_err(|e| AdbError::Parse(format!("meta.json: {e}")))
    }

    pub fn save(&self, m: &WorkspaceMeta) -> Result<()> {
        let s = serde_json::to_string_pretty(m).map_err(|e| AdbError::Parse(e.to_string()))?;
        let tmp = self.dir.join("meta.json.part");
        std::fs::write(&tmp, s)?;
        std::fs::rename(tmp, self.dir.join("meta.json"))?;
        Ok(())
    }

    /// Po dekompilacji: zachowuje oryginalny manifest i apktool.yml.
    pub fn snapshot_originals(&self) -> Result<()> {
        std::fs::copy(self.src().join("AndroidManifest.xml"), self.dir.join("manifest.orig.xml"))?;
        std::fs::copy(self.src().join("apktool.yml"), self.dir.join("apktool.orig.yml"))?;
        Ok(())
    }

    pub fn manifest_info(&self) -> Result<ManifestInfo> {
        let xml = std::fs::read_to_string(self.dir.join("manifest.orig.xml"))?;
        let yml = std::fs::read_to_string(self.dir.join("apktool.orig.yml")).ok();
        manifest::inspect(&xml, yml.as_deref())
    }

    pub fn manifest_text(&self, original: bool) -> Result<String> {
        let p = if original { self.dir.join("manifest.orig.xml") } else { self.src().join("AndroidManifest.xml") };
        Ok(std::fs::read_to_string(p)?)
    }

    /// Nakłada opcje edycji na ORYGINALNY manifest/yml i zapisuje do src/.
    /// Idempotentne - kolejne buildy z innymi opcjami nie kumulują zmian.
    pub fn apply_patch(&self, opts: &PatchOptions) -> Result<Vec<String>> {
        let xml = std::fs::read_to_string(self.dir.join("manifest.orig.xml"))?;
        let yml = std::fs::read_to_string(self.dir.join("apktool.orig.yml"))?;
        let res = manifest::patch(&xml, opts)?;
        let new_pkg = opts.new_package.as_deref().map(str::trim).filter(|p| !p.is_empty());
        std::fs::write(self.src().join("AndroidManifest.xml"), &res.manifest)?;
        std::fs::write(self.src().join("apktool.yml"), manifest::yml_set_rename(&yml, new_pkg))?;
        Ok(res.changes)
    }

    pub fn delete(self) -> Result<()> {
        std::fs::remove_dir_all(&self.dir)?;
        Ok(())
    }
}

pub fn list_all(root: &Path) -> Vec<WorkspaceMeta> {
    let mut v: Vec<WorkspaceMeta> = std::fs::read_dir(root)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let s = std::fs::read_to_string(e.path().join("meta.json")).ok()?;
                    serde_json::from_str::<WorkspaceMeta>(&s).ok()
                })
                .filter(|m| m.decoded)
                .collect()
        })
        .unwrap_or_default();
    v.sort_by_key(|m| std::cmp::Reverse(m.created));
    v
}

/// Przestrzenie bez udanej dekompilacji (przerwane) - sprzątane przy starcie listy.
pub fn cleanup_broken(root: &Path) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let ok = std::fs::read_to_string(p.join("meta.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<WorkspaceMeta>(&s).ok())
            .map(|m| m.decoded)
            .unwrap_or(false);
        let age = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // świeże mogą właśnie powstawać w innym zadaniu
        if !ok && age > 3600 {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

// --------------------------------------------------------------------------- //
// Eksplorator zasobów
// --------------------------------------------------------------------------- //

/// Bezpieczne rozwiązanie ścieżki względnej z GUI do pliku w `src/`.
pub fn resolve_rel(src: &Path, rel: &str) -> Result<PathBuf> {
    let rel = rel.trim().trim_matches('/');
    let bad = || AdbError::InvalidArg(format!("niedozwolona ścieżka / forbidden path: {rel}"));
    if rel.contains('\\') || rel.contains(':') || rel.contains('\0') {
        return Err(bad());
    }
    let mut parts = rel.split('/');
    match parts.next() {
        Some("res") | Some("assets") => {}
        _ => return Err(bad()),
    }
    if rel.split('/').any(|seg| seg.is_empty() || seg == "." || seg == "..") {
        return Err(bad());
    }
    let p = Path::new(rel);
    if !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(bad());
    }
    let full = src.join(p);
    // symlink nie może wyprowadzić poza src/
    if let (Ok(c), Ok(base)) = (full.canonicalize(), src.canonicalize()) {
        if !c.starts_with(&base) {
            return Err(bad());
        }
    }
    Ok(full)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResEntry {
    pub name: String,
    pub rel: String,
    pub dir: bool,
    pub size: u64,
    /// image | xml | text | font | audio | binary
    pub kind: String,
    pub replaced: bool,
    pub children: usize,
}

const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp"];
const TEXT_EXT: &[&str] = &[
    "txt", "json", "html", "htm", "js", "css", "properties", "cfg", "ini", "csv", "md", "yml", "yaml", "svg", "conf",
    "pem", "license", "version",
];

fn ext_of(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

pub fn kind_of(name: &str) -> &'static str {
    let e = ext_of(name);
    if IMAGE_EXT.contains(&e.as_str()) {
        "image"
    } else if e == "xml" {
        "xml"
    } else if TEXT_EXT.contains(&e.as_str()) {
        "text"
    } else if matches!(e.as_str(), "ttf" | "otf" | "woff" | "woff2") {
        "font"
    } else if matches!(e.as_str(), "mp3" | "ogg" | "wav" | "m4a" | "aac" | "flac") {
        "audio"
    } else {
        "binary"
    }
}

/// Zawartość katalogu (leniwie, poziom po poziomie). `rel` puste = korzeń (res, assets).
pub fn list_dir(src: &Path, rel: &str, replaced: &[String]) -> Result<Vec<ResEntry>> {
    let rel = rel.trim().trim_matches('/');
    let entries: Vec<(String, PathBuf)> = if rel.is_empty() {
        ["res", "assets"]
            .iter()
            .map(|n| (n.to_string(), src.join(n)))
            .filter(|(_, p)| p.is_dir())
            .collect()
    } else {
        let dir = resolve_rel(src, rel)?;
        std::fs::read_dir(&dir)?
            .flatten()
            .map(|e| (format!("{rel}/{}", e.file_name().to_string_lossy()), e.path()))
            .collect()
    };
    let mut out: Vec<ResEntry> = entries
        .into_iter()
        .filter_map(|(r, p)| {
            let md = std::fs::symlink_metadata(&p).ok()?;
            if md.file_type().is_symlink() {
                return None;
            }
            let name = p.file_name()?.to_string_lossy().to_string();
            let dir = md.is_dir();
            Some(ResEntry {
                kind: if dir { "dir".into() } else { kind_of(&name).into() },
                children: if dir { std::fs::read_dir(&p).map(|d| d.count()).unwrap_or(0) } else { 0 },
                size: if dir { 0 } else { md.len() },
                replaced: !dir && replaced.iter().any(|x| x == &r),
                name,
                rel: r,
                dir,
            })
        })
        .collect();
    out.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResPreview {
    pub rel: String,
    pub kind: String,
    pub size: u64,
    pub mime: Option<String>,
    /// base64 (obrazy)
    pub data: Option<String>,
    /// treść tekstowa (xml/tekst), obcięta do limitu
    pub text: Option<String>,
    pub truncated: bool,
    pub nine_patch: bool,
    pub replaced: bool,
    pub replaceable: bool,
}

const MAX_IMAGE_PREVIEW: u64 = 12 * 1024 * 1024;
const MAX_TEXT_PREVIEW: usize = 512 * 1024;

fn mime_for(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        _ => "application/octet-stream",
    }
}

/// Plik można podmienić: grafiki w res/ i assets/ oraz dowolny plik w assets/.
fn replaceable(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    kind_of(name) == "image" || rel.starts_with("assets/")
}

pub fn preview(src: &Path, rel: &str, replaced: &[String]) -> Result<ResPreview> {
    let p = resolve_rel(src, rel)?;
    let md = std::fs::metadata(&p)?;
    if md.is_dir() {
        return Err(AdbError::InvalidArg("to jest folder / is a directory".into()));
    }
    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let ext = ext_of(&name);
    let kind = kind_of(&name);
    let mut pv = ResPreview {
        rel: rel.to_string(),
        kind: kind.into(),
        size: md.len(),
        mime: None,
        data: None,
        text: None,
        truncated: false,
        nine_patch: name.to_ascii_lowercase().ends_with(".9.png"),
        replaced: replaced.iter().any(|x| x == rel),
        replaceable: replaceable(rel),
    };
    match kind {
        "image" if md.len() <= MAX_IMAGE_PREVIEW => {
            let bytes = std::fs::read(&p)?;
            pv.mime = Some(mime_for(&ext).into());
            pv.data = Some(base64::engine::general_purpose::STANDARD.encode(bytes));
        }
        "xml" | "text" => {
            let bytes = std::fs::read(&p)?;
            pv.truncated = bytes.len() > MAX_TEXT_PREVIEW;
            let slice = &bytes[..bytes.len().min(MAX_TEXT_PREVIEW)];
            pv.text = Some(String::from_utf8_lossy(slice).to_string());
        }
        _ => {}
    }
    Ok(pv)
}

/// Rozpoznaje format obrazu po nagłówku.
fn sniff_image(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some("webp")
    } else if head.starts_with(b"GIF8") {
        Some("gif")
    } else if head.starts_with(b"BM") {
        Some("bmp")
    } else {
        None
    }
}

const MAX_REPLACE: u64 = 64 * 1024 * 1024;

/// Podmienia zasób plikiem z dysku (oryginał trafia do backup/).
/// Grafika musi mieć ten sam format co oryginał (aapt2 / odwołania po nazwie pliku).
pub fn replace(ws: &Workspace, rel: &str, new_file: &Path) -> Result<Vec<String>> {
    let src = ws.src();
    let target = resolve_rel(&src, rel)?;
    if !target.is_file() {
        return Err(AdbError::InvalidArg(format!("brak pliku / no such file: {rel}")));
    }
    if !replaceable(rel) {
        return Err(AdbError::InvalidArg(
            "Podmieniać można grafiki (res/, assets/) oraz pliki w assets/ / only images and assets can be replaced".into(),
        ));
    }
    let md = std::fs::metadata(new_file)?;
    if !md.is_file() || md.len() == 0 || md.len() > MAX_REPLACE {
        return Err(AdbError::InvalidArg("plik pusty lub za duży (max 64 MB) / empty or too large".into()));
    }
    let mut warnings = Vec::new();
    let name = target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if kind_of(&name) == "image" {
        let mut head = vec![0u8; 16];
        {
            use std::io::Read;
            let mut f = std::fs::File::open(new_file)?;
            let n = f.read(&mut head)?;
            head.truncate(n);
        }
        let want = match ext_of(&name).as_str() {
            "jpeg" => "jpg".to_string(),
            e => e.to_string(),
        };
        match sniff_image(&head) {
            Some(fmt) if fmt == want => {}
            Some(fmt) => {
                return Err(AdbError::InvalidArg(format!(
                    "Zły format: wybrany plik to {fmt}, a zasób to {want}. Przekonwertuj grafikę do {want} \
                     / format mismatch: {fmt} vs {want}"
                )))
            }
            None => {
                return Err(AdbError::InvalidArg(
                    "Wybrany plik nie jest obsługiwaną grafiką (PNG/JPG/WEBP/GIF/BMP) / not an image".into(),
                ))
            }
        }
        if name.to_ascii_lowercase().ends_with(".9.png") {
            warnings.push(
                "To grafika 9-patch (.9.png): nowa musi mieć ramkę 1 px z czarnymi znacznikami rozciągania, \
                 inaczej aapt2 przerwie budowanie / 9-patch needs a 1px marker border."
                    .to_string(),
            );
        }
    }
    let backup = ws.backup().join(rel);
    if !backup.exists() {
        if let Some(p) = backup.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::copy(&target, &backup)?;
    }
    std::fs::copy(new_file, &target)?;
    let mut meta = ws.load()?;
    if !meta.replaced.iter().any(|x| x == rel) {
        meta.replaced.push(rel.to_string());
    }
    ws.save(&meta)?;
    Ok(warnings)
}

pub fn restore(ws: &Workspace, rel: &str) -> Result<()> {
    let target = resolve_rel(&ws.src(), rel)?;
    let backup = ws.backup().join(rel);
    if !backup.is_file() {
        return Err(AdbError::InvalidArg("brak kopii oryginału / no backup".into()));
    }
    std::fs::copy(&backup, &target)?;
    let _ = std::fs::remove_file(&backup);
    let mut meta = ws.load()?;
    meta.replaced.retain(|x| x != rel);
    ws.save(&meta)?;
    Ok(())
}

/// Eksport podmienianego pliku (np. żeby edytować grafikę w zewnętrznym programie).
pub fn export_file(src: &Path, rel: &str, dest: &Path) -> Result<()> {
    let p = resolve_rel(src, rel)?;
    if !p.is_file() {
        return Err(AdbError::InvalidArg(format!("brak pliku / no such file: {rel}")));
    }
    std::fs::copy(p, dest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_paths_are_confined() {
        let d = std::env::temp_dir().join(format!("adbst-ws-{}", std::process::id()));
        std::fs::create_dir_all(d.join("res/drawable")).unwrap();
        std::fs::write(d.join("res/drawable/a.png"), b"\x89PNG\r\n\x1a\n0000").unwrap();
        assert!(resolve_rel(&d, "res/drawable/a.png").is_ok());
        assert!(resolve_rel(&d, "/res/drawable/a.png").is_ok());
        for bad in ["../etc/passwd", "res/../../x", "lib/x.so", "AndroidManifest.xml", "res\\x", "C:/x", "res/./a"] {
            assert!(resolve_rel(&d, bad).is_err(), "{bad}");
        }
        let l = list_dir(&d, "", &[]).unwrap();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].rel, "res");
        let l = list_dir(&d, "res/drawable", &["res/drawable/a.png".into()]).unwrap();
        assert!(l[0].replaced && l[0].kind == "image");
        let pv = preview(&d, "res/drawable/a.png", &[]).unwrap();
        assert!(pv.data.is_some() && pv.replaceable);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sniffs_images() {
        assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\nxxxx"), Some("png"));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("jpg"));
        assert_eq!(sniff_image(b"hello"), None);
    }

    #[test]
    fn ids_validated() {
        let d = std::env::temp_dir();
        assert!(Workspace::open(&d, "../x").is_err());
        assert!(Workspace::open(&d, "a/b").is_err());
        assert!(Workspace::new_id("org.x y").starts_with("org.x_y-"));
    }
}
