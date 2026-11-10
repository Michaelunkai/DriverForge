//! NVIDIA GeForce driver provider (Game Ready / DCH / WHQL, no beta).
use crate::provider::Provider;
use crate::types::{Candidate, Device, InstallKind};

const LOOKUP: &str = "https://www.nvidia.com/Download/API/lookupValueSearch.aspx";
const AJAX: &str = "https://gfwsl.geforce.com/services_toolkit/services/com/nvidia/services/AjaxDriverService.php";
/// osID 135 = Windows 11 64-bit.
const OS_ID: u32 = 135;

struct P;

/// A parsed NVIDIA download entry.
#[derive(Debug, Clone, PartialEq)]
pub struct NvDownload {
    pub version: String,
    pub url: String,
    pub is_beta: bool,
    pub is_whql: bool,
    pub release_date: String,
    pub size_bytes: Option<u64>,
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn size_from_text(s: &str) -> Option<u64> {
    let mut it = s.split_whitespace();
    let n: f64 = it.next()?.parse().ok()?;
    let mul = match it.next()?.to_ascii_uppercase().as_str() {
        "GB" => 1024.0 * 1024.0 * 1024.0,
        "MB" => 1024.0 * 1024.0,
        "KB" => 1024.0,
        _ => return None,
    };
    Some((n * mul) as u64)
}

/// Parse a DriverManualLookup JSON response; None when no driver was found.
pub fn parse_driver_json(text: &str) -> Result<Option<NvDownload>, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("nvidia json: {e}"))?;
    let ids = match v.get("IDS").and_then(|x| x.as_array()) {
        Some(a) if !a.is_empty() => a,
        _ => return Ok(None),
    };
    let s = |d: &serde_json::Value, k: &str| d.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    for entry in ids {
        let d = match entry.get("downloadInfo") {
            Some(d) => d,
            None => continue,
        };
        let version = s(d, "Version");
        let url = s(d, "DownloadURL");
        if version.is_empty() || url.is_empty() {
            continue;
        }
        return Ok(Some(NvDownload {
            version,
            url,
            is_beta: s(d, "IsBeta") == "1",
            is_whql: s(d, "IsWHQL") == "1",
            release_date: s(d, "ReleaseDateTime"),
            size_bytes: size_from_text(&pct_decode(&s(d, "DownloadURLFileSize"))),
        }));
    }
    Ok(None)
}

/// Parse lookupValueSearch XML into (name, value) pairs.
pub fn parse_lookup_xml(xml: &str) -> Vec<(String, u32)> {
    let re = regex::Regex::new(r"<Name>([^<]*)</Name>\s*<Value>(\d+)</Value>").unwrap();
    re.captures_iter(xml)
        .filter_map(|c| Some((c[1].replace("&amp;", "&"), c[2].parse().ok()?)))
        .collect()
}

