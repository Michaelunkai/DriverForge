//! Microsoft Update Catalog provider: search by hardware id, parse results, resolve CAB URL.
use crate::provider::Provider;
use crate::types::{Candidate, Device, InstallKind};
use regex::Regex;
use std::sync::OnceLock;

const BASE: &str = "https://www.catalog.update.microsoft.com";
const COMMON_CLASSES: &[&str] = &[
    "media", "net", "wifi", "bluetooth", "system", "hdc", "scsiadapter", "usb", "image", "camera",
    "sensor", "firmware", "biometric", "smartcardreader", "softwarecomponent", "monitor", "hidclass", "hid",
    "diskdrive", "cdrom", "mtd", "printer", "ports", "processor", "display", "extension", "audioendpoint",
];

/// Pure: per device class, (total devices, devices the catalog will look up).
pub fn class_summary(devices: &[Device]) -> Vec<(String, usize, usize)> {
    let mut m: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    for d in devices {
        let e = m.entry(d.class.to_lowercase()).or_default();
        e.0 += 1;
        if wanted(d) {
            e.1 += 1;
        }
    }
    m.into_iter().map(|(k, (t, q))| (k, t, q)).collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub update_id: String,
    pub title: String,
    pub products: String,
    pub classification: String,
    /// ISO yyyy-mm-dd (sortable)
    pub date: String,
    pub version: String,
    pub size_bytes: Option<u64>,
}

fn re(cell: &'static OnceLock<Regex>, p: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(p).expect("static regex"))
}

