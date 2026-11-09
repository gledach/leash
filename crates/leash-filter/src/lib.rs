//! Ignore and priority rules for leash findings.
//!
//! This crate is platform-independent: paths are normalized to a lowercase
//! form with `/` separators so the same tests run on Linux and Windows.

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Built-in default rules, kept in sync with `config/default-filters.toml`.
pub const DEFAULT_FILTERS_TOML: &str = include_str!("../../../config/default-filters.toml");

/// How serious a kept finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Medium,
    High,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Kind of file change, used when classifying an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Create,
    Delete,
    Rename,
    DataOverwrite,
    DataExtend,
    DataTruncation,
    Other,
}

impl ChangeKind {
    pub fn is_create_or_delete(self) -> bool {
        matches!(self, Self::Create | Self::Delete | Self::Rename)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Delete => "delete",
            Self::Rename => "rename",
            Self::DataOverwrite => "data overwrite",
            Self::DataExtend => "data extend",
            Self::DataTruncation => "data truncation",
            Self::Other => "change",
        }
    }
}

/// Filter-file schema (TOML).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FilterConfig {
    #[serde(default = "default_true")]
    pub exit_on_high: bool,
    #[serde(default)]
    pub ignore_globs: Vec<String>,
    #[serde(default)]
    pub ignore_substrings: Vec<String>,
    #[serde(default)]
    pub ignore_names: Vec<String>,
    #[serde(default)]
    pub high_globs: Vec<String>,
    #[serde(default)]
    pub extra_ignore_globs: Vec<String>,
    #[serde(default)]
    pub extra_ignore_substrings: Vec<String>,
    #[serde(default)]
    pub extra_high_globs: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl FilterConfig {
    pub fn default_rules() -> Self {
        toml::from_str(DEFAULT_FILTERS_TOML).expect("embedded default-filters.toml must parse")
    }

    /// Load a user file. List keys that are present (even if empty) replace the
    /// default list; `extra_*` keys are always appended.
    pub fn load_overlay(text: &str) -> Result<Self, ConfigError> {
        let overlay: FilterConfig =
            toml::from_str(text).map_err(|e| ConfigError::Toml(e.to_string()))?;
        let mut base = Self::default_rules();
        // Detect which list keys the overlay actually set by reparsing to a table.
        let table: toml::Table = text
            .parse()
            .map_err(|e: toml::de::Error| ConfigError::Toml(e.to_string()))?;
        if table.contains_key("exit_on_high") {
            base.exit_on_high = overlay.exit_on_high;
        }
        if table.contains_key("ignore_globs") {
            base.ignore_globs = overlay.ignore_globs;
        }
        if table.contains_key("ignore_substrings") {
            base.ignore_substrings = overlay.ignore_substrings;
        }
        if table.contains_key("ignore_names") {
            base.ignore_names = overlay.ignore_names;
        }
        if table.contains_key("high_globs") {
            base.high_globs = overlay.high_globs;
        }
        base.extra_ignore_globs.extend(overlay.extra_ignore_globs);
        base.extra_ignore_substrings
            .extend(overlay.extra_ignore_substrings);
        base.extra_high_globs.extend(overlay.extra_high_globs);
        Ok(base)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("invalid filter config: {0}")]
    Toml(String),
    #[error("invalid glob {glob}: {message}")]
    Glob { glob: String, message: String },
}

/// Runtime paths that influence ignore / priority decisions.
#[derive(Debug, Clone, Default)]
pub struct FilterContext {
    pub repo: Option<String>,
    pub leash_dir: Option<String>,
    pub user_profile: Option<String>,
    pub appdata_roaming: Option<String>,
    pub startup_dirs: Vec<String>,
    pub temp_dirs: Vec<String>,
    pub other_project_roots: Vec<String>,
}

/// Decision for a single path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathDecision {
    Ignore { reason: String },
    Keep { severity: Severity, reason: String },
}

impl PathDecision {
    pub fn is_ignore(&self) -> bool {
        matches!(self, Self::Ignore { .. })
    }
}

/// Compiled, testable rule set.
#[derive(Debug)]
pub struct FilterEngine {
    pub config: FilterConfig,
    ignore_globs: GlobSet,
    high_globs: GlobSet,
    context: FilterContext,
}

