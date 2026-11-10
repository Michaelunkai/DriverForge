//! HTTPS-only HTTP helpers: get_text / get_json / download (resume, retry, sha256).
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";
const RETRIES: u32 = 3;
const MAX_REDIRECTS: u32 = 8;
const MAX_TEXT_BYTES: u64 = 64 * 1024 * 1024;

/// Scheme policy. Production: https only. Tests may allow plain http to loopback mocks.
fn scheme_ok(url: &str) -> Result<(), String> {
    let l = url.trim().to_ascii_lowercase();
    // Test switch: plain http to the loopback mock (127.0.0.1 only) when
    // DRIVERFORGE_ALLOW_LOOPBACK=1 (or in this crate's own unit tests).
    #[cfg(any(test, debug_assertions))]
    if is_loopback_mock(&l)
        && (cfg!(test) || std::env::var("DRIVERFORGE_ALLOW_LOOPBACK").map(|v| v == "1").unwrap_or(false))
    {
        return Ok(());
    }
    if !l.starts_with("https://") {
        return Err(format!("refusing non-https URL: {}", url));
    }
    allowed_host(&l).map_err(|h| format!("refusing host not in allowlist: {}", h))
}

/// Strictly `http://127.0.0.1:<digits>` followed by end, `/`, `?` or `#`. Compiled out of release builds.
#[cfg(any(test, debug_assertions))]
fn is_loopback_mock(lower: &str) -> bool {
    match lower.strip_prefix("http://127.0.0.1:") {
        Some(r) => {
            let end = r.find(|c| c == '/' || c == '?' || c == '#').unwrap_or(r.len());
            end > 0 && r[..end].chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

/// Exact hosts or `.suffix` matches permitted for every request and redirect hop.
const ALLOWED_HOSTS: &[&str] = &[
    "drivers.amd.com", "www.amd.com", "amd.com", "gfwsl.geforce.com", "www.nvidia.com",
    "us.download.nvidia.com", "international.download.nvidia.com", "download.nvidia.com",
    "download.windowsupdate.com", "catalog.update.microsoft.com", "aka.ms",
];
const ALLOWED_SUFFIXES: &[&str] = &[".delivery.mp.microsoft.com", ".microsoft.com", ".download.windowsupdate.com"];
/// Hard cap on a single download (4 GiB).
const MAX_DOWNLOAD_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// `lower` must be a lowercase https URL. Err(host) if the host is not allowed.
fn allowed_host(lower: &str) -> Result<(), String> {
    let rest = &lower["https://".len()..];
    let auth = rest.split(|c| c == '/' || c == '?' || c == '#').next().unwrap_or("");
    if auth.contains('@') || auth.contains('\\') {
        return Err(auth.to_string());
    }
    let host = match auth.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => h,
        Some(_) => return Err(auth.to_string()),
        None => auth,
    };
    let host = host.trim_end_matches('.');
    if ALLOWED_HOSTS.contains(&host) || ALLOWED_SUFFIXES.iter().any(|s| host.ends_with(s)) {
        Ok(())
    } else {
        Err(host.to_string())
    }
}

fn proxy_from_env() -> Option<String> {
    for k in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy", "HTTP_PROXY", "http_proxy"] {
        if let Ok(v) = std::env::var(k) {
            if !v.trim().is_empty() {
                return Some(v);
            }
        }
    }
    // Windows system proxy (WinINET)
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;
        if let Ok(k) = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
        {
            let en: u32 = k.get_value("ProxyEnable").unwrap_or(0);
            if en == 1 {
                if let Ok(s) = k.get_value::<String, _>("ProxyServer") {
                    let s = s.trim().to_string();
                    if s.is_empty() {
                        return None;
                    }
                    if s.contains('=') {
                        for part in s.split(';') {
                            if let Some(v) = part.strip_prefix("https=").or_else(|| part.strip_prefix("http=")) {
                                return Some(v.to_string());
                            }
                        }
                        return None;
                    }
                    return Some(s);
                }
            }
        }
    }
    None
}

