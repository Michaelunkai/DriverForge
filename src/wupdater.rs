// Windows Update Agent COM driver sweep - catches driver types Microsoft catalogs
// (audio, LAN, USB, chipset components, Intel, Realtek, etc.), not just GPU vendors.
use crate::comdisp::{com_initialize, create_from_progid, ComObj, Val};
use crate::provider::Provider;
use crate::types::{Candidate, Device, InstallKind};
use std::sync::mpsc;
use std::time::Duration;

pub const DRIVER_QUERY: &str = "IsInstalled=0 and Type='Driver' and IsHidden=0";
/// Upper bound for the online search (WUA Search is synchronous and can hang on bad networks).
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(180);
/// Upper bound for download+install of the selected updates.
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Default)]
pub struct WuEntry {
    pub title: String,
    pub update_id: String,
    pub category: String,
    pub manufacturer: String,
    pub version: String,
    pub hardware_id: String,
    pub size_mb: f64,
}

/// Result of an install run.
#[derive(Debug, Clone, PartialEq)]
pub struct WuInstallResult {
    pub installed: usize,
    pub total: usize,
    pub reboot_required: bool,
    pub message: String,
}

fn prop(o: &ComObj, name: &str) -> Val {
    o.get(name).unwrap_or(Val::Empty)
}

fn search_updates() -> Result<(ComObj, ComObj), String> {
    com_initialize();
    let session = create_from_progid("Microsoft.Update.Session")?;
    let searcher = session
        .call("CreateUpdateSearcher", &[])?
        .into_obj()
        .ok_or("CreateUpdateSearcher did not return an object")?;
    let result = searcher
        .call("Search", &[Val::Str(DRIVER_QUERY.into())])?
        .into_obj()
        .ok_or("Search did not return an object")?;
    let updates = result.get("Updates")?.into_obj().ok_or("Search result had no Updates collection")?;
    Ok((session, updates))
}

fn entry_from(item: &ComObj) -> WuEntry {
    let update_id = match prop(item, "Identity").into_obj() {
        Some(id) => prop(&id, "UpdateID").as_str(),
        None => String::new(),
    };
    WuEntry {
        title: prop(item, "Title").as_str(),
        update_id,
        category: prop(item, "DriverClass").as_str(),
        manufacturer: {
            let m = prop(item, "DriverManufacturer").as_str();
            if m.is_empty() { prop(item, "DriverProvider").as_str() } else { m }
        },
        version: {
            let v = prop(item, "DriverVerVersion").as_str();
            if v.trim().is_empty() { version_from_title(&prop(item, "Title").as_str()) } else { v }
        },
        hardware_id: prop(item, "DriverHardwareID").as_str(),
        size_mb: prop(item, "MaxDownloadSize").as_f64() / 1_048_576.0,
    }
}

fn scan_blocking() -> Result<Vec<WuEntry>, String> {
    let (_session, updates) = search_updates()?;
    let count = updates.get("Count")?.as_i64().ok_or("Update collection Count missing")?;
    let mut entries = Vec::new();
    for i in 0..count {
        if let Some(item) = updates.call("Item", &[Val::Int(i)])?.into_obj() {
            entries.push(entry_from(&item));
        }
    }
    Ok(entries)
}

fn run_with_timeout<T: Send + 'static>(
    what: &str,
    timeout: Duration,
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(format!("{} timed out after {}s", what, timeout.as_secs())),
        Err(_) => Err(format!("{} worker thread died", what)),
    }
}

const UNDELIVERABLE_DAYS: u64 = 7;

fn undeliverable_path() -> std::path::PathBuf {
    let base = std::env::var_os("ProgramData").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("DriverForge").join("undeliverable.txt")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Remember an update id Windows Update lists but cannot download (retried after a week).
pub fn mark_undeliverable(update_id: &str) {
    let p = undeliverable_path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut kept: Vec<String> = std::fs::read_to_string(&p)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with(update_id))
        .map(|l| l.to_string())
        .collect();
    kept.push(format!("{update_id}\t{}", now_secs()));
    let _ = std::fs::write(&p, kept.join("\n"));
}

/// Pure: ids recorded within the retry window.
pub fn recent_undeliverable(text: &str, now: u64) -> Vec<String> {
    text.lines()
        .filter_map(|l| {
            let (id, t) = l.split_once('\t')?;
            let t: u64 = t.trim().parse().ok()?;
            (now.saturating_sub(t) < UNDELIVERABLE_DAYS * 86_400).then(|| id.trim().to_ascii_lowercase())
        })
        .collect()
}

/// Read-only: list every driver update Windows Update offers that is not installed yet.
pub fn scan_wu_driver_updates() -> Result<Vec<WuEntry>, String> {
    run_with_timeout("Windows Update search", SEARCH_TIMEOUT, scan_blocking)
}

/// Pure: map a WUA IInstallationResult.ResultCode to (success, reboot-hint-text).
pub fn result_code_ok(code: i64) -> bool {
    matches!(code, 2 | 3) // orcSucceeded, orcSucceededWithErrors
}

