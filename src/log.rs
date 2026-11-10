//! Thread-safe logger: console + file. Never panics.
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static SINK: OnceLock<Mutex<Option<File>>> = OnceLock::new();

fn sink() -> &'static Mutex<Option<File>> {
    SINK.get_or_init(|| Mutex::new(None))
}

/// Default log path: %ProgramData%/DriverForge/logs/run-<timestamp>.log
pub fn default_path() -> PathBuf {
    let base = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("DriverForge").join("logs")
        .join(format!("run-{}.log", chrono::Local::now().format("%Y%m%d-%H%M%S")))
}

fn is_reparse_point(p: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    std::fs::symlink_metadata(p)
        .map(|m| m.file_attributes() & 0x400 != 0)
        .unwrap_or(false)
}

/// Open (append) the log file, creating parent dirs. Failure is reported on the console only.
pub fn init(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
        // Refuse to log through a junction/symlink planted in the log dir or its parent.
        if dir.ancestors().take(2).any(is_reparse_point) {
            eprintln!("WARN log directory {} is a reparse point; file logging disabled", dir.display());
            return;
        }
    }
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(f) => {
            let mut g = sink().lock().unwrap_or_else(|e| e.into_inner());
            *g = Some(f);
        }
        Err(e) => eprintln!("WARN cannot open log file {}: {e}", path.display()),
    }
}

fn write(level: &str, m: &str) {
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let line = format!("{ts} {level:<5} {m}");
    let mut g = sink().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(f) = g.as_mut() {
        let _ = writeln!(f, "{line}");
        let _ = f.flush();
    }
    // Console output goes to stderr only (stdout is reserved for the report/JSON),
    // under the same lock so lines never interleave; ignore broken pipes.
    if level == "INFO" {
        // file only: keep the console clean for the report
    } else {
        let _ = writeln!(std::io::stderr(), "{level} {m}");
    }
}

pub fn info(m: &str) { write("INFO", m); }
pub fn warn(m: &str) { write("WARN", m); }
pub fn err(m: &str) { write("ERR", m); }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_file_and_threads() {
        let dir = std::env::temp_dir().join(format!("df-log-test-{}", std::process::id()));
        let p = dir.join("nested").join("t.log");
        init(&p);
        let hs: Vec<_> = (0..8).map(|i| std::thread::spawn(move || {
            for j in 0..20 { info(&format!("t{i} m{j}")); }
        })).collect();
        for h in hs { h.join().unwrap(); }
        warn("w"); err("e");
        let s = std::fs::read_to_string(&p).unwrap();
        assert_eq!(s.lines().filter(|l| l.contains("INFO  t")).count(), 160);
        assert!(s.contains("WARN  w") && s.contains("ERR   e"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_path_no_panic() { init(Path::new("Z:/nonexistent-\0/x.log")); info("still ok"); }

    #[test]
    fn default_path_shape() {
        let p = default_path();
        assert!(p.to_string_lossy().contains("run-") && p.extension().unwrap() == "log");
    }
}