fn strip(s: &str) -> String {
    static TAG: OnceLock<Regex> = OnceLock::new();
    let t = re(&TAG, r"(?s)<[^>]*>").replace_all(s, " ");
    let t = t.replace("&amp;", "&").replace("&nbsp;", " ").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"");
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// m/d/yyyy -> yyyy-mm-dd
fn iso_date(s: &str) -> Option<String> {
    let p: Vec<&str> = s.trim().split('/').collect();
    if p.len() != 3 {
        return None;
    }
    let (m, d, y) = (p[0].parse::<u32>().ok()?, p[1].parse::<u32>().ok()?, p[2].parse::<u32>().ok()?);
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// Pure parser of a Search.aspx results page. Unknown markup yields an empty Vec.
pub fn parse_search(html: &str) -> Vec<Entry> {
    static ROW: OnceLock<Regex> = OnceLock::new();
    static CELL: OnceLock<Regex> = OnceLock::new();
    static SIZE: OnceLock<Regex> = OnceLock::new();
    let row = re(&ROW, r#"<tr id="([0-9a-fA-F-]{36})_R\d+""#);
    let cell = re(&CELL, r#"(?s)id="[0-9a-fA-F-]{36}_C(\d)_R\d+"[^>]*>(.*?)</td>"#);
    let size = re(&SIZE, r#"_originalSize">\s*(\d+)\s*<"#);
    let starts: Vec<(usize, String)> = row.captures_iter(html).map(|c| (c.get(0).unwrap().start(), c[1].to_string())).collect();
    let mut out = Vec::new();
    for (i, (s, id)) in starts.iter().enumerate() {
        let e = starts.get(i + 1).map(|x| x.0).unwrap_or(html.len());
        let chunk = &html[*s..e];
        let mut c: [String; 8] = Default::default();
        for m in cell.captures_iter(chunk) {
            if let Ok(n) = m[1].parse::<usize>() {
                if n < 8 {
                    c[n] = m[2].to_string();
                }
            }
        }
        let title = strip(&c[1]);
        let date = match iso_date(&strip(&c[4])) {
            Some(d) => d,
            None => continue,
        };
        if title.is_empty() {
            continue;
        }
        out.push(Entry {
            update_id: id.to_lowercase(),
            title,
            products: strip(&c[2]),
            classification: strip(&c[3]),
            date,
            version: strip(&c[5]),
            size_bytes: size.captures(&c[6]).and_then(|m| m[1].parse().ok()),
        });
    }
    out
}

/// Pure parser of a DownloadDialog.aspx response: all file URLs (https only).
pub fn parse_download_urls(html: &str) -> Vec<String> {
    static URL: OnceLock<Regex> = OnceLock::new();
    re(&URL, r#"files\[\d+\]\.url\s*=\s*'(https://[^']+)'"#)
        .captures_iter(html)
        .map(|c| c[1].to_string())
        .filter(|u| host_allowed(u))
        .collect()
}

/// Only Microsoft download hosts are accepted (host must end in windowsupdate.com or microsoft.com).
pub fn host_allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("").to_lowercase();
    if host.contains('@') || host.contains(':') {
        return false;
    }
    ["windowsupdate.com", "microsoft.com"].iter().any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// Base64 SHA-1 digests the dialog publishes per file (same order as the URLs).
pub fn parse_download_digests(html: &str) -> Vec<String> {
    static D: OnceLock<Regex> = OnceLock::new();
    re(&D, r#"files\[\d+\]\.digest\s*=\s*'([^']+)'"#).captures_iter(html).map(|c| c[1].to_string()).collect()
}

fn is_arm(e: &Entry) -> bool {
    let t = format!("{} {}", e.title, e.products).to_lowercase();
    t.contains("arm64") || t.contains("arm-based")
}

/// Newest applicable x64 Windows 11 driver (falls back to any non-ARM entry if none mention Windows 11).
pub fn pick_best(entries: &[Entry]) -> Option<&Entry> {
    let ok: Vec<&Entry> = entries.iter().filter(|e| !is_arm(e) && e.classification.to_lowercase().contains("driver")).collect();
    let w11: Vec<&Entry> = ok.iter().copied().filter(|e| e.products.contains("Windows 11")).collect();
    let pool = if w11.is_empty() { ok } else { w11 };
    pool.into_iter().max_by(|a, b| a.date.cmp(&b.date).then_with(|| crate::version::cmp(&a.version, &b.version)))
}

/// Reduce a hardware id to a catalog search key (drop SUBSYS/REV).
pub fn search_key(hwid: &str) -> String {
    let u = hwid.trim();
    match u.to_uppercase().find("&SUBSYS") {
        Some(i) => u[..i].to_string(),
        None => match u.to_uppercase().find("&REV") {
            Some(i) => u[..i].to_string(),
            None => u.to_string(),
        },
    }
}

fn vendor_handled(d: &Device) -> bool {
    let ids = d.hardware_ids.join(" ").to_uppercase();
    ids.contains("VEN_10DE") || ids.contains("VEN_1002") || ids.contains("VID_10DE") || ids.contains("VID_1002")
}

fn wanted(d: &Device) -> bool {
    if d.hardware_ids.is_empty() || vendor_handled(d) {
        return false;
    }
    let first = d.hardware_ids[0].to_uppercase();
    if first.starts_with("ROOT\\") || first.starts_with("SWD\\") || first.starts_with("BTH\\") {
        return false;
    }
    // Missing driver / driver problem: always look. Otherwise skip Windows inbox ("Microsoft",
    // "(Standard ...)") devices, whose updates come through Windows Update.
    let m = d.manufacturer.to_lowercase();
    let generic = m.contains("microsoft") || m.starts_with("(standard");
    d.problem_code != 0 || d.driver_version.is_none() || (!generic && COMMON_CLASSES.contains(&d.class.to_lowercase().as_str()))
}

pub fn search_url(key: &str) -> String {
    let enc: String = key.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    format!("{BASE}/Search.aspx?q={enc}")
}

/// Resolve the CAB URL for an update id via the DownloadDialog POST.
/// Returns (url, base64 SHA-1 digest if published).
fn resolve_cab(update_id: &str) -> Result<Option<(String, Option<String>)>, String> {
    let body = format!(r#"[{{"size":0,"languages":"","uidInfo":"{id}","updateID":"{id}"}}]"#, id = update_id);
    let html = crate::http::post_form(&format!("{BASE}/DownloadDialog.aspx"), &[("updateIDs", &body)])?;
    if is_throttled(&html) || html.contains("Too many") && !html.contains("files[") {
        return Err("Microsoft Update Catalog is throttling requests".into());
    }
    let digests = parse_download_digests(&html);
    // Digest index matches the raw files[] index, so locate the CAB among all raw URLs.
    static U: OnceLock<Regex> = OnceLock::new();
    let raw = re(&U, r#"files\[(\d+)\]\.url\s*=\s*'(https://[^']+)'"#);
    for c in raw.captures_iter(&html) {
        if host_allowed(&c[2]) && c[2].to_lowercase().ends_with(".cab") {
            let idx: usize = c[1].parse().unwrap_or(usize::MAX);
            return Ok(Some((c[2].to_string(), digests.get(idx).cloned())));
        }
    }
    Ok(None)
}

/// Pure: the "Supported Hardware IDs" of a Catalog details page, lower-cased.
pub fn parse_supported_ids(html: &str) -> Vec<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    let r = re(&R, r#"(?s)id="driverhwIDs"[^>]*>(.*?)</div>\s*</div>\s*</div>"#);
    let block = match r.captures(html) {
        Some(c) => c[1].to_string(),
        None => match html.find("id=\"driverhwIDs\"") {
            Some(i) => html[i..].chars().take(400_000).collect(),
            None => return Vec::new(),
        },
    };
    static D: OnceLock<Regex> = OnceLock::new();
    let d = re(&D, r#"(?s)<div[^>]*>\s*([^<\s][^<]*?)\s*</div>"#);
    d.captures_iter(&block).map(|c| c[1].replace("&amp;", "&").trim().to_lowercase()).filter(|x| x.contains('\\')).collect()
}

/// True when the package lists one of the device's hardware/compatible ids.
pub fn applies_to(device: &Device, supported: &[String]) -> bool {
    let set: std::collections::HashSet<&str> = supported.iter().map(|x| x.as_str()).collect();
    device.hardware_ids.iter().chain(device.compat_ids.iter()).any(|h| set.contains(h.to_lowercase().as_str()))
}

/// Entries in preference order (same filter/ordering as `pick_best`).
pub fn ranked(entries: &[Entry]) -> Vec<&Entry> {
    let ok: Vec<&Entry> = entries.iter().filter(|e| !is_arm(e) && e.classification.to_lowercase().contains("driver")).collect();
    let w11: Vec<&Entry> = ok.iter().copied().filter(|e| e.products.contains("Windows 11")).collect();
    let mut pool = if w11.is_empty() { ok } else { w11 };
    pool.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| crate::version::cmp(&b.version, &a.version)));
    pool
}

/// Fetch the details page of an update and check it really supports this device.
/// Returns Ok(true/false); Err when the page could not be read after retries.
fn package_applies(device: &Device, update_id: &str) -> Result<bool, String> {
    let url = format!("{BASE}/ScopedViewInline.aspx?updateid={update_id}");
    let mut last = String::new();
    for attempt in 0..4u64 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(5 * attempt));
        }
        match crate::http::get_text(&url) {
            Ok(h) if is_throttled(&h) => last = "Microsoft Update Catalog is throttling requests".into(),
            Ok(h) => {
                let ids = parse_supported_ids(&h);
                if ids.is_empty() {
                    return Err(format!("details page for {update_id} listed no hardware ids"));
                }
                return Ok(applies_to(device, &ids));
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

const WORKERS: usize = 1;
const PAUSE_MS: u64 = 1200;

/// True when the Catalog returned its "throttled" page instead of real results.
pub fn is_throttled(html: &str) -> bool {
    html.contains("Too many search requests") && !html.contains("_originalSize")
}

/// Look up one hardware id (with one retry). Ok(None) = nothing newer / nothing available.
fn lookup(d: &Device, key: &str) -> Result<Option<Candidate>, String> {
    // The Catalog throttles heavy use: it answers HTTP 200 with a page saying "Too many search
    // requests" and no results. That must never be mistaken for "no driver found", so we wait
    // and retry (up to ~6 minutes per device) and report the check as incomplete if it never clears.
    let mut last = String::new();
    let mut entries = Vec::new();
    let mut ok = false;
    for attempt in 0..8u64 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(10 * attempt * attempt.min(4)));
        }
        match crate::http::get_text(&search_url(key)) {
            Ok(h) if is_throttled(&h) => {
                last = "Microsoft Update Catalog is throttling requests".into();
                crate::log::info(&format!("catalog: {key} throttled (attempt {attempt})"));
            }
            Ok(h) => {
                entries = parse_search(&h);
                crate::log::info(&format!("catalog: {key} -> {} bytes, {} entries", h.len(), entries.len()));
                ok = true;
                break;
            }
            Err(e) => last = e,
        }
    }
    if !ok {
        return Err(format!("search {key} failed: {last}"));
    }
    // Walk the newest packages until one really supports this device (the Catalog search is fuzzy).
    let mut chosen: Option<&Entry> = None;
    for e in ranked(&entries).into_iter().take(4) {
        if let Some(inst) = &d.driver_version {
            if !crate::version::is_newer(&e.version, inst) {
                break; // sorted newest first: nothing further down can be newer
            }
        }
        match package_applies(d, &e.update_id) {
            Ok(true) => {
                chosen = Some(e);
                break;
            }
            Ok(false) => crate::log::info(&format!("catalog: {} not applicable to {}", e.title, d.name)),
            Err(er) => return Err(er),
        }
    }
    let Some(best) = chosen else { return Ok(None) };
    if let Some(inst) = &d.driver_version {
        if !crate::version::is_newer(&best.version, inst) {
            return Ok(None);
        }
    }
    // Windows ranks a driver by its date before its version, so a package published before the
    // installed driver's date would be staged but never selected; it is not a real update.
    if let Some(idate) = &d.driver_date {
        if !best.date.is_empty() && best.date.as_str() < idate.as_str() {
            crate::log::info(&format!("catalog: {} skipped, package date {} older than installed {}", d.name, best.date, idate));
            return Ok(None);
        }
    }
    let mut cab = Err(String::new());
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(1500 * attempt));
        }
        cab = resolve_cab(&best.update_id);
        if cab.is_ok() {
            break;
        }
    }
    match cab {
        Ok(Some((url, sha1))) => Ok(Some(Candidate {
            vendor: "Microsoft Update Catalog".into(),
            // SHA-1 (base64) is kept in the note because Candidate has no SHA-1 field.
            component: match sha1 {
                Some(s) => format!("{} [{}] sha1b64={}", d.name, best.title, s),
                None => format!("{} [{}]", d.name, best.title),
            },
            installed: d.driver_version.clone(),
            latest: best.version.clone(),
            url,
            sha256: None,
            size_bytes: best.size_bytes,
            // CAB: extract then `pnputil /add-driver *.inf /subdirs /install`
            kind: InstallKind::ZipThenSetup,
            silent_args: vec!["/add-driver".into(), "*.inf".into(), "/subdirs".into(), "/install".into()],
            needs_reboot_hint: false,
        })),
        Ok(None) => Ok(None),
        Err(e) => Err(format!("download dialog {} failed: {e}", best.update_id)),
    }
}

struct P;
impl Provider for P {
    fn name(&self) -> &'static str {
        "catalog"
    }
    fn check(&self, devices: &[Device]) -> Result<Vec<Candidate>, String> {
        for (c, t, q) in class_summary(devices) {
            crate::log::info(&format!("catalog: class {} - {} device(s), {} queried", if c.is_empty() { "(none)" } else { &c }, t, q));
        }
        // One job per unique hardware id; every wanted device is checked (no cap).
        let mut seen = std::collections::HashSet::new();
        let mut jobs: Vec<(&Device, String)> = Vec::new();
        for d in devices.iter().filter(|d| wanted(d)) {
            let key = search_key(&d.hardware_ids[0]);
            if seen.insert(key.to_uppercase()) {
                jobs.push((d, key));
            }
        }
        let next = std::sync::atomic::AtomicUsize::new(0);
        let results: std::sync::Mutex<Vec<Candidate>> = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for _ in 0..WORKERS.min(jobs.len().max(1)) {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let Some((d, key)) = jobs.get(i) else { break };
                    match lookup(d, key) {
                        Ok(Some(c)) => {
                            if let Ok(mut r) = results.lock() {
                                r.push(c);
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            crate::log::warn(&format!("catalog: {e}"));
                            crate::provider::note_incomplete(format!("Microsoft Update Catalog lookup for \"{}\" failed ({e})", d.name));
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(PAUSE_MS));
                });
            }
        });
        let mut out = results.into_inner().unwrap_or_default();
        out.sort_by(|a, b| a.component.cmp(&b.component));
        Ok(out)
    }
}
pub fn provider() -> Box<dyn Provider> {
    Box::new(P)
}

