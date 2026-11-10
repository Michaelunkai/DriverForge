//! Device inventory: WMI (Win32_PnPEntity + Win32_PnPSignedDriver + Win32_VideoController).
use crate::types::Device;
use serde::Deserialize;
use std::collections::HashMap;
use wmi::WMIConnection;

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct PnpEntityRow {
    pub name: Option<String>,
    #[serde(rename = "PNPDeviceID")]
    pub pnp_device_id: Option<String>,
    #[serde(rename = "HardwareID")]
    pub hardware_i_ds: Option<Vec<String>>,
    #[serde(rename = "CompatibleID")]
    pub compatible_i_ds: Option<Vec<String>>,
    #[serde(rename = "PNPClass")]
    pub pnp_class: Option<String>,
    pub manufacturer: Option<String>,
    pub config_manager_error_code: Option<u32>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct SignedDriverRow {
    pub device_name: Option<String>,
    #[serde(rename = "DeviceID")]
    pub device_id: Option<String>,
    pub driver_version: Option<String>,
    pub driver_date: Option<String>,
    pub inf_name: Option<String>,
    pub manufacturer: Option<String>,
    pub device_class: Option<String>,
    pub hard_ware_id: Option<String>,
    pub compat_id: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct VideoRow {
    pub name: Option<String>,
    #[serde(rename = "PNPDeviceID")]
    pub pnp_device_id: Option<String>,
    pub driver_version: Option<String>,
    pub driver_date: Option<String>,
    pub adapter_compatibility: Option<String>,
}

fn clean(s: &Option<String>) -> Option<String> {
    s.as_ref().map(|x| x.trim().to_string()).filter(|x| !x.is_empty())
}

/// WMI CIM datetime `20240131000000.000000-000` -> `2024-01-31`. Also accepts ISO `2024-01-31...`.
pub fn parse_wmi_date(s: &str) -> Option<String> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() >= 8 && b[..8].iter().all(|c| c.is_ascii_digit()) {
        let (y, m, d) = (&s[0..4], &s[4..6], &s[6..8]);
        let (mi, di): (u32, u32) = (m.parse().ok()?, d.parse().ok()?);
        if (1..=12).contains(&mi) && (1..=31).contains(&di) && y != "0000" {
            return Some(format!("{y}-{m}-{d}"));
        }
        return None;
    }
    if b.len() >= 10 && b[4] == b'-' && b[7] == b'-' {
        return Some(s[..10].to_string());
    }
    None
}

fn split_ids(s: &Option<String>) -> Vec<String> {
    clean(s).map(|x| vec![x]).unwrap_or_default()
}

/// Pure mapping from raw WMI rows to Devices (unit-testable).
pub fn map_rows(
    entities: &[PnpEntityRow],
    drivers: &[SignedDriverRow],
    videos: &[VideoRow],
) -> Vec<Device> {
    let mut by_id: HashMap<String, &SignedDriverRow> = HashMap::new();
    for d in drivers {
        if let Some(id) = clean(&d.device_id) {
            by_id.insert(id.to_uppercase(), d);
        }
    }
    let mut vid: HashMap<String, &VideoRow> = HashMap::new();
    for v in videos {
        if let Some(id) = clean(&v.pnp_device_id) {
            vid.insert(id.to_uppercase(), v);
        }
    }
    let mut out = Vec::with_capacity(entities.len());
    let mut seen = std::collections::HashSet::new();
    for e in entities {
        let Some(pnp) = clean(&e.pnp_device_id) else { continue };
        let key = pnp.to_uppercase();
        seen.insert(key.clone());
        let drv = by_id.get(&key);
        let v = vid.get(&key);
        let mut hw: Vec<String> = e.hardware_i_ds.clone().unwrap_or_default();
        if hw.is_empty() {
            if let Some(d) = drv {
                hw = split_ids(&d.hard_ware_id);
            }
        }
        let mut compat: Vec<String> = e.compatible_i_ds.clone().unwrap_or_default();
        if compat.is_empty() {
            if let Some(d) = drv {
                compat = split_ids(&d.compat_id);
            }
        }
        let version = drv
            .and_then(|d| clean(&d.driver_version))
            .or_else(|| v.and_then(|x| clean(&x.driver_version)));
        let date_raw = drv
            .and_then(|d| clean(&d.driver_date))
            .or_else(|| v.and_then(|x| clean(&x.driver_date)));
        let name = clean(&e.name)
            .or_else(|| drv.and_then(|d| clean(&d.device_name)))
            .or_else(|| v.and_then(|x| clean(&x.name)))
            .unwrap_or_else(|| pnp.clone());
        let class = clean(&e.pnp_class)
            .or_else(|| drv.and_then(|d| clean(&d.device_class)))
            .or_else(|| v.map(|_| "DISPLAY".to_string()))
            .unwrap_or_default();
        let manufacturer = clean(&e.manufacturer)
            .or_else(|| drv.and_then(|d| clean(&d.manufacturer)))
            .or_else(|| v.and_then(|x| clean(&x.adapter_compatibility)))
            .unwrap_or_default();
        out.push(Device {
            name,
            hardware_ids: hw,
            compat_ids: compat,
            class,
            manufacturer,
            driver_version: version,
            driver_date: date_raw.as_deref().and_then(parse_wmi_date),
            inf_name: drv.and_then(|d| clean(&d.inf_name)),
            problem_code: e.config_manager_error_code.unwrap_or(0),
            pnp_id: pnp,
        });
    }
    // Video controllers / signed drivers not present in PnPEntity.
    for v in videos {
        let Some(pnp) = clean(&v.pnp_device_id) else { continue };
        if seen.contains(&pnp.to_uppercase()) {
            continue;
        }
        let drv = by_id.get(&pnp.to_uppercase());
        out.push(Device {
            name: clean(&v.name).unwrap_or_else(|| pnp.clone()),
            hardware_ids: drv.map(|d| split_ids(&d.hard_ware_id)).unwrap_or_default(),
            compat_ids: drv.map(|d| split_ids(&d.compat_id)).unwrap_or_default(),
            class: "DISPLAY".into(),
            manufacturer: clean(&v.adapter_compatibility).unwrap_or_default(),
            driver_version: clean(&v.driver_version),
            driver_date: v.driver_date.as_deref().and_then(parse_wmi_date),
            inf_name: drv.and_then(|d| clean(&d.inf_name)),
            problem_code: 0,
            pnp_id: pnp,
        });
    }
    out
}

pub fn scan_devices() -> Result<Vec<Device>, String> {
    let com = wmi::COMLibrary::new()
        .or_else(|_| unsafe { Ok::<_, wmi::WMIError>(wmi::COMLibrary::assume_initialized()) })
        .map_err(|e| format!("COM init failed: {e}"))?;
    let con = WMIConnection::new(com).map_err(|e| format!("WMI connect failed: {e}"))?;
    let entities: Vec<PnpEntityRow> = con
        .raw_query("SELECT * FROM Win32_PnPEntity")
        .map_err(|e| format!("Win32_PnPEntity query failed: {e}"))?;
    let drivers: Vec<SignedDriverRow> = con
        .raw_query("SELECT * FROM Win32_PnPSignedDriver")
        .unwrap_or_default();
    let videos: Vec<VideoRow> = con
        .raw_query("SELECT * FROM Win32_VideoController")
        .unwrap_or_default();
    let devs = map_rows(&entities, &drivers, &videos);
    if devs.is_empty() {
        return Err("WMI returned no devices".into());
    }
    Ok(devs)
}

/// Devices with a Config Manager error that a driver could fix (problem_code != 0).
/// Missing driver (28) is included; 22 (disabled by user) and 24 (device not present) are not.
pub fn problem_devices(d: &[Device]) -> Vec<&Device> {
    d.iter()
        .filter(|x| x.problem_code != 0 && x.problem_code != 22 && x.problem_code != 24)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_parsing() {
        assert_eq!(parse_wmi_date("20240131000000.000000-000").as_deref(), Some("2024-01-31"));
        assert_eq!(parse_wmi_date("2024-02-03T00:00:00Z").as_deref(), Some("2024-02-03"));
        assert_eq!(parse_wmi_date("garbage"), None);
        assert_eq!(parse_wmi_date("20241345000000.000000-000"), None);
        assert_eq!(parse_wmi_date(""), None);
    }

    #[test]
    fn mapping_with_nulls_and_problems() {
        let ents = vec![
            PnpEntityRow {
                name: Some("NVIDIA GeForce RTX 5080".into()),
                pnp_device_id: Some("PCI\\VEN_10DE&DEV_2C02&SUBSYS_1".into()),
                hardware_i_ds: Some(vec!["PCI\\VEN_10DE&DEV_2C02".into()]),
                pnp_class: Some("Display".into()),
                manufacturer: Some("NVIDIA".into()),
                ..Default::default()
            },
            PnpEntityRow {
                name: None,
                pnp_device_id: Some("PCI\\VEN_8086&DEV_1".into()),
                config_manager_error_code: Some(28),
                ..Default::default()
            },
            PnpEntityRow { pnp_device_id: None, ..Default::default() },
        ];
        let drvs = vec![SignedDriverRow {
            device_id: Some("pci\\ven_10de&dev_2c02&subsys_1".into()),
            driver_version: Some("32.0.16.2114".into()),
            driver_date: Some("20260101000000.000000-000".into()),
            inf_name: Some("oem5.inf".into()),
            ..Default::default()
        }];
        let vids = vec![VideoRow {
            name: Some("AMD Radeon(TM) Graphics".into()),
            pnp_device_id: Some("PCI\\VEN_1002&DEV_13C0&SUBSYS_2".into()),
            driver_version: Some("32.0.21045.20006".into()),
            ..Default::default()
        }];
        let d = map_rows(&ents, &drvs, &vids);
        assert_eq!(d.len(), 3);
        assert_eq!(d[0].driver_version.as_deref(), Some("32.0.16.2114"));
        assert_eq!(d[0].driver_date.as_deref(), Some("2026-01-01"));
        assert_eq!(d[0].inf_name.as_deref(), Some("oem5.inf"));
        assert_eq!(d[1].name, "PCI\\VEN_8086&DEV_1");
        assert!(d[1].driver_version.is_none());
        assert_eq!(d[2].driver_version.as_deref(), Some("32.0.21045.20006"));
        assert_eq!(d[2].class, "DISPLAY");
        assert_eq!(problem_devices(&d).len(), 1);
        let mut extra = d.clone();
        for code in [22, 24] {
            extra.push(Device { problem_code: code, ..Default::default() });
        }
        assert_eq!(problem_devices(&extra).len(), 1);
    }
}
