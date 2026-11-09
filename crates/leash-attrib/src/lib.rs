//! Process-tree tracking and joining of ETW events onto leash findings.
//!
//! The tree, path matching and join logic are platform-independent and are
//! covered by unit tests on Linux. The ETW collector is Windows-only.

mod discover;
mod join;
mod logutil;
mod model;
mod paths;
mod picker;
mod resolve;
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
pub use discover::{
    discover_candidates, format_ps, live_views, matches_query, resolve_attach, views_from_entries,
    AppKind, AttachResolution, ProcView, WatchCandidate,
};
pub use resolve::{plan_launch, plan_launch_from_env, LaunchKind, LaunchPlan};
pub use picker::{build_picker, format_ps_rows, PickerModel, PickerRow, WatchSelection};

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

/// Editors and known coding agents, grouped so an agent running inside an
/// editor terminal is listed under that editor. Used by the window and `leash ps`.
pub fn list_watch_candidates() -> Vec<WatchCandidate> {
    discover::discover_candidates(&discover::live_views())
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
