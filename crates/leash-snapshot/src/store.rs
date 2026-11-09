use crate::model::{DiffReport, Snapshot};
use serde_json::Error as JsonError;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid snapshot json at {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: JsonError,
    },
    #[error("no current session; run `leash start` first")]
    NoCurrent,
    #[error("session {0} not found under {1}")]
    NotFound(String, PathBuf),
    #[error("session {0} is missing {1}")]
    MissingFile(String, &'static str),
}

#[derive(Debug, Clone)]
pub struct SessionPaths {
    pub id: String,
    pub dir: PathBuf,
}

impl SessionPaths {
    pub fn before(&self) -> PathBuf {
        self.dir.join("before.json")
    }
    pub fn after(&self) -> PathBuf {
        self.dir.join("after.json")
    }
    pub fn report_md(&self) -> PathBuf {
        self.dir.join("report.md")
    }
    pub fn report_json(&self) -> PathBuf {
        self.dir.join("report.json")
    }
    pub fn report_html(&self) -> PathBuf {
        self.dir.join("report.html")
    }
}

#[derive(Debug, Clone)]
pub struct SessionStore {
    pub root: PathBuf,
}

impl SessionStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    pub fn current_pointer(&self) -> PathBuf {
        self.root.join("current")
    }

    pub fn ensure_root(&self) -> Result<(), SessionError> {
        mkdir(&self.root)?;
        mkdir(&self.sessions_dir())?;
        Ok(())
    }

    pub fn create(&self, id: &str) -> Result<SessionPaths, SessionError> {
        self.ensure_root()?;
        let dir = self.sessions_dir().join(id);
        mkdir(&dir)?;
        fs::write(self.current_pointer(), id.as_bytes()).map_err(|source| SessionError::Io {
            path: self.current_pointer(),
            source,
        })?;
        Ok(SessionPaths {
            id: id.to_string(),
            dir,
        })
    }

    pub fn current_id(&self) -> Result<String, SessionError> {
        let ptr = self.current_pointer();
        let text = fs::read_to_string(&ptr).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                SessionError::NoCurrent
            } else {
                SessionError::Io {
                    path: ptr.clone(),
                    source: e,
                }
            }
        })?;
        let id = text.trim();
        if id.is_empty() {
            return Err(SessionError::NoCurrent);
        }
        Ok(id.to_string())
    }

    pub fn open(&self, id: Option<&str>) -> Result<SessionPaths, SessionError> {
        let id = match id {
            Some(id) => id.to_string(),
            None => self.current_id()?,
        };
        let dir = self.sessions_dir().join(&id);
        if !dir.is_dir() {
            return Err(SessionError::NotFound(id, self.sessions_dir()));
        }
        Ok(SessionPaths { id, dir })
    }

    pub fn write_snapshot(
        &self,
        paths: &SessionPaths,
        snap: &Snapshot,
    ) -> Result<PathBuf, SessionError> {
        let path = match snap.label {
            crate::model::SnapshotLabel::Before => paths.before(),
            crate::model::SnapshotLabel::After => paths.after(),
        };
        let json = serde_json::to_string_pretty(snap).map_err(|source| SessionError::Json {
            path: path.clone(),
            source,
        })?;
        fs::write(&path, json).map_err(|source| SessionError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }

    pub fn read_before(&self, paths: &SessionPaths) -> Result<Snapshot, SessionError> {
        read_snapshot(&paths.before())
    }

    pub fn read_after(&self, paths: &SessionPaths) -> Result<Snapshot, SessionError> {
        if !paths.after().exists() {
            return Err(SessionError::MissingFile(paths.id.clone(), "after.json"));
        }
        read_snapshot(&paths.after())
    }

    pub fn write_report(
        &self,
        paths: &SessionPaths,
        report: &DiffReport,
        markdown: &str,
        html: &str,
    ) -> Result<(), SessionError> {
        let json = serde_json::to_string_pretty(report).map_err(|source| SessionError::Json {
            path: paths.report_json(),
            source,
        })?;
        fs::write(paths.report_json(), json).map_err(|source| SessionError::Io {
            path: paths.report_json(),
            source,
        })?;
        fs::write(paths.report_md(), markdown).map_err(|source| SessionError::Io {
            path: paths.report_md(),
            source,
        })?;
        fs::write(paths.report_html(), html).map_err(|source| SessionError::Io {
            path: paths.report_html(),
            source,
        })?;
        Ok(())
    }

    pub fn read_report(&self, paths: &SessionPaths) -> Result<DiffReport, SessionError> {
        if !paths.report_json().exists() {
            return Err(SessionError::MissingFile(paths.id.clone(), "report.json"));
        }
        let text = fs::read_to_string(paths.report_json()).map_err(|source| SessionError::Io {
            path: paths.report_json(),
            source,
        })?;
        serde_json::from_str(&text).map_err(|source| SessionError::Json {
            path: paths.report_json(),
            source,
        })
    }

    pub fn read_report_md(&self, paths: &SessionPaths) -> Result<String, SessionError> {
        if !paths.report_md().exists() {
            return Err(SessionError::MissingFile(paths.id.clone(), "report.md"));
        }
        fs::read_to_string(paths.report_md()).map_err(|source| SessionError::Io {
            path: paths.report_md(),
            source,
        })
    }
}

fn mkdir(path: &Path) -> Result<(), SessionError> {
    fs::create_dir_all(path).map_err(|source| SessionError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn read_snapshot(path: &Path) -> Result<Snapshot, SessionError> {
    let text = fs::read_to_string(path).map_err(|source| SessionError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&text).map_err(|source| SessionError::Json {
        path: path.to_path_buf(),
        source,
    })
}

/// Resolve the on-disk data directory.
///
/// Override with `LEASH_DATA_DIR`. On Windows, `%LOCALAPPDATA%\leash`.
/// Elsewhere, `~/.local/share/leash` (or `%USERPROFILE%\.leash` as a last resort).
pub fn default_data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("LEASH_DATA_DIR") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Ok(p) = std::env::var("LOCALAPPDATA") {
        if !p.trim().is_empty() {
            return PathBuf::from(p).join("leash");
        }
    }
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        let xdg = PathBuf::from(&home).join(".local/share/leash");
        return xdg;
    }
    PathBuf::from(".leash")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Snapshot, SnapshotLabel};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp() -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("leash-session-test-{n}"));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn round_trip_snapshot_and_current_pointer() {
        let root = tmp();
        let store = SessionStore::new(&root);
        let paths = store.create("sess-1").unwrap();
        let mut snap = Snapshot::new(SnapshotLabel::Before, "sess-1");
        snap.taken_at = "2026-11-09T19:00:00Z".into();
        store.write_snapshot(&paths, &snap).unwrap();
        assert_eq!(store.current_id().unwrap(), "sess-1");
        let loaded = store.read_before(&paths).unwrap();
        assert_eq!(loaded.taken_at, "2026-11-09T19:00:00Z");
        assert_eq!(loaded.version, crate::SNAPSHOT_VERSION);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_after_is_a_clear_error() {
        let root = tmp();
        let store = SessionStore::new(&root);
        let paths = store.create("sess-2").unwrap();
        let err = store.read_after(&paths).unwrap_err();
        assert!(matches!(err, SessionError::MissingFile(_, "after.json")));
        let _ = fs::remove_dir_all(&root);
    }
}