fn norm(s: &str) -> String {
    let l = s.to_ascii_lowercase().replace("laptop gpu", "").replace("nvidia", "").replace("geforce", "");
    l.chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

/// Pick the product whose normalised name equals the GPU name.
pub fn match_product(gpu_name: &str, products: &[(String, u32)]) -> Option<u32> {
    let g = norm(gpu_name);
    if g.is_empty() {
        return None;
    }
    products.iter().find(|(n, _)| norm(n) == g).map(|(_, v)| *v)
}

/// Order series so the most likely one for the GPU name is tried first.
pub fn rank_series(gpu_name: &str, series: &[(String, u32)]) -> Vec<(String, u32)> {
    let laptop = gpu_name.to_ascii_lowercase().contains("laptop");
    let digits: String = gpu_name
        .split_whitespace()
        .find(|t| t.len() >= 3 && t.chars().take(3).all(|c| c.is_ascii_digit()))
        .unwrap_or("")
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let key = if digits.len() >= 4 { format!(" {} series", &digits[..2]) } else { String::new() };
    let mut v: Vec<(i32, (String, u32))> = series
        .iter()
        .map(|s| {
            let n = s.0.to_ascii_lowercase();
            let mut score = 0;
            if !key.is_empty() && n.contains(&key) {
                score += 10;
            }
            if n.contains("notebook") == laptop {
                score += 5;
            }
            (-score, s.clone())
        })
        .collect();
    v.sort_by_key(|x| x.0);
    v.into_iter().map(|x| x.1).collect()
}

/// Windows driver version (32.0.16.2114) -> NVIDIA version (621.14).
pub fn nvidia_version_from_windows(v: &str) -> Option<String> {
    crate::version::nvidia_from_windows(v)
}

fn newer(latest: &str, installed: &str) -> bool {
    crate::version::is_newer(latest, installed)
}

fn is_nvidia_gpu(d: &Device) -> bool {
    // A GPU with no driver may report an empty class; accept that too.
    (d.class.eq_ignore_ascii_case("Display") || d.class.is_empty())
        && (d.hardware_ids.iter().chain(d.compat_ids.iter()).any(|h| h.to_ascii_uppercase().contains("VEN_10DE"))
            || d.pnp_id.to_ascii_uppercase().contains("VEN_10DE"))
}

/// Core logic with injectable fetcher (tests use captured fixtures).
/// `studio`: request the Studio driver instead of Game Ready.
/// `always`: return a candidate even when the installed driver is current.
pub fn check_with(
    devices: &[Device],
    fetch: &dyn Fn(&str) -> Result<String, String>,
    studio: bool,
    always: bool,
) -> Result<Vec<Candidate>, String> {
    let mut out = Vec::new();
    let mut series_cache: Option<Vec<(String, u32)>> = None;
    for d in devices.iter().filter(|d| is_nvidia_gpu(d)) {
        let installed = d.driver_version.as_deref().and_then(nvidia_version_from_windows);
        if series_cache.is_none() {
            series_cache = Some(parse_lookup_xml(&fetch(&format!("{LOOKUP}?TypeID=2&ParentID=1"))?));
        }
        let series = rank_series(&d.name, series_cache.as_ref().unwrap());
        let mut found = None;
        for (_, sid) in &series {
            let prods = parse_lookup_xml(&fetch(&format!("{LOOKUP}?TypeID=3&ParentID={sid}"))?);
            if let Some(pid) = match_product(&d.name, &prods) {
                found = Some((*sid, pid));
                break;
            }
        }
        let (psid, pfid) = found.ok_or_else(|| format!("nvidia: no product match for '{}'", d.name))?;
        let url = format!(
            "{AJAX}?func=DriverManualLookup&psid={psid}&pfid={pfid}&osID={OS_ID}&languageCode=1033&beta=0&isWHQL=1&dltype=-1&dch=1&upCRD={}&qnf=0&ctk=0&sort1=1&numberOfResults=1",
            if studio { 1 } else { 0 }
        );
        let dl = parse_driver_json(&fetch(&url)?)?.ok_or("nvidia: no driver returned")?;
        crate::log::info(&format!("nvidia: {} installed={:?} latest={}", d.name, installed, dl.version));
        if dl.is_beta {
            continue;
        }
        let outdated = match &installed {
            None => true,
            Some(i) => newer(&dl.version, i),
        };
        if (outdated || always) && !out.iter().any(|c: &Candidate| c.url == dl.url) {
            out.push(Candidate {
                vendor: "NVIDIA".into(),
                component: format!("{} driver", d.name),
                installed,
                latest: dl.version,
                url: dl.url,
                sha256: None,
                size_bytes: dl.size_bytes,
                kind: InstallKind::ExeSilent,
                silent_args: ["-s", "-noreboot", "-noeula"].iter().map(|s| s.to_string()).collect(),
                needs_reboot_hint: false,
            });
        }
    }
    Ok(out)
}

impl Provider for P {
    fn name(&self) -> &'static str {
        "nvidia"
    }
    /// Returns a candidate only when the GPU driver is missing or outdated.
    fn check(&self, devices: &[Device]) -> Result<Vec<Candidate>, String> {
        check_with(devices, &|u| crate::http::get_text(u), false, false)
    }
}

