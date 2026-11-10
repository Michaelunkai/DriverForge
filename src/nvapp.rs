//! NVIDIA App provider.
//!
//! Latest version source (verified by real fetch 2026-11-10): the product page
//! https://www.nvidia.com/en-us/software/nvidia-app/ embeds the direct links
//! `https://us.download.nvidia.com/nvapp/client/<ver>/x64/NVIDIA_app_v<ver>_x64.exe`
//! (HEAD returned 200, 184718176 bytes). Installed version: Uninstall registry key
//! `{B2FE1952-0186-46C3-BAEC-A80AA35AC5B8}_Display.NvApp` (DisplayName "NVIDIA App <ver>").
//! Silent flags: the installer is NVIDIA's NSIS-style bootstrapper; `-s -noreboot -noeula` is the
//! documented community/NVIDIA-forum set (no vendor manifest exists to cite; not executed here).
use crate::provider::Provider;
use crate::types::{Candidate, Device, InstallKind};
use crate::{http, version};
use regex::Regex;

const PAGE: &str = "https://www.nvidia.com/en-us/software/nvidia-app/";

struct P;

/// Extract (version, url) of the newest x64 installer link on the page.
pub fn parse_latest(html: &str) -> Option<(String, String)> {
    let re = Regex::new(
        r"https://us\.download\.nvidia\.com/nvapp/client/([0-9]+(?:\.[0-9]+){2,3})/x64/NVIDIA_app_v[0-9.]+_x64\.exe",
    )
    .ok()?;
    let mut best: Option<(String, String)> = None;
    for c in re.captures_iter(html) {
        let (v, u) = (c[1].to_string(), c[0].to_string());
        if best.as_ref().map_or(true, |(b, _)| version::cmp(&v, b).is_gt()) {
            best = Some((v, u));
        }
    }
    best
}

/// Pick the NVIDIA App version from a DisplayName/DisplayVersion pair list.
pub fn pick_installed(entries: &[(String, String)]) -> Option<String> {
    entries
        .iter()
        .find(|(n, _)| n.trim().to_ascii_lowercase().starts_with("nvidia app") && !n.to_ascii_lowercase().contains("driver settings"))
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(windows)]
fn installed_version() -> Option<String> {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};
    use winreg::RegKey;
    let path = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall";
    let mut entries = Vec::new();
    for flag in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
        if let Ok(root) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(path, KEY_READ | flag) {
            for name in root.enum_keys().flatten() {
                if let Ok(k) = root.open_subkey_with_flags(&name, KEY_READ | flag) {
                    let n: Result<String, _> = k.get_value("DisplayName");
                    let v: Result<String, _> = k.get_value("DisplayVersion");
                    if let (Ok(n), Ok(v)) = (n, v) {
                        entries.push((n, v));
                    }
                }
            }
        }
    }
    pick_installed(&entries)
}

#[cfg(not(windows))]
fn installed_version() -> Option<String> {
    None
}

pub fn build_candidate(installed: Option<String>, latest: String, url: String) -> Option<Candidate> {
    if let Some(i) = &installed {
        if !version::is_newer(&latest, i) {
            return None;
        }
    }
    Some(Candidate {
        vendor: "NVIDIA".into(),
        component: "NVIDIA App".into(),
        installed,
        latest,
        url,
        sha256: None,
        size_bytes: None,
        kind: InstallKind::ExeSilent,
        silent_args: vec!["-s".into(), "-noreboot".into(), "-noeula".into()],
        needs_reboot_hint: false,
    })
}

impl Provider for P {
    fn name(&self) -> &'static str {
        "nvapp"
    }
    fn check(&self, devices: &[Device]) -> Result<Vec<Candidate>, String> {
        // NVIDIA App only matters when an NVIDIA GPU is present.
        let has_nv = devices.is_empty()
            || devices.iter().any(|d| d.hardware_ids.iter().any(|h| h.to_ascii_uppercase().contains("VEN_10DE")));
        if !has_nv {
            return Ok(vec![]);
        }
        let html = http::get_text(PAGE)?;
        let (latest, url) = parse_latest(&html).ok_or("no NVIDIA App installer link found on product page")?;
        crate::log::info(&format!("nvapp: installed={:?} latest={}", installed_version(), latest));
        crate::provider::note_app("NVIDIA App", installed_version().as_deref(), Some(&latest));
        Ok(build_candidate(installed_version(), latest, url).into_iter().collect())
    }
}

pub fn provider() -> Box<dyn Provider> {
    Box::new(P)
}

#[cfg(test)]
mod tests {
    use super::*;
    const PAGE_FIXTURE: &str = r#"<a href="https://us.download.nvidia.com/nvapp/client/11.0.9.585/arm64/NVIDIA_app_v11.0.9.585_arm64.exe">arm</a>
<a href="https://us.download.nvidia.com/nvapp/client/11.0.9.585/x64/NVIDIA_app_v11.0.9.585_x64.exe">x64</a>"#;

    #[test]
    fn parses_x64_only() {
        let (v, u) = parse_latest(PAGE_FIXTURE).unwrap();
        assert_eq!(v, "11.0.9.585");
        assert!(u.ends_with("NVIDIA_app_v11.0.9.585_x64.exe"));
    }
    #[test]
    fn none_when_absent() {
        assert!(parse_latest("<html></html>").is_none());
    }
    #[test]
    fn installed_pick() {
        let e = vec![
            ("NVIDIA App driver settings".to_string(), "1.0".to_string()),
            ("NVIDIA App 11.0.9.585".to_string(), "11.0.9.585".to_string()),
        ];
        assert_eq!(pick_installed(&e).as_deref(), Some("11.0.9.585"));
        assert_eq!(pick_installed(&[]), None);
    }
    #[test]
    fn candidate_logic() {
        let u = "https://x/y.exe".to_string();
        assert!(build_candidate(Some("11.0.9.585".into()), "11.0.9.585".into(), u.clone()).is_none());
        assert!(build_candidate(Some("11.0.8.1".into()), "11.0.9.585".into(), u.clone()).is_some());
        let c = build_candidate(None, "11.0.9.585".into(), u).unwrap();
        assert!(c.installed.is_none());
    }
    #[test]
    #[cfg(windows)]
    #[ignore = "live: reads registry + network"]
    fn live_check() {
        let c = P.check(&[]).unwrap();
        println!("installed={:?} cands={:?}", installed_version(), c);
    }
}
