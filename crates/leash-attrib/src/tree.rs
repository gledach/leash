//! Agent process tree. Host-independent, so it can be unit-tested on Linux.
//!
//! A process is in the tree when it is a declared root, or when a recorded
//! parent was in the tree at the child's start time. PID reuse is handled by
//! keeping start and end timestamps on every record.

use crate::paths::{image_basename, names_equal};
use serde::{Deserialize, Serialize};

/// Milliseconds since Unix epoch. Zero means "unknown"; unknown times still
/// match a live record when no overlapping interval exists.
pub type InstantMs = u64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessRecord {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub start_ms: InstantMs,
    pub end_ms: Option<InstantMs>,
    pub image: String,
    pub exe: Option<String>,
    pub command_line: Option<String>,
    pub in_agent_tree: bool,
}

impl ProcessRecord {
    pub fn display_name(&self) -> String {
        if !self.image.is_empty() {
            self.image.clone()
        } else if let Some(exe) = &self.exe {
            image_basename(exe)
        } else {
            format!("pid {}", self.pid)
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProcessTree {
    /// Root pids as originally declared (spawned child, --pid, or attach names).
    pub roots: Vec<u32>,
    pub records: Vec<ProcessRecord>,
}

impl ProcessTree {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_root(&mut self, mut rec: ProcessRecord) {
        rec.in_agent_tree = true;
        if !self.roots.contains(&rec.pid) {
            self.roots.push(rec.pid);
        }
        self.upsert(rec);
    }

    /// Attach every live process whose image matches `name` (with or without
    /// `.exe`) as a root, plus later descendants.
    pub fn add_roots_named(&mut self, name: &str, snapshot: &[ProcessRecord], at: InstantMs) {
        let _ = at;
        for rec in snapshot {
            if names_equal(&rec.image, name)
                || rec
                    .exe
                    .as_deref()
                    .is_some_and(|exe| names_equal(exe, name))
            {
                self.add_root(rec.clone());
            }
        }
    }

    pub fn on_start(&mut self, mut rec: ProcessRecord) {
        let at = rec.start_ms;
        rec.in_agent_tree = rec.in_agent_tree
            || self.roots.contains(&rec.pid)
            || rec
                .parent_pid
                .is_some_and(|ppid| self.is_agent_at(ppid, at.saturating_sub(1).max(1.min(at))));
        // If the parent started at the same millisecond, still count it.
        if !rec.in_agent_tree {
            if let Some(ppid) = rec.parent_pid {
                rec.in_agent_tree = self.is_agent_at(ppid, at);
            }
        }
        self.upsert(rec);
    }

    pub fn on_exit(&mut self, pid: u32, at: InstantMs) {
        if let Some(rec) = self.live_record_mut(pid, at) {
            rec.end_ms = Some(at);
        }
    }

    /// Fold a Toolhelp (or similar) snapshot into the tree. New children of
    /// current agent processes are added; unknown processes are recorded but
    /// not marked as the agent.
    pub fn ingest_snapshot(&mut self, snapshot: &[ProcessRecord], at: InstantMs) {
        // First pass: new roots already declared by pid, plus children of
        // known agent processes. Repeat a few times so a grandchild in the
        // same snapshot is picked up even if it appears before its parent.
        for _ in 0..8 {
            let mut added = false;
            for rec in snapshot {
                if self.lookup(rec.pid, at).is_some() {
                    continue;
                }
                let mut rec = rec.clone();
                rec.start_ms = if rec.start_ms == 0 { at } else { rec.start_ms };
                let is_root = self.roots.contains(&rec.pid);
                let parent_in = rec
                    .parent_pid
                    .is_some_and(|ppid| self.is_agent_at(ppid, rec.start_ms.max(at)));
                rec.in_agent_tree = is_root || parent_in;
                if rec.in_agent_tree || is_root {
                    self.upsert(rec);
                    added = true;
                }
            }
            if !added {
                break;
            }
        }
        // Record everyone else (for "other app" labels) without marking them
        // as the agent. Skip if we already have a live record.
        for rec in snapshot {
            if self.lookup(rec.pid, at).is_none() {
                let mut rec = rec.clone();
                rec.in_agent_tree = false;
                if rec.start_ms == 0 {
                    rec.start_ms = at;
                }
                self.upsert(rec);
            }
        }
    }

    pub fn is_agent_at(&self, pid: u32, at: InstantMs) -> bool {
        self.lookup(pid, at).is_some_and(|r| r.in_agent_tree)
    }

    pub fn lookup(&self, pid: u32, at: InstantMs) -> Option<&ProcessRecord> {
        let mut best: Option<&ProcessRecord> = None;
        for rec in &self.records {
            if rec.pid != pid {
                continue;
            }
            if interval_contains(rec, at) {
                return Some(rec);
            }
            // Fall back to the latest record for this pid if times are unknown.
            if rec.start_ms == 0 && rec.end_ms.is_none() {
                best = Some(rec);
            }
        }
        best
    }

    pub fn live_agent_pids(&self, at: InstantMs) -> Vec<u32> {
        let mut pids: Vec<u32> = self
            .records
            .iter()
            .filter(|r| r.in_agent_tree && interval_contains(r, at) && r.end_ms.is_none())
            .map(|r| r.pid)
            .collect();
        pids.sort_unstable();
        pids.dedup();
        pids
    }

    /// `node.exe ← claude.exe`, walking parents while they stay in the tree.
    pub fn chain(&self, pid: u32, at: InstantMs) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        let mut seen = Vec::new();
        let mut cur = pid;
        for _ in 0..16 {
            if seen.contains(&cur) {
                break;
            }
            seen.push(cur);
            let Some(rec) = self.lookup(cur, at) else {
                break;
            };
            parts.push(rec.display_name());
            if !rec.in_agent_tree {
                break;
            }
            match rec.parent_pid {
                Some(ppid) if self.is_agent_at(ppid, rec.start_ms.max(1)) => cur = ppid,
                _ => break,
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" ← "))
        }
    }

    pub fn agent_label(&self) -> Option<String> {
        let names: Vec<String> = self
            .roots
            .iter()
            .filter_map(|pid| {
                self.records
                    .iter()
                    .rev()
                    .find(|r| r.pid == *pid)
                    .map(|r| format!("{} (pid {pid})", r.display_name()))
            })
            .collect();
        if names.is_empty() {
            None
        } else {
            Some(names.join(", "))
        }
    }

    fn live_record_mut(&mut self, pid: u32, at: InstantMs) -> Option<&mut ProcessRecord> {
        self.records
            .iter_mut()
            .rev()
            .find(|r| r.pid == pid && interval_contains(r, at) && r.end_ms.is_none())
    }

    fn upsert(&mut self, rec: ProcessRecord) {
        if let Some(existing) = self
            .records
            .iter_mut()
            .rev()
            .find(|r| r.pid == rec.pid && r.end_ms.is_none() && intervals_overlap(r, &rec))
        {
            if existing.image.is_empty() && !rec.image.is_empty() {
                existing.image = rec.image;
            }
            if existing.exe.is_none() {
                existing.exe = rec.exe;
            }
            if existing.command_line.is_none() {
                existing.command_line = rec.command_line;
            }
            if rec.in_agent_tree {
                existing.in_agent_tree = true;
            }
            if existing.parent_pid.is_none() {
                existing.parent_pid = rec.parent_pid;
            }
            if existing.start_ms == 0 {
                existing.start_ms = rec.start_ms;
            }
            return;
        }
        self.records.push(rec);
    }
}

fn interval_contains(rec: &ProcessRecord, at: InstantMs) -> bool {
    if at == 0 {
        return rec.end_ms.is_none();
    }
    let start = rec.start_ms;
    if start != 0 && at < start {
        return false;
    }
    match rec.end_ms {
        Some(end) if end != 0 => at < end,
        _ => true,
    }
}

fn intervals_overlap(a: &ProcessRecord, b: &ProcessRecord) -> bool {
    let a_start = a.start_ms;
    let b_start = b.start_ms;
    let a_end = a.end_ms.unwrap_or(u64::MAX);
    let b_end = b.end_ms.unwrap_or(u64::MAX);
    a_start < b_end && b_start < a_end || (a_start == 0 && b_start == 0)
}