pub fn provider() -> Box<dyn Provider> {
    Box::new(P)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERIES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?><LookupValueSearch><LookupValues><LookupValue RequiresProduct="True" ParentID="1" IsSelectLess="False"><Name>GeForce RTX 50 Series (Notebooks)</Name><Value>133</Value></LookupValue><LookupValue RequiresProduct="True" ParentID="1" IsSelectLess="False"><Name>GeForce RTX 50 Series</Name><Value>131</Value></LookupValue><LookupValue RequiresProduct="True" ParentID="1" IsSelectLess="False"><Name>GeForce RTX 40 Series</Name><Value>127</Value></LookupValue></LookupValues></LookupValueSearch>"#;
    const PRODUCTS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?><LookupValueSearch><LookupValues><LookupValue ParentID="131"><Name>NVIDIA GeForce RTX 5090</Name><Value>1066</Value></LookupValue><LookupValue ParentID="131"><Name>NVIDIA GeForce RTX 5080</Name><Value>1065</Value></LookupValue><LookupValue ParentID="131"><Name>NVIDIA GeForce RTX 5070 Ti</Name><Value>1068</Value></LookupValue><LookupValue ParentID="131"><Name>NVIDIA GeForce RTX 5070</Name><Value>1070</Value></LookupValue></LookupValues></LookupValueSearch>"#;
    // Captured from DriverManualLookup psid=131&pfid=1065 (2026-11-10); long fields trimmed.
    const LOOKUP_JSON: &str = r#"{ "Success" : "1", "IDS" : [{ "downloadInfo": { "Success" : "1", "ID" : "283968", "DownloadTypeID" : "1", "Name" : "GeForce%20Game%20Ready%20Driver", "Release" : "620", "Version" : "621.14", "IsBeta" : "0", "IsWHQL" : "1", "IsRecommended" : "0", "IsNewest" : "1", "IsDC" : "1", "IsCRD" : "0", "ReleaseDateTime" : "Wed Nov 04, 2026", "DownloadURL" : "https://us.download.nvidia.com/Windows/621.14/621.14-desktop-win10-win11-64bit-international-dch-whql.exe", "DownloadURLFileSize" : "1.04 GB", "Is64Bit" : "1"}}]}"#;
    const EMPTY_JSON: &str = r#"{ "Success" : "0", "IDS" : [] }"#;

    fn fetch(u: &str) -> Result<String, String> {
        if u.contains("TypeID=2") {
            Ok(SERIES_XML.into())
        } else if u.contains("TypeID=3&ParentID=131") {
            Ok(PRODUCTS_XML.into())
        } else if u.contains("TypeID=3") {
            Ok("<LookupValueSearch><LookupValues></LookupValues></LookupValueSearch>".into())
        } else if u.contains("pfid=1065") {
            Ok(LOOKUP_JSON.into())
        } else {
            Ok(EMPTY_JSON.into())
        }
    }

    fn gpu(name: &str, ver: Option<&str>) -> Device {
        Device {
            name: name.into(),
            class: "Display".into(),
            hardware_ids: vec!["PCI\\VEN_10DE&DEV_2C02&SUBSYS_00000000&REV_A1".into()],
            driver_version: ver.map(|s| s.into()),
            ..Default::default()
        }
    }

    #[test]
    fn version_conversion() {
        assert_eq!(nvidia_version_from_windows("32.0.16.2114").as_deref(), Some("621.14"));
        assert_eq!(nvidia_version_from_windows("31.0.15.5222").as_deref(), Some("552.22"));
        assert_eq!(nvidia_version_from_windows("32.0.15.6094").as_deref(), Some("560.94"));
        assert_eq!(nvidia_version_from_windows("garbage"), None);
    }

    #[test]
    fn parses_json() {
        let d = parse_driver_json(LOOKUP_JSON).unwrap().unwrap();
        assert_eq!(d.version, "621.14");
        assert!(d.url.ends_with("621.14-desktop-win10-win11-64bit-international-dch-whql.exe"));
        assert!(d.is_whql && !d.is_beta);
        assert_eq!(d.size_bytes, Some((1.04 * 1024.0 * 1024.0 * 1024.0) as u64));
        assert_eq!(parse_driver_json(EMPTY_JSON).unwrap(), None);
        assert!(parse_driver_json("<html>").is_err());
    }

    #[test]
    fn parses_crlf_separated_xml() {
        // Real lookupValueSearch responses put CR/LF between tags.
        let xml = "<LookupValues>\r<LookupValue ParentID=\"1\">\r<Name>GeForce RTX 50 Series</Name>\r<Value>131</Value>\r</LookupValue>\r</LookupValues>";
        assert_eq!(parse_lookup_xml(xml), vec![("GeForce RTX 50 Series".to_string(), 131)]);
    }

    #[test]
    fn matches_product() {
        let p = parse_lookup_xml(PRODUCTS_XML);
        assert_eq!(match_product("NVIDIA GeForce RTX 5080", &p), Some(1065));
        assert_eq!(match_product("NVIDIA GeForce RTX 5070", &p), Some(1070));
        assert_eq!(match_product("NVIDIA GeForce RTX 5070 Ti", &p), Some(1068));
        assert_eq!(match_product("NVIDIA GeForce RTX 9999", &p), None);
        let s = rank_series("NVIDIA GeForce RTX 5080", &parse_lookup_xml(SERIES_XML));
        assert_eq!(s[0].1, 131);
    }

    #[test]
    fn current_driver_is_up_to_date() {
        let r = check_with(&[gpu("NVIDIA GeForce RTX 5080", Some("32.0.16.2114"))], &fetch, false, false).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn outdated_and_missing_yield_candidate() {
        let r = check_with(&[gpu("NVIDIA GeForce RTX 5080", Some("32.0.15.6094"))], &fetch, false, false).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].latest, "621.14");
        assert_eq!(r[0].installed.as_deref(), Some("560.94"));
        assert_eq!(r[0].silent_args, vec!["-s", "-noreboot", "-noeula"]);
        let m = check_with(&[gpu("NVIDIA GeForce RTX 5080", None)], &fetch, false, false).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].installed, None);
    }

    #[test]
    fn ignores_non_nvidia_and_always_mode() {
        let amd = Device {
            name: "AMD Radeon(TM) Graphics".into(),
            class: "Display".into(),
            hardware_ids: vec!["PCI\\VEN_1002&DEV_13C0".into()],
            ..Default::default()
        };
        assert!(check_with(&[amd], &fetch, false, true).unwrap().is_empty());
        let r = check_with(&[gpu("NVIDIA GeForce RTX 5080", Some("32.0.16.2114"))], &fetch, false, true).unwrap();
        assert_eq!(r[0].latest, "621.14");
    }
}
