//! Parser wyjścia `heimdall flash` (v1.4 … v2.2.x) + postęp ważony bajtami.
//!
//! Heimdall wypisuje procenty w JEDNEJ linii, cofając kursor znakami backspace:
//! `0%\x08\x0825%\x08\x08\x0850%\x08\x08\x0875%\x08\x08\x08100%`, a `\n` pojawia się
//! dopiero po 100%. Czytanie liniami (`read_line`) widzi więc postęp partycji
//! dopiero po jej wgraniu. Ten parser dostaje surowe porcje bajtów (dowolnie
//! pocięte) i od razu wydaje zdarzenia: procent po każdym `%`, linie statusu
//! po `\n` / `\r` / `\x08`.

/// Zdarzenie z wyjścia Heimdalla.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeimdallEvent {
    /// `Uploading SUPER`
    Uploading(String),
    /// `SUPER upload successful`
    Uploaded(String),
    /// procent bieżącej partycji (0..=100)
    Percent(u8),
    /// zwykła linia statusu / błędu (bez śmieci `\x08`)
    Line(String),
}

/// Strumieniowy tokenizer: `feed(bytes)` -> zdarzenia. Stan między porcjami w `pending`.
#[derive(Default)]
pub struct HeimdallParser {
    pending: Vec<u8>,
}

impl HeimdallParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<HeimdallEvent> {
        let mut out = Vec::new();
        for &b in bytes {
            match b {
                b'%' => {
                    // "47%" -> procent; "Uploading 100% done" itp. -> zwykły tekst
                    let digits = self.pending.iter().rev().take_while(|c| c.is_ascii_digit()).count();
                    let start = self.pending.len() - digits;
                    if digits > 0 && digits <= 3 && self.pending[..start].iter().all(|c| c.is_ascii_whitespace()) {
                        let v: u32 = std::str::from_utf8(&self.pending[start..]).ok().and_then(|s| s.parse().ok()).unwrap_or(0);
                        if v <= 100 {
                            out.push(HeimdallEvent::Percent(v as u8));
                            self.pending.clear();
                            continue;
                        }
                    }
                    self.pending.push(b);
                }
                b'\n' | b'\r' | 0x08 => self.flush(&mut out),
                _ => self.pending.push(b),
            }
        }
        out
    }

    /// Koniec strumienia - wydaj ostatnią linię bez `\n`.
    pub fn finish(&mut self) -> Vec<HeimdallEvent> {
        let mut out = Vec::new();
        self.flush(&mut out);
        out
    }

    fn flush(&mut self, out: &mut Vec<HeimdallEvent>) {
        if self.pending.is_empty() {
            return;
        }
        let line = String::from_utf8_lossy(&self.pending).trim().to_string();
        self.pending.clear();
        if line.is_empty() {
            return;
        }
        out.push(classify(&line));
    }
}

fn classify(line: &str) -> HeimdallEvent {
    if let Some(rest) = line.strip_prefix("Uploading ") {
        let part = rest.trim().trim_end_matches("...").trim();
        if !part.is_empty() && !part.contains(' ') {
            return HeimdallEvent::Uploading(part.to_string());
        }
    }
    let low = line.to_ascii_lowercase();
    if let Some(part) = low.strip_suffix(" upload successful") {
        let n = part.len();
        let original = line[..n].trim();
        if !original.is_empty() && !original.contains(' ') {
            return HeimdallEvent::Uploaded(original.to_string());
        }
    }
    HeimdallEvent::Line(line.to_string())
}

/// Postęp flashowania ważony rozmiarem partycji:
/// `(bajty_ukończonych + rozmiar_bieżącej * procent / 100) / suma_bajtów`.
pub struct FlashProgress {
    parts: Vec<(String, u64)>,
    done: Vec<bool>,
    current: Option<usize>,
    percent: u8,
}

impl FlashProgress {
    /// `parts`: (nazwa partycji z PIT, rozmiar pliku po dekompresji).
    pub fn new(parts: Vec<(String, u64)>) -> Self {
        let n = parts.len();
        Self { parts, done: vec![false; n], current: None, percent: 0 }
    }

    fn index(&self, name: &str) -> Option<usize> {
        self.parts.iter().position(|(p, _)| p.eq_ignore_ascii_case(name))
    }

    /// Zwraca true, gdy zdarzenie zmieniło stan postępu.
    pub fn apply(&mut self, ev: &HeimdallEvent) -> bool {
        match ev {
            HeimdallEvent::Uploading(p) => {
                self.current = self.index(p);
                self.percent = 0;
                true
            }
            HeimdallEvent::Percent(v) => {
                if self.current.is_some() && *v != self.percent {
                    self.percent = *v;
                    return true;
                }
                false
            }
            HeimdallEvent::Uploaded(p) => {
                if let Some(i) = self.index(p) {
                    self.done[i] = true;
                }
                self.current = None;
                self.percent = 0;
                true
            }
            HeimdallEvent::Line(_) => false,
        }
    }

    pub fn total_bytes(&self) -> u64 {
        self.parts.iter().map(|(_, s)| s).sum()
    }