fn agent() -> ureq::Agent {
    let mut b = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .redirects(0)
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .timeout_write(Duration::from_secs(30));
    if let Some(p) = proxy_from_env() {
        if let Ok(px) = ureq::Proxy::new(&p) {
            b = b.proxy(px);
        }
    }
    b.build()
}

/// GET with manual redirect following; every hop must be https.
fn request(url: &str, range_from: u64) -> Result<ureq::Response, String> {
    let ag = agent();
    let mut cur = url.trim().to_string();
    for _ in 0..=MAX_REDIRECTS {
        scheme_ok(&cur)?;
        let mut req = ag.get(&cur).set("Accept", "*/*").set("Accept-Language", "en-US,en;q=0.9");
        if cur.to_ascii_lowercase().starts_with("https://drivers.amd.com/") {
            // AMD's CDN refuses downloads (302 to an error page) unless the request carries an amd.com referer.
            req = req.set("Referer", "https://www.amd.com/");
        }
        if range_from > 0 {
            req = req.set("Range", &format!("bytes={}-", range_from));
        }
        let resp = match req.call() {
            Ok(r) => r,
            Err(ureq::Error::Status(c, r)) if (301..=308).contains(&c) => r,
            Err(ureq::Error::Status(416, r)) => return Ok(r),
            Err(ureq::Error::Status(c, _)) => return Err(format!("HTTP {} for {}", c, cur)),
            Err(e) => return Err(format!("request failed for {}: {}", cur, e)),
        };
        if (301..=308).contains(&resp.status()) && resp.status() != 304 {
            let loc = resp.header("Location").ok_or("redirect without Location")?.to_string();
            cur = join_url(&cur, &loc);
            continue;
        }
        return Ok(resp);
    }
    Err(format!("too many redirects for {}", url))
}

fn join_url(base: &str, loc: &str) -> String {
    if loc.contains("://") {
        return loc.to_string();
    }
    let scheme_end = base.find("://").map(|i| i + 3).unwrap_or(0);
    if loc.starts_with("//") {
        return format!("{}{}", &base[..scheme_end - 2], loc);
    }
    let host_end = base[scheme_end..].find('/').map(|i| i + scheme_end).unwrap_or(base.len());
    if loc.starts_with('/') {
        return format!("{}{}", &base[..host_end], loc);
    }
    let dir_end = base.rfind('/').filter(|&i| i >= host_end).map(|i| i + 1).unwrap_or(base.len());
    if dir_end == base.len() && !base.ends_with('/') {
        return format!("{}/{}", base, loc);
    }
    format!("{}{}", &base[..dir_end], loc)
}

fn backoff(attempt: u32) {
    std::thread::sleep(Duration::from_millis(
        if cfg!(test) { 10 } else { 1000 * (1u64 << attempt) },
    ));
}

fn with_retries<T>(mut f: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    let mut last = String::new();
    for a in 0..RETRIES {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                // permanent errors: don't retry
                if e.starts_with("refusing") || e.contains("HTTP 404") || e.contains("HTTP 403") || e.contains("HTTP 401") {
                    return Err(e);
                }
                last = e;
                if a + 1 < RETRIES {
                    backoff(a);
                }
            }
        }
    }
    Err(format!("{} (after {} attempts)", last, RETRIES))
}

pub fn get_text(url: &str) -> Result<String, String> {
    scheme_ok(url)?;
    with_retries(|| {
        let resp = request(url, 0)?;
        let mut buf = Vec::new();
        resp.into_reader()
            .take(MAX_TEXT_BYTES)
            .read_to_end(&mut buf)
            .map_err(|e| format!("read failed: {}", e))?;
        Ok(String::from_utf8_lossy(&buf).into_owned())
    })
}

