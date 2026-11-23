//! Parser `flashfile.xml` / `servicefile.xml` z firmware Motoroli.
//!
//! Format (fragment):
//! ```xml
//! <flashing>
//!   <header>
//!     <phone_model model="hawao_g"/>
//!     <software_version version="hawao_g-user 13 T3TS33..."/>
//!   </header>
//!   <steps interface="AP">
//!     <step operation="getvar" var="max-sparse-size"/>
//!     <step operation="oem" var="fb_mode_set"/>
//!     <step operation="flash" partition="partition" filename="gpt.bin" MD5="..."/>
//!     <step operation="erase" partition="userdata"/>
//!     <step operation="oem" var="fb_mode_clear"/>
//!   </steps>
//! </flashing>
//! ```
//! `flashfile.xml` = pełny flash (kasuje dane), `servicefile.xml` = bez
//! kasowania `userdata`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{AdbError, Result};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum StepOp {
    Flash,
    Erase,
    Getvar,
    Oem,
    RebootBootloader,
    Reboot,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MotoStep {
    pub index: usize,
    pub op: StepOp,
    pub operation: String,
    pub partition: Option<String>,
    pub filename: Option<String>,
    pub var: Option<String>,
    pub md5: Option<String>,
    /// rozmiar pliku (gdy istnieje na dysku)
    pub size: Option<u64>,
    pub file_exists: bool,
    /// Komenda do wyświetlenia, np. `fastboot flash boot boot.img`
    pub command: String,
    /// erase userdata / metadata - kasuje dane użytkownika
    pub wipes_data: bool,
}

impl MotoStep {
    /// Argumenty dla binarki `fastboot` (ścieżki plików bezwzględne).
    pub fn fastboot_args(&self, dir: &Path) -> Option<Vec<String>> {
        match self.op {
            StepOp::Flash => Some(vec![
                "flash".into(),
                self.partition.clone()?,
                dir.join(self.filename.as_ref()?).to_string_lossy().to_string(),
            ]),
            StepOp::Erase => Some(vec!["erase".into(), self.partition.clone()?]),
            StepOp::Getvar => Some(vec!["getvar".into(), self.var.clone()?]),
            StepOp::Oem => {
                let mut v = vec!["oem".to_string()];
                v.extend(self.var.as_ref()?.split_whitespace().map(str::to_string));
                Some(v)
            }
            StepOp::RebootBootloader => Some(vec!["reboot-bootloader".into()]),
            StepOp::Reboot => Some(vec!["reboot".into()]),
            StepOp::Unknown => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MotoFirmware {
    pub dir: String,
    pub xml_file: String,
    pub phone_model: Option<String>,
    pub software_version: Option<String>,
    pub steps: Vec<MotoStep>,
    pub missing_files: Vec<String>,
    pub total_bytes: u64,
    pub wipes_data: bool,
    /// Inne pliki XML znalezione w folderze (np. servicefile.xml obok flashfile.xml)
    pub alternatives: Vec<String>,
}

/// Parsuje sam XML (bez dostępu do dysku).
pub fn parse_xml(xml: &str) -> Result<(Option<String>, Option<String>, Vec<MotoStep>)> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| AdbError::Parse(format!("XML: {e}")))?;
    let attr_ci = |n: &roxmltree::Node, name: &str| -> Option<String> {
        n.attributes()
            .find(|a| a.name().eq_ignore_ascii_case(name))
            .map(|a| a.value().trim().to_string())
            .filter(|v| !v.is_empty())
    };

    let model = doc
        .descendants()
        .find(|n| n.has_tag_name("phone_model"))
        .and_then(|n| attr_ci(&n, "model"));
    let version = doc
        .descendants()
        .find(|n| n.has_tag_name("software_version"))
        .and_then(|n| attr_ci(&n, "version"));

    let mut steps = Vec::new();
    for (i, n) in doc.descendants().filter(|n| n.has_tag_name("step")).enumerate() {
        let operation = attr_ci(&n, "operation").unwrap_or_default().to_lowercase();
        let partition = attr_ci(&n, "partition");
        let filename = attr_ci(&n, "filename");
        let var = attr_ci(&n, "var");
        let md5 = attr_ci(&n, "md5").map(|s| s.to_lowercase());
        let op = match operation.as_str() {
            "flash" => StepOp::Flash,
            "erase" => StepOp::Erase,
            "getvar" => StepOp::Getvar,
            "oem" => StepOp::Oem,
            "reboot-bootloader" | "reboot_bootloader" => StepOp::RebootBootloader,
            "reboot" => StepOp::Reboot,
            _ => StepOp::Unknown,
        };
        // nazwy plików nie mogą wychodzić poza folder firmware
        if let Some(f) = &filename {
            if f.contains("..") || f.starts_with('/') || f.starts_with('\\') || f.contains(':') {
                return Err(AdbError::Parse(format!("niebezpieczna ścieżka w XML / unsafe filename: {f}")));
            }
        }
        let wipes_data = op == StepOp::Erase
            && matches!(partition.as_deref(), Some("userdata") | Some("metadata") | Some("cache"));
        let command = match op {
            StepOp::Flash => format!(
                "fastboot flash {} {}",
                partition.as_deref().unwrap_or("?"),
                filename.as_deref().unwrap_or("?")
            ),
            StepOp::Erase => format!("fastboot erase {}", partition.as_deref().unwrap_or("?")),
            StepOp::Getvar => format!("fastboot getvar {}", var.as_deref().unwrap_or("?")),
            StepOp::Oem => format!("fastboot oem {}", var.as_deref().unwrap_or("?")),
            StepOp::RebootBootloader => "fastboot reboot-bootloader".into(),
            StepOp::Reboot => "fastboot reboot".into(),
            StepOp::Unknown => format!("(nieobsługiwane / unsupported: {operation})"),
        };
        steps.push(MotoStep {
            index: i,
            op,
            operation,
            partition,
            filename,
            var,
            md5,
            size: None,
            file_exists: true,
            command,
            wipes_data,
        });
    }
    if steps.is_empty() {
        return Err(AdbError::Parse("XML nie zawiera kroków <step> / no steps".into()));
    }
    Ok((model, version, steps))
}

/// Szuka pliku XML w folderze firmware (preferowany: wskazany, potem flashfile, servicefile).
fn find_xml(dir: &Path, prefer: Option<&str>) -> Result<(PathBuf, Vec<String>)> {
    let mut xmls: Vec<String> = std::fs::read_dir(dir)?
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| {
            let l = n.to_lowercase();
            l.ends_with(".xml") && (l.contains("flashfile") || l.contains("servicefile"))
        })
        .collect();
    xmls.sort();
    let pick = prefer
        .and_then(|p| xmls.iter().find(|x| x.eq_ignore_ascii_case(p)).cloned())
        .or_else(|| xmls.iter().find(|x| x.eq_ignore_ascii_case("flashfile.xml")).cloned())
        .or_else(|| xmls.iter().find(|x| x.eq_ignore_ascii_case("servicefile.xml")).cloned())
        .or_else(|| xmls.first().cloned())
        .ok_or_else(|| {
            AdbError::Other(
                "W folderze nie ma flashfile.xml ani servicefile.xml - wskaż rozpakowany firmware / no flashfile.xml".into(),
            )
        })?;
    let others = xmls.into_iter().filter(|x| *x != pick).collect();
    Ok((dir.join(pick), others))
}

