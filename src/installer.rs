//! Silent installer runner: download, verify (sha256 + Authenticode), run, map exit codes.
//! Real execution only happens when `dry_run == false`.

use crate::types::{Candidate, InstallKind, Outcome};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const INSTALL_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const SIGNER_ALLOWLIST: [&str; 9] = [
    "NVIDIA Corporation",
    "NVIDIA",
    "Advanced Micro Devices, Inc.",
    "Advanced Micro Devices",
    "Microsoft Corporation",
    "Microsoft Windows",
    "Microsoft Windows Publisher",
    "Microsoft Windows Hardware Compatibility Publisher",
    ".NET", // CN of the Microsoft-issued .NET runtime installer signing cert
];

// ---------------------------------------------------------------- pure logic

/// Map a process exit code to an install outcome.
pub fn map_exit_code(code: i32) -> Outcome {
    match code {
        0 => Outcome::Installed,
        3010 | 1641 => Outcome::InstalledRebootNeeded,
        // pnputil: driver package staged but no device took it right now; the verification pass decides.
        259 => Outcome::Installed,
        c => Outcome::Failed(format!("installer exited with code {c}")),
    }
}

/// True if the signer subject (simple display name) is on the allowlist.
pub fn signer_allowed(signer: &str) -> bool {
    let s = signer.trim();
    SIGNER_ALLOWLIST.iter().any(|a| a.eq_ignore_ascii_case(s))
}

/// Reduce an arbitrary string to a safe single path component.
pub fn safe_component(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let out = out.trim_matches('.').to_string();
    if out.is_empty() { "file".into() } else { out }
}

/// File name taken from the URL path (query/fragment stripped), sanitized.
pub fn file_name_from_url(url: &str) -> String {
    let no_frag = url.split('#').next().unwrap_or("");
    let no_q = no_frag.split('?').next().unwrap_or("");
    let last = no_q.rsplit('/').next().unwrap_or("");
    safe_component(last)
}

/// Download directory for a vendor under a temp root.
pub fn work_dir(root: &Path, vendor: &str) -> PathBuf {
    root.join("DriverForge").join(safe_component(vendor))
}

/// Program + args to execute for a downloaded file. `extracted` is the folder
/// that holds the unpacked package for ZipThenSetup.
pub fn build_command(c: &Candidate, file: &Path, extracted: Option<&Path>) -> Result<(PathBuf, Vec<String>), String> {
    match c.kind {
        InstallKind::ExeSilent => Ok((file.to_path_buf(), c.silent_args.clone())),
        InstallKind::Msi => {
            let mut a = vec!["/i".to_string(), file.display().to_string()];
            if c.silent_args.is_empty() {
                a.push("/qn".into());
                a.push("/norestart".into());
            } else {
                a.extend(c.silent_args.iter().cloned());
            }
            Ok((sys32("msiexec.exe"), a))
        }
        InstallKind::ZipThenSetup => {
            let dir = extracted.ok_or("no extraction directory")?;
            let setup = find_setup(dir).ok_or("no setup.exe found in archive")?;
            Ok((setup, c.silent_args.clone()))
        }
        InstallKind::WindowsUpdate => Err("WindowsUpdate candidates are installed via wupdater".into()),
    }
}

/// pnputil arguments to install every INF below `dir`.
pub fn pnputil_args(dir: &Path) -> Vec<String> {
    let inf = dir.join("*.inf");
    vec!["/add-driver".into(), inf.display().to_string(), "/subdirs".into(), "/install".into()]
}

fn find_setup(dir: &Path) -> Option<PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    let mut found: Option<PathBuf> = None;
    while let Some(d) = stack.pop() {
        let rd = std::fs::read_dir(&d).ok()?;
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().map(|n| n.to_string_lossy().eq_ignore_ascii_case("setup.exe")).unwrap_or(false) {
                // prefer the shallowest
                if found.as_ref().map(|f| p.components().count() < f.components().count()).unwrap_or(true) {
                    found = Some(p);
                }
            }
        }
    }
    found
}

fn has_inf(dir: &Path) -> bool {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().map(|x| x.eq_ignore_ascii_case("inf")).unwrap_or(false) {
                    return true;
                }
            }
        }
    }
    false
}