/// POST application/x-www-form-urlencoded (https only, 3 retries, no redirects followed).
pub fn post_form(url: &str, fields: &[(&str, &str)]) -> Result<String, String> {
    scheme_ok(url)?;
    with_retries(|| {
        let resp = match agent().post(url).set("Accept", "*/*").send_form(fields) {
            Ok(r) => r,
            Err(ureq::Error::Status(c, _)) => return Err(format!("HTTP {} for {}", c, url)),
            Err(e) => return Err(format!("request failed for {}: {}", url, e)),
        };
        let mut buf = Vec::new();
        resp.into_reader()
            .take(MAX_TEXT_BYTES)
            .read_to_end(&mut buf)
            .map_err(|e| format!("read failed: {}", e))?;
        Ok(String::from_utf8_lossy(&buf).into_owned())
    })
}

pub fn get_json(url: &str) -> Result<serde_json::Value, String> {
    let t = get_text(url)?;
    serde_json::from_str(t.trim_start_matches('\u{feff}').trim())
        .map_err(|e| format!("invalid JSON from {}: {}", url, e))
}

pub fn download(url: &str, dest: &Path, expect_sha256: Option<&str>) -> Result<u64, String> {
    scheme_ok(url)?;
    if let Some(p) = dest.parent() {
        if !p.as_os_str().is_empty() {
            fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {}", p.display(), e))?;
        }
    }
    let mut part: PathBuf = dest.as_os_str().to_owned().into();
    let mut s = part.into_os_string();
    s.push(".part");
    part = PathBuf::from(s);
    let expect = expect_sha256.map(|s| s.trim().to_ascii_lowercase());
    if let Some(e) = &expect {
        if e.len() != 64 || !e.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("invalid expected sha256: {}", e));
        }
    }
    let mut verify_failed = false;
    let res = with_retries(|| fetch_part(url, &part));
    res?;
    let actual = sha256_file(&part)?;
    if let Some(e) = &expect {
        if &actual != e {
            verify_failed = true;
        }
    }
    if verify_failed {
        let _ = fs::remove_file(&part);
        return Err(format!("sha256 mismatch: expected {}, got {}", expect.unwrap(), actual));
    }
    let _ = fs::remove_file(dest);
    fs::rename(&part, dest).map_err(|e| format!("rename failed: {}", e))?;
    let n = fs::metadata(dest).map(|m| m.len()).map_err(|e| e.to_string())?;
    eprintln!("\r  downloaded {} ({} bytes, sha256 {})", dest.display(), n, actual);
    Ok(n)
}

/// Fetch into `part`, resuming if it exists. Complete when stream ends (or 416 on full file).
fn fetch_part(url: &str, part: &Path) -> Result<(), String> {
    let have = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    let resp = request(url, have)?;
    let status = resp.status();
    let (mut start, append) = match status {
        206 => (have, true),
        416 => {
            // Range not satisfiable: stale/oversized .part; discard and refetch from scratch.
            let _ = fs::remove_file(part);
            return Err("HTTP 416: stale partial file discarded, retrying".to_string());
        }
        _ => (0, false),
    };
    let total = if status == 206 {
        resp.header("Content-Range").and_then(|v| v.rsplit('/').next()).and_then(|v| v.parse::<u64>().ok())
    } else {
        resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok())
    };
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(part)
        .map_err(|e| format!("cannot open {}: {}", part.display(), e))?;
    let mut r = resp.into_reader();
    let mut buf = vec![0u8; 64 * 1024];
    let mut last_print = std::time::Instant::now();
    loop {
        let n = r.read(&mut buf).map_err(|e| format!("download interrupted: {}", e))?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n]).map_err(|e| format!("write failed: {}", e))?;
        start += n as u64;
        if start > MAX_DOWNLOAD_BYTES {
            return Err("refusing download larger than 4 GiB".to_string());
        }
        if last_print.elapsed() > Duration::from_millis(500) && !cfg!(test) {
            last_print = std::time::Instant::now();
            match total {
                Some(t) if t > 0 => eprint!("\r  {:>5.1}%  {} / {} KB", start as f64 * 100.0 / t as f64, start / 1024, t / 1024),
                _ => eprint!("\r  {} KB", start / 1024),
            }
        }
    }
    f.flush().map_err(|e| e.to_string())?;
    if let Some(t) = total {
        if start < t {
            return Err(format!("download truncated: {} of {} bytes", start, t));
        }
    }
    Ok(())
}

