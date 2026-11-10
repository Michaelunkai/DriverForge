//! AMD Radeon / Adrenalin driver provider.
//!
//! Source of truth (verified with real fetches, browser UA required):
//!  1. AMD product page (Ryzen 9000 iGPU page) lists every WHQL Adrenalin exe on drivers.amd.com.
//!  2. Release notes RN-RAD-WIN-<yy>-<m>-<n> contain "Windows Driver Store Version 32.0.x.y",
//!     which maps AMD marketing version (26.10.2) to the Windows driver version.
use crate::provider::Provider;
use crate::types::{Candidate, Device, InstallKind};
use regex::Regex;
use std::cmp::Ordering;

const PRODUCT_PAGE: &str = "https://www.amd.com/en/support/downloads/drivers.html/processors/ryzen/ryzen-9000-series/amd-ryzen-7-9800x3d.html";
const GENERIC_PAGE: &str = "https://www.amd.com/en/support/download/drivers.html";
const RN_BASE: &str = "https://www.amd.com/en/resources/support-articles/release-notes/RN-RAD-WIN-";

struct P;

/// True for AMD (VEN_1002) display adapters, including Ryzen iGPUs (e.g. DEV_13C0).
pub fn is_amd_gpu(d: &Device) -> bool {
    let amd = d.hardware_ids.iter().chain(d.compat_ids.iter()).any(|h| h.to_uppercase().contains("VEN_1002"));
    amd && (d.class.eq_ignore_ascii_case("Display") || d.class.is_empty())
}

fn nums(v: &str) -> Vec<u64> {
    v.split('.').map(|p| p.trim().parse::<u64>().unwrap_or(0)).collect()
}

fn cmp_ver(a: &str, b: &str) -> Ordering {
    let (x, y) = (nums(a), nums(b));
    for i in 0..x.len().max(y.len()) {
        let o = x.get(i).unwrap_or(&0).cmp(y.get(i).unwrap_or(&0));
        if o != Ordering::Equal {
            return o;
        }
    }
    Ordering::Equal
}


/// Installed AMD Software (Adrenalin) marketing version, e.g. "26.10.2", from the Uninstall keys.
pub fn installed_adrenalin() -> Option<String> {
    use winreg::enums::*;
    use winreg::RegKey;
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let mut best: Option<String> = None;
    for path in [
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
        r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
    ] {
        let Ok(k) = hklm.open_subkey(path) else { continue };
        for name in k.enum_keys().flatten() {
            let Ok(sk) = k.open_subkey(&name) else { continue };
            let dn: String = sk.get_value("DisplayName").unwrap_or_default();
            if dn.starts_with("AMD Software") {
                let v: String = sk.get_value("DisplayVersion").unwrap_or_default();
                if !v.is_empty() && best.as_deref().map_or(true, |b| cmp_ver(&v, b) == Ordering::Greater) {
                    best = Some(v);
                }
            }
        }
    }
    best
}

/// (marketing version, url) of the newest WHQL Adrenalin installer found in page HTML.
pub fn parse_latest_whql(html: &str) -> Option<(String, String)> {
    let re = Regex::new(r#"https://drivers\.amd\.com/drivers/whql-amd-software-adrenalin-edition-(\d+\.\d+\.\d+)-[A-Za-z0-9._-]*\.exe"#).ok()?;
    let mut best: Option<(String, String)> = None;
    for c in re.captures_iter(html) {
        let (v, u) = (c[1].to_string(), c[0].to_string());
        if best.as_ref().map_or(true, |(bv, _)| cmp_ver(&v, bv) == Ordering::Greater) {
            best = Some((v, u));
        }
    }
    best
}

/// Windows driver store version (32.0.x.y) from a release-notes page.
pub fn parse_store_version(html: &str) -> Option<String> {
    let re = Regex::new(r"Windows\s+Driver\s+Store\s+Version\s+(32\.0\.\d+\.\d+)").ok()?;
    re.captures(html).map(|c| c[1].to_string())
}

/// "26.10.2" -> "26-10-2" release-notes slug.
fn rn_url(marketing: &str) -> String {
    format!("{}{}.html", RN_BASE, marketing.replace('.', "-"))
}

impl Provider for P {
    fn name(&self) -> &'static str {
        "amd_gpu"
    }
    fn check(&self, devices: &[Device]) -> Result<Vec<Candidate>, String> {
        build(devices, false)
    }
}

