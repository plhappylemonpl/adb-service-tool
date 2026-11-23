//! Minimalny parser `resources.arsc` - tylko tyle, ile trzeba, by rozwiązać
//! identyfikator zasobu (0xPPTTEEEE) na wartości per-konfiguracja:
//! ścieżkę pliku (ikona), string (nazwa aplikacji) lub kolor (tło ikony).
//!
//! Obsługuje: wpisy sparse (FLAG_SPARSE), offsety 16-bit (FLAG_OFFSET16),
//! wpisy compact (Android 14, FLAG_COMPACT).

use super::bytes::{parse_string_pool, u16_at, u32_at, u8_at};

const RES_TABLE_TYPE: u16 = 0x0002;
const RES_STRING_POOL_TYPE: u16 = 0x0001;
const RES_TABLE_PACKAGE_TYPE: u16 = 0x0200;
const RES_TABLE_TYPE_TYPE: u16 = 0x0201;

const FLAG_SPARSE: u8 = 0x01;
const FLAG_OFFSET16: u8 = 0x02;
const ENTRY_FLAG_COMPLEX: u16 = 0x0001;
const ENTRY_FLAG_COMPACT: u16 = 0x0008;

#[derive(Debug, Clone, Default)]
pub struct Cfg {
    /// 0 = domyślna (mdpi), 0xFFFE = nodpi, 0xFFFF = anydpi
    pub density: u16,
    pub lang: String,
    pub night: bool,
}

#[derive(Debug, Clone)]
pub enum Val {
    Str(String),
    Ref(u32),
    /// ARGB
    Color(u32),
    Other,
}

struct TypeChunk {
    pkg_id: u32,
    type_id: u8,
    off: usize,
    hsz: usize,
    flags: u8,
    entry_count: usize,
    entries_start: usize,
    cfg: Cfg,
}

pub struct Arsc<'a> {
    buf: &'a [u8],
    strings: Vec<String>,
    types: Vec<TypeChunk>,
    single_pkg: Option<u32>,
}

impl<'a> Arsc<'a> {
    pub fn parse(buf: &'a [u8]) -> Option<Self> {
        if u16_at(buf, 0) != RES_TABLE_TYPE {
            return None;
        }
        let hsz = u16_at(buf, 2) as usize;
        let total = (u32_at(buf, 4) as usize).min(buf.len());
        let mut strings = Vec::new();
        let mut types = Vec::new();
        let mut pkg_ids = Vec::new();

        let mut off = hsz.max(12);
        while off + 8 <= total {
            let typ = u16_at(buf, off);
            let size = u32_at(buf, off + 4) as usize;
            if size < 8 || off + size > total {
                break;
            }
            match typ {
                RES_STRING_POOL_TYPE if strings.is_empty() => {
                    strings = parse_string_pool(&buf[off..off + size]);
                }
                RES_TABLE_PACKAGE_TYPE => {
                    let id = u32_at(buf, off + 8);
                    pkg_ids.push(id);
                    parse_package(buf, off, size, id, &mut types);
                }
                _ => {}
            }
            off += size;
        }
        let single_pkg = (pkg_ids.len() == 1).then(|| pkg_ids[0]);
        Some(Self {
            buf,
            strings,
            types,
            single_pkg,
        })
    }

