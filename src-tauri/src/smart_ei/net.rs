//! Sieć lokalna dla trybu "Bez ADB": wybór adresu IP w LAN, kod QR (SVG generowany
//! lokalnie, bez usług zewnętrznych), PIN i token sesji z kryptograficznego RNG.

use std::net::{IpAddr, Ipv4Addr};

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NetIf {
    pub name: String,
    pub ip: String,
    /// Docker / VPN / maszyny wirtualne - nie nadają się do połączenia z telefonem.
    pub virtual_if: bool,
    pub recommended: bool,
}

const VIRTUAL_HINTS: &[&str] = &[
    "docker", "br-", "veth", "virbr", "vmnet", "vboxnet", "virtualbox", "vmware", "hyper-v", "vethernet", "wsl",
    "tun", "tap", "wg", "wireguard", "zt", "zerotier", "tailscale", "utun", "ppp", "openvpn", "nordlynx", "proton",
    "npcap", "loopback", "lxc", "lxd", "podman", "cni", "flannel", "kube", "bluetooth",
];

pub fn is_virtual_name(name: &str) -> bool {
    let n = name.to_lowercase();
    VIRTUAL_HINTS.iter().any(|h| n.starts_with(h) || n.contains(&format!("({h}")) || (h.len() > 4 && n.contains(h)))
}

/// Prywatna sieć lokalna (RFC 1918). 100.64/10 (CGNAT, Tailscale) i 169.254 nie są LAN-em telefonu.
pub fn is_lan_v4(ip: Ipv4Addr) -> bool {
    ip.is_private()
}

/// Kto może łączyć się z mostkiem: wyłącznie adresy lokalne (loopback dla `adb reverse`,
/// sieć prywatna, link-local) - nigdy publiczne adresy z internetu.
pub fn peer_allowed(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || v6.to_ipv4_mapped().is_some_and(|m| peer_allowed(IpAddr::V4(m)))
        }
    }
}

/// Lista interfejsów IPv4 z oceną (rekomendowany: fizyczny, adres prywatny, preferencja 192.168.x).
pub fn classify(list: Vec<(String, IpAddr)>) -> Vec<NetIf> {
    let mut out: Vec<NetIf> = list
        .into_iter()
        .filter_map(|(name, ip)| match ip {
            IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => Some((name, v4)),
            _ => None,
        })
        .map(|(name, v4)| NetIf {
            virtual_if: is_virtual_name(&name) || !is_lan_v4(v4),
            name,
            ip: v4.to_string(),
            recommended: false,
        })
        .collect();
    out.sort_by_key(|n| {
        let pref = if n.ip.starts_with("192.168.") { 0 } else if n.ip.starts_with("10.") { 1 } else { 2 };
        (n.virtual_if, pref, n.name.clone())
    });
    out.dedup_by(|a, b| a.ip == b.ip);
    if let Some(first) = out.first_mut() {
        if !first.virtual_if {
            first.recommended = true;
        }
    }
    out
}

pub fn interfaces() -> Vec<NetIf> {
    classify(local_ip_address::list_afinet_netifas().unwrap_or_default())
}

/// Kod QR jako SVG (czarne moduły na białym tle - czytelne dla każdego aparatu).
pub fn qr_svg(data: &str) -> Option<String> {
    use qrcode::render::svg;
    let code = qrcode::QrCode::with_error_correction_level(data.as_bytes(), qrcode::EcLevel::M).ok()?;
    Some(
        code.render::<svg::Color>()
            .min_dimensions(240, 240)
            .quiet_zone(true)
            .dark_color(svg::Color("#000000"))
            .light_color(svg::Color("#ffffff"))
            .build(),
    )
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("system RNG");
    b
}

/// 6-cyfrowy PIN (bez przesunięcia modulo - losowanie z odrzucaniem).
pub fn new_pin() -> String {
    loop {
        let v = u32::from_le_bytes(random_bytes::<4>());
        // 4_294_000_000 jest wielokrotnością 1_000_000
        if v < 4_294_000_000 {
            return format!("{:06}", v % 1_000_000);
        }
    }
}

/// Jednorazowy token sesji (256 bitów, hex).
pub fn new_token() -> String {
    crate::utils::download::hex(&random_bytes::<32>())
}

/// Losowy port z zakresu dynamicznego (49152-65535).
pub fn random_port() -> u16 {
    49152 + (u16::from_le_bytes(random_bytes::<2>()) % 16000)
}

/// Porównanie w stałym czasie (PIN / token - brak wycieku przez czas odpowiedzi).
pub fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8 | u8::from(a.len() != b.len());
    for i in 0..a.len().max(b.len()) {
        diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0xff);
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_selection_skips_virtual() {
        let l = classify(vec![
            ("lo".into(), "127.0.0.1".parse().unwrap()),
            ("docker0".into(), "172.17.0.1".parse().unwrap()),
            ("vEthernet (WSL)".into(), "172.29.80.1".parse().unwrap()),
            ("tailscale0".into(), "100.101.2.3".parse().unwrap()),
            ("wlp3s0".into(), "192.168.1.23".parse().unwrap()),
            ("enp0s31f6".into(), "10.0.0.5".parse().unwrap()),
            ("wlp3s0".into(), "fe80::1".parse().unwrap()),
        ]);
        assert_eq!(l[0].ip, "192.168.1.23");
        assert!(l[0].recommended);
        assert_eq!(l[1].ip, "10.0.0.5");
        assert!(!l[1].virtual_if && !l[1].recommended);
        assert!(l.iter().filter(|n| n.name != "wlp3s0" && n.name != "enp0s31f6").all(|n| n.virtual_if));
        assert!(!l.iter().any(|n| n.ip == "127.0.0.1"));
        assert!(is_virtual_name("VMware Network Adapter VMnet8"));
        assert!(is_virtual_name("ZeroTier One [abc]"));
        assert!(!is_virtual_name("Wi-Fi"));
        assert!(!is_virtual_name("Ethernet"));
        assert!(!is_virtual_name("wlan0"));
    }

    #[test]
    fn only_local_peers() {
        for ok in ["127.0.0.1", "192.168.0.10", "10.1.2.3", "172.20.1.1", "169.254.3.4", "::1", "fe80::2", "::ffff:192.168.1.5"] {
            assert!(peer_allowed(ok.parse().unwrap()), "{ok}");
        }
        for bad in ["8.8.8.8", "100.64.1.1", "2001:4860::8888", "::ffff:1.1.1.1"] {
            assert!(!peer_allowed(bad.parse().unwrap()), "{bad}");
        }
    }

    #[test]
    fn pin_token_qr() {
        for _ in 0..200 {
            let p = new_pin();
            assert_eq!(p.len(), 6);
            assert!(p.chars().all(|c| c.is_ascii_digit()));
        }
        let t = new_token();
        assert_eq!(t.len(), 64);
        assert_ne!(t, new_token());
        assert!((49152..=65535).contains(&random_port()));
        assert!(ct_eq("123456", "123456"));
        assert!(!ct_eq("123456", "123457"));
        assert!(!ct_eq("123456", "12345"));
        assert!(!ct_eq("", "1"));
        assert!(ct_eq("", ""));
        let svg = qr_svg("http://192.168.1.23:50000/p#k=123456").unwrap();
        assert!(svg.starts_with("<?xml") || svg.starts_with("<svg"));
        assert!(svg.contains("</svg>"));
    }
}
