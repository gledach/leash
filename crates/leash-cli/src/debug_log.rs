//! Small rotating log for GUI actions: `%LOCALAPPDATA%\leash\leash.log`.

use leash_snapshot::default_data_dir;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Rotate when the log grows past this size (keep one `leash.log.1` backup).
pub const MAX_LOG_BYTES: u64 = 256 * 1024;

static DEFAULT_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn init(dir: &Path) {
    let _ = fs::create_dir_all(dir);
    if let Ok(mut guard) = DEFAULT_PATH.lock() {
        *guard = Some(dir.join("leash.log"));
    }
}

fn log_path() -> PathBuf {
    if let Ok(guard) = DEFAULT_PATH.lock() {
        if let Some(path) = guard.as_ref() {
            return path.clone();
        }
    }
    default_data_dir().join("leash.log")
}

/// Append one line. Never panics.
pub fn log(message: impl AsRef<str>) {
    let path = log_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = rotate_if_needed(&path, MAX_LOG_BYTES);
    let stamp = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f");
    let line = format!("{stamp}  {}\n", message.as_ref());
    let _ = append_line(&path, &line);
}

pub fn rotate_if_needed(path: &Path, max_bytes: u64) -> std::io::Result<()> {
    let len = match fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    if len < max_bytes {
        return Ok(());
    }
    let backup = path.with_extension("log.1");
    if backup.exists() {
        let _ = fs::remove_file(&backup);
    }
    fs::rename(path, backup)
}

pub fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())?;
    file.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("leash-log-test-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn appends_and_rotates() {
        let dir = tmp_dir();
        let path = dir.join("leash.log");
        append_line(&path, "first\n").unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("first"));

        // Force rotation with a tiny cap.
        append_line(&path, "second-line-is-long-enough\n").unwrap();
        rotate_if_needed(&path, 8).unwrap();
        assert!(!path.exists() || fs::metadata(&path).map(|m| m.len()).unwrap_or(0) == 0);
        let backup = dir.join("leash.log.1");
        assert!(backup.exists(), "expected rotated backup");
        let body = fs::read_to_string(&backup).unwrap();
        assert!(body.contains("first"));
        append_line(&path, "fresh\n").unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("fresh"));
        let _ = fs::remove_dir_all(&dir);
    }
}
