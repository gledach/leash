use crate::model::{
    split_path_var, DiffReport, FileEvent, FileInventoryEntry, Finding, HashedFile, IgnoreSummary,
    Package, PackageSets, ProcessEntry, SeverityCounts, Snapshot, UsnVolumeState,
};
use leash_filter::{ChangeKind, FilterEngine, PathDecision, Severity};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub struct DiffOptions<'a> {
    pub engine: &'a FilterEngine,
}

#[derive(Default)]
pub struct IgnoreTally {
    pub total: usize,
    pub by_reason: BTreeMap<String, usize>,
    pub examples: Vec<String>,
}

impl IgnoreTally {
    fn push(&mut self, path: &str, reason: &str) {
        self.total += 1;
        *self.by_reason.entry(reason.to_string()).or_insert(0) += 1;
        if self.examples.len() < 8 {
            self.examples.push(format!("{path} ({reason})"));
        }
    }

    fn into_summary(self) -> IgnoreSummary {
        IgnoreSummary {
            total: self.total,
            by_reason: self.by_reason,
            examples: self.examples,
        }
    }
}

pub fn diff_snapshots(before: &Snapshot, after: &Snapshot, opts: DiffOptions<'_>) -> DiffReport {
    let mut findings: Vec<Finding> = Vec::new();
    let mut ignored = IgnoreTally::default();
    let mut warnings = before.warnings.clone();
    for w in &after.warnings {
        if !warnings.contains(w) {
            warnings.push(w.clone());
        }
    }
    warnings.extend(journal_warnings(&before.usn, &after.usn));

    collect_file_findings(before, after, opts.engine, &mut findings, &mut ignored);
    findings.extend(diff_run_keys(before, after));
    findings.extend(diff_tasks(before, after));
    findings.extend(diff_services(before, after));
    findings.extend(diff_env(&before.env_user, &after.env_user, "user"));
    findings.extend(diff_env(&before.env_system, &after.env_system, "system"));
    findings.extend(diff_packages(&before.packages, &after.packages));
    findings.extend(diff_hashed(
        "shell profile",
        &before.profiles,
        &after.profiles,
        Severity::High,
    ));
    findings.extend(diff_hashed(
        "git config",
        &before.git_configs,
        &after.git_configs,
        Severity::High,
    ));
    findings.extend(diff_ssh(before, after));
    findings.extend(diff_ports(before, after));
    findings.extend(diff_processes(before, after));

    findings.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then(a.category.cmp(&b.category))
            .then(a.title.cmp(&b.title))
    });

    let counts = SeverityCounts::tally(&findings);
    DiffReport {
        session_id: after.session_id.clone(),
        before_at: before.taken_at.clone(),
        after_at: after.taken_at.clone(),
        repo_path: after.repo_path.clone().or_else(|| before.repo_path.clone()),
        file_mode: after.file_mode,
        elevated: after.elevated,
        warnings,
        findings,
        ignored: ignored.into_summary(),
        counts,
        attribution: crate::AttributionSummary::off(),
    }
}

fn journal_warnings(before: &[UsnVolumeState], after: &[UsnVolumeState]) -> Vec<String> {
    let mut out = Vec::new();
    for a in after {
        if !a.query_ok {
            out.push(format!(
                "USN journal on {} was not readable: {}",
                a.volume,
                a.error.as_deref().unwrap_or("unknown error")
            ));
            continue;
        }
        let Some(b) = before.iter().find(|b| b.volume == a.volume) else {
            continue;
        };
        if b.journal_id != 0 && a.journal_id != 0 && b.journal_id != a.journal_id {
            out.push(format!(
                "USN journal on {} was recreated (id changed). Changes between start and stop may be missing.",
                a.volume
            ));
        }
        if b.start_usn != 0 && a.lowest_valid_usn != 0 && b.start_usn < a.lowest_valid_usn {
            out.push(format!(
                "USN journal on {} wrapped past the start cursor; some early changes may be missing.",
                a.volume
            ));
        }
    }
    out
}