    /// Wszystkie wartości zasobu `id` we wszystkich konfiguracjach.
    pub fn lookup(&self, id: u32) -> Vec<(Cfg, Val)> {
        let pkg = id >> 24;
        let type_id = ((id >> 16) & 0xff) as u8;
        let entry = (id & 0xffff) as usize;
        let b = self.buf;
        let mut out = Vec::new();

        for t in self.types.iter().filter(|t| t.type_id == type_id) {
            if t.pkg_id != pkg && self.single_pkg != Some(t.pkg_id) {
                continue;
            }
            let offs = t.off + t.hsz;
            let eoff: Option<usize> = if t.flags & FLAG_SPARSE != 0 {
                (0..t.entry_count)
                    .map(|i| offs + i * 4)
                    .find(|&p| u16_at(b, p) as usize == entry)
                    .map(|p| u16_at(b, p + 2) as usize * 4)
            } else if t.flags & FLAG_OFFSET16 != 0 {
                if entry >= t.entry_count {
                    None
                } else {
                    let v = u16_at(b, offs + entry * 2);
                    (v != 0xffff).then_some(v as usize * 4)
                }
            } else if entry >= t.entry_count {
                None
            } else {
                let v = u32_at(b, offs + entry * 4);
                (v != u32::MAX).then_some(v as usize)
            };
            let Some(eo) = eoff else { continue };
            let e = t.off + t.entries_start + eo;
            if e + 8 > b.len() {
                continue;
            }
            let esize = u16_at(b, e) as usize;
            let eflags = u16_at(b, e + 2);
            let (dtype, data) = if eflags & ENTRY_FLAG_COMPACT != 0 {
                ((eflags >> 8) as u8, u32_at(b, e + 4))
            } else if eflags & ENTRY_FLAG_COMPLEX != 0 {
                continue;
            } else {
                let v = e + esize;
                (u8_at(b, v + 3), u32_at(b, v + 4))
            };
            out.push((t.cfg.clone(), self.to_val(dtype, data)));
        }
        out
    }

    fn to_val(&self, dtype: u8, data: u32) -> Val {
        match dtype {
            0x01 => Val::Ref(data),
            0x03 => self
                .strings
                .get(data as usize)
                .map(|s| Val::Str(s.clone()))
                .unwrap_or(Val::Other),
            0x1c => Val::Color(data),
            0x1d => Val::Color(0xff00_0000 | data),
            0x1e | 0x1f => {
                // #ARGB / #RGB (4 bity na kanał) -> 8 bitów
                let n = |s: u32| ((data >> s) & 0xf) * 0x11;
                let a = if dtype == 0x1f { 0xff } else { n(12) };
                Val::Color((a << 24) | (n(8) << 16) | (n(4) << 8) | n(0))
            }
            _ => Val::Other,
        }
    }

    /// Rozwija referencje rekurencyjnie (max 6 poziomów).
    pub fn resolve(&self, id: u32) -> Vec<(Cfg, Val)> {
        let mut out = Vec::new();
        self.resolve_into(id, 0, &mut out);
        out
    }

    fn resolve_into(&self, id: u32, depth: u32, out: &mut Vec<(Cfg, Val)>) {
        if depth > 6 {
            return;
        }
        for (cfg, v) in self.lookup(id) {
            match v {
                Val::Ref(r) if r != 0 && r != id => self.resolve_into(r, depth + 1, out),
                other => out.push((cfg, other)),
            }
        }
    }
}

fn parse_package(buf: &[u8], start: usize, size: usize, pkg_id: u32, types: &mut Vec<TypeChunk>) {
    let hsz = u16_at(buf, start + 2) as usize;
    let end = start + size;
    let mut off = start + hsz.max(8);
    while off + 8 <= end {
        let typ = u16_at(buf, off);
        let thsz = u16_at(buf, off + 2) as usize;
        let csz = u32_at(buf, off + 4) as usize;
        if csz < 8 || off + csz > end {
            break;
        }
        if typ == RES_TABLE_TYPE_TYPE {
            let c = off + 20; // ResTable_config
            let cfg_size = u32_at(buf, c) as usize;
            let density = if cfg_size >= 16 { u16_at(buf, c + 14) } else { 0 };
            let l0 = u8_at(buf, c + 8);
            let l1 = u8_at(buf, c + 9);
            let lang = if l0 != 0 && l0 & 0x80 == 0 && l1 != 0 {
                format!("{}{}", l0 as char, l1 as char)
            } else {
                String::new()
            };
            let ui_mode = if cfg_size >= 30 { u8_at(buf, c + 29) } else { 0 };
            types.push(TypeChunk {
                pkg_id,
                type_id: u8_at(buf, off + 8),
                off,
                hsz: thsz,
                flags: u8_at(buf, off + 9),
                entry_count: u32_at(buf, off + 12) as usize,
                entries_start: u32_at(buf, off + 16) as usize,
                cfg: Cfg {
                    density,
                    lang,
                    night: ui_mode & 0x30 == 0x20,
                },
            });
        }
        off += csz;
    }
}
