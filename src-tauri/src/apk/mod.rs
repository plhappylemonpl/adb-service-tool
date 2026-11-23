//! Odczyt metadanych APK: nazwa aplikacji (label), wersja, oryginalna ikona.
//!
//! Ścieżka: ZIP -> AndroidManifest.xml (AXML) -> `<application android:icon>`
//! -> resources.arsc -> plik ikony o najwyższej gęstości (png/webp).
//! Dla ikon adaptacyjnych (XML `adaptive-icon`) wyciągamy warstwę
//! `foreground` (bitmapa) i kolor/bitmapę `background` - GUI składa je w CSS.

pub mod arsc;
pub mod axml;
mod bytes;
pub mod zip;

use arsc::{Arsc, Cfg, Val};
use axml::{XmlElem, TYPE_REFERENCE, TYPE_STRING};
use zip::{ApkSource, Zip};

use crate::error::{AdbError, Result};

// android.R.attr.*
const ATTR_LABEL: u32 = 0x0101_0001;
const ATTR_ICON: u32 = 0x0101_0002;
const ATTR_DRAWABLE: u32 = 0x0101_0199;
const ATTR_VERSION_NAME: u32 = 0x0101_021c;
const ATTR_ROUND_ICON: u32 = 0x0101_052c;

#[derive(Debug, Clone, Default)]
pub struct IconData {
    pub mime: String,
    pub data: Vec<u8>,
    /// Tło ikony adaptacyjnej: kolor CSS (#rrggbb / rgba) ...
    pub bg_color: Option<String>,
    /// ... lub bitmapa tła
    pub bg_image: Option<(String, Vec<u8>)>,
    pub adaptive: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ApkDetails {
    pub label: Option<String>,
    pub version_name: Option<String>,
    pub icon: Option<IconData>,
}

pub async fn read_details(src: &ApkSource, lang: &str) -> Result<ApkDetails> {
    let zip = Zip::open(src).await?;
    let manifest_bytes = zip.read(src, "AndroidManifest.xml").await?;
    let elems = axml::parse(&manifest_bytes).ok_or_else(|| AdbError::Parse("AndroidManifest.xml".into()))?;

    let arsc_bytes = if zip.has("resources.arsc") {
        zip.read(src, "resources.arsc").await.ok()
    } else {
        None
    };
    let arsc = arsc_bytes.as_deref().and_then(Arsc::parse);

    let manifest = elems.iter().find(|e| e.name == "manifest");
    let app = elems.iter().find(|e| e.name == "application");

    let mut d = ApkDetails::default();
    if let Some(app) = app {
        d.label = attr_string(app, ATTR_LABEL, "label", arsc.as_ref(), lang);
    }
    if let Some(m) = manifest {
        d.version_name = attr_string(m, ATTR_VERSION_NAME, "versionName", arsc.as_ref(), lang);
    }

    // --- ikona ---
    if let (Some(app), Some(arsc)) = (app, arsc.as_ref()) {
        for (id, name) in [(ATTR_ICON, "icon"), (ATTR_ROUND_ICON, "roundIcon")] {
            let Some(a) = app.attr(id, name) else { continue };
            if a.typ != TYPE_REFERENCE || a.data == 0 {
                continue;
            }
            if let Some(icon) = icon_from_res(&zip, src, arsc, a.data).await {
                d.icon = Some(icon);
                break;
            }
        }
    }

    // fallback heurystyczny: typowe nazwy w mipmap-*
    if d.icon.is_none() {
        if let Some(name) = heuristic_icon(&zip) {
            if let Ok(data) = zip.read(src, &name).await {
                d.icon = Some(IconData {
                    mime: mime_for(&name).into(),
                    data,
                    ..Default::default()
                });
            }
        }
    }
    Ok(d)
}

/// Atrybut tekstowy: surowy string albo referencja do resources.arsc.
fn attr_string(e: &XmlElem, id: u32, name: &str, arsc: Option<&Arsc>, lang: &str) -> Option<String> {
    let a = e.attr(id, name)?;
    if let Some(raw) = &a.raw {
        if !raw.is_empty() {
            return Some(raw.clone());
        }
    }
    if a.typ == TYPE_REFERENCE {
        let vals = arsc?.resolve(a.data);
        let strings: Vec<(Cfg, String)> = vals
            .into_iter()
            .filter_map(|(c, v)| match v {
                Val::Str(s) if !s.is_empty() => Some((c, s)),
                _ => None,
            })
            .collect();
        return strings
            .iter()
            .find(|(c, _)| c.lang == lang)
            .or_else(|| strings.iter().find(|(c, _)| c.lang.is_empty()))
            .or_else(|| strings.first())
            .map(|(_, s)| s.clone());
    }
    if a.typ == TYPE_STRING {
        return a.raw.clone();
    }
    None
}

fn is_bitmap(p: &str) -> bool {
    let l = p.to_ascii_lowercase();
    (l.ends_with(".png") || l.ends_with(".webp") || l.ends_with(".jpg")) && !l.ends_with(".9.png")
}

fn mime_for(p: &str) -> &'static str {
    let l = p.to_ascii_lowercase();
    if l.ends_with(".webp") {
        "image/webp"
    } else if l.ends_with(".jpg") {
        "image/jpeg"
    } else {
        "image/png"
    }
}

fn density_rank(d: u16) -> u32 {
    match d {
        0 => 160,
        0xfffe => 1, // nodpi
        0xffff => 0, // anydpi (zwykle XML)
        x => x as u32,
    }
}