#[cfg(test)]
mod tests {
    #[test]
    fn class_summary_covers_every_class() {
        let mk = |c: &str| Device { class: c.into(), manufacturer: "Acme".into(), hardware_ids: vec![r"PCI\VEN_1&DEV_2".into()], driver_version: Some("1.0".into()), ..Default::default() };
        let d = vec![mk("MEDIA"), mk("NET"), mk("MONITOR"), mk("HIDCLASS"), mk("DISKDRIVE"), mk("USB"), mk("BLUETOOTH"), mk("SYSTEM"), mk("FIRMWARE"), mk("WEIRD")];
        let s = class_summary(&d);
        for c in ["media", "net", "monitor", "hidclass", "diskdrive", "usb", "bluetooth", "system", "firmware"] {
            assert!(s.iter().any(|x| x.0 == c && x.2 == 1), "class {c} not queried");
        }
        assert!(s.iter().any(|x| x.0 == "weird" && x.1 == 1 && x.2 == 0));
    }

    #[test]
    fn supported_ids_and_applicability() {
        let html = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/catalog_details_realtek.html")).unwrap();
        let ids = parse_supported_ids(&html);
        assert!(ids.len() > 3, "parsed {ids:?}");
        assert!(ids.contains(&"hdaudio\\func_01&ven_10ec&dev_0245&subsys_1558a742".to_string()));
        let hit = Device { hardware_ids: vec!["HDAUDIO\\FUNC_01&VEN_10EC&DEV_0245&SUBSYS_1558A742&REV_1001".into(), "HDAUDIO\\FUNC_01&VEN_10EC&DEV_0245&SUBSYS_1558A742".into()], ..Default::default() };
        assert!(applies_to(&hit, &ids));
        let miss = Device { hardware_ids: vec!["HDAUDIO\\FUNC_01&VEN_10EC&DEV_1220&SUBSYS_1458A0C3".into()], ..Default::default() };
        assert!(!applies_to(&miss, &ids));
    }