/// Absolute path of a System32 tool.
pub fn sys32(name: &str) -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    PathBuf::from(root).join("System32").join(name)
}

fn is_cab(p: &Path) -> bool {
    use std::io::Read;
    let mut b = [0u8; 4];
    std::fs::File::open(p).and_then(|mut f| f.read_exact(&mut b)).is_ok() && &b == b"MSCF"
}

/// Extract a ZIP (pure Rust) or CAB (expand.exe) into `dest`.
fn extract_archive(file: &Path, dest: &Path, timeout: Duration) -> Result<usize, String> {
    if !is_cab(file) {
        return extract_zip(file, dest);
    }
    let _ = std::fs::remove_dir_all(dest);
    std::fs::create_dir_all(dest).map_err(|e| format!("mkdir: {e}"))?;
    let args = vec![file.display().to_string(), "-F:*".into(), dest.display().to_string()];
    match run_with_timeout(&sys32("expand.exe"), &args, timeout)? {
        0 => Ok(count_files(dest)),
        c => Err(format!("expand.exe exit {c}")),
    }
}

fn count_files(dir: &Path) -> usize {
    let mut n = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() { stack.push(p) } else { n += 1 }
            }
        }
    }
    n
}

/// Every .cat in a driver package must be validly signed by an allowlisted signer.
fn verify_catalogs(dir: &Path) -> Result<(), String> {
    let mut stack = vec![dir.to_path_buf()];
    let mut cats = 0;
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).map_err(|e| format!("read_dir: {e}"))?.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().map(|x| x.eq_ignore_ascii_case("cat")).unwrap_or(false) {
                verify_signed_allowed(&p)?;
                cats += 1;
            }
        }
    }
    if cats == 0 { Err("driver package has no signed .cat file".into()) } else { Ok(()) }
}

/// Every extracted file must resolve (symlinks/junctions/.. included) to a path under `dest`.
fn check_contained(dest: &Path) -> Result<(), String> {
    let base = dest.canonicalize().map_err(|e| format!("canonicalize: {e}"))?;
    let mut stack = vec![dest.to_path_buf()];
    let mut count = 0usize;
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).map_err(|e| format!("read_dir: {e}"))?.flatten() {
            let p = e.path();
            let real = p.canonicalize().map_err(|e| format!("canonicalize: {e}"))?;
            if !real.starts_with(&base) {
                return Err(format!("extracted path escapes work folder: {}", p.display()));
            }
            count += 1;
            if count > 20_000 {
                return Err("archive has too many files".into());
            }
            if p.is_dir() {
                stack.push(p);
            }
        }
    }
    Ok(())
}

/// Pure-Rust zip extraction with zip-slip protection. Returns number of files.
pub fn extract_zip(zip_path: &Path, dest: &Path) -> Result<usize, String> {
    let f = std::fs::File::open(zip_path).map_err(|e| format!("open zip: {e}"))?;
    let mut ar = zip::ZipArchive::new(f).map_err(|e| format!("bad zip: {e}"))?;
    std::fs::create_dir_all(dest).map_err(|e| format!("mkdir: {e}"))?;
    let mut n = 0;
    for i in 0..ar.len() {
        let mut ent = ar.by_index(i).map_err(|e| format!("zip entry: {e}"))?;
        let rel = match ent.enclosed_name() {
            Some(p) => p,
            None => return Err(format!("unsafe path in archive: {}", ent.name())),
        };
        let out = dest.join(rel);
        if ent.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| format!("mkdir: {e}"))?;
        } else {
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p).map_err(|e| format!("mkdir: {e}"))?;
            }
            let mut w = std::fs::File::create(&out).map_err(|e| format!("create: {e}"))?;
            std::io::copy(&mut ent, &mut w).map_err(|e| format!("extract: {e}"))?;
            n += 1;
        }
    }
    Ok(n)
}

/// Run a program with a timeout; returns exit code.
pub fn run_with_timeout(prog: &Path, args: &[String], timeout: Duration) -> Result<i32, String> {
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", prog.display()))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return Ok(st.code().unwrap_or(-1)),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = std::process::Command::new(sys32("taskkill.exe")).args(["/F", "/T", "/PID", &child.id().to_string()]).output();
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("timed out after {}s", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(format!("wait failed: {e}")),
        }
    }
}

// ------------------------------------------------------------------ public