fn collect_file_findings(
    before: &Snapshot,
    after: &Snapshot,
    engine: &FilterEngine,
    findings: &mut Vec<Finding>,
    ignored: &mut IgnoreTally,
) {
    let mut events = after.file_events.clone();
    if events.is_empty() {
        events = inventory_events(&before.fallback_files, &after.fallback_files);
    } else if !before.fallback_files.is_empty() || !after.fallback_files.is_empty() {
        // Mixed mode: also surface inventory diffs that USN might have missed
        // (e.g. when a volume had no journal). Dedup by path+kind later.
        events.extend(inventory_events(
            &before.fallback_files,
            &after.fallback_files,
        ));
    }

    let mut seen = BTreeSet::new();
    for ev in events {
        let path = ev.display_path();
        let key = format!(
            "{:?}:{}:{}",
            ev.kind,
            path.to_lowercase(),
            ev.name.to_lowercase()
        );
        if !seen.insert(key) {
            continue;
        }
        match engine.classify(&path, ev.kind, ev.is_dir) {
            PathDecision::Ignore { reason } => ignored.push(&path, &reason),
            PathDecision::Keep { severity, reason } => {
                findings.push(file_finding(ev, severity, reason));
            }
        }
    }
}

fn file_finding(ev: FileEvent, severity: Severity, reason: String) -> Finding {
    let path = leash_filter::pretty_path(&ev.display_path());
    let kind = ev.kind.as_str();
    let title = if ev.is_dir {
        format!("Directory {kind}")
    } else {
        format!("File {kind}")
    };
    let mut detail = path.clone();
    if ev.kind == ChangeKind::Rename {
        if let (Some(old), Some(newp)) = (&ev.old_path, &ev.path) {
            detail = format!("{old} → {newp}");
        }
    }
    if ev.name != path {
        // keep the journal file name when the path is only a name
    }
    Finding {
        category: "file".into(),
        severity,
        title,
        detail,
        reason,
        path: Some(path),
        attribution: Default::default(),
    }
}

fn inventory_events(before: &[FileInventoryEntry], after: &[FileInventoryEntry]) -> Vec<FileEvent> {
    let before_map: HashMap<String, &FileInventoryEntry> =
        before.iter().map(|e| (normalize_key(&e.path), e)).collect();
    let after_map: HashMap<String, &FileInventoryEntry> =
        after.iter().map(|e| (normalize_key(&e.path), e)).collect();
    let mut events = Vec::new();
    for (key, a) in &after_map {
        match before_map.get(key) {
            None => events.push(inventory_event(a, ChangeKind::Create)),
            Some(b) => {
                let hash_changed = match (&b.sha256, &a.sha256) {
                    (Some(x), Some(y)) => x != y,
                    _ => false,
                };
                let meta_changed = b.size != a.size || b.mtime != a.mtime;
                // If both hashes exist and match, ignore mtime-only jitter.
                let changed = hash_changed
                    || (meta_changed && matches!((&b.sha256, &a.sha256), (None, _) | (_, None)));
                if changed {
                    let kind = if a.size > b.size {
                        ChangeKind::DataExtend
                    } else if a.size < b.size {
                        ChangeKind::DataTruncation
                    } else {
                        ChangeKind::DataOverwrite
                    };
                    events.push(inventory_event(a, kind));
                }
            }
        }
    }
    for (key, b) in &before_map {
        if !after_map.contains_key(key) {
            events.push(inventory_event(b, ChangeKind::Delete));
        }
    }
    events
}

fn inventory_event(e: &FileInventoryEntry, kind: ChangeKind) -> FileEvent {
    FileEvent {
        volume: None,
        path: Some(e.path.clone()),
        name: e
            .path
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(&e.path)
            .to_string(),
        old_path: None,
        kind,
        reasons_raw: 0,
        usn: None,
        file_id: None,
        is_dir: e.is_dir,
        timestamp: None,
    }
}

fn normalize_key(p: &str) -> String {
    leash_filter::normalize_path(p)
}

