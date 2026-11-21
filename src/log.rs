//! One-line action log, rolled at 1 MB.

use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const CAP: u64 = 1_048_576;

/// Best effort: I/O errors are ignored so logging can never stop the app.
pub fn log_line(dir: &Path, msg: &str) {
    let path = dir.join("log.txt");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > CAP) {
        let _ = std::fs::rename(&path, dir.join("log.old.txt"));
    }
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{secs} {msg}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_rolls_at_1mb() {
        let d = std::env::temp_dir().join(format!("ramopt-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("log.txt"), vec![b'x'; 1_048_577]).unwrap();
        log_line(&d, "hello");
        assert!(d.join("log.old.txt").exists());
        let s = std::fs::read_to_string(d.join("log.txt")).unwrap();
        assert!(s.ends_with("hello\n") && s.len() < 100);
    }

    #[test]
    fn log_appends() {
        let d = std::env::temp_dir().join(format!("ramopt-log2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        log_line(&d, "a");
        log_line(&d, "b");
        assert_eq!(std::fs::read_to_string(d.join("log.txt")).unwrap().lines().count(), 2);
    }
}