impl FilterEngine {
    pub fn new(config: FilterConfig, context: FilterContext) -> Result<Self, ConfigError> {
        let mut ignore = config.ignore_globs.clone();
        ignore.extend(config.extra_ignore_globs.iter().cloned());
        let mut high = config.high_globs.clone();
        high.extend(config.extra_high_globs.iter().cloned());
        Ok(Self {
            ignore_globs: compile_globs(&ignore)?,
            high_globs: compile_globs(&high)?,
            config,
            context,
        })
    }

    pub fn with_defaults(context: FilterContext) -> Self {
        Self::new(FilterConfig::default_rules(), context)
            .expect("default filter rules must compile")
    }

    pub fn classify_path(&self, path: &str, kind: ChangeKind) -> PathDecision {
        let norm = normalize_path(path);
        if norm.is_empty() {
            return PathDecision::Keep {
                severity: Severity::Info,
                reason: "path could not be resolved; showing the raw journal name".into(),
            };
        }

        if let Some(repo) = self.context.repo.as_deref() {
            if is_within(&norm, repo) && !self.high_globs.is_match(&norm) && !looks_secret(&norm) {
                return PathDecision::Ignore {
                    reason: "inside the watched repo (ignored by design in v1)".into(),
                };
            }
        }
        if let Some(dir) = self.context.leash_dir.as_deref() {
            if is_within(&norm, dir) {
                return PathDecision::Ignore {
                    reason: "leash's own data directory".into(),
                };
            }
        }
        for temp in &self.context.temp_dirs {
            if is_within(&norm, temp) {
                return PathDecision::Ignore {
                    reason: "temporary directory".into(),
                };
            }
        }

        let name = file_name(&norm);
        for ignore_name in self
            .config
            .ignore_names
            .iter()
            .chain(std::iter::once(&"$mft".to_string()))
        {
            if name == ignore_name.to_lowercase() {
                return PathDecision::Ignore {
                    reason: "NTFS metadata or known paging/dump file".into(),
                };
            }
        }

        if self.ignore_globs.is_match(&norm) {
            return PathDecision::Ignore {
                reason: "matches an ignore glob (cache, VCS, or toolchain noise)".into(),
            };
        }

        let mut substrings = self.config.ignore_substrings.clone();
        substrings.extend(self.config.extra_ignore_substrings.iter().cloned());
        for raw in &substrings {
            let needle = normalize_path(raw);
            if !needle.is_empty() && norm.contains(&needle) {
                return PathDecision::Ignore {
                    reason: format!("matches ignored path fragment '{raw}'"),
                };
            }
        }

        // Most of C:\Windows is OS noise; keep Task Scheduler folders.
        if windows_os_noise(&norm) {
            return PathDecision::Ignore {
                reason: "Windows system directory (prefetch, logs, and general OS noise)".into(),
            };
        }

        if self.high_globs.is_match(&norm) || looks_secret(&norm) {
            return PathDecision::Keep {
                severity: Severity::High,
                reason: "secret-looking or shell/git/ssh-related file".into(),
            };
        }

        for startup in &self.context.startup_dirs {
            if is_within(&norm, startup) {
                return PathDecision::Keep {
                    severity: Severity::High,
                    reason: "Startup folder — this file can run at logon".into(),
                };
            }
        }

        if let Some(roaming) = self.context.appdata_roaming.as_deref() {
            if is_within(&norm, roaming) {
                return PathDecision::Keep {
                    severity: Severity::High,
                    reason: "AppData\\Roaming — user-level app config and common persistence"
                        .into(),
                };
            }
        }

        if let Some(home) = self.context.user_profile.as_deref() {
            if is_direct_child(&norm, home) && name.starts_with('.') {
                return PathDecision::Keep {
                    severity: Severity::High,
                    reason: "dotfile in the user profile".into(),
                };
            }
        }

        for root in &self.context.other_project_roots {
            if is_within(&norm, root) {
                return PathDecision::Keep {
                    severity: Severity::High,
                    reason: "change in another project folder, outside the watched repo".into(),
                };
            }
        }

        if kind.is_create_or_delete() {
            if let Some(home) = self.context.user_profile.as_deref() {
                if is_within(&norm, home) {
                    return PathDecision::Keep {
                        severity: Severity::High,
                        reason: "create/delete under the user profile, outside the watched repo"
                            .into(),
                    };
                }
            }
            return PathDecision::Keep {
                severity: Severity::High,
                reason: "create/delete outside the watched repo".into(),
            };
        }

        if let Some(home) = self.context.user_profile.as_deref() {
            if is_within(&norm, home) {
                return PathDecision::Keep {
                    severity: Severity::Medium,
                    reason: "file content change under the user profile".into(),
                };
            }
        }

        PathDecision::Keep {
            severity: Severity::Info,
            reason: "file change that did not match a higher-priority rule".into(),
        }
    }
}