    pub fn bytes_done(&self) -> u64 {
        let finished: u64 = self.parts.iter().zip(&self.done).filter(|(_, d)| **d).map(|((_, s), _)| s).sum();
        let partial = match self.current {
            Some(i) if !self.done[i] => self.parts[i].1 * u64::from(self.percent) / 100,
            _ => 0,
        };
        (finished + partial).min(self.total_bytes())
    }

    pub fn done_count(&self) -> u64 {
        self.done.iter().filter(|d| **d).count() as u64
    }

    pub fn total_count(&self) -> u64 {
        self.parts.len() as u64
    }

    /// Opis do karty postępu, np. `SUPER (47%) — 12 / 21`.
    pub fn label(&self) -> String {
        let counter = format!("{} / {}", self.done_count(), self.total_count());
        match self.current {
            Some(i) => format!("{} ({}%) — {counter}", self.parts[i].0, self.percent),
            None => counter,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prawdziwy fragment wyjścia Heimdall v2.2.2 (BOOTLOADER + początek SUPER).
    const REAL: &[u8] = b"Initialising protocol...\nProtocol initialisation successful.\n\nBeginning session...\n\n\
Uploading BOOTLOADER\n0%\x08\x0825%\x08\x08\x0850%\x08\x08\x0875%\x08\x08\x08100%\n\
BOOTLOADER upload successful\n\nUploading SUPER\n0%\x08\x081%\x08\x082%\x08\x0847%\x08\x0899%\x08\x08100%\n\
SUPER upload successful\n\nEnding session...\nRebooting device...\n";

    fn parse_in_chunks(data: &[u8], chunk: usize) -> Vec<HeimdallEvent> {
        let mut p = HeimdallParser::new();
        let mut ev = Vec::new();
        for c in data.chunks(chunk) {
            ev.extend(p.feed(c));
        }
        ev.extend(p.finish());
        ev
    }

    #[test]
    fn parses_real_heimdall_222_output_in_small_chunks() {
        let whole = parse_in_chunks(REAL, REAL.len());
        for chunk in [1, 2, 3, 7, 64] {
            assert_eq!(parse_in_chunks(REAL, chunk), whole, "chunk {chunk}");
        }
        let pct: Vec<u8> = whole
            .iter()
            .filter_map(|e| if let HeimdallEvent::Percent(v) = e { Some(*v) } else { None })
            .collect();
        assert_eq!(pct, [0, 25, 50, 75, 100, 0, 1, 2, 47, 99, 100]);
        assert!(whole.contains(&HeimdallEvent::Uploading("BOOTLOADER".into())));
        assert!(whole.contains(&HeimdallEvent::Uploaded("SUPER".into())));
        // w liniach logu nie ma backspace ani gołych procentów
        for e in &whole {
            if let HeimdallEvent::Line(l) = e {
                assert!(!l.contains('\x08') && !l.ends_with('%'), "śmieci w logu: {l:?}");
            }
        }
        assert!(whole.contains(&HeimdallEvent::Line("Ending session...".into())));
    }

    #[test]
    fn percent_arrives_before_newline() {
        // 5.4 GB SUPER: procent musi przyjść od razu, bez czekania na '\n'
        let mut p = HeimdallParser::new();
        assert_eq!(p.feed(b"Uploading SUPER\n"), [HeimdallEvent::Uploading("SUPER".into())]);
        assert_eq!(p.feed(b"0%\x08\x081"), [HeimdallEvent::Percent(0)]);
        assert_eq!(p.feed(b"2%"), [HeimdallEvent::Percent(12)]);
        assert_eq!(p.feed(b"\x08\x08\x08"), []);
        // tekst z procentem w środku linii to zwykła linia
        assert_eq!(p.feed(b"ERROR: 150% fail\n"), [HeimdallEvent::Line("ERROR: 150% fail".into())]);
    }

    #[test]
    fn byte_weighted_progress() {
        let gb = 1u64 << 30;
        let mut fp = FlashProgress::new(vec![("BOOTLOADER".into(), 4 << 20), ("BOOT".into(), 64 << 20), ("SUPER".into(), 5 * gb)]);
        let total = fp.total_bytes();
        assert_eq!(fp.bytes_done(), 0);
        assert!(fp.apply(&HeimdallEvent::Uploading("BOOTLOADER".into())));
        fp.apply(&HeimdallEvent::Percent(50));
        assert_eq!(fp.bytes_done(), 2 << 20);
        fp.apply(&HeimdallEvent::Uploaded("BOOTLOADER".into()));
        fp.apply(&HeimdallEvent::Uploading("BOOT".into()));
        fp.apply(&HeimdallEvent::Uploaded("BOOT".into()));
        assert_eq!(fp.done_count(), 2);
        assert_eq!(fp.bytes_done(), 68 << 20);
        fp.apply(&HeimdallEvent::Uploading("super".into()));
        fp.apply(&HeimdallEvent::Percent(47));
        assert_eq!(fp.bytes_done(), (68 << 20) + 5 * gb * 47 / 100);
        assert_eq!(fp.label(), "SUPER (47%) — 2 / 3");
        fp.apply(&HeimdallEvent::Uploaded("SUPER".into()));
        assert_eq!(fp.bytes_done(), total);
        assert_eq!(fp.label(), "3 / 3");
        // nieznana partycja (np. PIT przy repartition) nie psuje licznika
        assert!(!fp.apply(&HeimdallEvent::Percent(10)));
    }
}
