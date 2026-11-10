//! Runtime / toolchain companions that drivers and GPU apps depend on.
//! - Microsoft Visual C++ 2015-2022 x64 Redistributable (installed when missing or older than the floor).
//! - GCC (MinGW-w64) presence is reported as an info line (no vendor-signed silent installer exists).
use crate::provider::Provider;
use crate::types::{Candidate, Device, InstallKind};
use crate::version;

const VC_URL: &str = "https://aka.ms/vs/17/release/vc_redist.x64.exe";
/// Oldest VC++ runtime accepted as current; anything older is replaced by the latest redistributable.
const VC_FLOOR: &str = "14.40.33810";

struct P;

pub fn parse_vc_version(raw: &str) -> String {
    raw.trim().trim_start_matches(['v', 'V']).to_string()
}

const VC_MANIFESTS: &str = "https://api.github.com/repos/microsoft/winget-pkgs/contents/manifests/m/Microsoft/VCRedist/2015%2B/x64";

/// Newest VC++ redistributable version, looked up live from the winget manifest listing.
/// Falls back to the floor only when that lookup fails (offline), never overriding a live answer.
pub fn vc_latest_live() -> Option<String> {
    let v = crate::http::get_json(VC_MANIFESTS).ok()?;
    let mut best: Option<String> = None;
    for e in v.as_array()? {
        if let Some(n) = e.get("name").and_then(|n| n.as_str()) {
            if n.chars().next().map_or(false, |c| c.is_ascii_digit()) && best.as_deref().map_or(true, |b| version::is_newer(n, b)) {
                best = Some(n.to_string());
            }
        }
    }
    best
}

pub fn build_vc(installed: Option<String>) -> Option<Candidate> {
    build_vc_latest(installed, VC_FLOOR)
}

pub fn build_vc_latest(installed: Option<String>, latest: &str) -> Option<Candidate> {
    if let Some(i) = &installed {
        if !version::is_newer(latest, i) {
            return None;
        }
    }
    Some(Candidate {
        vendor: "Microsoft".into(),
        component: "Visual C++ 2015-2022 Redistributable (x64)".into(),
        installed,
        latest: latest.into(),
        url: VC_URL.into(),
        sha256: None,
        size_bytes: None,
        kind: InstallKind::ExeSilent,
        silent_args: vec!["/install".into(), "/quiet".into(), "/norestart".into()],
        needs_reboot_hint: false,
    })
}

#[cfg(windows)]
fn vc_installed() -> Option<String> {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY};
    use winreg::RegKey;
    let k = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(r"SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64", KEY_READ | KEY_WOW64_64KEY)
        .ok()?;
    let v: String = k.get_value("Version").ok()?;
    Some(parse_vc_version(&v))
}
#[cfg(not(windows))]
fn vc_installed() -> Option<String> {
    None
}

// ---------------------------------------------------------------- .NET Desktop Runtime
const DOTNET_INDEX: &str = "https://builds.dotnet.microsoft.com/dotnet/release-metadata/releases-index.json";

/// Stable channels from the live releases index: (channel, latest-runtime, is_lts), newest first.
pub fn parse_dotnet_index(j: &serde_json::Value) -> Vec<(String, String, bool)> {
    let mut v: Vec<(String, String, bool)> = j["releases-index"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    let phase = c["support-phase"].as_str()?;
                    let rt = c["latest-runtime"].as_str()?;
                    if !(phase == "active" || phase == "maintenance") || rt.contains('-') {
                        return None;
                    }
                    Some((c["channel-version"].as_str()?.to_string(), rt.to_string(), c["release-type"].as_str() == Some("lts")))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| version::cmp(&b.1, &a.1));
    v
}

fn dotnet_candidate(installed: Option<String>, channel: &str, latest: &str) -> Candidate {
    Candidate {
        vendor: "Microsoft".into(),
        component: format!(".NET Desktop Runtime {channel} (x64)"),
        installed,
        latest: latest.into(),
        url: format!("https://builds.dotnet.microsoft.com/dotnet/WindowsDesktop/{latest}/windowsdesktop-runtime-{latest}-win-x64.exe"),
        sha256: None,
        size_bytes: None,
        kind: InstallKind::ExeSilent,
        silent_args: vec!["/install".into(), "/quiet".into(), "/norestart".into()],
        needs_reboot_hint: false,
    }
}

