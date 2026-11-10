use crate::types::{Candidate, Device};
use std::sync::{Mutex, OnceLock};

pub trait Provider: Send {
    fn name(&self) -> &'static str;
    fn check(&self, devices: &[Device]) -> Result<Vec<Candidate>, String>;
}

static INCOMPLETE: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

fn store() -> &'static Mutex<Vec<String>> {
    INCOMPLETE.get_or_init(|| Mutex::new(Vec::new()))
}

/// A provider calls this when part of its check could not be completed (e.g. one device lookup
/// failed). The report then refuses to claim "everything is up to date".
pub fn note_incomplete(msg: String) {
    if let Ok(mut v) = store().lock() {
        v.push(msg);
    }
}

pub fn take_incomplete() -> Vec<String> {
    store().lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default()
}

static APPS: OnceLock<Mutex<Vec<(String, String, String)>>> = OnceLock::new();

/// Inventory line for a related app/runtime: (name, installed or "not installed", latest or "unknown").
pub fn note_app(name: &str, installed: Option<&str>, latest: Option<&str>) {
    crate::log::info(&format!("app: {name} installed={} latest={}", installed.unwrap_or("-"), latest.unwrap_or("?")));
    if let Ok(mut v) = APPS.get_or_init(|| Mutex::new(Vec::new())).lock() {
        v.push((name.into(), installed.unwrap_or("not installed").into(), latest.unwrap_or("unknown").into()));
    }
}

pub fn take_apps() -> Vec<(String, String, String)> {
    let mut v = APPS.get_or_init(|| Mutex::new(Vec::new())).lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default();
    v.sort();
    v
}

static INFO: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

fn info_store() -> &'static Mutex<Vec<String>> {
    INFO.get_or_init(|| Mutex::new(Vec::new()))
}

/// Something worth telling the user that is NOT a pending install (e.g. an offer that cannot be delivered yet).
pub fn note_info(msg: String) {
    if let Ok(mut v) = info_store().lock() {
        v.push(msg);
    }
}

pub fn take_info() -> Vec<String> {
    info_store().lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default()
}