pub fn install(c: &Candidate, dry_run: bool) -> Outcome {
    if dry_run {
        return Outcome::DryRun;
    }
    if let InstallKind::WindowsUpdate = c.kind {
        return Outcome::Skipped("handled by Windows Update sweep".into());
    }
    let dir = match secure_run_dir(&c.vendor) {
        Ok(d) => d,
        Err(e) => return Outcome::Failed(format!("cannot create protected work folder: {e}")),
    };
    let out = install_in(c, &dir, INSTALL_TIMEOUT, true);
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// Create a brand-new, unpredictably named folder under ProgramData with a protected
/// DACL (SYSTEM + Administrators only). Creation fails if the name already exists, and
/// never falls back to a user-writable location.
fn secure_run_dir(vendor: &str) -> Result<PathBuf, String> {
    let pd = std::env::var_os("ProgramData").ok_or("ProgramData is not set")?;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = PathBuf::from(pd).join(format!("DriverForge-{}-{nanos}", std::process::id())).join(safe_component(vendor));
    let root = dir.parent().ok_or("bad path")?.to_path_buf();
    win::create_protected_dir(&root)?;
    std::fs::create_dir(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    Ok(dir)
}

/// Core install flow in an existing, trusted work directory `dir`.
fn install_in(c: &Candidate, dir: &Path, timeout: Duration, enforce_sig: bool) -> Outcome {
    if let InstallKind::WindowsUpdate = c.kind {
        return Outcome::Skipped("handled by Windows Update sweep".into());
    }
    if !c.url.to_ascii_lowercase().starts_with("https://") {
        return Outcome::Failed("refusing non-HTTPS download url".into());
    }
    let file = dir.join(file_name_from_url(&c.url));
    crate::log::info(&format!("downloading {} {} -> {}", c.vendor, c.component, file.display()));
    if let Err(e) = crate::http::download(&c.url, &file, c.sha256.as_deref()) {
        return Outcome::Failed(format!("download failed: {e}"));
    }
    run_downloaded(c, &file, dir, timeout, enforce_sig)
}

/// Verify and execute an already-downloaded file.
fn run_downloaded(c: &Candidate, file: &Path, dir: &Path, timeout: Duration, enforce_sig: bool) -> Outcome {
    if let Some(r) = amd_hook(c, file, timeout, enforce_sig) {
        return match r {
            Ok(code) => match map_exit_code(code) {
                Outcome::Installed if c.needs_reboot_hint => Outcome::InstalledRebootNeeded,
                o => o,
            },
            Err(e) => Outcome::Failed(e),
        };
    }
    let result = match c.kind {
        InstallKind::ZipThenSetup => {
            let ex = dir.join("extracted");
            if let Err(e) = extract_archive(file, &ex, timeout) {
                return Outcome::Failed(format!("extract failed: {e}"));
            }
            if let Err(e) = check_contained(&ex) {
                return Outcome::Failed(e);
            }
            if find_setup(&ex).is_some() {
                // Sibling payload files are not individually verified, so the archive hash is mandatory.
                if enforce_sig && c.sha256.is_none() {
                    return Outcome::Failed("archive with setup.exe requires a sha256".into());
                }
                let (prog, args) = match build_command(c, file, Some(&ex)) {
                    Ok(x) => x,
                    Err(e) => return Outcome::Failed(e),
                };
                if enforce_sig {
                    if let Err(e) = verify_signed_allowed(&prog) {
                        return Outcome::Failed(e);
                    }
                }
                run_with_timeout(&prog, &args, timeout)
            } else if has_inf(&ex) {
                // Driver package without setup: INFs go through pnputil (inbox, signed by OS policy).
                if let Err(e) = verify_catalogs(&ex) {
                    return Outcome::Failed(e);
                }
                run_with_timeout(&sys32("pnputil.exe"), &pnputil_args(&ex), timeout)
            } else {
                return Outcome::Failed("archive has neither setup.exe nor .inf files".into());
            }
        }
        _ => {
            if matches!(c.kind, InstallKind::Msi | InstallKind::ExeSilent) && enforce_sig {
                if let Err(e) = verify_signed_allowed(file) {
                    return Outcome::Failed(e);
                }
            }
            let (prog, args) = match build_command(c, file, None) {
                Ok(x) => x,
                Err(e) => return Outcome::Failed(e),
            };
            run_with_timeout(&prog, &args, timeout)
        }
    };
    match result {
        Ok(code) => match map_exit_code(code) {
            Outcome::Installed if c.needs_reboot_hint => Outcome::InstalledRebootNeeded,
            o => o,
        },
        Err(e) => Outcome::Failed(e),
    }
}

/// AMD Adrenalin "whql-amd-software-adrenalin-edition-*.exe" is a self-extractor that unpacks to
/// C:\AMD\AMD-Software-Installer and then opens its interactive GUI; it has no silent switch.
/// The documented unattended route is the extracted `Bin64\ATISetup.exe -INSTALL`. So: let the
/// package extract, close the GUI it opens, verify the signature of ATISetup.exe, run it silently.
fn amd_hook(c: &Candidate, file: &Path, timeout: Duration, enforce_sig: bool) -> Option<Result<i32, String>> {
    let name = file.file_name()?.to_string_lossy().to_ascii_lowercase();
    if !(c.vendor == "AMD" && name.contains("amd-software") && name.ends_with(".exe")) {
        return None;
    }
    Some(amd_adrenalin_install(file, timeout, enforce_sig))
}

fn gui_running() -> bool {
    std::process::Command::new(sys32("tasklist.exe"))
        .args(["/FI", "IMAGENAME eq AMDSoftwareInstaller.exe", "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("AMDSoftwareInstaller.exe"))
        .unwrap_or(false)
}

fn kill_amd_gui() {
    for img in ["AMDSoftwareInstaller.exe", "InstallManagerApp.exe", "AMDSplashScreen.exe"] {
        let _ = std::process::Command::new(sys32("taskkill.exe")).args(["/F", "/IM", img]).output();
    }
}

fn amd_adrenalin_install(file: &Path, timeout: Duration, enforce_sig: bool) -> Result<i32, String> {
    if enforce_sig {
        verify_signed_allowed(file)?;
    }
    let root = PathBuf::from(r"C:\AMD\AMD-Software-Installer");
    let _ = std::fs::remove_dir_all(&root);
    let mut sfx = std::process::Command::new(file).spawn().map_err(|e| format!("cannot start AMD package: {e}"))?;
    let setup = root.join("Bin64").join("ATISetup.exe");
    let start = std::time::Instant::now();
    loop {
        if gui_running() {
            break; // extraction is finished once the package launches its GUI
        }
        if let Ok(Some(_)) = sfx.try_wait() {
            if setup.exists() {
                break;
            }
        }
        if start.elapsed() > Duration::from_secs(20 * 60) {
            let _ = sfx.kill();
            return Err("AMD package did not finish extracting".into());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    std::thread::sleep(Duration::from_secs(2));
    kill_amd_gui();
    let _ = sfx.kill();
    if !setup.exists() {
        return Err("AMD package extracted without Bin64\\ATISetup.exe".into());
    }
    if enforce_sig {
        verify_signed_allowed(&setup)?;
    }
    // Full install first (driver + Adrenalin app); if AMD's installer reports a failure, retry
    // drivers only. ATISetup returns 67 when ANY package fails (even its own "Install Manager"
    // helper), so the per-package report decides: success when every driver package succeeded.
    let mut args: Vec<String> = ["-INSTALL", "-OUTPUT", "SCREEN"].iter().map(|x| x.to_string()).collect();
    let mut r = amd_run(&setup, &args, timeout, dir_for_out(&root));
    if !matches!(r, Ok(0) | Ok(3010)) {
        args.insert(1, "-DRIVERONLY".to_string());
        r = amd_run(&setup, &args, timeout, dir_for_out(&root));
    }
    let _ = std::fs::remove_dir_all(&root);
    r
}

fn dir_for_out(root: &Path) -> PathBuf {
    root.join("atisetup-output.txt")
}

/// Pure: decide from ATISetup's screen output. Ok(0) when at least one package installed and the only
/// failures are non-driver helpers; Err describes the failed packages.
pub fn judge_atisetup(exit: i32, out: &str) -> Result<i32, String> {
    if exit == 0 || exit == 3010 {
        return Ok(exit);
    }
    let mut ok = 0;
    let mut failed: Vec<String> = Vec::new();
    for chunk in out.split("Package Name =").skip(1) {
        let name = chunk.lines().next().unwrap_or("").trim().to_string();
        let code = chunk
            .split("ErrorCode =")
            .nth(1)
            .and_then(|x| x.split_whitespace().next())
            .and_then(|x| x.parse::<i64>().ok())
            .unwrap_or(-1);
        if code == 0 { ok += 1 } else { failed.push(format!("{name} (error {code})")) }
    }
    let only_helpers = failed.iter().all(|f| f.starts_with("AMD Install Manager") || f.starts_with("AMD Software Installer"));
    if ok > 0 && only_helpers {
        crate::log::warn(&format!("AMD installer: helper package(s) failed but all driver packages installed: {}", failed.join(", ")));
        Ok(0)
    } else if failed.is_empty() {
        Ok(exit)
    } else {
        Err(format!("AMD installer failed: {}", failed.join(", ")))
    }
}

fn amd_run(setup: &Path, args: &[String], timeout: Duration, out_file: PathBuf) -> Result<i32, String> {
    let f = std::fs::File::create(&out_file).map_err(|e| format!("cannot create {}: {e}", out_file.display()))?;
    let f2 = f.try_clone().map_err(|e| e.to_string())?;
    let mut child = std::process::Command::new(setup)
        .args(args)
        .stdout(f)
        .stderr(f2)
        .spawn()
        .map_err(|e| format!("cannot start ATISetup: {e}"))?;
    let start = std::time::Instant::now();
    let code = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.code().unwrap_or(-1),
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = std::process::Command::new(sys32("taskkill.exe")).args(["/F", "/T", "/PID", &child.id().to_string()]).output();
                    let _ = child.kill();
                    return Err("AMD installer timed out".into());
                }
                std::thread::sleep(Duration::from_secs(2));
            }
            Err(e) => return Err(format!("wait failed: {e}")),
        }
    };
    let text = String::from_utf8_lossy(&std::fs::read(&out_file).unwrap_or_default()).to_string();
    judge_atisetup(code, &text)
}

fn verify_signed_allowed(path: &Path) -> Result<(), String> {
    let signer = win::verify_authenticode(path)?;
    if signer_allowed(&signer) {
        Ok(())
    } else {
        Err(format!("signer '{signer}' is not on the allowlist"))
    }
}

pub fn create_restore_point() -> Result<(), String> {
    win::create_restore_point()
}

pub fn is_admin() -> bool {
    win::is_admin()
}

// ------------------------------------------------------------------ windows

mod win {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
    use windows::Win32::Security::Cryptography::*;
    use windows::Win32::Security::WinTrust::*;
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::System::Restore::*;
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
    }

    /// Create `dir` (must not exist) with a protected DACL: SYSTEM + Administrators only.
    pub fn create_protected_dir(dir: &Path) -> Result<(), String> {
        use windows::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
        use windows::Win32::Storage::FileSystem::CreateDirectoryW;
        use windows::Win32::Foundation::{LocalFree, HLOCAL};
        let sddl: Vec<u16> = "D:P(A;OICI;GA;;;SY)(A;OICI;GA;;;BA)\0".encode_utf16().collect();
        let w = wide(dir);
        unsafe {
            let mut sd = PSECURITY_DESCRIPTOR::default();
            ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(sddl.as_ptr()), SDDL_REVISION_1, &mut sd, None)
                .map_err(|e| format!("SDDL: {e}"))?;
            let sa = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd.0,
                bInheritHandle: false.into(),
            };
            let r = CreateDirectoryW(PCWSTR(w.as_ptr()), Some(&sa));
            let _ = LocalFree(Some(HLOCAL(sd.0)));
            r.map_err(|e| format!("CreateDirectoryW {}: {e}", dir.display()))
        }
    }

    pub fn is_admin() -> bool {
        unsafe {
            let mut tok = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok).is_err() {
                return false;
            }
            let mut el = TOKEN_ELEVATION::default();
            let mut ret = 0u32;
            let ok = GetTokenInformation(
                tok,
                TokenElevation,
                Some(&mut el as *mut _ as *mut _),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut ret,
            )
            .is_ok();
            let _ = CloseHandle(tok);
            ok && el.TokenIsElevated != 0
        }
    }

    pub fn create_restore_point() -> Result<(), String> {
        unsafe {
            let mut info = RESTOREPOINTINFOW::default();
            info.dwEventType = BEGIN_SYSTEM_CHANGE;
            info.dwRestorePtType = APPLICATION_INSTALL;
            let desc: Vec<u16> = "DriverForge driver update".encode_utf16().collect();
            for (i, ch) in desc.iter().enumerate().take(255) {
                info.szDescription[i] = *ch;
            }
            let mut st = STATEMGRSTATUS::default();
            if !SRSetRestorePointW(&info, &mut st).as_bool() {
                let code = { st.nStatus.0 };
                return Err(format!("SRSetRestorePoint failed (status {code})"));
            }
            let end = RESTOREPOINTINFOW {
                dwEventType: END_SYSTEM_CHANGE,
                dwRestorePtType: APPLICATION_INSTALL,
                llSequenceNumber: st.llSequenceNumber,
                ..Default::default()
            };
            let mut st2 = STATEMGRSTATUS::default();
            let _ = SRSetRestorePointW(&end, &mut st2);
            Ok(())
        }
    }

    /// Verify with WinVerifyTrust, return the signer's simple display name.
    pub fn verify_authenticode(path: &Path) -> Result<String, String> {
        let w = wide(path);
        unsafe {
            let mut fi = WINTRUST_FILE_INFO {
                cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
                pcwszFilePath: PCWSTR(w.as_ptr()),
                ..Default::default()
            };
            let mut data = WINTRUST_DATA {
                cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
                dwUIChoice: WTD_UI_NONE,
                fdwRevocationChecks: WTD_REVOKE_NONE,
                dwUnionChoice: WTD_CHOICE_FILE,
                dwStateAction: WTD_STATEACTION_VERIFY,
                ..Default::default()
            };
            data.Anonymous.pFile = &mut fi;
            let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
            let rc = WinVerifyTrust(HWND(std::ptr::null_mut()), &mut action, &mut data as *mut _ as *mut _);
            data.dwStateAction = WTD_STATEACTION_CLOSE;
            let _ = WinVerifyTrust(HWND(std::ptr::null_mut()), &mut action, &mut data as *mut _ as *mut _);
            if rc != 0 {
                return Err(format!("signature invalid or missing (WinVerifyTrust 0x{:08X})", rc as u32));
            }
        }
        signer_name(&w)
    }

    fn signer_name(w: &[u16]) -> Result<String, String> {
        unsafe {
            let mut store = HCERTSTORE::default();
            let mut msg: *mut core::ffi::c_void = std::ptr::null_mut();
            CryptQueryObject(
                CERT_QUERY_OBJECT_FILE,
                w.as_ptr() as *const _,
                CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED,
                CERT_QUERY_FORMAT_FLAG_BINARY,
                0,
                None,
                None,
                None,
                Some(&mut store),
                Some(&mut msg),
                None,
            )
            .map_err(|e| format!("CryptQueryObject: {e}"))?;
            let res = (|| {
                let mut sz = 0u32;
                CryptMsgGetParam(msg, CMSG_SIGNER_INFO_PARAM, 0, None, &mut sz)
                    .map_err(|e| format!("CryptMsgGetParam size: {e}"))?;
                let mut buf = vec![0u8; sz as usize];
                CryptMsgGetParam(msg, CMSG_SIGNER_INFO_PARAM, 0, Some(buf.as_mut_ptr() as *mut _), &mut sz)
                    .map_err(|e| format!("CryptMsgGetParam: {e}"))?;
                let si = &*(buf.as_ptr() as *const CMSG_SIGNER_INFO);
                let ci = CERT_INFO { Issuer: si.Issuer, SerialNumber: si.SerialNumber, ..Default::default() };
                let ctx = CertFindCertificateInStore(
                    store,
                    X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
                    0,
                    CERT_FIND_SUBJECT_CERT,
                    Some(&ci as *const _ as *const _),
                    None,
                );
                if ctx.is_null() {
                    return Err("signer certificate not found".to_string());
                }
                let n = CertGetNameStringW(ctx, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, None, None);
                let mut name = vec![0u16; n as usize];
                CertGetNameStringW(ctx, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, None, Some(&mut name));
                let _ = CertFreeCertificateContext(Some(ctx));
                let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
                Ok(String::from_utf16_lossy(&name[..end]))
            })();
            let _ = CertCloseStore(Some(store), 0);
            if !msg.is_null() {
                let _ = CryptMsgClose(Some(msg));
            }
            res
        }
    }

    #[allow(dead_code)]
    fn _unused(_: PWSTR) {}
}