pub fn sha256_file(p: &Path) -> Result<String, String> {
    let mut f = fs::File::open(p).map_err(|e| format!("open {}: {}", p.display(), e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finish_hex())
}

// ---- pure Rust SHA-256 ----
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01,
    0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
    0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
    0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08,
    0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

pub struct Sha256 {
    h: [u32; 8],
    buf: Vec<u8>,
    len: u64,
}

impl Sha256 {
    pub fn new() -> Self {
        Sha256 {
            h: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19],
            buf: Vec::with_capacity(64),
            len: 0,
        }
    }
    pub fn update(&mut self, mut d: &[u8]) {
        self.len += d.len() as u64;
        if !self.buf.is_empty() {
            let need = 64 - self.buf.len();
            let take = need.min(d.len());
            self.buf.extend_from_slice(&d[..take]);
            d = &d[take..];
            if self.buf.len() == 64 {
                let b = std::mem::take(&mut self.buf);
                self.block(&b);
            }
        }
        while d.len() >= 64 {
            self.block(&d[..64]);
            d = &d[64..];
        }
        self.buf.extend_from_slice(d);
    }
    fn block(&mut self, b: &[u8]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = self.h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for i in 0..8 {
            self.h[i] = self.h[i].wrapping_add(v[i]);
        }
    }
    pub fn finish_hex(mut self) -> String {
        let bits = self.len * 8;
        let mut pad = vec![0x80u8];
        while (self.len as usize + pad.len()) % 64 != 56 {
            pad.push(0);
        }
        pad.extend_from_slice(&bits.to_be_bytes());
        let l = self.len;
        self.update(&pad);
        self.len = l;
        self.h.iter().map(|x| format!("{:08x}", x)).collect()
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    h.finish_hex()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    #[test]
    fn sha_vectors() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let m = vec![b'a'; 1_000_000];
        assert_eq!(sha256_hex(&m), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    }

    #[test]
    fn rejects_http() {
        assert!(get_text("http://example.com/").unwrap_err().contains("non-https"));
        assert!(get_json("http://example.com/").is_err());
        let d = std::env::temp_dir().join("df_http_rej.bin");
        assert!(download("http://example.com/x", &d, None).unwrap_err().contains("non-https"));
        assert!(get_text("ftp://example.com/").is_err());
    }

    #[test]
    fn loopback_strict() {
        for bad in [
            "http://127.0.0.1.evil.com/",
            "http://127.0.0.1:80@evil.com/",
            "http://127.0.0.1:/x",
            "http://127.0.0.1:8x/",
            "http://127.0.0.2:80/",
            "http://localhost:80/",
        ] {
            assert!(scheme_ok(bad).is_err(), "{}", bad);
        }
        assert!(scheme_ok("http://127.0.0.1:8080/f").is_ok());
    }

    #[test]
    fn host_allowlist() {
        for ok in [
            "https://drivers.amd.com/drivers/x.exe",
            "https://us.download.nvidia.com/nvapp/a.exe",
            "https://catalog.s.download.windowsupdate.com/d/x.cab",
            "https://www.amd.com:443/en/x",
            "https://gfwsl.geforce.com/services?a=1",
        ] {
            assert!(scheme_ok(ok).is_ok(), "{}", ok);
        }
        for bad in [
            "https://drivers.amd.com@evil.com/x",
            "https://evil.com/drivers.amd.com",
            "https://drivers.amd.com.evil.com/x",
            "https://evildrivers.amd.com.attacker.io/",
            "https://notamd.com/",
            "https://evil.com#@drivers.amd.com/",
            "https://drivers.amd.com:evil/x",
        ] {
            assert!(scheme_ok(bad).unwrap_err().contains("allowlist"), "{}", bad);
        }
    }

    #[test]
    fn join() {
        assert_eq!(join_url("https://a.com/x", "//b.com/q"), "https://b.com/q");
        assert_eq!(join_url("https://a.com/x/y", "/z"), "https://a.com/z");
        assert_eq!(join_url("https://a.com/x/y", "z"), "https://a.com/x/z");
        assert_eq!(join_url("https://a.com", "z"), "https://a.com/z");
        assert_eq!(join_url("https://a.com/x", "http://b.com/q"), "http://b.com/q");
    }

    /// Minimal mock: serves `body` with Range support; fails the first `fail` connections mid-way.
    fn mock(body: Vec<u8>, fail: usize, redirect_to_http: bool) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut n = 0;
            for s in l.incoming() {
                let mut s = match s { Ok(s) => s, Err(_) => break };
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut range = 0usize;
                let mut line = String::new();
                loop {
                    line.clear();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" { break; }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        range = v.trim().trim_end_matches('-').parse().unwrap_or(0);
                    }
                }
                if redirect_to_http {
                    let _ = s.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://evil.example/x\r\nContent-Length: 0\r\n\r\n");
                    continue;
                }
                let part = &body[range..];
                let head = if range > 0 {
                    format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\n\r\n", range, body.len() - 1, body.len(), part.len())
                } else {
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", part.len())
                };
                let _ = s.write_all(head.as_bytes());
                if n < fail {
                    let _ = s.write_all(&part[..part.len() / 2]);
                } else {
                    let _ = s.write_all(part);
                }
                n += 1;
            }
        });
        format!("http://127.0.0.1:{}/f.bin", port)
    }

    #[test]
    fn download_resume_and_hash() {
        let body: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let want = sha256_hex(&body);
        let url = mock(body.clone(), 1, false);
        let dir = std::env::temp_dir().join("df_http_t1");
        let _ = fs::remove_dir_all(&dir);
        let dest = dir.join("f.bin");
        let n = download(&url, &dest, Some(&want)).unwrap();
        assert_eq!(n, body.len() as u64);
        assert_eq!(fs::read(&dest).unwrap(), body);
        assert!(!dir.join("f.bin.part").exists());
    }

    #[test]
    fn download_bad_hash() {
        let url = mock(b"hello".to_vec(), 0, false);
        let dest = std::env::temp_dir().join("df_http_t2").join("h.bin");
        let e = download(&url, &dest, Some(&"0".repeat(64))).unwrap_err();
        assert!(e.contains("sha256 mismatch"), "{}", e);
        assert!(!dest.exists());
    }

    #[test]
    fn redirect_to_http_rejected() {
        let url = mock(vec![], 0, true);
        let e = get_text(&url).unwrap_err();
        assert!(e.contains("non-https"), "{}", e);
    }

    #[test]
    fn mock_get_json() {
        let url = mock(br#"{"Success":"1"}"#.to_vec(), 0, false);
        assert_eq!(get_json(&url).unwrap()["Success"], "1");
    }

    #[test]
    #[ignore = "network"]
    fn live_amd_and_nvidia() {
        let t = get_text("https://www.amd.com/en/support/downloads/drivers.html/chipsets/am5/x670.html").unwrap();
        let re = regex::Regex::new(r#"https://drivers\.amd\.com[^"]*\.exe"#).unwrap();
        let m = re.find(&t).expect("amd exe link");
        println!("AMD: {}", m.as_str());
        let j = get_json("https://gfwsl.geforce.com/services_toolkit/services/com/nvidia/services/AjaxDriverService.php?func=DriverManualLookup&psid=131&pfid=1043&osID=135&languageCode=1033&beta=0&isWHQL=0&dltype=-1&dch=1&upCRD=0&qnf=0&ctk=0&sort1=1&numberOfResults=1").unwrap();
        println!("NV: {}", j);
        assert_eq!(j["Success"].to_string().trim_matches('"'), "1");
        let d = std::env::temp_dir().join("df_http_live").join("robots.txt");
        let n = download("https://www.amd.com/robots.txt", &d, None).unwrap();
        let h = sha256_file(&d).unwrap();
        let n2 = download("https://www.amd.com/robots.txt", &d, Some(&h)).unwrap();
        assert_eq!(n, n2);
    }
}
