use crate::types::Outcome;
use serde_json::json;

#[derive(Debug, Clone)]
pub struct Row {
    pub provider: String,
    pub component: String,
    pub installed: String,
    pub latest: String,
    pub outcome: String,
}

pub fn outcome_text(o: &Outcome) -> String {
    match o {
        Outcome::UpToDate => "UpToDate".into(),
        Outcome::Installed => "Installed".into(),
        Outcome::InstalledRebootNeeded => "InstalledRebootNeeded".into(),
        Outcome::Skipped(r) => format!("Skipped: {r}"),
        Outcome::Failed(r) => format!("Failed: {r}"),
        Outcome::DryRun => "DryRun (update available)".into(),
    }
}

pub fn table(rows: &[Row]) -> String {
    let h = ["Provider", "Component", "Installed", "Latest", "Result"];
    let cells: Vec<[&str; 5]> = rows
        .iter()
        .map(|r| [&*r.provider, &*r.component, &*r.installed, &*r.latest, &*r.outcome])
        .collect();
    let mut w = h.map(|s| s.chars().count());
    for c in &cells {
        for i in 0..5 {
            w[i] = w[i].max(c[i].chars().count());
        }
    }
    let line = |c: [&str; 5]| {
        let mut s = String::new();
        for i in 0..5 {
            s.push_str(&format!("{:<width$}  ", c[i], width = w[i]));
        }
        s.trim_end().to_string() + "
"
    };
    let mut out = line(h);
    out.push_str(&"-".repeat(w.iter().sum::<usize>() + 10));
    out.push('\n');
    for c in cells {
        out.push_str(&line(c));
    }
    out
}

pub fn json(rows: &[Row], apps: &[(String, String, String)], detected: &[(String, String)], device_count: usize, problem_count: usize, exit_code: i32) -> String {
    let r: Vec<_> = rows
        .iter()
        .map(|r| {
            json!({"provider": r.provider, "component": r.component, "installed": r.installed,
                   "latest": r.latest, "result": r.outcome})
        })
        .collect();
    serde_json::to_string_pretty(&json!({
        "devices": device_count,
        "detected_gpus": detected.iter().map(|(n, v)| json!({"name": n, "driver_version": v})).collect::<Vec<_>>(), "problem_devices": problem_count,
        "apps": apps.iter().map(|(n, i, l)| json!({"name": n, "installed": i, "latest": l})).collect::<Vec<_>>(), "results": r, "exit_code": exit_code}))
    .unwrap_or_else(|_| "{}".into())
}


// ---------------------------------------------------------------- pretty console output

#[cfg(windows)]
extern "system" {
    fn GetStdHandle(n: u32) -> isize;
    fn GetConsoleMode(h: isize, m: *mut u32) -> i32;
    fn SetConsoleMode(h: isize, m: u32) -> i32;
    fn SetConsoleOutputCP(cp: u32) -> i32;
}

static COLOR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// UTF-8 output + ANSI colours when stdout is a real console.
pub fn init_console() {
    #[cfg(windows)]
    unsafe {
        SetConsoleOutputCP(65001);
        let h = GetStdHandle(0xFFFF_FFF5); // STD_OUTPUT_HANDLE
        let mut m = 0u32;
        if h != 0 && h != -1 && GetConsoleMode(h, &mut m) != 0 && SetConsoleMode(h, m | 0x4) != 0 {
            COLOR.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

fn c(code: &str, t: &str) -> String {
    if COLOR.load(std::sync::atomic::Ordering::Relaxed) {
        format!("\x1b[{code}m{t}\x1b[0m")
    } else {
        t.to_string()
    }
}
pub fn green(t: &str) -> String { c("32", t) }
pub fn yellow(t: &str) -> String { c("33", t) }
pub fn red(t: &str) -> String { c("31;1", t) }
pub fn cyan(t: &str) -> String { c("36", t) }
pub fn bold(t: &str) -> String { c("1", t) }
pub fn dim(t: &str) -> String { c("2", t) }

/// Remove internal bookkeeping (SHA-1 note) from a component label.
pub fn clean_component(s: &str) -> String {
    match s.find(" sha1b64=") {
        Some(i) => s[..i].to_string(),
        None => s.to_string(),
    }
}

pub fn provider_label(id: &str) -> &'static str {
    match id {
        "nvidia" => "NVIDIA graphics driver",
        "nvapp" => "NVIDIA App",
        "amd_gpu" => "AMD Radeon graphics driver",
        "amd_chipset" => "AMD chipset / Ryzen platform",
        "windows-update" => "Windows Update drivers",
        "catalog" => "Microsoft Update Catalog",
        "scan" => "Device scan",
        _ => "Other",
    }
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n - 1).collect();
        format!("{t}…")
    }
}