fn compile_globs(patterns: &[String]) -> Result<GlobSet, ConfigError> {
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        let glob = Glob::new(&pat.to_lowercase()).map_err(|e| ConfigError::Glob {
            glob: pat.clone(),
            message: e.to_string(),
        })?;
        builder.add(glob);
    }
    builder.build().map_err(|e| ConfigError::Glob {
        glob: "*".into(),
        message: e.to_string(),
    })
}

/// Lowercase, `/`-separated path. Strips the `\\?\` / `//?/` prefix and a
/// trailing slash. Drive letters stay (`c:/users/alek`).
pub fn normalize_path(path: &str) -> String {
    let mut p = path.trim().replace('\\', "/");
    if let Some(stripped) = p.strip_prefix("//?/") {
        p = stripped.to_string();
    }
    if let Some(stripped) = p.strip_prefix("//./") {
        p = stripped.to_string();
    }
    // UNC or already-forward form of \\?\
    if let Some(stripped) = p.strip_prefix("\\\\?\\") {
        p = stripped.replace('\\', "/");
    }
    p = p.to_lowercase();
    while p.ends_with('/') && p.len() > 3 {
        p.pop();
    }
    p
}

pub fn is_within(child: &str, parent: &str) -> bool {
    let c = normalize_path(child);
    let p = normalize_path(parent);
    if p.is_empty() {
        return false;
    }
    c == p || c.starts_with(&format!("{p}/"))
}

fn is_direct_child(child: &str, parent: &str) -> bool {
    let c = normalize_path(child);
    let p = normalize_path(parent);
    match c.strip_prefix(&format!("{p}/")) {
        Some(rest) => !rest.is_empty() && !rest.contains('/'),
        None => false,
    }
}

fn file_name(norm: &str) -> String {
    norm.rsplit('/').next().unwrap_or(norm).to_string()
}

fn windows_os_noise(norm: &str) -> bool {
    // Only the OS tree at a volume root (c:/windows/...), not e.g.
    // AppData/Roaming/Microsoft/Windows/Start Menu.
    let keep = [
        "/windows/system32/tasks",
        "/windows/tasks",
        "/windows/syswow64/tasks",
    ];
    if keep.iter().any(|k| norm.contains(k)) {
        return false;
    }
    os_windows_root(norm)
}

fn os_windows_root(norm: &str) -> bool {
    // "c:/windows" or "c:/windows/..." or "c:/winnt/..."
    if norm.len() < 10 {
        return false;
    }
    let bytes = norm.as_bytes();
    if bytes.first().is_some_and(|c| c.is_ascii_alphabetic()) && bytes.get(1) == Some(&b':') {
        let rest = &norm[2..];
        return rest == "/windows"
            || rest.starts_with("/windows/")
            || rest == "/winnt"
            || rest.starts_with("/winnt/");
    }
    false
}

fn looks_secret(norm: &str) -> bool {
    let name = file_name(norm);
    name == ".env"
        || name.starts_with(".env.")
        || name == ".envrc"
        || name.ends_with(".pem")
        || name == "authorized_keys"
        || name == "id_rsa"
        || name == "id_ed25519"
        || name.contains("secret")
        || name.contains("credentials")
}

/// Guess common project roots under a parent folder (used by the fallback
/// scanner and for "other projects" classification). A directory counts if it
/// contains a well-known project marker.
pub fn looks_like_project_root(dir: &Path) -> bool {
    const MARKERS: &[&str] = &[
        ".git",
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
        "pom.xml",
        "sln",
        ".sln",
    ];
    if !dir.is_dir() {
        return false;
    }
    for marker in MARKERS {
        if *marker == "sln" {
            continue;
        }
        if dir.join(marker).exists() {
            return true;
        }
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for ent in rd.flatten() {
            if ent
                .path()
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("sln"))
            {
                return true;
            }
        }
    }
    false
}

