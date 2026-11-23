//! ADB przez Wi-Fi.
//!
//! Dwa standardowe tryby:
//! - **Parowanie kodem** (Android 11+, „Debugowanie bezprzewodowe”):
//!   `adb pair IP:PORT_PAROWANIA KOD` -> ustalenie portu połączenia
//!   (podany ręcznie, auto-połączenie serwera adb przez mDNS albo
//!   `adb mdns services`) -> `adb connect IP:PORT`.
//! - **TCP/IP** (Android 10 i starsze, urządzenie na kablu):
//!   odczyt IP z urządzenia -> `adb tcpip 5555` -> „można odłączyć kabel” ->
//!   `adb connect IP:5555` (z ponawianiem, adbd restartuje się kilka sekund).
//!
//! Każde wywołanie adb ma twardy timeout (10 s), a znane komunikaty
//! (connection refused, zły kod, brak trasy, brak autoryzacji...) są
//! tłumaczone na czytelne błędy dla GUI.

use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{AdbError, Result};

use super::manager::{Adb, Output};

/// Twardy limit jednego wywołania adb (pair / connect / tcpip).
pub const STEP_TIMEOUT_S: u64 = 10;
/// Ile czekamy na port połączenia po udanym parowaniu (mDNS).
const MDNS_WAIT: Duration = Duration::from_secs(8);
/// Ile ponawiamy `adb connect` po `adb tcpip` (restart adbd).
const TCPIP_CONNECT_WAIT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WifiResult {
    /// serial pod którym urządzenie widać w `adb devices`
    pub serial: String,
    /// adres IP:port
    pub address: String,
    /// stan po połączeniu: device | unauthorized | offline
    pub state: String,
    /// z jakiego źródła ustalono port połączenia (manual | mdns-auto | mdns | tcpip)
    pub via: String,
}

/// Etapy dla GUI (kanał na żywo).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WifiStage {
    /// pair | discover | connect | ip | tcpip | unplug | done
    pub stage: String,
    pub detail: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairRequest {
    pub host: String,
    pub pair_port: u16,
    pub code: String,
    /// port z ekranu „Debugowanie bezprzewodowe” (opcjonalny - inaczej mDNS)
    pub connect_port: Option<u16>,
}

// --------------------------------------------------------------------------- //
// Walidacja
// --------------------------------------------------------------------------- //

/// Serial połączenia sieciowego: `ip:port`, `[ipv6]:port` albo usługa mDNS.
pub fn is_wireless_serial(serial: &str) -> bool {
    if serial.contains("._adb-tls-connect._tcp") || serial.contains("._adb._tcp") {
        return true;
    }
    if serial.starts_with("emulator-") {
        return false;
    }
    match serial.rsplit_once(':') {
        Some((host, port)) => {
            port.parse::<u16>().is_ok()
                && (host.trim_matches(|c| c == '[' || c == ']').parse::<IpAddr>().is_ok() || valid_hostname(host))
        }
        None => false,
    }
}

