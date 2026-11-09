//! Registry, Task Scheduler, services, environment, ports, processes,
//! and hashed shell/git/ssh files.

mod files;
mod tasks_xml;

#[cfg(not(windows))]
mod stub;
#[cfg(windows)]
mod win;

use leash_snapshot::{
    HashedFile, ListeningPort, ProcessEntry, RunKeyEntry, ScheduledTask, ServiceEntry, SshSnapshot,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub use files::{collect_user_files, hash_file, UserFiles};
pub use tasks_xml::parse_schtasks_xml;

#[derive(Debug, Default)]
pub struct PersistCollection {
    pub run_keys: Vec<RunKeyEntry>,
    pub scheduled_tasks: Vec<ScheduledTask>,
    pub services: Vec<ServiceEntry>,
    pub env_user: BTreeMap<String, String>,
    pub env_system: BTreeMap<String, String>,
    pub profiles: Vec<HashedFile>,
    pub git_configs: Vec<HashedFile>,
    pub ssh: SshSnapshot,
    pub ports: Vec<ListeningPort>,
    pub processes: Vec<ProcessEntry>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PersistOptions {
    pub home: Option<PathBuf>,
}

pub fn collect(opts: &PersistOptions) -> PersistCollection {
    #[cfg(windows)]
    {
        win::collect(opts)
    }
    #[cfg(not(windows))]
    {
        stub::collect(opts)
    }
}

pub fn is_elevated() -> bool {
    #[cfg(windows)]
    {
        win::is_elevated()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|s| !s.is_empty())
}

pub fn username() -> Option<String> {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .ok()
        .filter(|s| !s.is_empty())
}

pub fn doctor_lines() -> Vec<String> {
    #[cfg(windows)]
    {
        win::doctor_lines()
    }
    #[cfg(not(windows))]
    {
        vec!["Persistence collectors (registry, services, ports): unavailable on this OS.".into()]
    }
}

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