/// Najlepsza bitmapa: najwyższa gęstość, bez trybu nocnego.
fn pick_bitmap(vals: &[(Cfg, Val)]) -> Option<String> {
    vals.iter()
        .filter_map(|(c, v)| match v {
            Val::Str(s) if is_bitmap(s) && !c.night => Some((density_rank(c.density), s)),
            _ => None,
        })
        .max_by_key(|(d, _)| *d)
        .map(|(_, s)| s.clone())
}

fn pick_xml(vals: &[(Cfg, Val)]) -> Option<String> {
    vals.iter()
        .filter_map(|(c, v)| match v {
            Val::Str(s) if s.ends_with(".xml") && !c.night => Some((density_rank(c.density), s)),
            _ => None,
        })
        .max_by_key(|(d, _)| *d)
        .map(|(_, s)| s.clone())
}

fn css_color(argb: u32) -> String {
    let a = (argb >> 24) & 0xff;
    let r = (argb >> 16) & 0xff;
    let g = (argb >> 8) & 0xff;
    let b = argb & 0xff;
    if a == 0xff {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("rgba({r},{g},{b},{:.3})", a as f32 / 255.0)
    }
}

async fn icon_from_res(zip: &Zip, src: &ApkSource, arsc: &Arsc<'_>, id: u32) -> Option<IconData> {
    let vals = arsc.resolve(id);

    if let Some(path) = pick_bitmap(&vals) {
        let data = zip.read(src, &path).await.ok()?;
        return Some(IconData {
            mime: mime_for(&path).into(),
            data,
            ..Default::default()
        });
    }

    // adaptive-icon (XML): foreground + background
    let xml_path = pick_xml(&vals)?;
    let xml = zip.read(src, &xml_path).await.ok()?;
    let elems = axml::parse(&xml)?;
    if !elems.iter().any(|e| e.name == "adaptive-icon") {
        return None;
    }
    let layer = |name: &str| {
        elems
            .iter()
            .find(|e| e.name == name)
            .and_then(|e| e.attr(ATTR_DRAWABLE, "drawable").cloned())
    };

    let fg = layer("foreground")?;
    let fg_vals = if fg.typ == TYPE_REFERENCE { arsc.resolve(fg.data) } else { vec![] };
    let fg_path = pick_bitmap(&fg_vals)?;
    let fg_data = zip.read(src, &fg_path).await.ok()?;

    let mut icon = IconData {
        mime: mime_for(&fg_path).into(),
        data: fg_data,
        adaptive: true,
        ..Default::default()
    };

    if let Some(bg) = layer("background") {
        match bg.typ {
            0x1c..=0x1f => {
                if let Val::Color(c) = arsc_color(bg.typ, bg.data) {
                    icon.bg_color = Some(css_color(c));
                }
            }
            TYPE_REFERENCE => {
                let bv = arsc.resolve(bg.data);
                if let Some((_, Val::Color(c))) = bv.iter().find(|(c, v)| !c.night && matches!(v, Val::Color(_))) {
                    icon.bg_color = Some(css_color(*c));
                } else if let Some(p) = pick_bitmap(&bv) {
                    if let Ok(data) = zip.read(src, &p).await {
                        icon.bg_image = Some((mime_for(&p).into(), data));
                    }
                }
            }
            _ => {}
        }
    }
    if icon.bg_color.is_none() && icon.bg_image.is_none() {
        icon.bg_color = Some("#ffffff".into());
    }
    Some(icon)
}

/// Kolor zapisany bezpośrednio w atrybucie XML.
fn arsc_color(typ: u8, data: u32) -> Val {
    match typ {
        0x1c => Val::Color(data),
        0x1d => Val::Color(0xff00_0000 | data),
        0x1e | 0x1f => {
            let n = |s: u32| ((data >> s) & 0xf) * 0x11;
            let a = if typ == 0x1f { 0xff } else { n(12) };
            Val::Color((a << 24) | (n(8) << 16) | (n(4) << 8) | n(0))
        }
        _ => Val::Other,
    }
}

/// Gdy resources.arsc zawiedzie: res/mipmap-*/ic_launcher*.png|webp, najwyższa gęstość.
fn heuristic_icon(zip: &Zip) -> Option<String> {
    let rank = |n: &str| -> u32 {
        for (k, r) in [("xxxhdpi", 640), ("xxhdpi", 480), ("xhdpi", 320), ("hdpi", 240), ("mdpi", 160)] {
            if n.contains(k) {
                return r;
            }
        }
        100
    };
    zip.names()
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.starts_with("res/") && l.contains("ic_launcher") && !l.contains("foreground") && is_bitmap(&l)
        })
        .max_by_key(|n| (rank(n), !n.contains("round")))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test na prawdziwych APK: `APK_TEST_DIR=/ścieżka cargo test apk_real -- --nocapture`
    #[tokio::test]
    async fn apk_real() {
        let Ok(dir) = std::env::var("APK_TEST_DIR") else { return };
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if p.extension().map(|x| x != "apk").unwrap_or(true) {
                continue;
            }
            let size = std::fs::metadata(&p).unwrap().len();
            let src = ApkSource::Local { path: p.clone(), size };
            let d = read_details(&src, "pl").await.expect("parse");
            let ic = d.icon.as_ref();
            println!(
                "{}: label={:?} ver={:?} icon={:?} {}B adaptive={:?} bg={:?}",
                p.display(),
                d.label,
                d.version_name,
                ic.map(|i| i.mime.clone()),
                ic.map(|i| i.data.len()).unwrap_or(0),
                ic.map(|i| i.adaptive),
                ic.and_then(|i| i.bg_color.clone())
            );
            if let Some(i) = ic {
                let ext = if i.mime.contains("webp") { "webp" } else { "png" };
                std::fs::write(p.with_extension(format!("icon.{ext}")), &i.data).unwrap();
            }
            assert!(d.label.is_some());
        }
    }
}