/// Update every installed supported channel that is behind; when no supported channel is
/// installed at all, install the newest LTS.
pub fn build_dotnet(channels: &[(String, String, bool)], installed: &[String]) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut any = false;
    for (ch, latest, _) in channels {
        let best = installed
            .iter()
            .filter(|v| v.starts_with(&format!("{ch}.")))
            .max_by(|a, b| version::cmp(a, b))
            .cloned();
        if let Some(b) = best {
            any = true;
            if version::is_newer(latest, &b) {
                out.push(dotnet_candidate(Some(b), ch, latest));
            }
        }
    }
    if !any {
        if let Some((ch, latest, _)) = channels.iter().find(|c| c.2) {
            out.push(dotnet_candidate(None, ch, latest));
        }
    }
    out
}

#[cfg(windows)]
fn dotnet_installed() -> Vec<String> {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY};
    use winreg::RegKey;
    let mut v: Vec<String> = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(r"SOFTWARE\dotnet\Setup\InstalledVersions\x64\sharedfx\Microsoft.WindowsDesktop.App", KEY_READ | KEY_WOW64_64KEY)
        .map(|k| k.enum_values().flatten().map(|(n, _)| n).collect())
        .unwrap_or_default();
    // The registry is not always populated (e.g. after a bundle install): also scan the
    // shared-framework folders and ask `dotnet --list-runtimes`.
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    for var in ["ProgramFiles", "DOTNET_ROOT"] {
        if let Ok(p) = std::env::var(var) {
            let base = std::path::PathBuf::from(p);
            roots.push(if var == "ProgramFiles" { base.join("dotnet") } else { base });
        }
    }
    roots.push(std::path::PathBuf::from(r"C:\Program Files\dotnet"));
    for r in roots {
        if let Ok(rd) = std::fs::read_dir(r.join("shared").join("Microsoft.WindowsDesktop.App")) {
            for e in rd.flatten() {
                if e.path().is_dir() {
                    v.push(e.file_name().to_string_lossy().to_string());
                }
            }
        }
    }
    let mut cmd = std::process::Command::new("dotnet");
    cmd.arg("--list-runtimes");
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    if let Ok(o) = cmd.output() {
        for l in String::from_utf8_lossy(&o.stdout).lines() {
            let mut it = l.split_whitespace();
            if it.next() == Some("Microsoft.WindowsDesktop.App") {
                if let Some(ver) = it.next() {
                    v.push(ver.to_string());
                }
            }
        }
    }
    v.sort();
    v.dedup();
    v
}
#[cfg(not(windows))]
fn dotnet_installed() -> Vec<String> {
    vec![]
}

// ---------------------------------------------------------------- DirectX End-User Runtime
const DX_URL: &str = "https://download.microsoft.com/download/1/7/1/1718CCC4-6315-4D8E-9543-8E28A4E18C4C/dxwebsetup.exe";
/// Legacy D3DX DLLs that games and GPU apps import. The June 2010 redist is frozen, so presence
/// of these files (not a version) is the correct "is it installed" signal.
const DX_MARKERS: [&str; 3] = ["d3dx9_43.dll", "d3dx10_43.dll", "d3dcompiler_43.dll"];

pub fn build_directx(missing: &[String]) -> Option<Candidate> {
    if missing.is_empty() {
        return None;
    }
    Some(Candidate {
        vendor: "Microsoft".into(),
        component: "DirectX End-User Runtime (legacy D3DX)".into(),
        installed: None,
        latest: "9.29.1974".into(),
        url: DX_URL.into(),
        sha256: None,
        size_bytes: None,
        kind: InstallKind::ExeSilent,
        silent_args: vec!["/Q".into()],
        needs_reboot_hint: false,
    })
}

fn dx_missing() -> Vec<String> {
    let root = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into());
    DX_MARKERS.iter().filter(|m| !root.join("System32").join(m).is_file()).map(|s| s.to_string()).collect()
}

fn gcc_present() -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join("gcc.exe").is_file()))
        .unwrap_or(false)
}