fn diff_run_keys(before: &Snapshot, after: &Snapshot) -> Vec<Finding> {
    let b: BTreeMap<_, _> = before.run_keys.iter().map(|k| (k.id(), k)).collect();
    let a: BTreeMap<_, _> = after.run_keys.iter().map(|k| (k.id(), k)).collect();
    let mut out = Vec::new();
    for (id, key) in &a {
        match b.get(id) {
            None => out.push(Finding {
                category: "registry".into(),
                severity: Severity::Medium,
                title: format!("New {} {} value", key.hive, key.key),
                detail: format!("{} = {}", key.name, key.value),
                reason: "Run/RunOnce values start a program at logon or at next boot".into(),
                path: Some(id.clone()),
                attribution: Default::default(),
            }),
            Some(old) if old.value != key.value => out.push(Finding {
                category: "registry".into(),
                severity: Severity::Medium,
                title: format!("Changed {} {} value", key.hive, key.key),
                detail: format!("{}: {} → {}", key.name, old.value, key.value),
                reason: "Run/RunOnce values start a program at logon or at next boot".into(),
                path: Some(id.clone()),
                attribution: Default::default(),
            }),
            _ => {}
        }
    }
    for (id, key) in &b {
        if !a.contains_key(id) {
            out.push(Finding {
                category: "registry".into(),
                severity: Severity::Medium,
                title: format!("Removed {} {} value", key.hive, key.key),
                detail: format!("{} = {}", key.name, key.value),
                reason: "a previously registered startup program disappeared".into(),
                path: Some(id.clone()),
                attribution: Default::default(),
            });
        }
    }
    out
}

fn diff_tasks(before: &Snapshot, after: &Snapshot) -> Vec<Finding> {
    let b: BTreeMap<_, _> = before.scheduled_tasks.iter().map(|t| (t.id(), t)).collect();
    let a: BTreeMap<_, _> = after.scheduled_tasks.iter().map(|t| (t.id(), t)).collect();
    let mut out = Vec::new();
    for (id, task) in &a {
        match b.get(id) {
            None => out.push(Finding {
                category: "task".into(),
                severity: Severity::Medium,
                title: "New scheduled task".into(),
                detail: format!("{}  {}", id, task.command_line()),
                reason: "scheduled tasks can run in the background without an open window".into(),
                path: Some(id.clone()),
                attribution: Default::default(),
            }),
            Some(old) => {
                let mut changes = Vec::new();
                if old.command_line() != task.command_line() {
                    changes.push(format!(
                        "command: {} → {}",
                        old.command_line(),
                        task.command_line()
                    ));
                }
                if old.enabled != task.enabled {
                    changes.push(format!("enabled: {:?} → {:?}", old.enabled, task.enabled));
                }
                if old.user != task.user {
                    changes.push(format!("user: {:?} → {:?}", old.user, task.user));
                }
                if !changes.is_empty() {
                    out.push(Finding {
                        category: "task".into(),
                        severity: Severity::Medium,
                        title: "Scheduled task changed".into(),
                        detail: format!("{}  {}", id, changes.join("; ")),
                        reason: "the task's command, identity, or enabled state changed".into(),
                        path: Some(id.clone()),
                        attribution: Default::default(),
                    });
                }
            }
        }
    }
    for (id, task) in &b {
        if !a.contains_key(id) {
            out.push(Finding {
                category: "task".into(),
                severity: Severity::Medium,
                title: "Scheduled task removed".into(),
                detail: format!("{}  {}", id, task.command_line()),
                reason: "a previously registered scheduled task disappeared".into(),
                path: Some(id.clone()),
                attribution: Default::default(),
            });
        }
    }
    out
}

