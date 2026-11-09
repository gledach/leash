//! Attribution records stored with a session and copied onto each finding.

use crate::tree::ProcessTree;
use serde::{Deserialize, Serialize};

pub use leash_snapshot::{
    AttributionBucket, AttributionStatus, AttributionSummary, FindingAttribution,
    SYSTEM_WIDE_CAVEAT,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceOp {
    Create,
    Write,
    Delete,
    Rename,
    SetInfo,
    SetValue,
    DeleteValue,
    CreateKey,
    DeleteKey,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileTraceEvent {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub op: TraceOp,
    pub pid: u32,
    /// Unix time in milliseconds.
    pub at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegistryTraceEvent {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub op: TraceOp,
    pub pid: u32,
    pub at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceLog {
    pub status: AttributionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default)]
    pub tree: ProcessTree,
    #[serde(default)]
    pub file_events: Vec<FileTraceEvent>,
    #[serde(default)]
    pub registry_events: Vec<RegistryTraceEvent>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub dropped_other_events: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Default for TraceLog {
    fn default() -> Self {
        Self {
            status: AttributionStatus::Off,
            target: None,
            tree: ProcessTree::new(),
            file_events: Vec::new(),
            registry_events: Vec::new(),
            warnings: Vec::new(),
            dropped_other_events: 0,
            error: None,
        }
    }
}

impl TraceLog {
    pub fn off() -> Self {
        Self::default()
    }

    pub fn unavailable(why: impl Into<String>) -> Self {
        let why = why.into();
        Self {
            status: AttributionStatus::Unavailable,
            error: Some(why),
            ..Self::default()
        }
    }
}

/// What a session should watch, if anything.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WatchTarget {
    Command {
        argv: Vec<String>,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
    },
    Pid {
        pid: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Names {
        names: Vec<String>,
        #[serde(default)]
        pids: Vec<u32>,
    },
}

impl WatchTarget {
    pub fn label(&self) -> String {
        match self {
            Self::Command { argv, pid, .. } => {
                let cmd = argv.first().map(|s| s.as_str()).unwrap_or("command");
                match pid {
                    Some(pid) => format!("{cmd} (pid {pid})"),
                    None => cmd.to_string(),
                }
            }
            Self::Pid { pid, name } => match name {
                Some(n) => format!("{n} (pid {pid})"),
                None => format!("pid {pid}"),
            },
            Self::Names { names, pids } => {
                if pids.is_empty() {
                    names.join(", ")
                } else {
                    format!("{} (pids {})", names.join(", "), join_pids(pids))
                }
            }
        }
    }
}

fn join_pids(pids: &[u32]) -> String {
    pids.iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchFile {
    pub session_id: String,
    pub target: WatchTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watcher_pid: Option<u32>,
    #[serde(default)]
    pub elevated: bool,
}

use std::path::{Path, PathBuf};

pub fn watch_path(session_dir: &Path) -> PathBuf {
    session_dir.join("watch.json")
}

pub fn attrib_path(session_dir: &Path) -> PathBuf {
    session_dir.join("attrib.json")
}

pub fn stop_flag_path(session_dir: &Path) -> PathBuf {
    session_dir.join("attrib.stop")
}

pub fn write_watch(session_dir: &Path, watch: &WatchFile) -> anyhow::Result<()> {
    std::fs::create_dir_all(session_dir)?;
    std::fs::write(watch_path(session_dir), serde_json::to_vec_pretty(watch)?)?;
    Ok(())
}

pub fn read_watch(session_dir: &Path) -> anyhow::Result<Option<WatchFile>> {
    let path = watch_path(session_dir);
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)?;
    Ok(Some(serde_json::from_str(&text)?))
}

pub fn write_trace(session_dir: &Path, log: &TraceLog) -> anyhow::Result<()> {
    std::fs::create_dir_all(session_dir)?;
    std::fs::write(attrib_path(session_dir), serde_json::to_vec_pretty(log)?)?;
    Ok(())
}

pub fn read_trace(session_dir: &Path) -> anyhow::Result<Option<TraceLog>> {
    let path = attrib_path(session_dir);
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)?;
    Ok(Some(serde_json::from_str(&text)?))
}

pub fn request_stop(session_dir: &Path) -> anyhow::Result<()> {
    std::fs::write(stop_flag_path(session_dir), b"stop\n")?;
    #[cfg(windows)]
    {
        crate::win::signal_stop_event(session_dir);
    }
    Ok(())
}

pub fn stop_requested(session_dir: &Path) -> bool {
    stop_flag_path(session_dir).exists()
}

pub fn clear_stop(session_dir: &Path) {
    let _ = std::fs::remove_file(stop_flag_path(session_dir));
}
