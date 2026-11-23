//! Live Logcat: `adb logcat -v threadtime` w tle, filtrowanie (regex + poziom)
//! PO STRONIE RUSTA, wysyłka do GUI paczkami co 100 ms.
//!
//! Dlaczego tak: przy tysiącach linii/s wysyłanie każdej linii osobno przez IPC
//! i filtrowanie w JS zamraża WebView. Tu GUI dostaje tylko pasujące linie,
//! max `MAX_BATCH` na paczkę (+ licznik pominiętych), a pełny strumień trzymamy
//! w buforze kołowym (eksport do pliku, ponowne filtrowanie po zmianie filtra).

use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::adb::manager::hide_console;
use crate::adb::Adb;
use crate::error::{AdbError, Result};
use crate::utils::CancelToken;

const RING_CAP: usize = 50_000;
const MAX_BATCH: usize = 1500;
const RESET_LINES: usize = 3000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    pub id: u64,
    pub ts: String,
    pub pid: String,
    pub level: char,
    pub tag: String,
    pub msg: String,
}

/// Filtr ustawiany z GUI (zmiana działa natychmiast, bez restartu logcat).
pub struct Filter {
    pub regex: Option<Regex>,
    /// minimalny poziom: V < D < I < W < E < F
    pub min_level: u8,
}

fn level_rank(c: char) -> u8 {
    match c {
        'V' => 0,
        'D' => 1,
        'I' => 2,
        'W' => 3,
        'E' => 4,
        'F' | 'A' => 5,
        _ => 0,
    }
}

impl Filter {
    fn matches(&self, l: &LogLine) -> bool {
        if l.level != '?' && level_rank(l.level) < self.min_level {
            return false;
        }
        match &self.regex {
            Some(r) => r.is_match(&l.msg) || r.is_match(&l.tag),
            None => true,
        }
    }
}

/// Stan sesji współdzielony z komendami (filtr, eksport).
pub struct LogcatSession {
    pub filter: RwLock<Filter>,
    ring: Mutex<VecDeque<LogLine>>,
    seq: AtomicU64,
    /// GUI prosi o ponowne przefiltrowanie bufora (po zmianie filtra)
    reset_requested: std::sync::atomic::AtomicBool,
}