fn diff_services(before: &Snapshot, after: &Snapshot) -> Vec<Finding> {
    let b: BTreeMap<_, _> = before
        .services
        .iter()
        .map(|s| (s.name.to_lowercase(), s))
        .collect();
    let a: BTreeMap<_, _> = after
        .services
        .iter()
        .map(|s| (s.name.to_lowercase(), s))
        .collect();
    let mut out = Vec::new();
    for (name, svc) in &a {
        match b.get(name) {
            None => out.push(Finding {
                category: "service".into(),
                severity: Severity::Medium,
                title: "New Windows service".into(),
                detail: format!(
                    "{} ({})  {}  {}",
                    svc.name,
                    svc.display_name.clone().unwrap_or_default(),
                    svc.start_type.clone().unwrap_or_default(),
                    svc.binary_path.clone().unwrap_or_default()
                ),
                reason: "a new service can start automatically and run as a system account".into(),
                path: Some(svc.name.clone()),
                attribution: Default::default(),
            }),
            Some(old) => {
                let mut changes = Vec::new();
                if old.binary_path != svc.binary_path {
                    changes.push(format!(
                        "binary: {:?} → {:?}",
                        old.binary_path, svc.binary_path
                    ));
                }
                if old.start_type != svc.start_type {
                    changes.push(format!(
                        "start type: {:?} → {:?}",
                        old.start_type, svc.start_type
                    ));
                }
                if old.account != svc.account {
                    changes.push(format!("account: {:?} → {:?}", old.account, svc.account));
                }
                if !changes.is_empty() {
                    out.push(Finding {
                        category: "service".into(),
                        severity: Severity::Medium,
                        title: format!("Service {} configuration changed", svc.name),
                        detail: changes.join("; "),
                        reason: "the service binary, start type, or account changed".into(),
                        path: Some(svc.name.clone()),
                        attribution: Default::default(),
                    });
                } else if old.state != svc.state {
                    out.push(Finding {
                        category: "service".into(),
                        severity: Severity::Info,
                        title: format!("Service {} state changed", svc.name),
                        detail: format!("{:?} → {:?}", old.state, svc.state),
                        reason:
                            "running/stopped state only; the service configuration is unchanged"
                                .into(),
                        path: Some(svc.name.clone()),
                        attribution: Default::default(),
                    });
                }
            }
        }
    }
    out
}

fn diff_env(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
    scope: &str,
) -> Vec<Finding> {
    let mut out = Vec::new();
    let keys: BTreeSet<_> = before.keys().chain(after.keys()).cloned().collect();
    for key in keys {
        let b = before.get(&key);
        let a = after.get(&key);
        if key.eq_ignore_ascii_case("PATH") || key.eq_ignore_ascii_case("Path") {
            let b_ents = b.map(|v| split_path_var(v)).unwrap_or_default();
            let a_ents = a.map(|v| split_path_var(v)).unwrap_or_default();
            let b_set: BTreeSet<_> = b_ents.iter().map(|s| s.to_lowercase()).collect();
            let a_set: BTreeSet<_> = a_ents.iter().map(|s| s.to_lowercase()).collect();
            for (raw, low) in a_ents.iter().map(|s| (s, s.to_lowercase())) {
                if !b_set.contains(&low) {
                    out.push(Finding {
                        category: "env".into(),
                        severity: Severity::Medium,
                        title: format!("New {scope} PATH entry"),
                        detail: raw.clone(),
                        reason: "programs in this folder can be resolved first, or shadow others"
                            .into(),
                        path: Some(raw.clone()),
                        attribution: Default::default(),
                    });
                }
            }
            for (raw, low) in b_ents.iter().map(|s| (s, s.to_lowercase())) {
                if !a_set.contains(&low) {
                    out.push(Finding {
                        category: "env".into(),
                        severity: Severity::Medium,
                        title: format!("Removed {scope} PATH entry"),
                        detail: raw.clone(),
                        reason: "a folder disappeared from PATH".into(),
                        path: Some(raw.clone()),
                        attribution: Default::default(),
                    });
                }
            }
            continue;
        }
        match (b, a) {
            (None, Some(v)) => out.push(Finding {
                category: "env".into(),
                severity: Severity::Medium,
                title: format!("New {scope} environment variable"),
                detail: format!("{key}={v}"),
                reason: "a new persistent environment variable appeared".into(),
                path: Some(key.clone()),
                attribution: Default::default(),
            }),
            (Some(old), Some(newv)) if old != newv => out.push(Finding {
                category: "env".into(),
                severity: Severity::Medium,
                title: format!("Changed {scope} environment variable"),
                detail: format!("{key}: {old} → {newv}"),
                reason: "a persistent environment variable changed".into(),
                path: Some(key.clone()),
                attribution: Default::default(),
            }),
            (Some(old), None) => out.push(Finding {
                category: "env".into(),
                severity: Severity::Medium,
                title: format!("Removed {scope} environment variable"),
                detail: format!("{key}={old}"),
                reason: "a persistent environment variable disappeared".into(),
                path: Some(key.clone()),
                attribution: Default::default(),
            }),
            _ => {}
        }
    }
    out
}

