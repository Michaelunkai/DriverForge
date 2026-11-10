mod amd_chipset;
mod amd_gpu;
mod catalog;
mod cli;
mod comdisp;
mod http;
mod installer;
mod log;
mod nvapp;
mod nvidia;
mod provider;
mod report;
mod runtimes;
mod scan;
mod types;
mod version;
mod wupdater;

use cli::Only;
use provider::Provider;
use report::Row;
use types::{Candidate, Outcome};

type Factory = fn() -> Box<dyn Provider>;

fn factories(only: Only) -> Vec<Factory> {
    let nv: [Factory; 2] = [nvidia::provider, nvapp::provider];
    let amd: [Factory; 2] = [amd_gpu::provider, amd_chipset::provider];
    let win: [Factory; 3] = [wupdater::provider, catalog::provider, runtimes::provider];
    match only {
        Only::Nvidia => nv.to_vec(),
        Only::Amd => amd.to_vec(),
        Only::Windows => win.to_vec(),
        Only::All => nv.iter().chain(&amd).chain(&win).copied().collect(),
    }
}

/// Run every provider in parallel; return what is pending (newer than installed) and provider errors.
fn gather(only: Only, devices: &[types::Device]) -> (Vec<(String, Candidate)>, Vec<(String, String)>) {
    gather_with(factories(only), devices)
}

