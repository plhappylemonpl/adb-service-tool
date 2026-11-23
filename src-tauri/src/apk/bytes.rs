//! Bezpieczne (bez paniki) odczyty little-endian z bufora.
//! Dane z APK traktujemy jak niezaufane - poza zakresem zwracamy 0.

#[inline]
pub fn u8_at(b: &[u8], o: usize) -> u8 {
    b.get(o).copied().unwrap_or(0)
}

#[inline]
pub fn u16_at(b: &[u8], o: usize) -> u16 {
    match b.get(o..o.saturating_add(2)) {
        Some(s) => u16::from_le_bytes([s[0], s[1]]),
        None => 0,
    }
}

#[inline]
pub fn u32_at(b: &[u8], o: usize) -> u32 {
    match b.get(o..o.saturating_add(4)) {
        Some(s) => u32::from_le_bytes([s[0], s[1], s[2], s[3]]),
        None => 0,
    }
}

#[inline]
pub fn u64_at(b: &[u8], o: usize) -> u64 {
    match b.get(o..o.saturating_add(8)) {
        Some(s) => {
            let mut a = [0u8; 8];
            a.copy_from_slice(s);
            u64::from_le_bytes(a)
        }
        None => 0,
    }
}

/// Pula stringów (ResStringPool) - wspólna dla binarnego XML i resources.arsc.
pub fn parse_string_pool(chunk: &[u8]) -> Vec<String> {
    let hsz = u16_at(chunk, 2) as usize;
    let count = u32_at(chunk, 8) as usize;
    let flags = u32_at(chunk, 16);
    let strings_start = u32_at(chunk, 20) as usize;
    let utf8 = flags & 0x100 != 0;

    let mut v = Vec::with_capacity(count.min(1 << 16));
    for i in 0..count {
        let op = hsz + i * 4;
        if op + 4 > chunk.len() {
            break;
        }
        let so = strings_start.saturating_add(u32_at(chunk, op) as usize);
        v.push(read_pool_string(chunk, so, utf8).unwrap_or_default());
    }
    v
}

fn read_pool_string(c: &[u8], mut p: usize, utf8: bool) -> Option<String> {
    if utf8 {
        // długość w znakach UTF-16 (1-2 bajty), potem długość w bajtach UTF-8 (1-2 bajty)
        let (_, n1) = len8(c, p)?;
        p += n1;
        let (blen, n2) = len8(c, p)?;
        p += n2;
        let bytes = c.get(p..p.checked_add(blen)?)?;
        Some(String::from_utf8_lossy(bytes).into_owned())
    } else {
        let (clen, n) = len16(c, p)?;
        p += n;
        let raw = c.get(p..p.checked_add(clen.checked_mul(2)?)?)?;
        let units: Vec<u16> = raw.chunks_exact(2).map(|w| u16::from_le_bytes([w[0], w[1]])).collect();
        Some(String::from_utf16_lossy(&units))
    }
}

fn len8(c: &[u8], p: usize) -> Option<(usize, usize)> {
    let b = *c.get(p)? as usize;
    if b & 0x80 != 0 {
        let b2 = *c.get(p + 1)? as usize;
        Some((((b & 0x7f) << 8) | b2, 2))
    } else {
        Some((b, 1))
    }
}

fn len16(c: &[u8], p: usize) -> Option<(usize, usize)> {
    let w = u16_at(c, p) as usize;
    if p + 2 > c.len() {
        return None;
    }
    if w & 0x8000 != 0 {
        let w2 = u16_at(c, p + 2) as usize;
        Some((((w & 0x7fff) << 16) | w2, 4))
    } else {
        Some((w, 2))
    }
}