pub fn banner(title: &str) -> String {
    let bar = "═".repeat(78);
    format!("{}
  {}  {}
{}
", cyan(&bar), bold("DriverForge"), dim(&format!("· {title}")), cyan(&bar))
}

pub fn pc_summary(devices: usize, problems: usize, gpus: &[(String, String)]) -> String {
    let mut o = format!("  {} {} devices inspected", bold("This PC:"), devices);
    if problems > 0 {
        o += &format!(", {}", yellow(&format!("{problems} with driver problems")));
    }
    o.push('\n');
    for (n, v) in gpus {
        o += &format!("    • {}  {}
", n, dim(&format!("(driver {v})")));
    }
    o
}

/// Rows that describe a real, pending update/installation (not "up to date", not a check failure).
pub fn is_pending(r: &Row) -> bool {
    r.outcome.starts_with("DryRun")
}

/// Provider-level failures (the check itself could not complete).
pub fn is_check_failure(r: &Row) -> bool {
    r.component == "-" && r.outcome.starts_with("Failed")
}

fn update_table(rows: &[&Row]) -> String {
    let mut o = String::new();
    for (i, r) in rows.iter().enumerate() {
        let name = clean_component(&r.component)
            .trim_start_matches("Microsoft Update Catalog ")
            .trim_start_matches("Microsoft Corporation ")
            .to_string();
        let inst = if r.installed == "-" { "not installed".to_string() } else { r.installed.clone() };
        o += &format!("  {:>2}. {}
", i + 1, bold(&trunc(&name, 100)));
        o += &format!("      {}  {}  {}   {}
", yellow(&inst), dim("→"), green(&r.latest), dim(&format!("[{}]", provider_label(&r.provider))));
    }
    o
}

/// The /scan view: only what would be installed or updated, or a clear "nothing to do".
pub fn scan_view(rows: &[Row]) -> String {
    let pending: Vec<&Row> = rows.iter().filter(|r| is_pending(r)).collect();
    let failed: Vec<&Row> = rows.iter().filter(|r| is_check_failure(r)).collect();
    let mut o = String::new();
    o.push('\n');
    if pending.is_empty() && failed.is_empty() {
        o += &format!("  {} {}
", green("✔"), bold("Everything is up to date. Nothing needs to be installed or updated."));
    } else if pending.is_empty() {
        o += &format!("  {}
", bold("No updates were found in the checks that completed."));
    } else {
        o += &format!("  {} {}

", yellow("●"), bold(&format!("{} item(s) would be installed or updated:", pending.len())));
        o += &update_table(&pending);
        o += &format!("
  {}
", dim("Run DriverForge.exe without /scan to install these."));
    }
    o += "
  Checked:
";
    for id in ["nvidia", "nvapp", "amd_gpu", "amd_chipset", "windows-update", "catalog"] {
        let bad = failed.iter().find(|r| r.provider == id);
        let n = pending.iter().filter(|r| r.provider == id).count();
        match bad {
            Some(r) => o += &format!("    {} {:<32} {}
", red("✘"), provider_label(id), red(&format!("could not be checked: {}", r.outcome.trim_start_matches("Failed: ")))),
            None if n > 0 => o += &format!("    {} {:<32} {}
", yellow("●"), provider_label(id), yellow(&format!("{n} update(s)"))),
            None => o += &format!("    {} {:<32} {}
", green("✔"), provider_label(id), dim("up to date")),
        }
    }
    o
}

/// Informational block: offers that exist but cannot be installed right now (not errors).
pub fn apps_block(apps: &[(String, String, String)]) -> String {
    if apps.is_empty() {
        return String::new();
    }
    let mut o = String::from("
  Related apps (installed -> latest):
");
    for (n, i, l) in apps {
        o += &format!("    - {:<44} {:<20} -> {}
", n, i, l);
    }
    o
}

pub fn info_block(items: &[String]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let mut o = format!("\n  {} {}\n", yellow("–"), bold("Not installable right now:"));
    for i in items {
        o += &format!("    - {}\n", trunc(i, 120));
    }
    o
}

/// Warning block shown whenever a check could not be completed, so "up to date" is never claimed falsely.
pub fn incomplete_block(items: &[String]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let mut o = format!("
  {} {}
", red("⚠ INCOMPLETE:"), bold("some checks could not finish, so this list may be missing items."));
    for i in items {
        o += &format!("    - {}
", trunc(i, 110));
    }
    o += &format!("  {}
", dim("Check your internet connection and run again."));
    o
}

pub fn install_line(i: usize, n: usize, r: &Row) -> String {
    let (mark, txt) = if r.outcome.starts_with("Failed") {
        (red("✘"), red(&r.outcome))
    } else if r.outcome.starts_with("Skipped") {
        (yellow("–"), yellow(&r.outcome))
    } else if r.outcome == "UpToDate" {
        (green("✔"), dim("already current"))
    } else {
        (green("✔"), green(&r.outcome))
    };
    format!("  [{i}/{n}] {mark} {}  {}
", trunc(&clean_component(&r.component), 64), txt)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renders() {
        let rows = vec![Row {
            provider: "nvidia".into(),
            component: "GPU".into(),
            installed: "621.14".into(),
            latest: "621.14".into(),
            outcome: "UpToDate".into(),
        }];
        assert!(table(&rows).contains("621.14"));
        assert!(json(&rows, &[], &[], 3, 1, 0).contains("\"problem_devices\": 1"));
    }
}