fn valid_hostname(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.contains('.')
        && h.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

/// Normalizuje host z GUI: IPv4, IPv6 (bez nawiasów) albo nazwa w sieci lokalnej.
pub fn normalize_host(host: &str) -> Result<String> {
    let h = host.trim().trim_matches(|c| c == '[' || c == ']');
    if h.is_empty() {
        return Err(AdbError::InvalidArg("Podaj adres IP urządzenia / enter the device IP address".into()));
    }
    if h.parse::<IpAddr>().is_ok() || valid_hostname(h) {
        Ok(h.to_string())
    } else {
        Err(AdbError::InvalidArg(format!(
            "Nieprawidłowy adres IP: '{h}' (np. 192.168.1.20) / invalid IP address"
        )))
    }
}

/// `host:port` w formacie rozumianym przez adb (IPv6 w nawiasach).
pub fn address(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn valid_port(p: u16, what: &str) -> Result<u16> {
    if p == 0 {
        return Err(AdbError::InvalidArg(format!("Nieprawidłowy {what} (1-65535) / invalid port")));
    }
    Ok(p)
}

/// Kod parowania: 6 cyfr (spacje/myślniki z przepisywania są usuwane).
pub fn normalize_code(code: &str) -> Result<String> {
    let c: String = code.chars().filter(|c| !c.is_whitespace() && *c != '-').collect();
    if c.len() == 6 && c.chars().all(|x| x.is_ascii_digit()) {
        Ok(c)
    } else {
        Err(AdbError::InvalidArg(
            "Kod parowania to 6 cyfr z okna „Paruj urządzenie za pomocą kodu parowania” / pairing code must be 6 digits".into(),
        ))
    }
}

// --------------------------------------------------------------------------- //
// Tłumaczenie błędów adb
// --------------------------------------------------------------------------- //

/// Zamienia surowy komunikat adb na czytelny błąd (PL / EN).
pub fn explain(raw: &str) -> String {
    let l = raw.to_lowercase();
    let raw = raw.trim();
    let msg = if l.contains("wrong password") || l.contains("incorrect") && l.contains("code") {
        "Zły kod parowania (PIN) albo kod wygasł - otwórz ponownie „Paruj za pomocą kodu” na urządzeniu i wpisz nowy kod / \
         wrong pairing code"
    } else if l.contains("connection refused") || l.contains("(111)") || l.contains("10061") {
        "Połączenie odrzucone - zły port lub debugowanie bezprzewodowe jest wyłączone. Port parowania i port połączenia \
         są różne i zmieniają się po każdym włączeniu / connection refused"
    } else if l.contains("no route to host") || l.contains("network is unreachable") || l.contains("(113)") || l.contains("10065") {
        "Brak trasy do urządzenia - komputer i urządzenie muszą być w tej samej sieci (sprawdź Wi-Fi, izolację klientów AP, VPN) / \
         no route to host"
    } else if l.contains("timed out") || l.contains("timeout") || l.contains("(110)") || l.contains("10060") {
        "Przekroczono czas połączenia - urządzenie nie odpowiada (inna sieć, uśpiony ekran, zapora) / connection timed out"
    } else if l.contains("failed to authenticate") || l.contains("unauthorized") {
        "Urządzenie nie autoryzowało komputera - zaakceptuj „Zezwolić na debugowanie?” na ekranie urządzenia i połącz ponownie / \
         not authorized"
    } else if l.contains("unknown command") || l.contains("unknown host service") && l.contains("pair") {
        "Zainstalowany adb jest za stary na parowanie (wymagany platform-tools 30+) - zaktualizuj platform-tools / adb too old"
    } else if l.contains("unable to start pairing client") || l.contains("protocol fault") {
        "Nie udało się rozpocząć parowania - sprawdź IP i port PAROWANIA (z okienka z kodem, nie z ekranu głównego \
         Debugowania bezprzewodowego) i czy okienko z kodem jest nadal otwarte / pairing failed to start"
    } else if l.contains("cannot resolve host") || l.contains("name or service not known") || l.contains("no such host") {
        "Nie można rozwiązać nazwy hosta - użyj adresu IP / cannot resolve host"
    } else if l.contains("no devices/emulators found") || l.contains("device not found") || l.contains("no such device") {
        "Urządzenie nie jest już podłączone / device not found"
    } else {
        ""
    };
    if msg.is_empty() {
        if raw.is_empty() {
            "adb nie zwrócił szczegółów / no details from adb".into()
        } else {
            format!("adb: {raw}")
        }
    } else {
        msg.to_string()
    }
}

fn joined(o: &Output) -> String {
    format!("{}\n{}", o.stdout.trim(), o.stderr.trim()).trim().to_string()
}

/// Uruchamia adb z limitem 10 s; timeout -> czytelny błąd.
async fn run10(adb: &Adb, serial: Option<&str>, args: &[&str]) -> Result<Output> {
    match adb.run(serial, args, STEP_TIMEOUT_S).await {
        Err(AdbError::Timeout(_)) => Err(AdbError::Other(format!(
            "Przekroczono limit {STEP_TIMEOUT_S} s (adb {}) - urządzenie nie odpowiada. Sprawdź, czy jest w tej samej sieci \
             i ma włączone debugowanie / timed out",
            args.first().copied().unwrap_or("")
        ))),
        other => other,
    }
}

// --------------------------------------------------------------------------- //
// adb devices / mdns
// --------------------------------------------------------------------------- //

/// (serial, stan) z `adb devices`.
async fn list_devices(adb: &Adb) -> Vec<(String, String)> {
    adb.run(None, &["devices"], 5)
        .await
        .map(|o| {
            o.stdout
                .lines()
                .skip_while(|l| !l.starts_with("List of devices"))
                .skip(1)
                .filter_map(|l| {
                    let mut p = l.split_whitespace();
                    Some((p.next()?.to_string(), p.next()?.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Usługi `_adb-tls-connect` z `adb mdns services`: (nazwa, adres).
async fn mdns_connect_services(adb: &Adb) -> Vec<(String, String)> {
    adb.run(None, &["mdns", "services"], 5)
        .await
        .map(|o| parse_mdns(&o.stdout))
        .unwrap_or_default()
}

pub fn parse_mdns(stdout: &str) -> Vec<(String, String)> {
    stdout
        .lines()
        .filter_map(|l| {
            let parts: Vec<&str> = l.split_whitespace().collect();
            let ty = parts.iter().position(|p| p.starts_with("_adb-tls-connect"))?;
            let name = parts.first()?.to_string();
            let addr = parts.get(ty + 1)?.to_string();
            addr.rsplit_once(':')?.1.parse::<u16>().ok()?;
            Some((name, addr))
        })
        .collect()
}

/// GUID z `Successfully paired to 1.2.3.4:5 [guid=adb-XXXX-yyyy]`.
pub fn parse_pair_guid(out: &str) -> Option<String> {
    let start = out.find("guid=")? + 5;
    let rest = &out[start..];
    let end = rest.find([']', ' ', '\n']).unwrap_or(rest.len());
    let g = rest[..end].trim();
    (!g.is_empty()).then(|| g.to_string())
}

// --------------------------------------------------------------------------- //
// Operacje
// --------------------------------------------------------------------------- //

/// `adb connect host:port` + sprawdzenie stanu w `adb devices`.
pub async fn connect(adb: &Adb, host: &str, port: u16) -> Result<WifiResult> {
    let host = normalize_host(host)?;
    let port = valid_port(port, "port")?;
    let addr = address(&host, port);
    let o = run10(adb, None, &["connect", &addr]).await?;
    let text = joined(&o);
    let low = text.to_lowercase();
    if !(low.contains("connected to") && !low.contains("failed") && !low.contains("cannot")) {
        return Err(AdbError::Other(explain(&text)));
    }
    // stan po połączeniu (unauthorized = trzeba zaakceptować klucz na urządzeniu)
    let mut state = "device".to_string();
    for _ in 0..6 {
        if let Some((_, st)) = list_devices(adb).await.into_iter().find(|(s, _)| s == &addr) {
            state = st;
            if state == "device" {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Ok(WifiResult { serial: addr.clone(), address: addr, state, via: "manual".into() })
}

/// Parowanie kodem (Android 11+) i od razu połączenie.
pub async fn pair_and_connect(adb: &Adb, req: &PairRequest, stage: impl Fn(&str, String)) -> Result<WifiResult> {
    let host = normalize_host(&req.host)?;
    let pair_port = valid_port(req.pair_port, "port parowania")?;
    let code = normalize_code(&req.code)?;
    let pair_addr = address(&host, pair_port);

    stage("pair", pair_addr.clone());
    let o = run10(adb, None, &["pair", &pair_addr, &code]).await?;
    let text = joined(&o);
    if !text.to_lowercase().contains("successfully paired") {
        return Err(AdbError::Other(explain(&text)));
    }
    let guid = parse_pair_guid(&text);

    // 1. port podany ręcznie
    if let Some(p) = req.connect_port.filter(|p| *p > 0) {
        stage("connect", address(&host, p));
        let mut r = connect(adb, &host, p).await?;
        r.via = "manual".into();
        return Ok(r);
    }

    // 2. serwer adb sam łączy się po mDNS / 3. port z `adb mdns services`
    stage("discover", guid.clone().unwrap_or_default());
    let started = Instant::now();
    let mut tried: Vec<String> = Vec::new();
    while started.elapsed() < MDNS_WAIT {
        if let Some(g) = guid.as_deref() {
            if let Some((serial, st)) = list_devices(adb).await.into_iter().find(|(s, _)| s.starts_with(g)) {
                if st == "device" {
                    return Ok(WifiResult { address: serial.clone(), serial, state: st, via: "mdns-auto".into() });
                }
            }
        }
        for (name, addr) in mdns_connect_services(adb).await {
            let ours = guid.as_deref().map(|g| name.starts_with(g)).unwrap_or(false)
                || addr.rsplit_once(':').map(|(h, _)| h.trim_matches(['[', ']']) == host).unwrap_or(false);
            if !ours || tried.contains(&addr) {
                continue;
            }
            tried.push(addr.clone());
            let Some(port) = addr.rsplit_once(':').and_then(|(_, p)| p.parse::<u16>().ok()) else { continue };
            stage("connect", address(&host, port));
            if let Ok(mut r) = connect(adb, &host, port).await {
                r.via = "mdns".into();
                return Ok(r);
            }
        }
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
    Err(AdbError::Other(
        "Sparowano poprawnie, ale nie udało się ustalić portu połączenia (mDNS niedostępne w tej sieci). Wpisz „Port połączenia” \
         z głównego ekranu Debugowania bezprzewodowego (Adres IP i port) i spróbuj ponownie - parowanie nie jest już potrzebne / \
         paired, but connect port unknown - enter it manually"
            .into(),
    ))
}

/// Adres IPv4 urządzenia w sieci lokalnej (Wi-Fi, potem Ethernet - Android TV).
pub async fn device_lan_ip(adb: &Adb, serial: &str) -> Result<(String, String)> {
    let o = adb
        .shell(
            Some(serial),
            "ip -o -4 addr show 2>/dev/null; echo ___; ifconfig 2>/dev/null; echo ___; \
             getprop dhcp.wlan0.ipaddress; getprop dhcp.eth0.ipaddress",
            STEP_TIMEOUT_S,
        )
        .await?;
    parse_lan_ip(&o.stdout).ok_or_else(|| {
        AdbError::Other(
            "Urządzenie nie ma adresu w sieci lokalnej - połącz je z tą samą siecią Wi-Fi (lub Ethernet) co komputer / \
             device has no LAN address"
                .into(),
        )
    })
}

fn usable_iface(name: &str) -> bool {
    !(name == "lo"
        || ["rmnet", "ccmni", "dummy", "tun", "v4-", "ifb", "sit", "ip6", "r_rmnet", "seth", "rev_rmnet"]
            .iter()
            .any(|p| name.starts_with(p)))
}

fn iface_rank(name: &str) -> u8 {
    if name.starts_with("wlan") || name.starts_with("wifi") {
        0
    } else if name.starts_with("eth") {
        1
    } else {
        2
    }
}

/// (ip, interfejs) z wyjścia `ip -o -4 addr show` / `ifconfig` / getprop.
pub fn parse_lan_ip(out: &str) -> Option<(String, String)> {
    let mut found: Vec<(u8, String, String)> = Vec::new();
    let ok_ip = |ip: &str| ip.parse::<Ipv4Addr>().ok().filter(|a| !a.is_loopback() && !a.is_unspecified() && !a.is_link_local());
    let mut sections = out.split("___");
    // ip -o -4 addr show: "3: wlan0    inet 192.168.1.20/24 brd ... scope global wlan0"
    for l in sections.next().unwrap_or("").lines() {
        let p: Vec<&str> = l.split_whitespace().collect();
        let (Some(i), Some(name)) = (p.iter().position(|x| *x == "inet"), p.get(1)) else { continue };
        let name = name.trim_end_matches(':');
        let Some(ip) = p.get(i + 1).and_then(|c| c.split('/').next()) else { continue };
        if usable_iface(name) && ok_ip(ip).is_some() {
            found.push((iface_rank(name), ip.to_string(), name.to_string()));
        }
    }
    // ifconfig (stare Androidy): "wlan0 Link encap..." / "inet addr:192.168.1.20"
    let mut cur = String::new();
    for l in sections.next().unwrap_or("").lines() {
        if !l.starts_with(' ') && !l.starts_with('\t') && !l.trim().is_empty() {
            cur = l.split_whitespace().next().unwrap_or("").trim_end_matches(':').to_string();
        }
        if let Some(rest) = l.trim().strip_prefix("inet addr:").or_else(|| l.trim().strip_prefix("inet ")) {
            let ip = rest.split_whitespace().next().unwrap_or("").trim_start_matches("addr:");
            if usable_iface(&cur) && ok_ip(ip).is_some() {
                found.push((iface_rank(&cur), ip.to_string(), cur.clone()));
            }
        }
    }
    // getprop dhcp.*.ipaddress
    for (l, name) in sections.next().unwrap_or("").lines().map(str::trim).filter(|l| !l.is_empty()).zip(["wlan0", "eth0"]) {
        if ok_ip(l).is_some() {
            found.push((iface_rank(name), l.to_string(), name.to_string()));
        }
    }
    found.sort_by_key(|f| f.0);
    found.into_iter().next().map(|(_, ip, n)| (ip, n))
}

/// Tryb TCP/IP dla urządzenia na kablu: IP -> `adb tcpip` -> connect.
pub async fn tcpip_switch(adb: &Adb, serial: &str, port: u16, stage: impl Fn(&str, String)) -> Result<WifiResult> {
    let port = valid_port(port, "port")?;
    if is_wireless_serial(serial) {
        return Err(AdbError::InvalidArg(
            "To urządzenie jest już połączone bezprzewodowo / device is already wireless".into(),
        ));
    }
    match list_devices(adb).await.into_iter().find(|(s, _)| s == serial) {
        Some((_, st)) if st == "device" => {}
        Some((_, st)) => {
            return Err(AdbError::Other(format!(
                "Urządzenie jest w stanie '{st}' - wymagany tryb ADB z autoryzacją (Android uruchomiony) / device not ready"
            )))
        }
        None => return Err(AdbError::Other(explain("device not found"))),
    }

    // IP odczytujemy PRZED restartem adbd (potem kabel przestaje działać)
    stage("ip", String::new());
    let (ip, iface) = device_lan_ip(adb, serial).await?;
    stage("ip", format!("{ip} ({iface})"));

    stage("tcpip", port.to_string());
    let port_s = port.to_string();
    let o = run10(adb, Some(serial), &["tcpip", &port_s]).await?;
    let text = joined(&o);
    let low = text.to_lowercase();
    if !low.contains("restarting") && (low.contains("error") || low.contains("failed")) {
        return Err(AdbError::Other(explain(&text)));
    }

    stage("unplug", address(&ip, port));
    // adbd potrzebuje chwili na restart w trybie TCP
    tokio::time::sleep(Duration::from_millis(2500)).await;
    stage("connect", address(&ip, port));
    let started = Instant::now();
    let mut last = String::new();
    while started.elapsed() < TCPIP_CONNECT_WAIT {
        match connect(adb, &ip, port).await {
            Ok(mut r) => {
                r.via = "tcpip".into();
                return Ok(r);
            }
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;
    }
    Err(AdbError::Other(format!(
        "Urządzenie przełączone w tryb TCP/IP ({}), ale połączenie się nie udało: {last}",
        address(&ip, port)
    )))
}

/// `adb disconnect <serial>` - tylko dla połączeń sieciowych.
pub async fn disconnect(adb: &Adb, serial: &str) -> Result<String> {
    if !is_wireless_serial(serial) {
        return Err(AdbError::InvalidArg(format!(
            "'{serial}' nie jest połączeniem bezprzewodowym / not a wireless device"
        )));
    }
    let o = run10(adb, None, &["disconnect", serial]).await?;
    let text = joined(&o);
    if text.to_lowercase().contains("disconnected") {
        Ok(text)
    } else {
        Err(AdbError::Other(explain(&text)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wireless_serials() {
        assert!(is_wireless_serial("192.168.1.20:5555"));
        assert!(is_wireless_serial("[fe80::1]:37000"));
        assert!(is_wireless_serial("adb-R58M41ABCDE-2xMhX0._adb-tls-connect._tcp"));
        assert!(!is_wireless_serial("R58M41ABCDE"));
        assert!(!is_wireless_serial("emulator-5554"));
    }

    #[test]
    fn validates_input() {
        assert_eq!(normalize_code(" 123 456 ").unwrap(), "123456");
        assert!(normalize_code("12345").is_err());
        assert!(normalize_code("abcdef").is_err());
        assert!(normalize_host("192.168.1.20").is_ok());
        assert!(normalize_host("[fe80::1]").is_ok());
        assert!(normalize_host("192.168.1.20; rm -rf").is_err());
        assert!(normalize_host("").is_err());
        assert_eq!(address("fe80::1", 5555), "[fe80::1]:5555");
    }

    #[test]
    fn parses_pair_and_mdns() {
        let g = parse_pair_guid("Successfully paired to 192.168.1.20:37215 [guid=adb-R58M41ABCDE-2xMhX0]");
        assert_eq!(g.as_deref(), Some("adb-R58M41ABCDE-2xMhX0"));
        let m = parse_mdns(
            "List of discovered mdns services\nadb-R58M41ABCDE-2xMhX0\t_adb-tls-connect._tcp\t192.168.1.20:41871\n\
             adb-R58M41ABCDE-2xMhX0\t_adb-tls-pairing._tcp\t192.168.1.20:37215\n",
        );
        assert_eq!(m, vec![("adb-R58M41ABCDE-2xMhX0".to_string(), "192.168.1.20:41871".to_string())]);
    }

    #[test]
    fn parses_lan_ip() {
        let out = "1: lo    inet 127.0.0.1/8 scope host lo\n\
                   12: rmnet_data0    inet 10.12.4.5/30 scope global rmnet_data0\n\
                   30: wlan0    inet 192.168.1.20/24 brd 192.168.1.255 scope global wlan0\n___\n___\n";
        assert_eq!(parse_lan_ip(out), Some(("192.168.1.20".into(), "wlan0".into())));
        let tv = "2: eth0    inet 10.0.0.7/24 brd 10.0.0.255 scope global eth0\n___\n___\n";
        assert_eq!(parse_lan_ip(tv), Some(("10.0.0.7".into(), "eth0".into())));
        let old = "___\nwlan0: ip 192.168.0.5 mask 255.255.255.0 flags [up]\n          inet addr:192.168.0.5  Bcast:192.168.0.255\n___\n";
        assert_eq!(parse_lan_ip(old), Some(("192.168.0.5".into(), "wlan0".into())));
        assert_eq!(parse_lan_ip("___\n___\n192.168.43.9\n"), Some(("192.168.43.9".into(), "wlan0".into())));
        assert_eq!(parse_lan_ip("1: lo inet 127.0.0.1/8\n___\n___\n"), None);
    }

    #[test]
    fn explains_errors() {
        assert!(explain("failed to connect to '1.2.3.4:5555': Connection refused").contains("odrzucone"));
        assert!(explain("Failed: Wrong password or connection was interrupted.").contains("kod"));
        assert!(explain("cannot connect to 1.2.3.4:5555: No route to host (113)").contains("trasy"));
        assert!(explain("weird").starts_with("adb: weird"));
    }
}