pub fn result_code_text(code: i64) -> &'static str {
    match code {
        0 => "not started",
        1 => "in progress",
        2 => "succeeded",
        3 => "succeeded with errors",
        4 => "failed",
        5 => "aborted",
        _ => "unknown result",
    }
}

/// Pure: WU driver titles end with "(1.2.3.4)"; use it when DriverVerVersion is blank.
pub fn version_from_title(title: &str) -> String {
    let t = title.trim_end();
    if let (true, Some(open)) = (t.ends_with(')'), t.rfind('(')) {
        let inner = &t[open + 1..t.len() - 1];
        if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit() || c == '.') && inner.chars().any(|c| c.is_ascii_digit()) {
            return inner.to_string();
        }
    }
    String::new()
}

/// Pure: build a Candidate from a WU entry.
pub fn candidate_from_entry(e: &WuEntry) -> Candidate {
    let component = if e.title.is_empty() { e.update_id.clone() } else { e.title.clone() };
    Candidate {
        vendor: if e.manufacturer.is_empty() { "Windows Update".to_string() } else { e.manufacturer.clone() },
        component,
        installed: None,
        latest: e.version.clone(),
        // The WU update id; install_updates() resolves it back to the COM update object.
        url: e.update_id.clone(),
        sha256: None,
        size_bytes: Some((e.size_mb * 1_048_576.0) as u64),
        kind: InstallKind::WindowsUpdate,
        silent_args: Vec::new(),
        needs_reboot_hint: true,
    }
}

/// Download + install the given WU update ids (this is the ONLY mutating path; callers must
/// not invoke it in dry-run). Ids not currently offered are ignored.
pub fn install_updates(update_ids: &[String]) -> Result<WuInstallResult, String> {
    let ids: Vec<String> = update_ids.to_vec();
    if ids.is_empty() {
        return Ok(WuInstallResult { installed: 0, total: 0, reboot_required: false, message: "nothing to install".into() });
    }
    run_with_timeout("Windows Update install", INSTALL_TIMEOUT, move || install_blocking(&ids))
}

fn install_blocking(ids: &[String]) -> Result<WuInstallResult, String> {
    let (session, updates) = search_updates()?;
    let selected = create_from_progid("Microsoft.Update.UpdateColl")?;
    let count = updates.get("Count")?.as_i64().ok_or("Update collection Count missing")?;
    for i in 0..count {
        if let Some(item) = updates.call("Item", &[Val::Int(i)])?.into_obj() {
            let e = entry_from(&item);
            if ids.iter().any(|id| id.eq_ignore_ascii_case(&e.update_id)) {
                // EULA must be accepted for the download to be allowed (drivers normally have none).
                let _ = item.call("AcceptEula", &[]);
                selected.call("Add", &[Val::Obj(item)])?;
            }
        }
    }
    let total = selected.get("Count")?.as_i64().unwrap_or(0) as usize;
    if total == 0 {
        return Ok(WuInstallResult { installed: 0, total: 0, reboot_required: false, message: "no matching updates offered".into() });
    }
    let downloader = session.call("CreateUpdateDownloader", &[])?.into_obj().ok_or("CreateUpdateDownloader failed")?;
    downloader.put("Updates", &Val::Obj(clone_ref(&selected)?))?;
    let dres = downloader.call("Download", &[])?.into_obj().ok_or("Download returned no result")?;
    let dcode = prop(&dres, "ResultCode").as_i64().unwrap_or(-1);
    if !result_code_ok(dcode) {
        return Ok(WuInstallResult { installed: 0, total, reboot_required: false, message: format!("download {} (HRESULT 0x{:08X})", result_code_text(dcode), prop(&dres, "HResult").as_i64().unwrap_or(0) as u32) });
    }
    let installer = session.call("CreateUpdateInstaller", &[])?.into_obj().ok_or("CreateUpdateInstaller failed")?;
    installer.put("Updates", &Val::Obj(clone_ref(&selected)?))?;
    let ires = installer.call("Install", &[])?.into_obj().ok_or("Install returned no result")?;
    let code = prop(&ires, "ResultCode").as_i64().unwrap_or(-1);
    let reboot = prop(&ires, "RebootRequired").as_bool() || prop(&installer, "RebootRequired").as_bool();
    let ok = result_code_ok(code);
    Ok(WuInstallResult {
        installed: if ok { total } else { 0 },
        total,
        reboot_required: reboot,
        message: format!("install {} (HRESULT 0x{:08X})", result_code_text(code), prop(&ires, "HResult").as_i64().unwrap_or(0) as u32),
    })
}

/// Obtain a second owned handle to the same COM object (Val::Obj consumes its ComObj).
fn clone_ref(o: &ComObj) -> Result<ComObj, String> {
    // Round-trip through a property that returns the same object is not available; use Item-less
    // trick: UpdateColl.Copy() returns an independent collection with the same updates.
    o.call("Copy", &[])?.into_obj().ok_or_else(|| "Copy failed".to_string())
}