/// `force` = produce the candidate even when the installed Adrenalin is already current (--reinstall amd).
pub fn build(devices: &[Device], force: bool) -> Result<Vec<Candidate>, String> {
    {
        let gpus: Vec<&Device> = devices.iter().filter(|d| is_amd_gpu(d)).collect();
        if gpus.is_empty() {
            return Ok(vec![]);
        }
        let page = crate::http::get_text(PRODUCT_PAGE)
            .ok()
            .and_then(|h| parse_latest_whql(&h).map(|r| r))
            .or_else(|| crate::http::get_text(GENERIC_PAGE).ok().and_then(|h| parse_latest_whql(&h)))
            .ok_or("amd_gpu: no WHQL Adrenalin link found on AMD pages")?;
        let (marketing, url) = page;
        crate::provider::note_app("AMD Radeon Software Adrenalin", installed_adrenalin().as_deref(), Some(&marketing));
        // Compare marketing versions first: the release notes list the Driver Store version of the
        // flagship Radeon cards, which differs from the one bundled for iGPUs (32.0.21045.x for
        // Ryzen), so comparing Windows driver versions alone gives false "updates".
        if let Some(inst) = installed_adrenalin() {
            if !force && cmp_ver(&marketing, &inst) != Ordering::Greater {
                return Ok(vec![]);
            }
            let mut out = Vec::new();
            for g in &gpus {
                out.push(Candidate {
                    vendor: "AMD".into(),
                    component: format!("AMD Radeon Software Adrenalin {} ({})", marketing, g.name),
                    installed: Some(inst.clone()),
                    latest: marketing.clone(),
                    url: url.clone(),
                    sha256: None,
                    size_bytes: None,
                    kind: InstallKind::ExeSilent,
                    silent_args: vec!["-INSTALL".into(), "-OUTPUT".into(), "SCREEN".into()],
                    needs_reboot_hint: true,
                });
                break; // one package updates every AMD GPU
            }
            return Ok(out);
        }
        let rn = crate::http::get_text(&rn_url(&marketing))?;
        let store = parse_store_version(&rn).ok_or_else(|| format!("amd_gpu: no Driver Store Version in release notes for {}", marketing))?;
        let mut out = Vec::new();
        for g in gpus {
            let installed = g.driver_version.clone();
            let newer = installed.as_deref().map_or(true, |i| cmp_ver(&store, i) == Ordering::Greater);
            if newer {
                out.push(Candidate {
                    vendor: "AMD".into(),
                    component: format!("AMD Radeon Software Adrenalin {} ({})", marketing, g.name),
                    installed,
                    latest: store.clone(),
                    url: url.clone(),
                    sha256: None,
                    size_bytes: None,
                    kind: InstallKind::ExeSilent,
                    // AMD installer silent switches (unverified on this PC: installs are forbidden for agents).
                    silent_args: vec!["-INSTALL".into(), "-OUTPUT".into(), "SCREEN".into()],
                    needs_reboot_hint: true,
                });
            }
        }
        Ok(out)
    }
}

pub fn provider() -> Box<dyn Provider> {
    Box::new(P)
}

#[cfg(test)]
mod tests {
    use super::*;
    const PAGE: &str = r#"<a href="https://drivers.amd.com/drivers/whql-amd-software-adrenalin-edition-26.10.1-win11-a.exe">x</a>
<a href="https://drivers.amd.com/drivers/installer/26.20/beta/amd-software-adrenalin-edition-26.10.2-minimalsetup-261029_web.exe">m</a>
<a href="https://drivers.amd.com/drivers/whql-amd-software-adrenalin-edition-26.10.2-win11-a.exe">y</a>
<a href="https://drivers.amd.com/drivers/whql-amd-software-adrenalin-edition-26.9.10-win11-a.exe">old</a>"#;
    const RN: &str = "<li>AMD Software: Adrenalin Edition 26.10.2 Driver Version 26.20.15.12 for Windows (Windows Driver Store Version 32.0.32015.12012).</li>";

    #[test]
    fn latest_whql_picks_highest() {
        let (v, u) = parse_latest_whql(PAGE).unwrap();
        assert_eq!(v, "26.10.2");
        assert!(u.ends_with("26.10.2-win11-a.exe"));
    }
    #[test]
    fn store_version() {
        assert_eq!(parse_store_version(RN).unwrap(), "32.0.32015.12012");
        assert!(parse_store_version("nothing").is_none());
    }
    #[test]
    fn compare() {
        assert_eq!(cmp_ver("32.0.32015.12012", "32.0.21045.20006"), Ordering::Greater);
        assert_eq!(cmp_ver("26.10.2", "26.9.10"), Ordering::Greater);
        assert_eq!(cmp_ver("32.0.21045.20006", "32.0.21045.20006"), Ordering::Equal);
    }
    #[test]
    fn detects_igpu() {
        let d = Device { name: "AMD Radeon(TM) Graphics".into(), hardware_ids: vec!["PCI\\VEN_1002&DEV_13C0&SUBSYS_1".into()], class: "Display".into(), ..Default::default() };
        assert!(is_amd_gpu(&d));
        let n = Device { hardware_ids: vec!["PCI\\VEN_10DE&DEV_2C02".into()], class: "Display".into(), ..Default::default() };
        assert!(!is_amd_gpu(&n));
    }
    #[test]
    fn rn_slug() {
        assert!(rn_url("26.10.2").ends_with("RN-RAD-WIN-26-10-2.html"));
    }
    #[test]
    #[ignore]
    fn live_check() {
        let d = Device { name: "AMD Radeon(TM) Graphics".into(), hardware_ids: vec!["PCI\\VEN_1002&DEV_13C0".into()], class: "Display".into(), driver_version: Some("32.0.21045.20006".into()), ..Default::default() };
        for c in provider().check(&[d]).unwrap() {
            println!("LIVE installed={:?} latest={} url={}", c.installed, c.latest, c.url);
        }
    }
}
