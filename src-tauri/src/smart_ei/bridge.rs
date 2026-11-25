//! Smart Wi-Fi Bridge - tymczasowy, zabezpieczony serwer HTTP w sieci lokalnej
//! (albo na 127.0.0.1 + `adb reverse` w trybie ADB). Agent na telefonie łączy się z nim,
//! paruje 6-cyfrowym PINem i dostaje jednorazowy token sesji (nagłówek `X-SmartEI-Token`).
//!
//! Zabezpieczenia:
//! - nasłuch tylko na wybranym adresie LAN (albo loopback), losowy port, tylko na czas zadania,
//! - połączenia wyłącznie z adresów lokalnych ([`net::peer_allowed`]),
//! - PIN porównywany w stałym czasie, po 5 błędnych próbach parowanie jest blokowane,
//! - tylko JEDNO urządzenie na sesję (kolejne parowanie -> 409),
//! - bez tokenu dostępne są tylko: `/ping`, `/p` (strona dla aparatu z QR) i `/agent.apk`,
//! - ścieżki plików walidowane ([`manifest::safe_rel_path`]); import serwuje wyłącznie pliki z planu.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use super::apps::{normalize, AgentApp, AppRecord, AppSource};
use super::manifest::{safe_rel_path, ModuleEntry};
use super::net;
use super::queue::{Action, PlayQueue};
use crate::utils::CancelToken;

pub const TOKEN_HEADER: &str = "x-smartei-token";
pub const MAX_PIN_FAILURES: u32 = 5;
const MAX_HEAD: usize = 16 * 1024;
const MAX_JSON: u64 = 16 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 24;
const IO_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Export,
    Import,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Export => "export",
            Role::Import => "import",
        }
    }
}

/// Zdarzenia od agenta dla przebiegu zadania (flow).
#[derive(Debug, Clone)]
pub enum AgentEvent {
    Paired(Value),
    Hello(Value),
    Apps(Vec<AppRecord>),
    Settings(Value),
    Module(ModuleEntry),
    Progress(Value),
    Log { level: String, message: String },
    Uploaded { path: String, size: u64 },
    Stage(String),
    Queue,
    Done(Value),
    Error(String),
}

/// Stan współdzielony między serwerem a przebiegiem zadania.
pub struct Inner {
    pub token: Option<String>,
    pub failures: u32,
    pub locked: bool,
    pub role: Role,
    pub adb: bool,
    pub dir: PathBuf,
    pub agent_apk: Option<PathBuf>,
    /// Opis zadania dla agenta (`GET /job`): "state": wait | go | cancel | done + moduły.
    pub job: Value,
    /// Plan importu (`GET /import/plan`).
    pub plan: Option<Value>,
    /// Pliki, które agent może pobrać przy imporcie (ścieżki względne z planu).
    pub allowed_files: HashSet<String>,
    /// Agent sam wysyła APK aplikacji spoza Sklepu Play (tryb Wi-Fi).
    pub agent_sends_apks: bool,
    pub queue: Option<PlayQueue>,
    pub cancelled: bool,
    pub uploaded_files: u64,
    pub uploaded_bytes: u64,
    /// Ostatnie zapytanie agenta z tokenem (w tym `/ping` co 8 s) - wykrywanie zabitego procesu.
    pub last_seen: Option<std::time::Instant>,
}

pub struct Shared {
    pub pin: String,
    pub inner: Mutex<Inner>,
    events: mpsc::UnboundedSender<AgentEvent>,
}

impl Shared {
    pub fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
    fn emit(&self, e: AgentEvent) {
        let _ = self.events.send(e);
    }
}

