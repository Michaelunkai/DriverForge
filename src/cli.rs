use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Only {
    All,
    Nvidia,
    Amd,
    Windows,
}

#[derive(Debug, Clone)]
pub struct Args {
    pub scan_only: bool,
    pub dry_run: bool,
    pub yes: bool,
    pub only: Only,
    pub json: bool,
    pub no_restore_point: bool,
    pub log: Option<PathBuf>,
    pub help: bool,
    pub reinstall: Option<String>,
}

pub const USAGE: &str = "DriverForge - scan, find and silently install the latest drivers

USAGE: DriverForge [/scan] [--dry-run] [--yes] [--only nvidia|amd|windows|all] [--json] [--no-restore-point] [--log <path>]

  (no switch)          find EVERYTHING missing/outdated and install it
  /scan                scan only: list exactly what would be installed/updated, install nothing
                       (also --scan, --scan-only)
  --dry-run            do everything except actually installing
  --yes                non-interactive (no prompts)
  --only <set>         limit providers: nvidia, amd, windows or all (default all)
  --json               machine-readable report on stdout
  --reinstall amd|chipset   repair: reinstall the latest AMD Radeon / chipset package even if current
  --no-restore-point   skip creating a system restore point
  --log <path>         log file path
  -h, --help, /?       show this help

EXIT CODES: 0 ok/up to date, 1 failures or incomplete check, 2 reboot needed, 3 not administrator,
            5 (/scan) updates available, 64 usage error";

pub fn parse<I: IntoIterator<Item = String>>(it: I) -> Result<Args, String> {
    let mut a = Args {
        scan_only: false,
        dry_run: false,
        yes: false,
        only: Only::All,
        json: false,
        no_restore_point: false,
        log: None,
        help: false,
        reinstall: None,
    };
    let mut it = it.into_iter();
    while let Some(arg) = it.next() {
        // Accept Windows-style switches: /scan, /yes, /? ... (same as --scan, --yes, --help)
        let arg = match arg.strip_prefix('/') {
            Some("?") => "--help".to_string(),
            Some(r) if !r.is_empty() && !r.contains(['/', '\\', ':']) => format!("--{r}"),
            // Git Bash rewrites "/scan" into "C:/Program Files/Git/scan"; recover the switch name.
            None if arg.to_ascii_lowercase().ends_with("/git/scan") => "--scan".to_string(),
            _ => arg,
        };
        match arg.to_ascii_lowercase().as_str() {
            "--scan-only" | "--scan" | "-scan" | "scan" | "-s" => a.scan_only = true,
            "--dry-run" => a.dry_run = true,
            "--yes" | "-y" => a.yes = true,
            "--json" => a.json = true,
            "--no-restore-point" => a.no_restore_point = true,
            "-h" | "--help" => a.help = true,
            "--only" => {
                let v = it.next().ok_or("--only needs a value")?;
                a.only = match v.to_ascii_lowercase().as_str() {
                    "all" => Only::All,
                    "nvidia" => Only::Nvidia,
                    "amd" => Only::Amd,
                    "windows" => Only::Windows,
                    o => return Err(format!("invalid --only value '{o}'")),
                };
            }
            "--reinstall" => a.reinstall = Some(it.next().ok_or("--reinstall needs amd")?.to_ascii_lowercase()),
            "--log" => a.log = Some(PathBuf::from(it.next().ok_or("--log needs a path")?)),
            o => return Err(format!("unknown argument '{o}'")),
        }
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn p(s: &[&str]) -> Result<Args, String> {
        parse(s.iter().map(|x| x.to_string()))
    }
    #[test]
    fn flags() {
        let a = p(&["--scan-only", "--json", "--only", "AMD", "--log", "x.log"]).unwrap();
        assert!(a.scan_only && a.json && a.only == Only::Amd);
        assert!(a.log.is_some());
    }
    #[test]
    fn slash_scan() {
        assert!(p(&["/scan"]).unwrap().scan_only);
        assert!(p(&["/SCAN"]).unwrap().scan_only);
        assert!(p(&["/yes"]).unwrap().yes);
        assert!(p(&["/?"]).unwrap().help);
        assert!(!p(&[]).unwrap().scan_only);
    }
    #[test]
    fn bad() {
        assert!(p(&["--nope"]).is_err());
        assert!(p(&["--only"]).is_err());
        assert!(p(&["--only", "x"]).is_err());
    }
}
