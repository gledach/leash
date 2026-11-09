use crate::SNAPSHOT_VERSION;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use leash_filter::{ChangeKind, Severity};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotLabel {
    Before,
    After,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileMode {
    Usn,
    FallbackScan,
    Mixed,
    #[default]
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Snapshot {
    pub version: u32,
    pub label: SnapshotLabel,
    pub session_id: String,
    pub taken_at: String,
    pub hostname: Option<String>,
    pub username: Option<String>,
    pub elevated: bool,
    pub file_mode: FileMode,
    pub repo_path: Option<String>,
    pub warnings: Vec<String>,
    pub usn: Vec<UsnVolumeState>,
    pub file_events: Vec<FileEvent>,
    pub fallback_files: Vec<FileInventoryEntry>,
    pub run_keys: Vec<RunKeyEntry>,
    pub scheduled_tasks: Vec<ScheduledTask>,
    pub services: Vec<ServiceEntry>,
    pub env_user: BTreeMap<String, String>,
    pub env_system: BTreeMap<String, String>,
    pub packages: PackageSets,
    pub profiles: Vec<HashedFile>,
    pub git_configs: Vec<HashedFile>,
    pub ssh: SshSnapshot,
    pub ports: Vec<ListeningPort>,
    pub processes: Vec<ProcessEntry>,
}

impl Snapshot {
    pub fn new(label: SnapshotLabel, session_id: impl Into<String>) -> Self {
        Self {
            version: SNAPSHOT_VERSION,
            label,
            session_id: session_id.into(),
            taken_at: String::new(),
            hostname: None,
            username: None,
            elevated: false,
            file_mode: FileMode::Unavailable,
            repo_path: None,
            warnings: Vec::new(),
            usn: Vec::new(),
            file_events: Vec::new(),
            fallback_files: Vec::new(),
            run_keys: Vec::new(),
            scheduled_tasks: Vec::new(),
            services: Vec::new(),
            env_user: BTreeMap::new(),
            env_system: BTreeMap::new(),
            packages: PackageSets::default(),
            profiles: Vec::new(),
            git_configs: Vec::new(),
            ssh: SshSnapshot::default(),
            ports: Vec::new(),
            processes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsnVolumeState {
    pub volume: String,
    pub journal_id: u64,
    pub start_usn: i64,
    pub next_usn: i64,
    pub first_usn: i64,
    pub lowest_valid_usn: i64,
    pub query_ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEvent {
    pub volume: Option<String>,
    pub path: Option<String>,
    pub name: String,
    pub old_path: Option<String>,
    pub kind: ChangeKind,
    pub reasons_raw: u32,
    pub usn: Option<i64>,
    pub file_id: Option<String>,
    pub is_dir: bool,
    pub timestamp: Option<String>,
}

impl FileEvent {
    pub fn display_path(&self) -> String {
        let raw = self
            .path
            .clone()
            .or_else(|| self.old_path.clone())
            .unwrap_or_else(|| self.name.clone());
        leash_filter::pretty_path(&raw)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileInventoryEntry {
    pub path: String,
    pub size: u64,
    pub mtime: i64,
    pub sha256: Option<String>,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunKeyEntry {
    pub hive: String,
    pub key: String,
    pub name: String,
    pub value: String,
}

impl RunKeyEntry {
    pub fn id(&self) -> String {
        format!("{}\\{}\\{}", self.hive, self.key, self.name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduledTask {
    pub path: String,
    pub name: String,
    pub enabled: Option<bool>,
    pub command: Option<String>,
    pub arguments: Option<String>,
    pub user: Option<String>,
}

impl ScheduledTask {
    pub fn id(&self) -> String {
        if self.path.ends_with(&self.name) || self.path.ends_with(&format!("\\{}", self.name)) {
            self.path.clone()
        } else {
            format!("{}\\{}", self.path.trim_end_matches('\\'), self.name)
        }
    }

    pub fn command_line(&self) -> String {
        match (&self.command, &self.arguments) {
            (Some(c), Some(a)) if !a.is_empty() => format!("{c} {a}"),
            (Some(c), _) => c.clone(),
            _ => String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceEntry {
    pub name: String,
    pub display_name: Option<String>,
    pub start_type: Option<String>,
    pub state: Option<String>,
    pub binary_path: Option<String>,
    pub account: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackageSets {
    pub npm: Vec<Package>,
    pub pnpm: Vec<Package>,
    pub pip: Vec<Package>,
    pub cargo: Vec<Package>,
    /// Tools that were missing or failed, so the set may be incomplete.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Package {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HashedFile {
    pub path: String,
    pub exists: bool,
    pub sha256: Option<String>,
    pub size: Option<u64>,
    pub mtime: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SshSnapshot {
    pub config: Option<HashedFile>,
    pub authorized_keys: Option<HashedFile>,
    /// File names + hashes only; never the key material.
    pub key_files: Vec<HashedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ListeningPort {
    pub protocol: String,
    pub local_addr: String,
    pub local_port: u16,
    pub pid: u32,
    pub process_name: Option<String>,
    pub state: Option<String>,
}

impl ListeningPort {
    pub fn id(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.protocol, self.local_addr, self.local_port, self.pid
        )
    }

    pub fn bind(&self) -> String {
        format!(
            "{} {}:{}/{}",
            self.protocol, self.local_addr, self.local_port, self.pid
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessEntry {
    pub pid: u32,
    pub ppid: Option<u32>,
    pub name: String,
    pub exe: Option<String>,
    pub command_line: Option<String>,
    pub create_time: Option<i64>,
}

impl ProcessEntry {
    pub fn identity(&self) -> String {
        match self.create_time {
            Some(t) => format!("{}-{}", self.pid, t),
            None => format!("{}-{}", self.pid, self.name.to_lowercase()),
        }
    }
}

/// A single reported finding after filtering.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Finding {
    pub category: String,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    pub reason: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffReport {
    pub session_id: String,
    pub before_at: String,
    pub after_at: String,
    pub repo_path: Option<String>,
    pub file_mode: FileMode,
    pub elevated: bool,
    pub warnings: Vec<String>,
    pub findings: Vec<Finding>,
    pub ignored: IgnoreSummary,
    pub counts: SeverityCounts,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct IgnoreSummary {
    pub total: usize,
    pub by_reason: BTreeMap<String, usize>,
    pub examples: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SeverityCounts {
    pub high: usize,
    pub medium: usize,
    pub info: usize,
}

impl SeverityCounts {
    pub fn tally(findings: &[Finding]) -> Self {
        let mut c = Self::default();
        for f in findings {
            match f.severity {
                Severity::High => c.high += 1,
                Severity::Medium => c.medium += 1,
                Severity::Info => c.info += 1,
            }
        }
        c
    }
}

/// Split a Windows PATH (or similarly delimited) value into entries.
pub fn split_path_var(value: &str) -> Vec<String> {
    let delim = if looks_windows_path(value) { ';' } else { ':' };
    value
        .split(delim)
        .map(|s| s.trim().trim_end_matches(['\\', '/']).to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn looks_windows_path(value: &str) -> bool {
    if value.contains(';') {
        return true;
    }
    let lower = value.to_ascii_lowercase();
    lower.contains("%systemroot%") || lower.contains("%userprofile%") || drive_slash(value)
}

fn drive_slash(value: &str) -> bool {
    // "C:\..." or "C:/..." — a drive letter at the start of an entry, not Unix ":/" .
    let b = value.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}