pub struct Bridge {
    pub addr: SocketAddr,
    pub shared: Arc<Shared>,
    stop: CancelToken,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl Bridge {
    /// Start serwera na `ip` (losowy port; kilka prób przy zajętym porcie).
    pub async fn start(
        ip: std::net::IpAddr,
        role: Role,
        adb: bool,
        dir: PathBuf,
        agent_apk: Option<PathBuf>,
    ) -> std::io::Result<(Self, mpsc::UnboundedReceiver<AgentEvent>)> {
        let mut last = None;
        let mut listener = None;
        for _ in 0..20 {
            match TcpListener::bind(SocketAddr::new(ip, net::random_port())).await {
                Ok(l) => {
                    listener = Some(l);
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        let listener = match listener {
            Some(l) => l,
            None => return Err(last.unwrap_or_else(|| std::io::Error::other("bind"))),
        };
        Ok(Self::with_listener(listener, role, adb, dir, agent_apk, net::new_pin()))
    }

    pub fn with_listener(
        listener: TcpListener,
        role: Role,
        adb: bool,
        dir: PathBuf,
        agent_apk: Option<PathBuf>,
        pin: String,
    ) -> (Self, mpsc::UnboundedReceiver<AgentEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let addr = listener.local_addr().expect("local addr");
        let shared = Arc::new(Shared {
            pin,
            inner: Mutex::new(Inner {
                token: None,
                failures: 0,
                locked: false,
                role,
                adb,
                dir,
                agent_apk,
                job: json!({ "state": "wait" }),
                plan: None,
                allowed_files: HashSet::new(),
                agent_sends_apks: false,
                queue: None,
                cancelled: false,
                uploaded_files: 0,
                uploaded_bytes: 0,
                last_seen: None,
            }),
            events: tx,
        });
        let stop = CancelToken::new();
        let (s2, st2) = (shared.clone(), stop.clone());
        tokio::spawn(async move {
            let sem = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
            loop {
                tokio::select! {
                    _ = st2.cancelled() => break,
                    acc = listener.accept() => {
                        let Ok((stream, peer)) = acc else { continue };
                        if !net::peer_allowed(peer.ip()) {
                            drop(stream);
                            continue;
                        }
                        let Ok(permit) = sem.clone().try_acquire_owned() else {
                            drop(stream);
                            continue;
                        };
                        let s3 = s2.clone();
                        tokio::spawn(async move {
                            let _ = handle(stream, s3).await;
                            drop(permit);
                        });
                    }
                }
            }
        });
        (Self { addr, shared, stop }, rx)
    }

    pub fn stop(&self) {
        self.stop.cancel();
    }
}

// --------------------------------------------------------------------------- //
// HTTP
// --------------------------------------------------------------------------- //

#[derive(Debug)]
struct Head {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    content_length: u64,
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_head(raw: &str) -> Option<Head> {
    let mut lines = raw.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_string();
    let target = first.next()?;
    if !first.next()?.starts_with("HTTP/1.") {
        return None;
    }
    let (path, q) = target.split_once('?').unwrap_or((target, ""));
    let query = q
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect();
    let mut headers = HashMap::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    if headers.get("transfer-encoding").is_some_and(|t| !t.eq_ignore_ascii_case("identity")) {
        return None; // agent zawsze wysyła stałą długość
    }
    let content_length = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    Some(Head { method, path: percent_decode(path), query, headers, content_length })
}

async fn read_head<R: AsyncRead + Unpin>(r: &mut BufReader<R>) -> Option<String> {
    let mut buf = Vec::new();
    loop {
        let n = tokio::time::timeout(Duration::from_secs(20), r.read_until(b'\n', &mut buf)).await.ok()?.ok()?;
        if n == 0 || buf.len() > MAX_HEAD {
            return None;
        }
        if buf.ends_with(b"\r\n\r\n") {
            buf.truncate(buf.len() - 4);
            return String::from_utf8(buf).ok();
        }
    }
}

async fn respond<W: AsyncWrite + Unpin>(w: &mut W, code: u16, ctype: &str, body: &[u8]) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        410 => "Gone",
        413 => "Payload Too Large",
        423 => "Locked",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    );
    w.write_all(head.as_bytes()).await?;
    w.write_all(body).await?;
    w.flush().await
}

async fn respond_json<W: AsyncWrite + Unpin>(w: &mut W, code: u16, v: Value) -> std::io::Result<()> {
    respond(w, code, "application/json; charset=utf-8", v.to_string().as_bytes()).await
}

async fn read_json<R: AsyncRead + Unpin>(r: &mut BufReader<R>, len: u64) -> Option<Value> {
    if len > MAX_JSON {
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    tokio::time::timeout(IO_TIMEOUT, r.read_exact(&mut buf)).await.ok()?.ok()?;
    if buf.is_empty() {
        return Some(json!({}));
    }
    serde_json::from_slice(&buf).ok()
}

async fn handle(stream: TcpStream, sh: Arc<Shared>) -> std::io::Result<()> {
    let _ = stream.set_nodelay(true);
    let (rd, mut wr) = stream.into_split();
    let mut r = BufReader::with_capacity(64 * 1024, rd);
    let Some(raw) = read_head(&mut r).await else { return Ok(()) };
    let Some(h) = parse_head(&raw) else {
        return respond_json(&mut wr, 400, json!({"error": "bad request"})).await;
    };

    // ---- publiczne
    match (h.method.as_str(), h.path.as_str()) {
        ("GET", "/ping") => {
            return respond_json(&mut wr, 200, json!({"app": "adb-service-tool", "smartei": 1})).await;
        }
        ("GET", "/p") => return respond(&mut wr, 200, "text/html; charset=utf-8", PAIR_PAGE.as_bytes()).await,
        ("GET", "/agent.apk") => {
            let apk = sh.lock().agent_apk.clone();
            return match apk {
                Some(p) => send_file(&mut wr, &p, "application/vnd.android.package-archive").await,
                None => respond_json(&mut wr, 404, json!({"error": "no apk"})).await,
            };
        }
        ("POST", "/pair") => {
            let body = read_json(&mut r, h.content_length).await.unwrap_or(Value::Null);
            return pair(&mut wr, &sh, body).await;
        }
        _ => {}
    }

    // ---- wymagany token
    let authorized = {
        let g = sh.lock();
        match (&g.token, h.headers.get(TOKEN_HEADER)) {
            (Some(t), Some(given)) => net::ct_eq(t, given),
            _ => false,
        }
    };
    if !authorized {
        return respond_json(&mut wr, 401, json!({"error": "unauthorized"})).await;
    }
    sh.lock().last_seen = Some(std::time::Instant::now());
    if sh.lock().cancelled {
        return respond_json(&mut wr, 410, json!({"state": "cancel"})).await;
    }

    match (h.method.as_str(), h.path.as_str()) {
        ("GET", "/job") => {
            let job = sh.lock().job.clone();
            respond_json(&mut wr, 200, job).await
        }
        ("PUT", "/export/file") => upload(&mut r, &mut wr, &sh, &h).await,
        ("GET", "/export/has") => {
            let size: u64 = h.query.get("size").and_then(|s| s.parse().ok()).unwrap_or(u64::MAX);
            let Some(rel) = h.query.get("path").and_then(|p| safe_rel_path(p)) else {
                return respond_json(&mut wr, 400, json!({"error": "bad path"})).await;
            };
            let p = sh.lock().dir.join(rel);
            let have = std::fs::metadata(&p).map(|m| m.is_file() && m.len() == size).unwrap_or(false);
            respond_json(&mut wr, 200, json!({ "have": have })).await
        }
        ("GET", "/import/plan") => {
            let plan = sh.lock().plan.clone();
            match plan {
                Some(p) => respond_json(&mut wr, 200, p).await,
                None => respond_json(&mut wr, 404, json!({"state": "wait"})).await,
            }
        }
        ("GET", "/import/file") => {
            let rel = h.query.get("path").cloned().unwrap_or_default();
            let path = {
                let g = sh.lock();
                (g.role == Role::Import && g.allowed_files.contains(&rel))
                    .then(|| safe_rel_path(&rel).map(|r| g.dir.join(r)))
                    .flatten()
            };
            match path {
                Some(p) => send_file(&mut wr, &p, "application/octet-stream").await,
                None => respond_json(&mut wr, 403, json!({"error": "not in plan"})).await,
            }
        }
        ("GET", "/queue") => {
            let q = sh.lock().queue.clone();
            respond_json(&mut wr, 200, queue_json(q.as_ref())).await
        }
        ("POST", p) => {
            let Some(body) = read_json(&mut r, h.content_length).await else {
                return respond_json(&mut wr, 413, json!({"error": "bad body"})).await;
            };
            post(&mut wr, &sh, p, body).await
        }
        _ => respond_json(&mut wr, 404, json!({"error": "not found"})).await,
    }
}

pub fn queue_json(q: Option<&PlayQueue>) -> Value {
    match q {
        None => json!({ "active": false }),
        Some(q) => {
            let (pos, total) = q.position();
            json!({
                "active": true,
                "finished": q.finished(),
                "position": pos,
                "total": total,
                "rev": q.rev,
                "current": q.current(),
                "items": q.items,
            })
        }
    }
}

async fn pair<W: AsyncWrite + Unpin>(w: &mut W, sh: &Shared, body: Value) -> std::io::Result<()> {
    let given = body.get("pin").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let res = {
        let mut g = sh.lock();
        if g.cancelled {
            Err((410, "cancelled"))
        } else if g.locked {
            Err((423, "locked"))
        } else if g.token.is_some() {
            Err((409, "already paired"))
        } else if net::ct_eq(&given, &sh.pin) {
            let t = net::new_token();
            g.token = Some(t.clone());
            g.last_seen = Some(std::time::Instant::now());
            Ok((t, g.role, g.adb))
        } else {
            g.failures += 1;
            if g.failures >= MAX_PIN_FAILURES {
                g.locked = true;
            }
            Err((403, "bad pin"))
        }
    };
    match res {
        Ok((token, role, adb)) => {
            sh.emit(AgentEvent::Paired(body.get("device").cloned().unwrap_or(Value::Null)));
            respond_json(w, 200, json!({ "token": token, "role": role.as_str(), "adb": adb })).await
        }
        Err((code, msg)) => {
            if code == 403 {
                tokio::time::sleep(Duration::from_millis(600)).await;
            }
            if code == 423 || code == 403 {
                sh.emit(AgentEvent::Log { level: "warn".into(), message: format!("pair: {msg}") });
            }
            respond_json(w, code, json!({ "error": msg })).await
        }
    }
}

async fn post<W: AsyncWrite + Unpin>(w: &mut W, sh: &Shared, path: &str, body: Value) -> std::io::Result<()> {
    match path {
        "/hello" => sh.emit(AgentEvent::Hello(body)),
        "/ping" => {}
        "/export/apps" => {
            let raw: Vec<AgentApp> = serde_json::from_value(body.get("apps").cloned().unwrap_or(Value::Null)).unwrap_or_default();
            let apps = normalize(raw);
            let sends = sh.lock().agent_sends_apks;
            let apks: Vec<&str> = if sends {
                apps.iter().filter(|a| a.source == AppSource::Sideload).map(|a| a.package.as_str()).collect()
            } else {
                vec![]
            };
            let resp = json!({ "apks": apks, "count": apps.len() });
            sh.emit(AgentEvent::Apps(apps));
            return respond_json(w, 200, resp).await;
        }
        "/export/settings" => sh.emit(AgentEvent::Settings(body)),
        "/export/module" => {
            if let Ok(m) = serde_json::from_value::<ModuleEntry>(body) {
                sh.emit(AgentEvent::Module(m));
            }
        }
        "/progress" => sh.emit(AgentEvent::Progress(body)),
        "/log" => {
            let level = body.get("level").and_then(|v| v.as_str()).unwrap_or("info").to_string();
            let message: String = body.get("message").and_then(|v| v.as_str()).unwrap_or("").chars().take(2000).collect();
            sh.emit(AgentEvent::Log { level, message });
        }
        "/stage" => sh.emit(AgentEvent::Stage(body.get("stage").and_then(|v| v.as_str()).unwrap_or("").chars().take(64).collect())),
        "/queue/next" | "/queue/skip" | "/queue/installed" => {
            let snapshot = {
                let mut g = sh.lock();
                if let Some(q) = g.queue.as_mut() {
                    match path {
                        "/queue/next" => {
                            q.act(Action::Next);
                        }
                        "/queue/skip" => {
                            q.act(Action::Skip);
                        }
                        _ => {
                            if let Some(p) = body.get("package").and_then(|v| v.as_str()) {
                                q.mark_installed(p);
                            }
                        }
                    }
                }
                queue_json(g.queue.as_ref())
            };
            sh.emit(AgentEvent::Queue);
            return respond_json(w, 200, snapshot).await;
        }
        "/done" => sh.emit(AgentEvent::Done(body)),
        "/error" => sh.emit(AgentEvent::Error(
            body.get("error").and_then(|v| v.as_str()).unwrap_or("agent error").chars().take(2000).collect(),
        )),
        _ => return respond_json(w, 404, json!({"error": "not found"})).await,
    }
    respond_json(w, 200, json!({"ok": true})).await
}

/// `PUT /export/file?path=<rel>&size=<n>` - zapis do `<plik>.part`, kontrola długości, zmiana nazwy.
async fn upload<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    r: &mut BufReader<R>,
    w: &mut W,
    sh: &Shared,
    h: &Head,
) -> std::io::Result<()> {
    let declared: Option<u64> = h.query.get("size").and_then(|s| s.parse().ok());
    let rel_s = h.query.get("path").cloned().unwrap_or_default();
    let (Some(rel), Some(size)) = (safe_rel_path(&rel_s), declared) else {
        return respond_json(w, 400, json!({"error": "bad path/size"})).await;
    };
    let allowed = {
        let g = sh.lock();
        g.role == Role::Export && g.job.get("state").and_then(|v| v.as_str()) == Some("go")
    };
    if size != h.content_length || !allowed {
        return respond_json(w, 400, json!({"error": "size mismatch"})).await;
    }
    let dest = sh.lock().dir.join(&rel);
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut part_name = dest.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    part_name.push(".part");
    let part = dest.with_file_name(part_name);
    let mut f = tokio::fs::File::create(&part).await?;
    let mut left = size;
    let mut buf = vec![0u8; 256 * 1024];
    while left > 0 {
        let want = buf.len().min(left as usize);
        let n = match tokio::time::timeout(IO_TIMEOUT, r.read(&mut buf[..want])).await {
            Ok(Ok(n)) if n > 0 => n,
            _ => break,
        };
        f.write_all(&buf[..n]).await?;
        left -= n as u64;
    }
    f.flush().await?;
    drop(f);
    let got = tokio::fs::metadata(&part).await.map(|m| m.len()).unwrap_or(0);
    if left > 0 || got != size {
        let _ = tokio::fs::remove_file(&part).await;
        return respond_json(w, 400, json!({"error": "incomplete", "got": got, "size": size})).await;
    }
    tokio::fs::rename(&part, &dest).await?;
    {
        let mut g = sh.lock();
        g.uploaded_files += 1;
        g.uploaded_bytes += size;
    }
    let rel_str = rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect::<Vec<_>>().join("/");
    sh.emit(AgentEvent::Uploaded { path: rel_str, size });
    respond_json(w, 200, json!({"ok": true, "size": size})).await
}

async fn send_file<W: AsyncWrite + Unpin>(w: &mut W, p: &std::path::Path, ctype: &str) -> std::io::Result<()> {
    let mut f = match tokio::fs::File::open(p).await {
        Ok(f) => f,
        Err(_) => return respond_json(w, 404, json!({"error": "missing"})).await,
    };
    let len = f.metadata().await?.len();
    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {len}\r\nContent-Disposition: attachment; filename=\"{}\"\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        name.replace('"', "")
    );
    w.write_all(head.as_bytes()).await?;
    tokio::io::copy(&mut f, w).await?;
    w.flush().await
}

/// Strona otwierana aparatem z kodu QR parowania: `http://IP:PORT/p#k=PIN`.
/// PIN jest w części `#` adresu - nie trafia do serwera ani do logów; strona tylko buduje
/// odnośnik `intent://` otwierający agenta (albo pobiera Agent.apk, gdy go brak).
const PAIR_PAGE: &str = r##"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Smart EI</title><style>body{background:#0a0a0a;color:#ddd;font-family:sans-serif;text-align:center;padding:32px 18px}
h1{color:#c9a249;font-size:22px}a{display:block;margin:18px auto;max-width:360px;padding:18px;border-radius:12px;font-size:18px;font-weight:bold;text-decoration:none}
.p{background:#c9a249;color:#000}.s{border:2px solid #c9a249;color:#c9a249}code{font-size:20px;color:#c9a249}</style></head>
<body><h1>Smart EI &middot; ADB Service Tool</h1><p><code id="hp"></code></p><p>PIN: <code id="pin"></code></p>
<a class="p" id="open" href="#">&#9654; Drapco Agent</a><a class="s" href="/agent.apk">&#11015; Agent.apk</a>
<script>var k=(location.hash.match(/k=(\d{6})/)||[])[1]||"";var h=location.hostname,p=location.port;
document.getElementById("hp").textContent=h+":"+p;document.getElementById("pin").textContent=k;
var fb=encodeURIComponent(location.origin+"/agent.apk");
document.getElementById("open").href="intent://pair?h="+h+"&p="+p+"&k="+k+"#Intent;scheme=drapco-smartei;package=com.drapco.agent;S.browser_fallback_url="+fb+";end";
</script></body></html>"##;

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    async fn req(addr: SocketAddr, method: &str, path: &str, token: Option<&str>, body: &[u8]) -> (u16, Vec<u8>) {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let mut head = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n", body.len());
        if let Some(t) = token {
            head.push_str(&format!("X-SmartEI-Token: {t}\r\n"));
        }
        head.push_str("\r\n");
        s.write_all(head.as_bytes()).await.unwrap();
        s.write_all(body).await.unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).await.unwrap();
        let text = String::from_utf8_lossy(&out).to_string();
        let code: u16 = text.split(' ').nth(1).unwrap().parse().unwrap();
        let pos = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        (code, out[pos..].to_vec())
    }

    fn js(b: &[u8]) -> Value {
        serde_json::from_slice(b).unwrap()
    }

    async fn server(role: Role) -> (Bridge, mpsc::UnboundedReceiver<AgentEvent>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("smartei-bridge-{}-{}", std::process::id(), net::new_pin()));
        std::fs::create_dir_all(&dir).unwrap();
        let apk = dir.join("Agent.apk");
        std::fs::write(&apk, b"PK\x03\x04apk").unwrap();
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (b, rx) = Bridge::with_listener(l, role, false, dir.clone(), Some(apk), "123456".into());
        (b, rx, dir)
    }

    #[tokio::test]
    async fn pin_auth_and_lockout() {
        let (b, mut rx, dir) = server(Role::Export).await;
        let a = b.addr;
        // publiczne
        assert_eq!(req(a, "GET", "/ping", None, b"").await.0, 200);
        let (c, apk) = req(a, "GET", "/agent.apk", None, b"").await;
        assert_eq!((c, apk.as_slice()), (200, b"PK\x03\x04apk".as_slice()));
        assert_eq!(req(a, "GET", "/p", None, b"").await.0, 200);
        // bez tokenu - nic więcej
        assert_eq!(req(a, "GET", "/job", None, b"").await.0, 401);
        assert_eq!(req(a, "GET", "/job", Some("deadbeef"), b"").await.0, 401);
        assert_eq!(req(a, "PUT", "/export/file?path=files/a&size=1", None, b"x").await.0, 401);
        // zły PIN
        let (c, _) = req(a, "POST", "/pair", None, br#"{"pin":"000000"}"#).await;
        assert_eq!(c, 403);
        // dobry PIN -> token
        let (c, body) = req(a, "POST", "/pair", None, br#"{"pin":"123456","device":{"model":"X"}}"#).await;
        assert_eq!(c, 200);
        let v = js(&body);
        let token = v["token"].as_str().unwrap().to_string();
        assert_eq!(token.len(), 64);
        assert_eq!(v["role"], "export");
        assert!(matches!(rx.recv().await, Some(AgentEvent::Log { .. })));
        assert!(matches!(rx.recv().await, Some(AgentEvent::Paired(_))));
        // drugie urządzenie nie przejmie sesji
        assert_eq!(req(a, "POST", "/pair", None, br#"{"pin":"123456"}"#).await.0, 409);
        let (c, body) = req(a, "GET", "/job", Some(&token), b"").await;
        assert_eq!((c, js(&body)["state"].clone()), (200, json!("wait")));
        b.shared.lock().last_seen = None;
        assert_eq!(req(a, "POST", "/ping", Some(&token), b"").await.0, 200);
        assert!(b.shared.lock().last_seen.is_some(), "sygnał życia agenta");
        b.shared.lock().last_seen = None;
        assert_eq!(req(a, "POST", "/ping", Some("bad"), b"").await.0, 401);
        assert!(b.shared.lock().last_seen.is_none(), "bez tokenu nie liczy się jako agent");
        // anulowanie
        b.shared.lock().cancelled = true;
        assert_eq!(req(a, "GET", "/job", Some(&token), b"").await.0, 410);
        drop(b);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn brute_force_locked() {
        let (b, _rx, dir) = server(Role::Import).await;
        for _ in 0..MAX_PIN_FAILURES {
            assert_eq!(req(b.addr, "POST", "/pair", None, br#"{"pin":"999999"}"#).await.0, 403);
        }
        // po 5 próbach nawet poprawny PIN nie działa
        assert_eq!(req(b.addr, "POST", "/pair", None, br#"{"pin":"123456"}"#).await.0, 423);
        assert!(b.shared.lock().locked);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn export_endpoints() {
        let (b, mut rx, dir) = server(Role::Export).await;
        let a = b.addr;
        let (_, body) = req(a, "POST", "/pair", None, br#"{"pin":"123456"}"#).await;
        let t = js(&body)["token"].as_str().unwrap().to_string();
        let _ = rx.recv().await;
        // przed startem (podsumowanie) agent niczego nie zapisze
        assert_eq!(req(a, "PUT", "/export/file?path=files/a.jpg&size=1", Some(&t), b"x").await.0, 400);
        b.shared.lock().job = json!({"state": "go"});
        // upload z weryfikacją rozmiaru
        let (c, _) = req(a, "PUT", "/export/file?path=files/DCIM/Camera/IMG%201.jpg&size=5", Some(&t), b"hello").await;
        assert_eq!(c, 200);
        assert_eq!(std::fs::read(dir.join("files/DCIM/Camera/IMG 1.jpg")).unwrap(), b"hello");
        match rx.recv().await {
            Some(AgentEvent::Uploaded { path, size }) => assert_eq!((path.as_str(), size), ("files/DCIM/Camera/IMG 1.jpg", 5)),
            o => panic!("{o:?}"),
        }
        let (_, body) = req(a, "GET", "/export/has?path=files/DCIM/Camera/IMG%201.jpg&size=5", Some(&t), b"").await;
        assert_eq!(js(&body)["have"], true);
        let (_, body) = req(a, "GET", "/export/has?path=files/DCIM/Camera/IMG%201.jpg&size=6", Some(&t), b"").await;
        assert_eq!(js(&body)["have"], false);
        // niezgodny rozmiar / wyjście poza folder kopii
        assert_eq!(req(a, "PUT", "/export/file?path=files/x&size=9", Some(&t), b"abc").await.0, 400);
        assert_eq!(req(a, "PUT", "/export/file?path=../evil&size=3", Some(&t), b"abc").await.0, 400);
        assert_eq!(req(a, "PUT", "/export/file?path=manifest.json&size=3", Some(&t), b"abc").await.0, 400);
        assert!(!dir.join("files/x").exists() && !dir.join("files/x.part").exists());
        // lista aplikacji -> klasyfikacja; agent wysyła APK tylko gdy komputer o to prosi (Wi-Fi)
        b.shared.lock().agent_sends_apks = true;
        let apps = br#"{"apps":[{"package":"com.whatsapp","label":"WhatsApp","installer":"com.android.vending"},
            {"package":"org.fdroid.fdroid","label":"F-Droid","installer":null},
            {"package":"com.android.systemui","system":true}]}"#;
        let (c, body) = req(a, "POST", "/export/apps", Some(&t), apps).await;
        assert_eq!(c, 200);
        assert_eq!(js(&body)["apks"], json!(["org.fdroid.fdroid"]));
        match rx.recv().await {
            Some(AgentEvent::Apps(l)) => assert_eq!(l.len(), 2),
            o => panic!("{o:?}"),
        }
        // import nie jest dostępny w sesji eksportu
        assert_eq!(req(a, "GET", "/import/file?path=files/DCIM/Camera/IMG%201.jpg", Some(&t), b"").await.0, 403);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn import_whitelist_and_queue() {
        let (b, _rx, dir) = server(Role::Import).await;
        let a = b.addr;
        std::fs::create_dir_all(dir.join("contacts_sms")).unwrap();
        std::fs::write(dir.join("contacts_sms/contacts.vcf"), b"VCARD").unwrap();
        std::fs::write(dir.join("contacts_sms/sms.xml"), b"SMS").unwrap();
        let (_, body) = req(a, "POST", "/pair", None, br#"{"pin":"123456"}"#).await;
        let t = js(&body)["token"].as_str().unwrap().to_string();
        assert_eq!(req(a, "GET", "/import/plan", Some(&t), b"").await.0, 404);
        {
            let mut g = b.shared.lock();
            g.plan = Some(json!({"contacts": true}));
            g.allowed_files.insert("contacts_sms/contacts.vcf".into());
            g.queue = Some(PlayQueue::new(vec![("com.a".into(), "A".into()), ("com.b".into(), "B".into())]));
        }
        assert_eq!(req(a, "GET", "/import/plan", Some(&t), b"").await.0, 200);
        let (c, f) = req(a, "GET", "/import/file?path=contacts_sms/contacts.vcf", Some(&t), b"").await;
        assert_eq!((c, f.as_slice()), (200, b"VCARD".as_slice()));
        // plik spoza planu - odmowa, nawet jeśli istnieje
        assert_eq!(req(a, "GET", "/import/file?path=contacts_sms/sms.xml", Some(&t), b"").await.0, 403);
        assert_eq!(req(a, "GET", "/import/file?path=manifest.json", Some(&t), b"").await.0, 403);
        // kolejka Sklepu Play
        let (_, body) = req(a, "GET", "/queue", Some(&t), b"").await;
        assert_eq!((js(&body)["position"].clone(), js(&body)["total"].clone()), (json!(1), json!(2)));
        let (_, body) = req(a, "POST", "/queue/installed", Some(&t), br#"{"package":"com.a"}"#).await;
        assert_eq!(js(&body)["current"]["package"], "com.b");
        let (_, body) = req(a, "POST", "/queue/skip", Some(&t), b"").await;
        assert_eq!(js(&body)["finished"], true);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn head_parsing() {
        let h = parse_head("GET /export/has?path=files%2FDCIM%2Fa+b.jpg&size=3 HTTP/1.1\r\nX-SmartEI-Token: t\r\nContent-Length: 0").unwrap();
        assert_eq!(h.path, "/export/has");
        assert_eq!(h.query["path"], "files/DCIM/a b.jpg");
        assert_eq!(h.headers[TOKEN_HEADER], "t");
        assert!(parse_head("GET / FTP/1.0").is_none());
        assert!(parse_head("PUT /x HTTP/1.1\r\nTransfer-Encoding: chunked").is_none());
        assert_eq!(percent_decode("%zz%41"), "%zzA");
    }
}
