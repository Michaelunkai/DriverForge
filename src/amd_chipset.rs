//! AMD chipset (Ryzen platform) provider: installed version from the registry
//! Uninstall keys, latest from the AMD chipset download page (AM5 by default).
use crate::provider::Provider;
use crate::types::{Candidate, Device, InstallKind};
use regex::Regex;

const PAGE_URL: &str = "https://www.amd.com/en/support/downloads/drivers.html/chipsets/am5/x870.html";
const UNINSTALL: [&str; 2] = [
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
    r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
];

/// Latest chipset package version and URL found in the AMD page HTML.
pub fn parse_latest(html: &str) -> Option<(String, String)> {
    let re = Regex::new(r#"https://drivers\.amd\.com/drivers/amd_chipset_software_(\d+(?:\.\d+)+)\.exe"#).ok()?;
    let mut best: Option<(String, String)> = None;
    for c in re.captures_iter(html) {
        let (v, u) = (c[1].to_string(), c[0].to_string());
        if best.as_ref().map_or(true, |(b, _)| crate::version::is_newer(&v, b)) {
            best = Some((v, u));
        }
    }
    best
}

/// True for names of the AMD chipset package entry in Add/Remove Programs.
pub fn is_chipset_entry(display_name: &str) -> bool {
    let n = display_name.to_ascii_lowercase().replace('_', " ");
    n.contains("amd chipset software") || n.contains("amd chipset drivers")
}

/// Highest installed chipset package version from the given (name, version) pairs.
pub fn pick_installed(entries: &[(String, String)]) -> Option<String> {
    let mut best: Option<String> = None;
    for (_, v) in entries.iter().filter(|(n, _)| is_chipset_entry(n)) {
        if best.as_ref().map_or(true, |b| crate::version::is_newer(v, b)) {
            best = Some(v.clone());
        }
    }
    best
}

fn registry_entries() -> Vec<(String, String)> {
    use winreg::{enums::*, RegKey};
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let mut out = Vec::new();
    for path in UNINSTALL {
        if let Ok(root) = hklm.open_subkey(path) {
            for k in root.enum_keys().flatten() {
                if let Ok(sk) = root.open_subkey(&k) {
                    if let (Ok(n), Ok(v)) = (
                        sk.get_value::<String, _>("DisplayName"),
                        sk.get_value::<String, _>("DisplayVersion"),
                    ) {
                        out.push((n, v));
                    }
                }
            }
        }
    }
    out
}

/// Build the candidate when the page offers a newer (or any, if none installed) package.
pub fn build_candidate(installed: Option<String>, latest: Option<(String, String)>) -> Vec<Candidate> {
    let Some((ver, url)) = latest else { return vec![] };
    if let Some(i) = &installed {
        if !crate::version::is_newer(&ver, i) {
            return vec![];
        }
    }
    vec![Candidate {
        vendor: "AMD".into(),
        component: "AMD Chipset Software".into(),
        installed,
        latest: ver,
        url,
        sha256: None,
        size_bytes: None,
        kind: InstallKind::ExeSilent,
        silent_args: vec!["/S".into()],
        needs_reboot_hint: true,
    }]
}

struct P;
impl Provider for P {
    fn name(&self) -> &'static str { "amd_chipset" }
    fn check(&self, _d: &[Device]) -> Result<Vec<Candidate>, String> {
        let installed = pick_installed(&registry_entries());
        let html = crate::http::get_text(PAGE_URL)?;
        let latest = parse_latest(&html).ok_or("no chipset package link on AMD page")?;
        crate::provider::note_app("AMD Chipset Software", installed.as_deref(), Some(&latest.0));
        Ok(build_candidate(installed, Some(latest)))
    }
}
/// Repair mode (--reinstall chipset): the latest package regardless of what is installed.
pub fn forced() -> Result<Vec<Candidate>, String> {
    let html = crate::http::get_text(PAGE_URL)?;
    let latest = parse_latest(&html).ok_or("no chipset package link on AMD page")?;
    Ok(build_candidate(None, Some(latest)))
}
pub fn provider() -> Box<dyn Provider> { Box::new(P) }

#[cfg(test)]
mod tests {
    use super::*;
    const HTML: &str = r#"<a href="https://drivers.amd.com/drivers/amd_chipset_software_8.09.28.1124.exe">x</a>
        <a href="https://drivers.amd.com/drivers/amd_npu.exe">n</a>"#;
    #[test]
    fn parses_latest() {
        let (v, u) = parse_latest(HTML).unwrap();
        assert_eq!(v, "8.09.28.1124");
        assert!(u.ends_with("8.09.28.1124.exe"));
        assert!(parse_latest("nothing").is_none());
    }
    #[test]
    fn matches_entries() {
        assert!(is_chipset_entry("AMD_Chipset_Drivers"));
        assert!(is_chipset_entry("AMD Chipset Software"));
        assert!(!is_chipset_entry("AMD Software"));
        let e = vec![
            ("AMD_Chipset_Drivers".to_string(), "8.09.28.1124".to_string()),
            ("AMD Software".into(), "26.10.1".into()),
        ];
        assert_eq!(pick_installed(&e).as_deref(), Some("8.09.28.1124"));
    }
    #[test]
    fn candidate_logic() {
        let l = parse_latest(HTML);
        assert!(build_candidate(Some("8.09.28.1124".into()), l.clone()).is_empty());
        let c = build_candidate(Some("8.01.00.0".into()), l.clone());
        assert_eq!(c[0].silent_args, vec!["/S"]);
        assert_eq!(build_candidate(None, l).len(), 1);
    }
    #[test]
    #[ignore]
    fn live() {
        let c = provider().check(&[]).unwrap();
        println!("{:?}", c);
    }
}