// -------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    #[test]
    fn atisetup_helper_failure_is_not_fatal() {
        let out = "Package Name =AMD Display Driver\n Version =1\n ErrorCode =0\nPackage Name =AMD Install Manager\n ErrorCode =643\n";
        assert_eq!(judge_atisetup(67, out), Ok(0));
        let bad = "Package Name =AMD Display Driver\n ErrorCode =1603\nPackage Name =AMD Install Manager\n ErrorCode =643\n";
        assert!(judge_atisetup(67, bad).is_err());
        assert_eq!(judge_atisetup(0, ""), Ok(0));
        assert_eq!(judge_atisetup(3010, ""), Ok(3010));
    }

    use super::*;

    fn cand(kind: InstallKind, args: &[&str]) -> Candidate {
        Candidate {
            vendor: "NVIDIA".into(),
            component: "GPU".into(),
            installed: None,
            latest: "1".into(),
            url: "https://x.example/a/setup.exe?x=1".into(),
            sha256: None,
            size_bytes: None,
            kind,
            silent_args: args.iter().map(|s| s.to_string()).collect(),
            needs_reboot_hint: false,
        }
    }

    #[test]
    fn exit_codes() {
        assert_eq!(map_exit_code(0), Outcome::Installed);
        assert_eq!(map_exit_code(3010), Outcome::InstalledRebootNeeded);
        assert!(matches!(map_exit_code(1), Outcome::Failed(_)));
        assert!(matches!(map_exit_code(-1), Outcome::Failed(_)));
    }

    #[test]
    fn signer_list() {
        assert!(signer_allowed("NVIDIA Corporation"));
        assert!(signer_allowed("Advanced Micro Devices, Inc."));
        assert!(signer_allowed("Microsoft Corporation"));
        assert!(signer_allowed(".NET"));
        assert!(signer_allowed("Microsoft Windows Hardware Compatibility Publisher"));
        assert!(!signer_allowed("Evil Corp"));
        assert!(!signer_allowed("Microsoft Evil Ltd"));
        assert!(!signer_allowed("NVIDIA Fake Inc"));
        assert!(!signer_allowed("Not Microsoft Corporation"));
        assert!(!signer_allowed(""));
    }

    #[test]
    fn names_safe() {
        assert_eq!(file_name_from_url("https://a/b/c.exe?x=1#f"), "c.exe");
        assert!(!safe_component("../../evil").contains('/'));
        assert!(!safe_component("..\\x:y").contains('\\'));
        assert_eq!(safe_component(".."), "file");
        let d = work_dir(Path::new("C:/T"), "AMD/../x");
        assert!(d.starts_with("C:/T/DriverForge"));
    }

    #[test]
    fn arg_building() {
        let f = Path::new("C:/t/a.exe");
        let (p, a) = build_command(&cand(InstallKind::ExeSilent, &["-s", "-noreboot"]), f, None).unwrap();
        assert_eq!(p, f);
        assert_eq!(a, vec!["-s", "-noreboot"]);
        let (p, a) = build_command(&cand(InstallKind::Msi, &[]), Path::new("C:/t/a.msi"), None).unwrap();
        assert_eq!(p, sys32("msiexec.exe"));
        assert_eq!(a, vec!["/i", "C:/t/a.msi", "/qn", "/norestart"]);
        assert!(build_command(&cand(InstallKind::ZipThenSetup, &[]), f, None).is_err());
        assert!(build_command(&cand(InstallKind::WindowsUpdate, &[]), f, None).is_err());
        let pa = pnputil_args(Path::new("C:/x"));
        assert_eq!(pa[0], "/add-driver");
        assert_eq!(&pa[2..], ["/subdirs", "/install"]);
    }

    #[test]
    fn dry_run_touches_nothing() {
        let mut c = cand(InstallKind::ExeSilent, &[]);
        c.vendor = "DryRunProbeVendor".into();
        assert_eq!(install(&c, true), Outcome::DryRun);
        assert!(!std::env::temp_dir().join("DriverForge").join("DryRunProbeVendor").exists());
    }

    #[test]
    fn rejects_http_and_wu() {
        let mut c = cand(InstallKind::ExeSilent, &[]);
        c.url = "http://x/a.exe".into();
        assert!(matches!(install_in(&c, &std::env::temp_dir(), INSTALL_TIMEOUT, true), Outcome::Failed(_)));
        let w = cand(InstallKind::WindowsUpdate, &[]);
        assert!(matches!(install_in(&w, &std::env::temp_dir(), INSTALL_TIMEOUT, true), Outcome::Skipped(_)));
    }

    #[test]
    fn unsigned_fake_installer_refused() {
        let dir = std::env::temp_dir().join("DriverForgeInstTest_unsigned");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("fake.exe");
        std::fs::write(&f, b"MZ not a real exe").unwrap();
        let c = cand(InstallKind::ExeSilent, &[]);
        let o = run_downloaded(&c, &f, &dir, Duration::from_secs(5), true);
        assert!(matches!(o, Outcome::Failed(ref m) if m.contains("signature")), "{o:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn signed_microsoft_exe_verifies() {
        let p = Path::new("C:/Windows/System32/whoami.exe");
        if !p.exists() {
            return;
        }
        let s = win::verify_authenticode(p);
        // Inbox binaries are catalog-signed on some builds; accept either, but never a bad signer.
        if let Ok(name) = s {
            assert!(signer_allowed(&name), "{name}");
        }
    }

    #[test]
    fn timeout_kills_and_exit_code_runs() {
        let cmd = Path::new("C:/Windows/System32/cmd.exe");
        if !cmd.exists() {
            return;
        }
        let bat = std::env::temp_dir().join("DriverForgeInstTest_exit.cmd");
        std::fs::write(&bat, "@exit /b 3010\r\n").unwrap();
        let code = run_with_timeout(&bat, &[], Duration::from_secs(10)).unwrap();
        let _ = std::fs::remove_file(&bat);
        assert_eq!(code, 3010);
        let slow = std::env::temp_dir().join("DriverForgeInstTest_slow.cmd");
        std::fs::write(&slow, "@ping -n 20 127.0.0.1 >nul\r\n").unwrap();
        let r = run_with_timeout(&slow, &[], Duration::from_millis(500));
        let _ = std::fs::remove_file(&slow);
        assert!(r.unwrap_err().contains("timed out"));
    }

    #[test]
    fn zip_extract_and_slip_guard() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("DriverForgeInstTest_zip");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let zp = dir.join("a.zip");
        {
            let mut w = zip::ZipWriter::new(std::fs::File::create(&zp).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            w.start_file("sub/setup.exe", o).unwrap();
            w.write_all(b"x").unwrap();
            w.finish().unwrap();
        }
        let out = dir.join("out");
        assert_eq!(extract_zip(&zp, &out).unwrap(), 1);
        assert!(find_setup(&out).is_some());
        let bad = dir.join("bad.zip");
        {
            let mut w = zip::ZipWriter::new(std::fs::File::create(&bad).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            w.start_file("../evil.txt", o).unwrap();
            w.write_all(b"x").unwrap();
            w.finish().unwrap();
        }
        assert!(extract_zip(&bad, &dir.join("out2")).is_err());
        assert!(!dir.join("evil.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn protected_dir_created_once_and_refuses_existing() {
        let d = std::env::temp_dir().join(format!("DriverForgeInstTest_prot_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        win::create_protected_dir(&d).unwrap();
        assert!(d.is_dir());
        assert!(win::create_protected_dir(&d).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn setup_zip_without_sha_is_refused() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("DriverForgeInstTest_nosha_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let zp = dir.join("a.zip");
        let mut w = zip::ZipWriter::new(std::fs::File::create(&zp).unwrap());
        w.start_file("setup.exe", zip::write::SimpleFileOptions::default()).unwrap();
        w.write_all(b"x").unwrap();
        w.finish().unwrap();
        let c = cand(InstallKind::ZipThenSetup, &[]);
        let o = run_downloaded(&c, &zp, &dir, Duration::from_secs(5), true);
        assert!(matches!(o, Outcome::Failed(ref m) if m.contains("sha256")), "{o:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn admin_check_does_not_panic() {
        let _ = is_admin();
    }
}
