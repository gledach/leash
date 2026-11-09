//! Process-tree tracking and joining of ETW events onto leash findings.
//!
//! The tree, path matching and join logic are platform-independent and are
//! covered by unit tests on Linux. The ETW collector is Windows-only.

mod join;
mod logutil;
mod model;
mod paths;
mod service;
mod tree;

#[cfg(windows)]
pub mod win;
#[cfg(not(windows))]
mod stub;

pub use join::apply_attribution;
pub use logutil::{log, set_logger};
pub use model::*;
pub use paths::{image_basename, names_equal, normalize_path, normalize_registry, paths_match, registry_match};
pub use service::{is_kernel_pid, is_service_image};
pub use tree::{InstantMs, ProcessRecord, ProcessTree};

use leash_snapshot::ProcessEntry;

/// Collect a process snapshot. Empty on non-Windows hosts.
pub fn list_processes() -> Vec<ProcessEntry> {
    #[cfg(windows)]
    {
        win::list_processes()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Top-level user processes suitable for a "Watch a running app" list:
/// skip obvious system images, prefer one row per image name.
pub fn list_watch_candidates() -> Vec<ProcessEntry> {
    let mut procs = list_processes();
    procs.retain(|p| {
        if p.pid <= 4 {
            return false;
        }
        !is_service_image(&p.name)
    });
    procs.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then(a.pid.cmp(&b.pid))
    });
    let mut seen = std::collections::BTreeSet::new();
    procs.retain(|p| seen.insert(p.name.to_ascii_lowercase()));
    procs
}

pub fn process_entry_to_record(p: &ProcessEntry) -> ProcessRecord {
    ProcessRecord {
        pid: p.pid,
        parent_pid: p.ppid,
        start_ms: p.create_time.unwrap_or(0).max(0) as u64 * 1000,
        end_ms: None,
        image: p.name.clone(),
        exe: p.exe.clone(),
        command_line: p.command_line.clone(),
        in_agent_tree: false,
    }
}

/// Parse a command line the way a Windows user would type it into a box:
/// quoted segments, otherwise split on whitespace.
pub fn split_command_line(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    for c in input.trim().chars() {
        match c {
            '"' => in_quote = !in_quote,
            ch if ch.is_whitespace() && !in_quote => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            ch => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests;