/// Wczytuje firmware z folderu: XML + sprawdzenie plików i rozmiarów.
pub fn load(dir: &Path, prefer: Option<&str>) -> Result<MotoFirmware> {
    let (xml_path, alternatives) = find_xml(dir, prefer)?;
    let xml = std::fs::read_to_string(&xml_path)?;
    let (phone_model, software_version, mut steps) = parse_xml(&xml)?;

    let mut missing = Vec::new();
    let mut total = 0u64;
    for s in steps.iter_mut() {
        if let Some(f) = &s.filename {
            match std::fs::metadata(dir.join(f)) {
                Ok(m) => {
                    s.size = Some(m.len());
                    if s.op == StepOp::Flash {
                        total += m.len();
                    }
                }
                Err(_) => {
                    s.file_exists = false;
                    if !missing.contains(f) {
                        missing.push(f.clone());
                    }
                }
            }
        }
    }
    let wipes_data = steps.iter().any(|s| s.wipes_data);
    Ok(MotoFirmware {
        dir: dir.to_string_lossy().to_string(),
        xml_file: xml_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        phone_model,
        software_version,
        steps,
        missing_files: missing,
        total_bytes: total,
        wipes_data,
        alternatives,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0"?>
<flashing>
  <header>
    <phone_model model="hawao_g"/>
    <software_version version="hawao_g-user 13 T3TS33.11"/>
  </header>
  <steps interface="AP">
    <step operation="getvar" var="max-sparse-size"/>
    <step operation="oem" var="fb_mode_set"/>
    <step operation="flash" partition="partition" filename="gpt.bin" MD5="ABCDEF0123"/>
    <step operation="flash" partition="boot" filename="boot.img" MD5="11"/>
    <step operation="erase" partition="carrier"/>
    <step operation="erase" partition="userdata"/>
    <step operation="oem" var="fb_mode_clear"/>
  </steps>
</flashing>"#;

    #[test]
    fn parses_flashfile() {
        let (m, v, steps) = parse_xml(XML).unwrap();
        assert_eq!(m.as_deref(), Some("hawao_g"));
        assert!(v.unwrap().contains("T3TS33"));
        assert_eq!(steps.len(), 7);
        assert_eq!(steps[2].command, "fastboot flash partition gpt.bin");
        assert_eq!(steps[2].md5.as_deref(), Some("abcdef0123"));
        assert!(steps[5].wipes_data);
        assert!(!steps[4].wipes_data);
        let args = steps[3].fastboot_args(Path::new("/fw")).unwrap();
        assert_eq!(args[0], "flash");
        assert_eq!(args[1], "boot");
        assert!(args[2].ends_with("boot.img"));
        assert_eq!(steps[1].fastboot_args(Path::new("/fw")).unwrap(), vec!["oem", "fb_mode_set"]);
    }

    #[test]
    fn rejects_traversal() {
        let bad = r#"<flashing><steps><step operation="flash" partition="boot" filename="../../etc/passwd"/></steps></flashing>"#;
        assert!(parse_xml(bad).is_err());
    }
}