fn diff_packages(before: &PackageSets, after: &PackageSets) -> Vec<Finding> {
    let mut out = Vec::new();
    out.extend(diff_pkg_list("npm", &before.npm, &after.npm));
    out.extend(diff_pkg_list("pnpm", &before.pnpm, &after.pnpm));
    out.extend(diff_pkg_list("pip", &before.pip, &after.pip));
    out.extend(diff_pkg_list("cargo", &before.cargo, &after.cargo));
    out
}

fn diff_pkg_list(tool: &str, before: &[Package], after: &[Package]) -> Vec<Finding> {
    let b: BTreeMap<_, _> = before.iter().map(|p| (p.name.clone(), p)).collect();
    let a: BTreeMap<_, _> = after.iter().map(|p| (p.name.clone(), p)).collect();
    let mut out = Vec::new();
    for (name, pkg) in &a {
        match b.get(name) {
            None => out.push(Finding {
                category: "package".into(),
                severity: Severity::Medium,
                title: format!("New global {tool} package"),
                detail: format!("{}@{}", pkg.name, pkg.version),
                reason:
                    "a global tool or library was installed on the machine, not just in the repo"
                        .into(),
                path: Some(pkg.name.clone()),
                attribution: Default::default(),
            }),
            Some(old) if old.version != pkg.version => out.push(Finding {
                category: "package".into(),
                severity: Severity::Medium,
                title: format!("Updated global {tool} package"),
                detail: format!("{} {} → {}", pkg.name, old.version, pkg.version),
                reason: "a global package version changed".into(),
                path: Some(pkg.name.clone()),
                attribution: Default::default(),
            }),
            _ => {}
        }
    }
    for (name, pkg) in &b {
        if !a.contains_key(name) {
            out.push(Finding {
                category: "package".into(),
                severity: Severity::Medium,
                title: format!("Removed global {tool} package"),
                detail: format!("{}@{}", pkg.name, pkg.version),
                reason: "a global package disappeared".into(),
                path: Some(pkg.name.clone()),
                attribution: Default::default(),
            });
        }
    }
    out
}

fn diff_hashed(
    label: &str,
    before: &[HashedFile],
    after: &[HashedFile],
    severity: Severity,
) -> Vec<Finding> {
    let b: BTreeMap<_, _> = before.iter().map(|f| (normalize_key(&f.path), f)).collect();
    let a: BTreeMap<_, _> = after.iter().map(|f| (normalize_key(&f.path), f)).collect();
    let mut out = Vec::new();
    let keys: BTreeSet<_> = b.keys().chain(a.keys()).cloned().collect();
    for key in keys {
        let old = b.get(&key);
        let new = a.get(&key);
        match (old, new) {
            (None, Some(f)) if f.exists => out.push(hashed_finding(
                label,
                &f.path,
                "appeared",
                format!("sha256={}", f.sha256.as_deref().unwrap_or("unknown")),
                severity,
            )),
            (Some(f), None) if f.exists => out.push(hashed_finding(
                label,
                &f.path,
                "disappeared",
                format!("sha256={}", f.sha256.as_deref().unwrap_or("unknown")),
                severity,
            )),
            (Some(old), Some(new)) => {
                if old.exists && !new.exists {
                    out.push(hashed_finding(
                        label,
                        &new.path,
                        "was deleted",
                        String::new(),
                        severity,
                    ));
                } else if !old.exists && new.exists {
                    out.push(hashed_finding(
                        label,
                        &new.path,
                        "appeared",
                        format!("sha256={}", new.sha256.as_deref().unwrap_or("unknown")),
                        severity,
                    ));
                } else if old.sha256 != new.sha256 && (old.exists || new.exists) {
                    out.push(hashed_finding(
                        label,
                        &new.path,
                        "content changed",
                        format!(
                            "{} → {}",
                            old.sha256.as_deref().unwrap_or("unknown"),
                            new.sha256.as_deref().unwrap_or("unknown")
                        ),
                        severity,
                    ));
                }
            }
            _ => {}
        }
    }
    out
}

