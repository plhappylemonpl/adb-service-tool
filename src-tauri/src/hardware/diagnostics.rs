//! Diagnostyka sprzętu: bateria (sysfs power_supply) + czujniki temperatury
//! (thermal_zone*). Jeden `adb shell` na odczyt -> lekko nawet co 0,5 s.
//!
//! Jednostki w sysfs różnią się między producentami (µV/mV, µA/mA,
//! °C/0,1°C/m°C) - normalizujemy heurystycznie i oznaczamy brak wsparcia
//! (None), zamiast zgadywać wartości.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;

use crate::adb::Adb;
use crate::error::Result;
use crate::utils::CancelToken;

/// Skrypt: znajduje katalog baterii (type=Battery), czyta pola, źródła zasilania,
/// strefy termiczne; na końcu `dumpsys battery` jako źródło zapasowe.
const SCRIPT: &str = r#"B=/sys/class/power_supply/battery
if [ ! -d "$B" ]; then for d in /sys/class/power_supply/*; do if [ "$(cat $d/type 2>/dev/null)" = "Battery" ]; then B=$d; break; fi; done; fi
echo "DIR=$B"
for f in capacity cycle_count battery_cycle voltage_now current_now current_avg temp status health charge_full charge_full_design charge_counter technology charge_type time_to_full_now input_current_limit voltage_max; do v=$(cat $B/$f 2>/dev/null); echo "B:$f=$v"; done
for p in /sys/class/power_supply/*; do n=${p##*/}; echo "P:$n=$(cat $p/online 2>/dev/null)|$(cat $p/type 2>/dev/null)"; done
for z in /sys/class/thermal/thermal_zone*; do echo "T:$(cat $z/type 2>/dev/null)|$(cat $z/temp 2>/dev/null)"; done
echo __DUMPSYS__
dumpsys battery 2>/dev/null"#;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatteryInfo {
    pub source: String,
    pub capacity: Option<f64>,
    pub cycle_count: Option<u64>,
    pub voltage_mv: Option<f64>,
    /// dodatni = ładowanie, ujemny = rozładowanie (znormalizowane wg statusu)
    pub current_ma: Option<f64>,
    pub temp_c: Option<f64>,
    pub status: Option<String>,
    pub health: Option<String>,
    pub charge_full_mah: Option<f64>,
    pub charge_full_design_mah: Option<f64>,
    /// pojemność rzeczywista / projektowa w %
    pub health_pct: Option<f64>,
    pub technology: Option<String>,
    pub charge_type: Option<String>,
    pub power_w: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThermalZone {
    pub name: String,
    pub temp_c: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PowerSource {
    pub name: String,
    pub kind: String,
    pub online: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagSnapshot {
    pub ts: i64,
    pub battery: BatteryInfo,
    pub thermal: Vec<ThermalZone>,
    pub sources: Vec<PowerSource>,
    /// Pola, których urządzenie nie udostępnia (do komunikatu "brak wsparcia")
    pub unsupported: Vec<String>,
}

fn num(s: Option<&String>) -> Option<f64> {
    s.and_then(|v| v.trim().parse::<f64>().ok())
}

/// µV -> mV (większość urządzeń), mV zostaje.
fn norm_voltage_mv(v: f64) -> f64 {
    if v.abs() > 100_000.0 {
        v / 1000.0
    } else {
        v
    }
}

/// µA -> mA (gdy |v| > 20 000), mA zostaje (Samsung).
fn norm_current_ma(v: f64) -> f64 {
    if v.abs() > 20_000.0 {
        v / 1000.0
    } else {
        v
    }
}

/// m°C -> °C, 0,1°C -> °C, °C zostaje.
fn norm_temp_c(v: f64) -> f64 {
    if v.abs() >= 1000.0 {
        v / 1000.0
    } else if v.abs() >= 150.0 {
        v / 10.0
    } else {
        v
    }
}

/// µAh -> mAh
fn norm_charge_mah(v: f64) -> f64 {
    if v > 100_000.0 {
        v / 1000.0
    } else {
        v
    }
}

pub fn parse(text: &str) -> DiagSnapshot {
    let mut b: HashMap<String, String> = HashMap::new();
    let mut thermal = Vec::new();
    let mut sources = Vec::new();
    let mut dump: HashMap<String, String> = HashMap::new();
    let mut in_dump = false;

    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if line.trim() == "__DUMPSYS__" {
            in_dump = true;
            continue;
        }
        if in_dump {
            if let Some((k, v)) = line.trim().split_once(':') {
                dump.insert(k.trim().to_lowercase(), v.trim().to_string());
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("B:") {
            if let Some((k, v)) = rest.split_once('=') {
                if !v.trim().is_empty() {
                    b.insert(k.to_string(), v.trim().to_string());
                }
            }
        } else if let Some(rest) = line.strip_prefix("P:") {
            if let Some((name, v)) = rest.split_once('=') {
                let (online, kind) = v.split_once('|').unwrap_or((v, ""));
                if name != "battery" && kind != "Battery" && !online.trim().is_empty() {
                    sources.push(PowerSource {
                        name: name.to_string(),
                        kind: kind.trim().to_string(),
                        online: online.trim() == "1",
                    });
                }
            }
        } else if let Some(rest) = line.strip_prefix("T:") {
            if let Some((name, t)) = rest.split_once('|') {
                if let Ok(v) = t.trim().parse::<f64>() {
                    let c = norm_temp_c(v);
                    // odrzucamy czujniki wyłączone / bez sensu (-273, 0, >150)
                    if c > -40.0 && c < 150.0 && c != 0.0 {
                        thermal.push(ThermalZone {
                            name: if name.trim().is_empty() { "?".into() } else { name.trim().to_string() },
                            temp_c: (c * 10.0).round() / 10.0,
                        });
                    }
                }
            }
        }
    }

    let mut bat = BatteryInfo {
        source: "sysfs".into(),
        ..Default::default()
    };
    bat.capacity = num(b.get("capacity"));
    bat.cycle_count = num(b.get("cycle_count"))
        .or_else(|| num(b.get("battery_cycle")))
        .map(|v| v as u64);
    bat.voltage_mv = num(b.get("voltage_now")).map(norm_voltage_mv);
    bat.current_ma = num(b.get("current_now"))
        .or_else(|| num(b.get("current_avg")))
        .map(norm_current_ma);
    bat.temp_c = num(b.get("temp")).map(norm_temp_c);
    bat.status = b.get("status").cloned();
    bat.health = b.get("health").cloned();
    bat.technology = b.get("technology").cloned();
    bat.charge_type = b.get("charge_type").cloned();
    bat.charge_full_mah = num(b.get("charge_full")).map(norm_charge_mah);
    bat.charge_full_design_mah = num(b.get("charge_full_design")).map(norm_charge_mah);

    // zapas: dumpsys battery (mV, 0,1°C)
    let mut used_dump = false;
    if bat.capacity.is_none() {
        bat.capacity = num(dump.get("level"));
        used_dump |= bat.capacity.is_some();
    }
    if bat.voltage_mv.is_none() {
        bat.voltage_mv = num(dump.get("voltage")).map(norm_voltage_mv);
        used_dump |= bat.voltage_mv.is_some();
    }
    if bat.temp_c.is_none() {
        bat.temp_c = num(dump.get("temperature")).map(|t| t / 10.0);
        used_dump |= bat.temp_c.is_some();
    }
    if bat.status.is_none() {
        bat.status = dump.get("status").map(|s| {
            match s.as_str() {
                "2" => "Charging",
                "3" => "Discharging",
                "4" => "Not charging",
                "5" => "Full",
                _ => "Unknown",
            }
            .to_string()
        });
    }
    if used_dump {
        bat.source = "sysfs + dumpsys".into();
    }

    // znak prądu: ujednolicamy (ładowanie = dodatni)
    if let (Some(c), Some(st)) = (bat.current_ma, bat.status.as_deref()) {
        let charging = st.eq_ignore_ascii_case("charging");
        let discharging = st.eq_ignore_ascii_case("discharging");
        if (charging && c < 0.0) || (discharging && c > 0.0) {
            bat.current_ma = Some(-c);
        }
    }
    if let (Some(f), Some(d)) = (bat.charge_full_mah, bat.charge_full_design_mah) {
        if d > 0.0 {
            bat.health_pct = Some(((f / d) * 1000.0).round() / 10.0);
        }
    }
    if let (Some(v), Some(i)) = (bat.voltage_mv, bat.current_ma) {
        bat.power_w = Some(((v * i) / 1_000_000.0 * 100.0).round() / 100.0);
    }

    thermal.sort_by(|a, b| b.temp_c.partial_cmp(&a.temp_c).unwrap_or(std::cmp::Ordering::Equal));

    let mut unsupported = Vec::new();
    for (k, missing) in [
        ("cycleCount", bat.cycle_count.is_none()),
        ("currentMa", bat.current_ma.is_none()),
        ("voltageMv", bat.voltage_mv.is_none()),
        ("healthPct", bat.health_pct.is_none()),
        ("thermal", thermal.is_empty()),
    ] {
        if missing {
            unsupported.push(k.to_string());
        }
    }

    DiagSnapshot {
        ts: chrono::Utc::now().timestamp_millis(),
        battery: bat,
        thermal,
        sources,
        unsupported,
    }
}

pub async fn snapshot(adb: &Adb, serial: &str) -> Result<DiagSnapshot> {
    let out = adb.shell(Some(serial), SCRIPT, 15).await?;
    Ok(parse(&out.stdout))
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum DiagEvent {
    Snapshot { data: Box<DiagSnapshot> },
    Error { message: String },
    Stopped { reason: String },
}

/// Strumień odczytów co `interval_ms` aż do anulowania (lub 5 błędów z rzędu).
pub async fn run_stream(
    adb: Arc<Adb>,
    serial: String,
    interval_ms: u64,
    channel: tauri::ipc::Channel<DiagEvent>,
    cancel: CancelToken,
) {
    let mut fails = 0u32;
    let interval = Duration::from_millis(interval_ms.clamp(500, 10_000));
    loop {
        match snapshot(&adb, &serial).await {
            Ok(s) => {
                fails = 0;
                let _ = channel.send(DiagEvent::Snapshot { data: Box::new(s) });
            }
            Err(e) => {
                fails += 1;
                let _ = channel.send(DiagEvent::Error { message: e.to_string() });
                if fails >= 5 {
                    let _ = channel.send(DiagEvent::Stopped {
                        reason: "5 błędów z rzędu - urządzenie odłączone? / device disconnected?".into(),
                    });
                    return;
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = cancel.cancelled() => {
                let _ = channel.send(DiagEvent::Stopped { reason: "stop".into() });
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sysfs_and_normalizes() {
        let t = "DIR=/sys/class/power_supply/battery\nB:capacity=87\nB:cycle_count=412\nB:voltage_now=4211000\nB:current_now=-1234000\nB:temp=291\nB:status=Charging\nB:health=Good\nB:charge_full=3820000\nB:charge_full_design=4500000\nP:usb=1|USB\nP:ac=0|Mains\nT:cpu-0-0|43500\nT:battery|29\nT:disabled|-273000\n__DUMPSYS__\n  level: 87\n";
        let s = parse(t);
        let b = &s.battery;
        assert_eq!(b.capacity, Some(87.0));
        assert_eq!(b.cycle_count, Some(412));
        assert_eq!(b.voltage_mv, Some(4211.0));
        assert_eq!(b.current_ma, Some(1234.0)); // ładowanie -> dodatni
        assert_eq!(b.temp_c, Some(29.1));
        assert_eq!(b.health_pct, Some(84.9));
        assert_eq!(s.thermal.len(), 2);
        assert_eq!(s.thermal[0].temp_c, 43.5);
        assert_eq!(s.sources.iter().filter(|p| p.online).count(), 1);
        assert!(s.unsupported.is_empty());
    }

    #[test]
    fn falls_back_to_dumpsys() {
        let t = "DIR=/sys/class/power_supply/battery\nB:capacity=\n__DUMPSYS__\n  level: 55\n  voltage: 3912\n  temperature: 305\n  status: 3\n";
        let s = parse(t);
        assert_eq!(s.battery.capacity, Some(55.0));
        assert_eq!(s.battery.voltage_mv, Some(3912.0));
        assert_eq!(s.battery.temp_c, Some(30.5));
        assert_eq!(s.battery.status.as_deref(), Some("Discharging"));
        assert!(s.unsupported.contains(&"cycleCount".to_string()));
    }

    #[test]
    fn samsung_ma_current() {
        assert_eq!(norm_current_ma(-350.0), -350.0);
        assert_eq!(norm_current_ma(-350000.0), -350.0);
    }
}