impl Provider for P {
    fn name(&self) -> &'static str {
        "runtimes"
    }
    fn check(&self, _devices: &[Device]) -> Result<Vec<Candidate>, String> {
        if !gcc_present() {
            crate::provider::note_info(
                "GCC (MinGW-w64) not found on PATH - install it manually (winlibs/MSYS2); no signed silent installer is available.".into(),
            );
        }
        let vc_inst = vc_installed();
        let vc_latest = vc_latest_live().unwrap_or_else(|| VC_FLOOR.to_string());
        crate::log::info(&format!("runtimes: vc++ installed={:?} latest={}", vc_inst, vc_latest));
        crate::provider::note_app("Visual C++ 2015-2022 Redistributable (x64)", vc_inst.as_deref(), Some(&vc_latest));
        let mut out: Vec<Candidate> = build_vc_latest(vc_inst, &vc_latest).into_iter().collect();
        let mut errs = Vec::new();

        let dx = dx_missing();
        crate::log::info(&format!("runtimes: directx missing markers={:?}", dx));
        crate::provider::note_app("DirectX End-User Runtime", if dx.is_empty() { Some("present") } else { None }, Some("current"));
        out.extend(build_directx(&dx));
        crate::provider::note_app("GCC (MinGW-w64)", if gcc_present() { Some("on PATH") } else { None }, None);

        let dn = dotnet_installed();
        crate::log::info(&format!("runtimes: .net desktop installed={:?}", dn));
        match crate::http::get_json(DOTNET_INDEX) {
            Ok(j) => {
                let ch = parse_dotnet_index(&j);
                if ch.is_empty() {
                    errs.push("no supported .NET channel in releases index".to_string());
                }
                for (c, l, _) in &ch {
                    let inst = dn.iter().find(|v| v.starts_with(&format!("{c}.")));
                    crate::provider::note_app(&format!(".NET Desktop Runtime {c}"), inst.map(|s| s.as_str()), Some(l));
                }
                out.extend(build_dotnet(&ch, &dn));
            }
            Err(e) => errs.push(format!(".NET releases index: {e}")),
        }
        // A failed sub-lookup must not hide the others (VC++/DirectX still returned).
        for e in errs {
            crate::provider::note_incomplete(format!("runtimes: {e}"));
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
    #[test]
    fn parses_version() {
        assert_eq!(parse_vc_version("v14.44.35211.00"), "14.44.35211.00");
    }
    fn idx() -> serde_json::Value {
        serde_json::json!({"releases-index":[
            {"channel-version":"11.0","latest-runtime":"11.0.0-rc.3.1","support-phase":"go-live","release-type":"sts"},
            {"channel-version":"10.0","latest-runtime":"10.0.14","support-phase":"active","release-type":"lts"},
            {"channel-version":"8.0","latest-runtime":"8.0.33","support-phase":"maintenance","release-type":"lts"},
            {"channel-version":"6.0","latest-runtime":"6.0.36","support-phase":"eol","release-type":"lts"}]})
    }
    #[test]
    fn dotnet_index_filters_preview_and_eol() {
        let c = parse_dotnet_index(&idx());
        assert_eq!(c.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), vec!["10.0", "8.0"]);
    }
    #[test]
    fn dotnet_updates_installed_or_installs_lts() {
        let c = parse_dotnet_index(&idx());
        let u = build_dotnet(&c, &["8.0.10".into(), "8.0.2".into()]);
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].latest, "8.0.33");
        assert_eq!(u[0].installed.as_deref(), Some("8.0.10"));
        assert!(build_dotnet(&c, &["8.0.33".into()]).is_empty());
        let n = build_dotnet(&c, &[]);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].latest, "10.0.14");
        assert!(n[0].url.ends_with("windowsdesktop-runtime-10.0.14-win-x64.exe"));
    }
    #[test]
    fn directx_only_when_markers_missing() {
        assert!(build_directx(&[]).is_none());
        assert!(build_directx(&["d3dx9_43.dll".into()]).is_some());
    }
    #[test]
    fn candidate_rules() {
        assert!(build_vc(None).is_some());
        assert!(build_vc(Some("14.30.1.0".into())).is_some());
        assert!(build_vc(Some("14.44.35211.0".into())).is_none());
    }
}