/// Pure: match the entry's hardware id to a scanned device; fill `installed` and drop offers that
/// are not newer than the installed driver. Unmatched entries are kept (WU says they apply).
pub fn candidate_for_devices(e: &WuEntry, devices: &[Device]) -> Option<Candidate> {
    let mut c = candidate_from_entry(e);
    let hw = e.hardware_id.trim();
    if hw.is_empty() {
        return Some(c);
    }
    let dev = devices.iter().find(|d| {
        d.hardware_ids.iter().chain(d.compat_ids.iter()).any(|id| id.eq_ignore_ascii_case(hw))
    });
    if let Some(d) = dev {
        if let Some(inst) = d.driver_version.as_deref().filter(|v| !v.is_empty()) {
            if !e.version.is_empty() && !crate::version::is_newer(&e.version, inst) {
                return None;
            }
            c.installed = Some(inst.to_string());
        }
    }
    Some(c)
}

struct WuProvider;

impl Provider for WuProvider {
    fn name(&self) -> &'static str {
        "windows-update"
    }
    fn check(&self, devices: &[Device]) -> Result<Vec<Candidate>, String> {
        let skip = recent_undeliverable(&std::fs::read_to_string(undeliverable_path()).unwrap_or_default(), now_secs());
        let mut out = Vec::new();
        for e in scan_wu_driver_updates()? {
            if skip.contains(&e.update_id.to_ascii_lowercase()) {
                crate::provider::note_info(format!(
                    "{} - Windows Update offers it but cannot deliver it yet (no downloadable package); it is retried weekly",
                    e.title
                ));
                continue;
            }
            if let Some(c) = candidate_for_devices(&e, devices) {
                out.push(c);
            }
        }
        Ok(out)
    }
}

pub fn provider() -> Box<dyn Provider> {
    Box::new(WuProvider)
}

#[cfg(test)]
mod tests {
    #[test]
    fn undeliverable_window() {
        let now = 10_000_000u64;
        let txt = format!("AAA\t{}\nbbb\t{}\nbad line", now - 100, now - 8 * 86_400);
        assert_eq!(recent_undeliverable(&txt, now), vec!["aaa".to_string()]);
    }

    use super::*;

    #[test]
    fn candidate_mapping() {
        let e = WuEntry {
            title: "Realtek - Net - 10.1".into(), update_id: "abc-123".into(), manufacturer: "Realtek".into(),
            version: "10.1.2.3".into(), size_mb: 2.0, ..Default::default()
        };
        let c = candidate_from_entry(&e);
        assert_eq!(c.vendor, "Realtek");
        assert_eq!(c.url, "abc-123");
        assert_eq!(c.latest, "10.1.2.3");
        assert_eq!(c.size_bytes, Some(2 * 1_048_576));
        assert!(matches!(c.kind, InstallKind::WindowsUpdate));
        let c2 = candidate_from_entry(&WuEntry { update_id: "x".into(), ..Default::default() });
        assert_eq!(c2.vendor, "Windows Update");
        assert_eq!(c2.component, "x");
    }

    #[test]
    fn matches_devices_and_skips_current() {
        let e = WuEntry { update_id: "u".into(), hardware_id: r"PCI\VEN_10EC&DEV_8125".into(), version: "10.2".into(), ..Default::default() };
        let mut d = Device { hardware_ids: vec![r"pci\ven_10ec&dev_8125".into()], driver_version: Some("10.1".into()), ..Default::default() };
        let c = candidate_for_devices(&e, &[d.clone()]).unwrap();
        assert_eq!(c.installed.as_deref(), Some("10.1"));
        d.driver_version = Some("10.2".into());
        assert!(candidate_for_devices(&e, &[d]).is_none());
        assert!(candidate_for_devices(&e, &[]).unwrap().installed.is_none());
    }

    #[test]
    fn title_version() {
        assert_eq!(version_from_title("Microsoft AudioProcessingObject Driver Update (1.0.4.55794)"), "1.0.4.55794");
        assert_eq!(version_from_title("No version here"), "");
        assert_eq!(version_from_title("Thing (beta)"), "");
    }

    #[test]
    fn result_codes() {
        assert!(result_code_ok(2) && result_code_ok(3));
        assert!(!result_code_ok(4) && !result_code_ok(-1));
        assert_eq!(result_code_text(4), "failed");
    }

    #[test]
    fn empty_install_is_noop() {
        let r = install_updates(&[]).unwrap();
        assert_eq!(r.total, 0);
    }

    #[test]
    fn timeout_helper() {
        let r: Result<(), String> = run_with_timeout("x", Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_millis(500));
            Ok(())
        });
        assert!(r.unwrap_err().contains("timed out"));
    }

    /// Real read-only search on this PC (run with --ignored --nocapture).
    #[test]
    #[ignore]
    fn live_search() {
        let v = scan_wu_driver_updates().expect("search");
        println!("WU pending driver updates: {}", v.len());
        for e in &v {
            println!("  - {} [{}] {} {:.1}MB", e.title, e.category, e.version, e.size_mb);
        }
    }
}