impl LogcatSession {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            filter: RwLock::new(Filter { regex: None, min_level: 0 }),
            ring: Mutex::new(VecDeque::with_capacity(4096)),
            seq: AtomicU64::new(0),
            reset_requested: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub fn set_filter(&self, pattern: &str, min_level: char, case_insensitive: bool) -> Result<()> {
        let regex = if pattern.trim().is_empty() {
            None
        } else {
            let p = if case_insensitive {
                format!("(?i){pattern}")
            } else {
                pattern.to_string()
            };
            Some(
                regex::RegexBuilder::new(&p)
                    .size_limit(1 << 20)
                    .build()
                    .map_err(|e| AdbError::InvalidArg(format!("regex: {e}")))?,
            )
        };
        if let Ok(mut f) = self.filter.write() {
            *f = Filter {
                regex,
                min_level: level_rank(min_level),
            };
        }
        self.reset_requested.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Zapis pełnego bufora (bez filtra) do pliku.
    pub fn export(&self, path: &Path) -> Result<usize> {
        use std::io::Write;
        let ring = self.ring.lock().map_err(|_| AdbError::Other("lock".into()))?;
        let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
        for l in ring.iter() {
            writeln!(f, "{} {:>5} {} {}: {}", l.ts, l.pid, l.level, l.tag, l.msg)?;
        }
        f.flush()?;
        Ok(ring.len())
    }
}

/// Parsuje linię `-v threadtime`:
/// `10-29 14:15:00.123  1234  5678 E Tag     : wiadomość`
pub fn parse_line(id: u64, raw: &str) -> LogLine {
    let fallback = || LogLine {
        id,
        ts: String::new(),
        pid: String::new(),
        level: '?',
        tag: String::new(),
        msg: raw.to_string(),
    };
    let mut it = raw.split_whitespace();
    let (Some(date), Some(time), Some(pid), Some(_tid), Some(lvl)) = (it.next(), it.next(), it.next(), it.next(), it.next())
    else {
        return fallback();
    };
    if date.len() != 5 || !date.contains('-') || !pid.chars().all(|c| c.is_ascii_digit()) || lvl.len() != 1 {
        return fallback();
    }
    let level = lvl.chars().next().unwrap_or('?');
    // tag + wiadomość: wszystko po poziomie, rozdzielone pierwszym ": "
    let after = raw
        .find(&format!(" {lvl} "))
        .map(|i| &raw[i + 3..])
        .unwrap_or("");
    let (tag, msg) = match after.split_once(": ") {
        Some((t, m)) => (t.trim().to_string(), m.to_string()),
        None => (after.trim_end_matches(':').trim().to_string(), String::new()),
    };
    LogLine {
        id,
        ts: format!("{date} {time}"),
        pid: pid.to_string(),
        level,
        tag,
        msg,
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogcatOptions {
    /// np. ["main","system","crash"] (+ "kernel" - zwykle wymaga roota)
    #[serde(default)]
    pub buffers: Vec<String>,
    /// ile ostatnich linii pobrać na start (0 = tylko nowe)
    #[serde(default)]
    pub tail: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum LogcatEvent {
    /// Nowe pasujące linie
    Lines { lines: Vec<LogLine>, dropped: u64 },
    /// Pełna podmiana widoku (po zmianie filtra)
    Reset { lines: Vec<LogLine> },
    Stats { total: u64, matched: u64, per_sec: u64, buffered: usize },
    Ended { message: String },
}

pub async fn run(
    adb: Arc<Adb>,
    serial: String,
    opts: LogcatOptions,
    session: Arc<LogcatSession>,
    channel: tauri::ipc::Channel<LogcatEvent>,
    cancel: CancelToken,
) {
    let mut args: Vec<String> = vec!["-s".into(), serial.clone(), "logcat".into(), "-v".into(), "threadtime".into()];
    for b in opts.buffers.iter().filter(|b| b.chars().all(|c| c.is_ascii_alphanumeric())) {
        args.push("-b".into());
        args.push(b.clone());
    }
    if opts.tail > 0 {
        args.push("-T".into());
        args.push(opts.tail.min(10_000).to_string());
    }

    let mut cmd = Command::new(&adb.adb);
    hide_console(&mut cmd);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = channel.send(LogcatEvent::Ended { message: e.to_string() });
            return;
        }
    };

    // czytnik w osobnym zadaniu -> kanał (read_until nie jest bezpieczne w select!)
    let (tx, mut rx) = mpsc::channel::<String>(20_000);
    if let Some(out) = child.stdout.take() {
        tokio::spawn(async move {
            let mut r = BufReader::with_capacity(256 * 1024, out);
            let mut buf = Vec::with_capacity(512);
            loop {
                buf.clear();
                match r.read_until(b'\n', &mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let s = String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_string();
                        if s.is_empty() || s.starts_with("--------- beginning of") {
                            continue;
                        }
                        if tx.send(s).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });
    }
    let mut stderr_text = String::new();
    let mut stderr = child.stderr.take();

    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut stats_tick = tokio::time::interval(Duration::from_secs(1));
    let mut batch: Vec<LogLine> = Vec::with_capacity(MAX_BATCH);
    let mut dropped = 0u64;
    let (mut total, mut matched, mut last_total) = (0u64, 0u64, 0u64);

    let end_message = loop {
        tokio::select! {
            line = rx.recv() => {
                let Some(raw) = line else {
                    // koniec strumienia - zbierz stderr (np. "invalid buffer kernel")
                    if let Some(mut e) = stderr.take() {
                        use tokio::io::AsyncReadExt;
                        let mut b = Vec::new();
                        let _ = tokio::time::timeout(Duration::from_secs(1), e.read_to_end(&mut b)).await;
                        stderr_text = String::from_utf8_lossy(&b).trim().to_string();
                    }
                    break if stderr_text.is_empty() {
                        "logcat zakończony (urządzenie odłączone?) / logcat ended".to_string()
                    } else {
                        stderr_text.clone()
                    };
                };
                total += 1;
                let id = session.seq.fetch_add(1, Ordering::Relaxed);
                let l = parse_line(id, &raw);
                let ok = session.filter.read().map(|f| f.matches(&l)).unwrap_or(true);
                if ok {
                    matched += 1;
                    if batch.len() < MAX_BATCH {
                        batch.push(l.clone());
                    } else {
                        dropped += 1;
                    }
                }
                if let Ok(mut ring) = session.ring.lock() {
                    if ring.len() >= RING_CAP {
                        ring.pop_front();
                    }
                    ring.push_back(l);
                }
            }
            _ = tick.tick() => {
                if session.reset_requested.swap(false, Ordering::SeqCst) {
                    // przefiltruj bufor od końca - GUI dostaje od razu wynik nowego filtra
                    let lines: Vec<LogLine> = {
                        let f = session.filter.read();
                        let ring = session.ring.lock();
                        match (f, ring) {
                            (Ok(f), Ok(ring)) => {
                                let mut v: Vec<LogLine> = ring.iter().rev().filter(|l| f.matches(l)).take(RESET_LINES).cloned().collect();
                                v.reverse();
                                v
                            }
                            _ => Vec::new(),
                        }
                    };
                    batch.clear();
                    dropped = 0;
                    let _ = channel.send(LogcatEvent::Reset { lines });
                } else if !batch.is_empty() || dropped > 0 {
                    let lines = std::mem::replace(&mut batch, Vec::with_capacity(MAX_BATCH));
                    let _ = channel.send(LogcatEvent::Lines { lines, dropped });
                    dropped = 0;
                }
            }
            _ = stats_tick.tick() => {
                let buffered = session.ring.lock().map(|r| r.len()).unwrap_or(0);
                let _ = channel.send(LogcatEvent::Stats { total, matched, per_sec: total - last_total, buffered });
                last_total = total;
            }
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                break "zatrzymano / stopped".to_string();
            }
        }
    };
    if !batch.is_empty() {
        let _ = channel.send(LogcatEvent::Lines { lines: batch, dropped });
    }
    let _ = channel.send(LogcatEvent::Ended { message: end_message });
}

/// `adb logcat -c` - czyści bufory logów na urządzeniu.
pub async fn clear_device(adb: &Adb, serial: &str) -> Result<()> {
    let o = adb.run(Some(serial), &["logcat", "-c"], 15).await?;
    if o.ok() {
        Ok(())
    } else {
        Err(AdbError::Command(o.err_text()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_threadtime() {
        let l = parse_line(1, "10-29 14:15:00.123  1234  5678 E AndroidRuntime: FATAL EXCEPTION: main");
        assert_eq!(l.level, 'E');
        assert_eq!(l.pid, "1234");
        assert_eq!(l.tag, "AndroidRuntime");
        assert_eq!(l.msg, "FATAL EXCEPTION: main");
        assert_eq!(l.ts, "10-29 14:15:00.123");
        let junk = parse_line(2, "some random line");
        assert_eq!(junk.level, '?');
    }

    #[test]
    fn filter_levels_and_regex() {
        let s = LogcatSession::new();
        s.set_filter("batt|charg", 'W', true).unwrap();
        let f = s.filter.read().unwrap();
        assert!(f.matches(&parse_line(1, "10-29 14:15:00.123  1  1 E healthd : battery l=5 v=3500")));
        assert!(!f.matches(&parse_line(2, "10-29 14:15:00.123  1  1 I healthd : battery l=5")));
        assert!(!f.matches(&parse_line(3, "10-29 14:15:00.123  1  1 E Wifi : scan failed")));
        drop(f);
        assert!(s.set_filter("([", 'V', false).is_err());
    }
}