/// Convenience: build default temp / startup / roaming paths from a user profile
/// and Windows-style environment values (works with synthetic strings in tests).
pub fn context_from_env(
    repo: Option<&Path>,
    leash_dir: Option<&Path>,
    user_profile: Option<&Path>,
    local_app_data: Option<&Path>,
    appdata: Option<&Path>,
    temp: Option<&Path>,
    other_projects: Vec<PathBuf>,
) -> FilterContext {
    let mut startup_dirs = Vec::new();
    if let Some(app) = appdata {
        startup_dirs.push(
            app.join("Microsoft/Windows/Start Menu/Programs/Startup")
                .to_string_lossy()
                .into_owned(),
        );
    }
    let mut temp_dirs = Vec::new();
    if let Some(t) = temp {
        temp_dirs.push(t.to_string_lossy().into_owned());
    }
    if let Some(local) = local_app_data {
        temp_dirs.push(local.join("Temp").to_string_lossy().into_owned());
    }
    FilterContext {
        repo: repo.map(|p| p.to_string_lossy().into_owned()),
        leash_dir: leash_dir.map(|p| p.to_string_lossy().into_owned()),
        user_profile: user_profile.map(|p| p.to_string_lossy().into_owned()),
        appdata_roaming: appdata.map(|p| p.to_string_lossy().into_owned()),
        startup_dirs,
        temp_dirs,
        other_project_roots: other_projects
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn engine() -> FilterEngine {
        let ctx = context_from_env(
            Some(Path::new(r"C:\Users\Alek\src\myapp")),
            Some(Path::new(r"C:\Users\Alek\AppData\Local\leash")),
            Some(Path::new(r"C:\Users\Alek")),
            Some(Path::new(r"C:\Users\Alek\AppData\Local")),
            Some(Path::new(r"C:\Users\Alek\AppData\Roaming")),
            Some(Path::new(r"C:\Users\Alek\AppData\Local\Temp")),
            vec![PathBuf::from(r"C:\Users\Alek\src\other")],
        );
        FilterEngine::with_defaults(ctx)
    }

    fn keep(path: &str, kind: ChangeKind) -> (Severity, String) {
        match engine().classify_path(path, kind) {
            PathDecision::Keep { severity, reason } => (severity, reason),
            PathDecision::Ignore { reason } => {
                panic!("expected keep for {path}, ignored: {reason}")
            }
        }
    }

    fn ignored(path: &str) -> String {
        match engine().classify_path(path, ChangeKind::Create) {
            PathDecision::Ignore { reason } => reason,
            PathDecision::Keep { severity, reason } => {
                panic!("expected ignore for {path}, got {severity}: {reason}")
            }
        }
    }

    #[test]
    fn default_toml_parses_and_compiles() {
        let cfg = FilterConfig::default_rules();
        assert!(cfg.exit_on_high);
        assert!(cfg.ignore_globs.iter().any(|g| g.contains("node_modules")));
        FilterEngine::with_defaults(FilterContext::default());
    }

    #[test]
    fn normalize_strips_extended_prefix_and_slashes() {
        assert_eq!(
            normalize_path(r"\\?\C:\Users\Alek\file.txt"),
            "c:/users/alek/file.txt"
        );
        assert_eq!(
            normalize_path(r"C:\Users\Alek\file.txt"),
            "c:/users/alek/file.txt"
        );
        assert_eq!(normalize_path("C:/Users/Alek/"), "c:/users/alek");
    }

    #[test]
    fn is_within_is_case_insensitive() {
        assert!(is_within(
            r"C:\Users\Alek\src\myapp\src\main.rs",
            r"c:\users\alek\src\myapp"
        ));
        assert!(!is_within(
            r"C:\Users\Alek\src\myapp-other\x",
            r"C:\Users\Alek\src\myapp"
        ));
    }

    #[test]
    fn ignores_the_watched_repo() {
        let reason = ignored(r"C:\Users\Alek\src\myapp\src\main.rs");
        assert!(reason.contains("watched repo"));
    }

    #[test]
    fn high_pattern_inside_repo_is_kept() {
        let (sev, reason) = keep(r"C:\Users\Alek\src\myapp\.env", ChangeKind::DataOverwrite);
        assert_eq!(sev, Severity::High);
        assert!(reason.contains("secret") || reason.contains("shell"));
    }

    #[test]
    fn ignores_node_modules_and_git() {
        ignored(r"C:\Users\Alek\src\other\node_modules\foo\index.js");
        ignored(r"C:\Users\Alek\src\other\.git\objects\aa\bb");
    }

    #[test]
    fn ignores_temp_prefetch_and_leash_dir() {
        ignored(r"C:\Users\Alek\AppData\Local\Temp\abc.tmp");
        ignored(r"C:\Windows\Prefetch\FOO.pf");
        ignored(r"C:\Windows\Logs\CBS\CBS.log");
        ignored(r"C:\Users\Alek\AppData\Local\leash\sessions\1\before.json");
    }

    #[test]
    fn ignores_browser_and_editor_caches() {
        ignored(r"C:\Users\Alek\AppData\Local\Google\Chrome\User Data\Default\Cache\data_0");
        ignored(r"C:\Users\Alek\AppData\Roaming\Cursor\Cache\Cache_Data\f");
        ignored(r"C:\Users\Alek\AppData\Local\npm-cache\_cacache\x");
    }

    #[test]
    fn ignores_ntfs_metadata_names() {
        ignored("$MFT");
        ignored("pagefile.sys");
    }

    #[test]
    fn ignores_generic_windows_tree_but_not_tasks() {
        ignored(r"C:\Windows\System32\drivers\etc\hosts.bak");
        let (sev, _) = keep(
            r"C:\Windows\System32\Tasks\OddAgentTask.xml",
            ChangeKind::Create,
        );
        assert_eq!(sev, Severity::High);
    }

    #[test]
    fn env_and_ssh_are_high() {
        assert_eq!(
            keep(r"C:\Users\Alek\projects\x\.env.local", ChangeKind::Create).0,
            Severity::High
        );
        assert_eq!(
            keep(
                r"C:\Users\Alek\.ssh\authorized_keys",
                ChangeKind::DataOverwrite
            )
            .0,
            Severity::High
        );
        assert_eq!(
            keep(r"C:\Users\Alek\.gitconfig", ChangeKind::DataOverwrite).0,
            Severity::High
        );
    }

    #[test]
    fn roaming_and_startup_are_high() {
        assert_eq!(
            keep(
                r"C:\Users\Alek\AppData\Roaming\MyApp\settings.json",
                ChangeKind::Create
            )
            .0,
            Severity::High
        );
        assert_eq!(
            keep(
                r"C:\Users\Alek\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup\hook.lnk",
                ChangeKind::Create
            )
            .0,
            Severity::High
        );
    }

    #[test]
    fn other_project_create_is_high() {
        let (sev, reason) = keep(r"C:\Users\Alek\src\other\README.md", ChangeKind::Delete);
        assert_eq!(sev, Severity::High);
        assert!(reason.contains("another project") || reason.contains("user profile"));
    }

    #[test]
    fn profile_dotfile_is_high() {
        assert_eq!(
            keep(r"C:\Users\Alek\.bashrc", ChangeKind::DataOverwrite).0,
            Severity::High
        );
    }

    #[test]
    fn data_change_outside_home_is_info() {
        let (sev, _) = keep(r"D:\scratch\notes.txt", ChangeKind::DataOverwrite);
        assert_eq!(sev, Severity::Info);
    }

    #[test]
    fn create_outside_repo_is_high() {
        let (sev, _) = keep(r"D:\scratch\new.exe", ChangeKind::Create);
        assert_eq!(sev, Severity::High);
    }

    #[test]
    fn overlay_appends_extra_globs_and_can_replace_lists() {
        let cfg = FilterConfig::load_overlay(
            r#"
extra_ignore_globs = ["**/vendor/**"]
exit_on_high = false
"#,
        )
        .unwrap();
        assert!(!cfg.exit_on_high);
        assert!(cfg.ignore_globs.iter().any(|g| g.contains("node_modules")));
        assert!(cfg.extra_ignore_globs.iter().any(|g| g.contains("vendor")));

        let replaced = FilterConfig::load_overlay(
            r#"
ignore_globs = ["**/only-this/**"]
"#,
        )
        .unwrap();
        assert_eq!(replaced.ignore_globs, vec!["**/only-this/**".to_string()]);
    }

    #[test]
    fn invalid_glob_is_reported() {
        let mut cfg = FilterConfig::default_rules();
        cfg.ignore_globs.push("**/[broken".into());
        let err = FilterEngine::new(cfg, FilterContext::default()).unwrap_err();
        match err {
            ConfigError::Glob { glob, .. } => assert!(glob.contains("broken")),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn user_config_extra_ignore_is_honoured() {
        let mut cfg = FilterConfig::default_rules();
        cfg.extra_ignore_globs.push("**/vendor/**".into());
        let engine = FilterEngine::new(cfg, FilterContext::default()).unwrap();
        match engine.classify_path(r"C:\work\app\vendor\x.c", ChangeKind::Create) {
            PathDecision::Ignore { .. } => {}
            other => panic!("expected ignore, got {other:?}"),
        }
    }
}
