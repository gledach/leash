//! Persisted UI preferences (`ui.toml` under the data directory).

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum UiTheme {
    #[default]
    Light,
    Dark,
}

impl UiTheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn parse(raw: &str) -> Self {
        if raw.trim().eq_ignore_ascii_case("dark") {
            Self::Dark
        } else {
            Self::Light
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            Self::Light => Self::Dark,
            Self::Dark => Self::Light,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct UiPrefs {
    #[serde(default)]
    pub last_repo_path: Option<String>,
    #[serde(default)]
    pub theme: UiTheme,
}

impl UiPrefs {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("ui.toml")
    }

    /// Missing or unreadable file → defaults, never an error (the UI must open).
    pub fn load(data_dir: &Path) -> Self {
        let path = Self::path(data_dir);
        let Ok(text) = fs::read_to_string(&path) else {
            return Self::default();
        };
        toml::from_str(&text).unwrap_or_default()
    }

    pub fn save(&self, data_dir: &Path) -> Result<PathBuf, PrefsError> {
        fs::create_dir_all(data_dir).map_err(|source| PrefsError::Io {
            path: data_dir.to_path_buf(),
            source,
        })?;
        let path = Self::path(data_dir);
        let text = toml::to_string_pretty(self).map_err(|source| PrefsError::Toml {
            path: path.clone(),
            message: source.to_string(),
        })?;
        fs::write(&path, text).map_err(|source| PrefsError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }

    pub fn remember_repo(&mut self, path: &Path) {
        let s = path.to_string_lossy().to_string();
        if !s.trim().is_empty() {
            self.last_repo_path = Some(s);
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PrefsError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("toml error at {path}: {message}")]
    Toml { path: PathBuf, message: String },
}

/// Pick the project folder for a start/stop: an explicit path wins, then the
/// path stored on the before snapshot (so Stop works after the window is
/// closed and reopened), then the last UI preference, then the current directory.
pub fn resolve_repo_path(
    explicit: Option<&Path>,
    session_repo: Option<&str>,
    last_pref: Option<&str>,
    cwd: &Path,
) -> PathBuf {
    if let Some(p) = explicit {
        if !p.as_os_str().is_empty() {
            return p.to_path_buf();
        }
    }
    if let Some(s) = session_repo.map(str::trim).filter(|s| !s.is_empty()) {
        return PathBuf::from(s);
    }
    if let Some(s) = last_pref.map(str::trim).filter(|s| !s.is_empty()) {
        return PathBuf::from(s);
    }
    cwd.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp() -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("leash-prefs-{n}"))
    }

    #[test]
    fn load_missing_file_is_default() {
        let root = tmp();
        let prefs = UiPrefs::load(&root);
        assert_eq!(prefs, UiPrefs::default());
        assert_eq!(prefs.theme, UiTheme::Light);
        assert!(prefs.last_repo_path.is_none());
    }

    #[test]
    fn round_trip_theme_and_last_repo() {
        let root = tmp();
        let mut prefs = UiPrefs {
            theme: UiTheme::Dark,
            ..Default::default()
        };
        prefs.remember_repo(Path::new(r"C:\src\demo"));
        prefs.save(&root).unwrap();
        let loaded = UiPrefs::load(&root);
        assert_eq!(loaded.theme, UiTheme::Dark);
        assert_eq!(loaded.last_repo_path.as_deref(), Some(r"C:\src\demo"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn theme_toggle_and_parse() {
        assert_eq!(UiTheme::Light.toggle(), UiTheme::Dark);
        assert_eq!(UiTheme::parse("DARK"), UiTheme::Dark);
        assert_eq!(UiTheme::parse("nope"), UiTheme::Light);
    }

    #[test]
    fn resolve_prefers_explicit_then_session_then_pref_then_cwd() {
        let cwd = Path::new("/tmp/cwd");
        assert_eq!(
            resolve_repo_path(Some(Path::new("/explicit")), Some("/sess"), Some("/pref"), cwd),
            PathBuf::from("/explicit")
        );
        assert_eq!(
            resolve_repo_path(None, Some("/sess"), Some("/pref"), cwd),
            PathBuf::from("/sess")
        );
        assert_eq!(
            resolve_repo_path(None, Some("  "), Some("/pref"), cwd),
            PathBuf::from("/pref")
        );
        assert_eq!(
            resolve_repo_path(None, None, None, cwd),
            PathBuf::from("/tmp/cwd")
        );
    }
}
