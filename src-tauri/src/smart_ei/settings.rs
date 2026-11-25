//! `settings.json` - WYŁĄCZNIE przenośne i bezpieczne ustawienia (bez kont, sieci,
//! zabezpieczeń, ustawień deweloperskich itp.). Wartości są walidowane przy zapisie
//! i przed przywróceniem, więc zmodyfikowany plik nie przestawi niczego groźnego.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Klucze `settings ... system` i dozwolone wartości.
pub const SYSTEM_KEYS: &[&str] = &[
    "screen_off_timeout",
    "font_scale",
    "accelerometer_rotation",
    "time_12_24",
    "haptic_feedback_enabled",
    "vibrate_when_ringing",
];

/// Strumienie głośności: nazwa -> numer strumienia AudioManager.
pub const STREAMS: &[(&str, u8)] = &[("ring", 2), ("music", 3), ("alarm", 4), ("notification", 5)];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Volume {
    pub value: u32,
    pub max: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct PortableSettings {
    pub format_version: u32,
    pub system: BTreeMap<String, String>,
    /// yes | no | auto
    pub ui_night_mode: Option<String>,
    pub volumes: BTreeMap<String, Volume>,
}

/// Normalizuje i sprawdza wartość klucza systemowego (None = odrzucona).
pub fn check_system(key: &str, raw: &str) -> Option<String> {
    let v = raw.trim();
    match key {
        "screen_off_timeout" => {
            let n: u64 = v.parse().ok()?;
            (10_000..=1_800_000).contains(&n).then(|| n.to_string())
        }
        "font_scale" => {
            let f: f64 = v.parse().ok()?;
            (0.5..=2.0).contains(&f).then(|| format!("{}", (f * 100.0).round() / 100.0))
        }
        "accelerometer_rotation" | "haptic_feedback_enabled" | "vibrate_when_ringing" => {
            matches!(v, "0" | "1").then(|| v.to_string())
        }
        "time_12_24" => matches!(v, "12" | "24").then(|| v.to_string()),
        _ => None,
    }
}

pub fn check_night(raw: &str) -> Option<String> {
    let v = raw.trim().to_lowercase();
    let v = v.strip_prefix("night mode:").map(str::trim).unwrap_or(&v).to_string();
    // agent podaje UiModeManager: 0 auto, 1 no, 2 yes, 3 custom
    let v = match v.as_str() {
        "0" | "3" | "custom" => "auto".to_string(),
        "1" => "no".to_string(),
        "2" => "yes".to_string(),
        _ => v,
    };
    matches!(v.as_str(), "yes" | "no" | "auto").then_some(v)
}

impl PortableSettings {
    /// Z dowolnego JSON-a (agent / plik) - tylko klucze z białej listy z poprawnymi wartościami.
    pub fn sanitize(raw: &serde_json::Value) -> Self {
        let mut s = PortableSettings { format_version: 1, ..Default::default() };
        let as_text = |v: &serde_json::Value| match v {
            serde_json::Value::String(t) => Some(t.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            serde_json::Value::Bool(b) => Some(if *b { "1".into() } else { "0".into() }),
            _ => None,
        };
        if let Some(sys) = raw.get("system").and_then(|v| v.as_object()) {
            for k in SYSTEM_KEYS {
                if let Some(v) = sys.get(*k).and_then(as_text).and_then(|t| check_system(k, &t)) {
                    s.system.insert((*k).to_string(), v);
                }
            }
        }
        s.ui_night_mode = raw.get("uiNightMode").and_then(as_text).and_then(|t| check_night(&t));
        if let Some(vols) = raw.get("volumes").and_then(|v| v.as_object()) {
            for (name, _) in STREAMS {
                let Some(o) = vols.get(*name) else { continue };
                let value = o.get("value").and_then(|v| v.as_u64()).unwrap_or(u64::MAX);
                let max = o.get("max").and_then(|v| v.as_u64()).unwrap_or(0);
                if (1..=100).contains(&max) && value <= max {
                    s.volumes.insert((*name).to_string(), Volume { value: value as u32, max: max as u32 });
                }
            }
        }
        s
    }

    /// Łączy (wartości z `other` mają pierwszeństwo).
    pub fn merge(&mut self, other: &PortableSettings) {
        for (k, v) in &other.system {
            self.system.insert(k.clone(), v.clone());
        }
        if other.ui_night_mode.is_some() {
            self.ui_night_mode = other.ui_night_mode.clone();
        }
        for (k, v) in &other.volumes {
            self.volumes.insert(k.clone(), *v);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.system.is_empty() && self.ui_night_mode.is_none() && self.volumes.is_empty()
    }

    pub fn count(&self) -> usize {
        self.system.len() + usize::from(self.ui_night_mode.is_some()) + self.volumes.len()
    }

    /// Polecenia `adb shell` przywracające ustawienia. `target_max` - maksymalne głośności
    /// urządzenia docelowego (skalowanie proporcjonalne: 5/7 -> 11/15).
    pub fn adb_commands(&self, target_max: &BTreeMap<String, u32>) -> Vec<String> {
        let mut v = Vec::new();
        for (k, val) in &self.system {
            // ponowna walidacja - plik mógł zostać zmieniony ręcznie
            if let Some(ok) = check_system(k, val) {
                v.push(format!("settings put system {k} {ok}"));
            }
        }
        if let Some(n) = self.ui_night_mode.as_deref().and_then(check_night) {
            v.push(format!("cmd uimode night {n}"));
        }
        for (name, stream) in STREAMS {
            let (Some(vol), Some(tmax)) = (self.volumes.get(*name), target_max.get(*name)) else { continue };
            if vol.max == 0 || *tmax == 0 {
                continue;
            }
            let scaled = ((vol.value as f64) * (*tmax as f64) / (vol.max as f64)).round() as u32;
            v.push(format!("cmd media_session volume --stream {stream} --set {}", scaled.min(*tmax)));
        }
        v
    }
}

/// `cmd media_session volume --stream N --get` -> "volume is 5 in range [0..15]".
pub fn parse_volume(out: &str) -> Option<Volume> {
    let l = out.lines().find(|l| l.contains("volume is"))?;
    let after = l.split("volume is").nth(1)?.trim();
    let value: u32 = after.split_whitespace().next()?.parse().ok()?;
    let range = after.split("..").nth(1)?;
    let max: u32 = range.trim_end_matches([']', ' ']).trim().parse().ok()?;
    (max > 0 && value <= max).then_some(Volume { value, max })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_safe_values_survive() {
        let s = PortableSettings::sanitize(&json!({
            "system": {
                "screen_off_timeout": 60000,
                "font_scale": "1.149999",
                "accelerometer_rotation": true,
                "time_12_24": "24",
                "haptic_feedback_enabled": "7",
                "adb_enabled": "1",
                "lockscreen.password_type": "0"
            },
            "uiNightMode": 2,
            "volumes": {"ring": {"value": 5, "max": 7}, "music": {"value": 20, "max": 15}, "foo": {"value": 1, "max": 2}}
        }));
        assert_eq!(s.system.get("screen_off_timeout").map(String::as_str), Some("60000"));
        assert_eq!(s.system.get("font_scale").map(String::as_str), Some("1.15"));
        assert_eq!(s.system.get("accelerometer_rotation").map(String::as_str), Some("1"));
        assert!(!s.system.contains_key("haptic_feedback_enabled"), "spoza zakresu");
        assert!(!s.system.contains_key("adb_enabled"), "spoza białej listy");
        assert_eq!(s.ui_night_mode.as_deref(), Some("yes"));
        assert_eq!(s.volumes.len(), 1, "music: value > max odrzucone, foo nieznane");
        assert_eq!(s.count(), 6);
        assert!(check_system("screen_off_timeout", "2147483647").is_none());
        assert!(check_system("font_scale", "9").is_none());
        assert_eq!(check_night("Night mode: auto").as_deref(), Some("auto"));
    }

    #[test]
    fn adb_commands_scaled() {
        let mut s = PortableSettings::default();
        s.system.insert("time_12_24".into(), "24".into());
        s.system.insert("font_scale".into(), "1; reboot".into());
        s.ui_night_mode = Some("no".into());
        s.volumes.insert("ring".into(), Volume { value: 5, max: 7 });
        s.volumes.insert("alarm".into(), Volume { value: 7, max: 7 });
        let tmax: BTreeMap<String, u32> = [("ring".to_string(), 15u32)].into();
        let c = s.adb_commands(&tmax);
        assert_eq!(
            c,
            vec![
                "settings put system time_12_24 24".to_string(),
                "cmd uimode night no".to_string(),
                "cmd media_session volume --stream 2 --set 11".to_string(),
            ],
            "wstrzyknięcie odrzucone, alarm bez znanego maksimum pominięty"
        );
        assert_eq!(parse_volume("volume is 5 in range [0..15]"), Some(Volume { value: 5, max: 15 }));
        assert_eq!(parse_volume("Error"), None);
    }
}