    use super::*;
    const SEARCH: &str = include_str!("../qa/fixtures/catalog_search_realtek.html");
    const DIALOG: &str = include_str!("../qa/fixtures/catalog_download_dialog.html");

    #[test]
    fn parses_real_search() {
        let e = parse_search(SEARCH);
        assert!(e.len() >= 20, "{}", e.len());
        let f = &e[0];
        assert_eq!(f.update_id, "a6317cda-a850-45f2-a302-a2ded3a980bc");
        assert_eq!(f.title, "Realtek Net Driver Update (1125.30.50.508)");
        assert_eq!(f.version, "1125.30.50.508");
        assert_eq!(f.date, "2026-05-07");
        assert_eq!(f.size_bytes, Some(328320));
        assert!(f.classification.contains("Networking"));
    }
    #[test]
    fn picks_newest() {
        let e = parse_search(SEARCH);
        let b = pick_best(&e).unwrap();
        assert!(e.iter().all(|x| is_arm(x) || x.date <= b.date || !x.products.contains("Windows 11")));
    }
    #[test]
    fn parses_download_url() {
        let u = parse_download_urls(DIALOG);
        assert!(u[0].starts_with("https://catalog.s.download.windowsupdate.com/") && u[0].ends_with(".cab"));
    }
    #[test]
    fn host_allowlist_and_digest() {
        assert!(host_allowed("https://catalog.s.download.windowsupdate.com/x.cab"));
        assert!(!host_allowed("http://catalog.s.download.windowsupdate.com/x.cab"));
        assert!(!host_allowed("https://evil.com/windowsupdate.com/x.cab"));
        assert!(!host_allowed("https://windowsupdate.com.evil.com/x.cab"));
        assert!(!host_allowed("https://microsoft.com@evil.com/x.cab"));
        assert_eq!(parse_download_digests(DIALOG)[0], "Aub8HVoF0bUobX4mj09fyFMi+L4=");
    }
    #[test]
    fn garbage_is_graceful() {
        assert!(parse_search("<html>changed</html>").is_empty());
        assert!(parse_download_urls("").is_empty());
    }
    #[test]
    fn keys() {
        assert_eq!(search_key("PCI\\VEN_10EC&DEV_8125&SUBSYS_E0001458&REV_0C"), "PCI\\VEN_10EC&DEV_8125");
        assert_eq!(search_url("PCI\\VEN_10EC&DEV_8125"), format!("{BASE}/Search.aspx?q=PCI%5CVEN_10EC%26DEV_8125"));
    }
}
