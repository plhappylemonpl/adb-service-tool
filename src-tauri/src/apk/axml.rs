//! Parser binarnego XML Androida (AXML) - `AndroidManifest.xml`,
//! `res/mipmap-anydpi-v26/ic_launcher.xml` (adaptive-icon) itp.
//!
//! Zwraca płaską listę elementów - wystarcza do znalezienia
//! `<application android:icon=...>` czy `<foreground android:drawable=...>`.

use super::bytes::{parse_string_pool, u16_at, u32_at, u8_at};

pub const RES_XML_TYPE: u16 = 0x0003;
const RES_STRING_POOL_TYPE: u16 = 0x0001;
const RES_XML_RESOURCE_MAP_TYPE: u16 = 0x0180;
const RES_XML_START_ELEMENT_TYPE: u16 = 0x0102;

// typy wartości (Res_value::dataType)
pub const TYPE_REFERENCE: u8 = 0x01;
pub const TYPE_STRING: u8 = 0x03;

#[derive(Debug, Clone)]
pub struct XmlAttr {
    pub name: String,
    pub res_id: u32,
    pub raw: Option<String>,
    pub typ: u8,
    pub data: u32,
}

#[derive(Debug, Clone)]
pub struct XmlElem {
    pub name: String,
    pub attrs: Vec<XmlAttr>,
}

impl XmlElem {
    /// Atrybut po ID zasobu (odporne na obfuskację nazw) lub po nazwie.
    pub fn attr(&self, res_id: u32, name: &str) -> Option<&XmlAttr> {
        self.attrs
            .iter()
            .find(|a| a.res_id == res_id)
            .or_else(|| self.attrs.iter().find(|a| a.name == name))
    }
}

pub fn parse(buf: &[u8]) -> Option<Vec<XmlElem>> {
    if buf.len() < 8 || u16_at(buf, 0) != RES_XML_TYPE {
        return None;
    }
    let header_size = u16_at(buf, 2) as usize;
    let total = (u32_at(buf, 4) as usize).min(buf.len());

    let mut strings: Vec<String> = Vec::new();
    let mut resmap: Vec<u32> = Vec::new();
    let mut out = Vec::new();
    let mut off = header_size.max(8);

    while off + 8 <= total {
        let typ = u16_at(buf, off);
        let hsz = u16_at(buf, off + 2) as usize;
        let size = u32_at(buf, off + 4) as usize;
        if size < 8 || off + size > total {
            break;
        }
        let chunk = &buf[off..off + size];
        match typ {
            RES_STRING_POOL_TYPE => strings = parse_string_pool(chunk),
            RES_XML_RESOURCE_MAP_TYPE => {
                resmap = (hsz..size).step_by(4).map(|i| u32_at(chunk, i)).collect();
            }
            RES_XML_START_ELEMENT_TYPE => {
                let ext = hsz;
                let name_idx = u32_at(chunk, ext + 4) as usize;
                let attr_start = u16_at(chunk, ext + 8) as usize;
                let attr_size = (u16_at(chunk, ext + 10) as usize).max(20);
                let count = u16_at(chunk, ext + 12) as usize;
                let mut attrs = Vec::with_capacity(count.min(64));
                for i in 0..count {
                    let a = ext + attr_start + i * attr_size;
                    if a + 20 > size {
                        break;
                    }
                    let an = u32_at(chunk, a + 4) as usize;
                    let raw = u32_at(chunk, a + 8);
                    attrs.push(XmlAttr {
                        name: strings.get(an).cloned().unwrap_or_default(),
                        res_id: resmap.get(an).copied().unwrap_or(0),
                        raw: if raw == u32::MAX {
                            None
                        } else {
                            strings.get(raw as usize).cloned()
                        },
                        typ: u8_at(chunk, a + 15),
                        data: u32_at(chunk, a + 16),
                    });
                }
                out.push(XmlElem {
                    name: strings.get(name_idx).cloned().unwrap_or_default(),
                    attrs,
                });
            }
            _ => {}
        }
        off += size;
    }
    Some(out)
}