fn gather_with(facs: Vec<Factory>, devices: &[types::Device]) -> (Vec<(String, Candidate)>, Vec<(String, String)>) {
    let results: Vec<(String, Result<Vec<Candidate>, String>)> = std::thread::scope(|s| {
        let handles: Vec<_> = facs
            .into_iter()
            .map(|f| {
                s.spawn(move || {
                    let p = f();
                    let name = p.name().to_string();
                    (name, p.check(devices))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| ("?".into(), Err("provider panicked".into()))))
            .collect()
    });
    let mut pending = Vec::new();
    let mut errors = Vec::new();
    for (name, res) in results {
        match res {
            Err(e) => errors.push((name, e)),
            Ok(cands) => {
                for c in cands {
                    let newer = match &c.installed {
                        Some(i) => version::is_newer(&c.latest, i),
                        None => true,
                    };
                    if newer {
                        pending.push((name.clone(), c));
                    }
                }
            }
        }
    }
    (pending, errors)
}

fn fail_row(provider: String, why: String) -> Row {
    Row {
        provider,
        component: "-".into(),
        installed: "-".into(),
        latest: "-".into(),
        outcome: format!("Failed: {why}"),
    }
}

/// True when Windows itself says a restart is pending (Windows Update / servicing).
fn windows_reboot_pending() -> bool {
    use winreg::enums::*;
    use winreg::RegKey;
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    hklm.open_subkey(r"SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired").is_ok()
        || hklm.open_subkey(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootPending").is_ok()
}

/// Ask ONCE whether to restart now or later. Returns true if a restart was started.
fn ask_reboot() -> bool {
    use std::io::{BufRead, IsTerminal, Write};
    println!();
    println!("  {}", report::yellow("A restart is required to finish installing the drivers."));
    if !std::io::stdin().is_terminal() {
        println!("  (non-interactive run: restart later)");
        return false;
    }
    loop {
        print!("  Restart now or later?  [N]ow / [L]ater : ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
            return false;
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "n" | "now" => {
                println!("  Restarting in 10 seconds...");
                let ok = std::process::Command::new("shutdown")
                    .args(["/r", "/t", "10", "/c", "DriverForge: restarting to finish driver installation"])
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                if !ok {
                    println!("  {}", report::red("Could not start the restart automatically. Please restart manually."));
                }
                return ok;
            }
            "l" | "later" => {
                println!("  OK - please restart when convenient.");
                return false;
            }
            _ => println!("  Please type N (now) or L (later)."),
        }
    }
}

fn run() -> i32 {
    report::init_console();
    let args = match cli::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}\n\n{}", cli::USAGE);
            return 64;
        }
    };
    if args.help {
        println!("{}", cli::USAGE);
        return 0;
    }
    let scan_mode = args.scan_only;
    let dry = args.dry_run || scan_mode;
    if !dry && !installer::is_admin() {
        eprintln!("DriverForge must run as Administrator (or use /scan or --dry-run).");
        return 3;
    }
    let logp = args.log.clone().unwrap_or_else(|| std::env::temp_dir().join("DriverForge.log"));
    log::init(&logp);

    let quiet = args.json;
    if !quiet {
        print!("{}", report::banner(if scan_mode { "scan (nothing will be installed)" } else if dry { "dry run" } else { "scan + install" }));
        println!("  Scanning this PC and checking vendors online - this takes a minute...");
    }

    let mut rows: Vec<Row> = Vec::new();
    let mut failures = 0usize;
    let mut reboot = false;
    let mut verify_left = 0usize;

    let devices = match scan::scan_devices() {
        Ok(d) => d,
        Err(e) => {
            log::err(&format!("device scan failed: {e}"));
            failures += 1;
            rows.push(fail_row("scan".into(), e));
            Vec::new()
        }
    };
    let problems = scan::problem_devices(&devices).len();
    let detected: Vec<(String, String)> = devices
        .iter()
        .filter(|d| d.class.eq_ignore_ascii_case("DISPLAY"))
        .map(|d| (d.name.clone(), d.driver_version.clone().unwrap_or_else(|| "-".into())))
        .collect();
    if !quiet {
        print!("{}", report::pc_summary(devices.len(), problems, &detected));
    }

    let (mut pending, errors) = if let Some(what) = &args.reinstall {
        match what.as_str() {
            "amd" => match amd_gpu::build(&devices, true) {
                Ok(c) => (c.into_iter().map(|c| ("amd_gpu".to_string(), c)).collect(), Vec::new()),
                Err(e) => (Vec::new(), vec![("amd_gpu".to_string(), e)]),
            },
            "chipset" => match amd_chipset::forced() {
                Ok(c) => (c.into_iter().map(|c| ("amd_chipset".to_string(), c)).collect(), Vec::new()),
                Err(e) => (Vec::new(), vec![("amd_chipset".to_string(), e)]),
            },
            other => {
                eprintln!("--reinstall supports: amd, chipset (got '{other}')");
                return 64;
            }
        }
    } else {
        gather(args.only, &devices)
    };
    for (name, e) in errors {
        failures += 1;
        rows.push(fail_row(name, e));
    }
    let incomplete = provider::take_incomplete();
    let mut info = provider::take_info();
    let apps = provider::take_apps();
    if !incomplete.is_empty() {
        failures += 1;
    }

    let mk_row = |name: &str, c: &Candidate, outcome: String| Row {
        provider: name.to_string(),
        component: report::clean_component(&if c.component.starts_with(&c.vendor) { c.component.clone() } else { format!("{} {}", c.vendor, c.component) }),
        installed: c.installed.clone().unwrap_or_else(|| "-".into()),
        latest: c.latest.clone(),
        outcome,
    };

    // Phase 2: install (or just list).
    if dry {
        for (name, c) in &pending {
            rows.push(mk_row(name, c, report::outcome_text(&Outcome::DryRun)));
        }
    } else if !pending.is_empty() {
        if !args.no_restore_point {
            if let Err(e) = installer::create_restore_point() {
                log::warn(&format!("restore point failed: {e}"));
            }
        }
        let n = pending.len();
        if !quiet {
            println!("\n  {} item(s) to install:\n", n);
        }
        for (i, (name, c)) in pending.iter().enumerate() {
            if !quiet {
                println!("  [{}/{}] Installing {} ...", i + 1, n, report::clean_component(&c.component));
            }
            let o = if matches!(c.kind, types::InstallKind::WindowsUpdate) {
                match wupdater::install_updates(&[c.url.clone()]) {
                    Ok(r) if r.installed == 0 && r.total > 0 && r.message.contains("0x80240022") => {
                        {
                        wupdater::mark_undeliverable(&c.url);
                        info.push(format!("{} - Windows Update offers it but cannot deliver it yet; it is retried weekly", report::clean_component(&c.component)));
                        Outcome::Skipped("Windows Update lists this update but cannot deliver it yet (no downloadable package)".into())
                    }
                    }
                    Ok(r) if r.installed == 0 && r.total > 0 => Outcome::Failed(r.message),
                    Ok(r) if r.reboot_required => Outcome::InstalledRebootNeeded,
                    Ok(_) => Outcome::Installed,
                    Err(e) => Outcome::Failed(e),
                }
            } else {
                installer::install(c, false)
            };
            match &o {
                Outcome::Failed(_) => failures += 1,
                Outcome::InstalledRebootNeeded => reboot = true,
                _ => {}
            }
            let row = mk_row(name, c, report::outcome_text(&o));
            if !quiet {
                print!("{}", report::install_line(i + 1, n, &row));
            }
            rows.push(row);
        }
        if reboot || windows_reboot_pending() {
            reboot = true;
        }
        // Verify: re-scan and re-check, so "installed" is never taken on trust.
        if !quiet {
            println!("\n  Verifying - re-scanning this PC...");
        }
        let _ = provider::take_incomplete();
        let devices2 = scan::scan_devices().unwrap_or_default();
        let (still, _) = gather(args.only, &devices2);
        let _ = provider::take_incomplete();
        if !quiet {
            if still.is_empty() {
                println!("  {} {}", report::green("✔"), "Verified: nothing is outdated any more.");
            } else {
                println!("  {} {} item(s) still report an older version{}:", report::yellow("●"), still.len(), if reboot { " (a restart should apply them)" } else { "" });
                for (_, c) in &still {
                    println!("      - {}  {} → {}", report::clean_component(&c.component), c.installed.clone().unwrap_or_else(|| "-".into()), c.latest);
                }
            }
        }
        verify_left = still.len();
        // Completeness guarantee: a device that still has a driver problem after every provider ran
        // is never reported as success; list its hardware IDs and fail the run.
        for d in scan::problem_devices(&devices2) {
            failures += 1;
            let ids = d.hardware_ids.first().cloned().unwrap_or_default();
            log::err(&format!("unresolved device: {} [{}] code {}", d.name, ids, d.problem_code));
            rows.push(fail_row(
                "unresolved".into(),
                format!("{} ({}) still has problem code {} - no driver source found", d.name, ids, d.problem_code),
            ));
        }
        // A driver that installed fine but is not yet bound to its device is applied by a restart.
        let installed_ok: Vec<String> = rows
            .iter()
            .filter(|r| r.outcome.starts_with("Installed"))
            .map(|r| r.component.clone())
            .collect();
        let waiting = still.iter().filter(|(_, c)| {
            let name = report::clean_component(&c.component);
            installed_ok.iter().any(|i| i.contains(&name) || name.contains(i.as_str()))
        }).count();
        if waiting > 0 {
            reboot = true;
            if !quiet {
                println!("  {} {} driver(s) are installed and will take effect after a restart.", report::yellow("●"), waiting);
            }
        }
    }

    let updates_available = scan_mode && rows.iter().any(report::is_pending);
    let code = if failures > 0 {
        1
    } else if reboot {
        2
    } else if updates_available {
        5
    } else {
        0
    };

    if args.json {
        println!("{}", report::json(&rows, &apps, &detected, devices.len(), problems, code));
        return code;
    }
    if dry {
        print!("{}", report::scan_view(&rows));
        print!("{}", report::apps_block(&apps));
        print!("{}", report::info_block(&info));
        print!("{}", report::incomplete_block(&incomplete));
    } else {
        let failed_rows: Vec<&Row> = rows.iter().filter(|r| r.outcome.starts_with("Failed")).collect();
        println!();
        if pending.is_empty() && failures == 0 {
            println!("  {} {}", report::green("✔"), report::bold("Everything is up to date. Nothing needed to be installed."));
        } else if failed_rows.is_empty() && incomplete.is_empty() {
            let skipped: Vec<&Row> = rows.iter().filter(|r| r.outcome.starts_with("Skipped")).collect();
            if skipped.is_empty() && verify_left == 0 {
                println!("  {} {}", report::green("✔"), report::bold("All done - everything is installed and up to date."));
            } else if skipped.is_empty() {
                println!("  {} {}", report::green("✔"), report::bold("Install finished; the items listed above still report an older version."));
            } else {
                println!("  {} {}", report::green("✔"), report::bold("Everything that can be installed has been installed."));
                for r in &skipped {
                    println!("  {} not installed: {} - {}", report::yellow("–"), report::clean_component(&r.component), r.outcome.trim_start_matches("Skipped: "));
                }
            }
        } else {
            for r in &failed_rows {
                println!("  {} {} - {}", report::red("✘"), r.provider, r.outcome);
            }
            print!("{}", report::incomplete_block(&incomplete));
        }
        print!("{}", report::info_block(&info));
        if reboot && !args.yes {
            ask_reboot();
        } else if reboot {
            println!("  {}", report::yellow("A restart is required to finish installing the drivers (run without /yes to be asked)."));
        }
    }
    println!("\n  Log: {}   Exit code {code}", logp.display());
    code
}