fn hashed_finding(
    label: &str,
    path: &str,
    verb: &str,
    detail: String,
    severity: Severity,
) -> Finding {
    Finding {
        category: label.into(),
        severity,
        title: format!("{label} {verb}"),
        detail: if detail.is_empty() {
            path.to_string()
        } else {
            format!("{path}  {detail}")
        },
        reason: format!("{label} files are a common place for agents to hook a shell or git"),
        path: Some(path.to_string()),
        attribution: Default::default(),
    }
}

fn diff_ssh(before: &Snapshot, after: &Snapshot) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut before_files = Vec::new();
    let mut after_files = Vec::new();
    if let Some(f) = &before.ssh.config {
        before_files.push(f.clone());
    }
    if let Some(f) = &after.ssh.config {
        after_files.push(f.clone());
    }
    out.extend(diff_hashed(
        "ssh config",
        &before_files,
        &after_files,
        Severity::High,
    ));
    before_files.clear();
    after_files.clear();
    if let Some(f) = &before.ssh.authorized_keys {
        before_files.push(f.clone());
    }
    if let Some(f) = &after.ssh.authorized_keys {
        after_files.push(f.clone());
    }
    out.extend(diff_hashed(
        "ssh authorized_keys",
        &before_files,
        &after_files,
        Severity::High,
    ));
    out.extend(diff_hashed(
        "ssh key file",
        &before.ssh.key_files,
        &after.ssh.key_files,
        Severity::High,
    ));
    out
}

fn diff_ports(before: &Snapshot, after: &Snapshot) -> Vec<Finding> {
    let b: BTreeSet<_> = before.ports.iter().map(|p| p.bind()).collect();
    let a: BTreeSet<_> = after.ports.iter().map(|p| p.bind()).collect();
    // Compare by protocol+addr+port, ignore pid churn for the same bind.
    fn listen_key(p: &crate::ListeningPort) -> String {
        format!(
            "{}:{}:{}",
            p.protocol.to_lowercase(),
            p.local_addr.to_lowercase(),
            p.local_port
        )
    }
    let b_keys: BTreeMap<_, _> = before.ports.iter().map(|p| (listen_key(p), p)).collect();
    let a_keys: BTreeMap<_, _> = after.ports.iter().map(|p| (listen_key(p), p)).collect();
    let mut out = Vec::new();
    for (k, p) in &a_keys {
        if !b_keys.contains_key(k) {
            out.push(Finding {
                category: "port".into(),
                severity: Severity::Medium,
                title: "New listening port".into(),
                detail: format!(
                    "{} {}:{}  pid {} {}",
                    p.protocol,
                    p.local_addr,
                    p.local_port,
                    p.pid,
                    p.process_name.clone().unwrap_or_default()
                ),
                reason: "a process started accepting connections during the session".into(),
                path: None,
                attribution: Default::default(),
            });
        }
    }
    let _ = (b, a);
    out
}

fn diff_processes(before: &Snapshot, after: &Snapshot) -> Vec<Finding> {
    let b: BTreeSet<_> = before
        .processes
        .iter()
        .map(ProcessEntry::identity)
        .collect();
    let mut out = Vec::new();
    for proc in &after.processes {
        if !b.contains(&proc.identity()) {
            out.push(Finding {
                category: "process".into(),
                severity: Severity::Medium,
                title: "Process started during the session and still running".into(),
                detail: format!(
                    "pid {}  ppid {}  {}  {}",
                    proc.pid,
                    proc.ppid
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "-".into()),
                    proc.name,
                    proc.command_line
                        .as_deref()
                        .or(proc.exe.as_deref())
                        .unwrap_or("")
                ),
                reason: "still running after stop; v1 cannot tell which agent started it".into(),
                path: proc.exe.clone(),
                attribution: Default::default(),
            });
        }
    }
    out
}
