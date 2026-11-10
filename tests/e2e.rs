//! End-to-end tests: run the real exe with --scan-only --dry-run (never installs) and check fixtures.
use std::process::Command;

fn run(args: &[&str]) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_DriverForge")).args(args).output().expect("spawn exe");
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into(), String::from_utf8_lossy(&o.stderr).into())
}

#[test]
fn scan_only_dry_run_json() {
    let (code, out, err) = run(&["--scan-only", "--dry-run", "--json", "--no-restore-point"]);
    assert!(matches!(code, 0 | 2 | 5), "exit {code}\n{err}");
    assert!(!err.contains("panicked") && !out.contains("panicked"), "panic: {err}");
    // NVIDIA 621.14 is current: provider "nvidia" must report UpToDate (installed version is not
    // printed by report.rs for up-to-date rows; tracked as QA bug #1 in qa/RESULTS.md).
    let v: serde_json::Value = serde_json::from_str(&out[out.find('{').expect("json")..]).expect("valid json");
    // Up-to-date providers produce no pending row; NVIDIA 621.14 is current, so no pending nvidia row.
    let pending_nv = v["results"].as_array().unwrap().iter().any(|r| r["provider"] == "nvidia" && r["result"].as_str().unwrap_or("").starts_with("DryRun"));
    assert!(!pending_nv, "nvidia unexpectedly pending: {out}");
    assert!(v["devices"].as_u64().unwrap() > 50);
    let gpus = v["detected_gpus"].to_string();
    assert!(gpus.contains("32.0.16.2114"), "RTX 5080 (NVIDIA 621.14) missing: {gpus}");
    assert!(gpus.contains("32.0.21045.20006"), "AMD Radeon version missing: {gpus}");
    assert!(out.contains("32.0.21045.20006"), "AMD Radeon version missing:\n{out}");
}

#[test]
fn slash_scan_prints_report_and_installs_nothing() {
    let (code, out, err) = run(&["/scan"]);
    assert!(matches!(code, 0 | 1 | 5), "exit {code}
{err}");
    assert!(out.contains("Checked:"), "no checked list:
{out}");
    assert!(out.contains("Everything is up to date") || out.contains("would be installed or updated") || out.contains("INCOMPLETE"), "{out}");
    assert!(!out.contains("Installing"), "scan must never install:
{out}");
}

#[test]
fn unknown_flag_does_not_panic() {
    let (_c, o, e) = run(&["--definitely-not-a-flag", "--scan-only", "--dry-run"]);
    assert!(!o.contains("panicked") && !e.contains("panicked"));
}

#[test]
fn fixture_nvidia_lookup() {
    let s = std::fs::read_to_string("tests/fixtures/nvidia_lookup_5080.json").unwrap();
    let v: serde_json::Value = serde_json::from_str(&s).expect("valid json");
    assert_eq!(v["Success"], "1");
    assert!(s.contains("621.14"));
}

#[test]
fn fixture_amd_page_has_exe_link() {
    let s = std::fs::read_to_string("tests/fixtures/amd_am5_x670.html").unwrap();
    let re = regex::Regex::new(r#"https://drivers\.amd\.com[^"]*\.exe"#).unwrap();
    assert!(re.find(&s).is_some());
}