fn main() {
    let code = std::panic::catch_unwind(run).unwrap_or(1);
    std::process::exit(code);
}

#[cfg(test)]
mod robustness_tests {
    use super::*;
    use crate::types::{Candidate, InstallKind};

    struct Offline;
    impl Provider for Offline {
        fn name(&self) -> &'static str { "offline" }
        fn check(&self, _: &[types::Device]) -> Result<Vec<Candidate>, String> { Err("network unreachable".into()) }
    }
    struct Panics;
    impl Provider for Panics {
        fn name(&self) -> &'static str { "panics" }
        fn check(&self, _: &[types::Device]) -> Result<Vec<Candidate>, String> { panic!("boom") }
    }
    struct Good;
    impl Provider for Good {
        fn name(&self) -> &'static str { "good" }
        fn check(&self, _: &[types::Device]) -> Result<Vec<Candidate>, String> {
            Ok(vec![Candidate { vendor: "v".into(), component: "c".into(), installed: Some("1.0".into()), latest: "2.0".into(),
                url: "https://x".into(), sha256: None, size_bytes: None, kind: InstallKind::ExeSilent, silent_args: vec![], needs_reboot_hint: false }])
        }
    }
    fn off() -> Box<dyn Provider> { Box::new(Offline) }
    fn pan() -> Box<dyn Provider> { Box::new(Panics) }
    fn good() -> Box<dyn Provider> { Box::new(Good) }

    #[test]
    fn one_offline_or_panicking_provider_does_not_abort_others() {
        let (pending, errors) = gather_with(vec![off, pan, good], &[]);
        assert_eq!(pending.len(), 1, "good provider's update must survive");
        assert_eq!(errors.len(), 2, "both failures reported, not swallowed");
        assert!(errors.iter().any(|(n, e)| n == "offline" && e.contains("network")));
    }
}
