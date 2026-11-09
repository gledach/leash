//! Join ETW file/registry events onto snapshot findings. Never guesses:
//! no matching event means Unattributed, not Agent.

use crate::model::{
    AttributionBucket, AttributionStatus, AttributionSummary, FileTraceEvent, FindingAttribution,
    RegistryTraceEvent, TraceLog, SYSTEM_WIDE_CAVEAT,
};
use crate::paths::{paths_match, registry_match};
use crate::service::{is_kernel_pid, is_service_image};
use crate::tree::ProcessTree;
use leash_snapshot::{DiffReport, Finding};

pub fn apply_attribution(report: &mut DiffReport, log: &TraceLog) {
    report.attribution = summary_from_log(log);
    if !log.status.has_events() {
        if matches!(log.status, AttributionStatus::Off) {
            report.attribution = AttributionSummary::off();
        } else if matches!(log.status, AttributionStatus::Unavailable) {
            let why = log
                .error
                .as_deref()
                .unwrap_or("not running as administrator");
            report.attribution = AttributionSummary {
                status: AttributionStatus::Unavailable,
                target: log.target.clone(),
                caveat: format!(
                    "Attribution is off ({why}). This report shows all changes on this PC during the session, not only your agent's."
                ),
                ..AttributionSummary::default()
            };
        } else if matches!(log.status, AttributionStatus::Failed) {
            let why = log.error.as_deref().unwrap_or("tracing failed");
            report.attribution = AttributionSummary {
                status: AttributionStatus::Failed,
                target: log.target.clone(),
                caveat: format!(
                    "Attribution failed ({why}). This report shows all changes on this PC during the session, not only your agent's."
                ),
                ..AttributionSummary::default()
            };
        }
        for w in &log.warnings {
            if !report.warnings.contains(w) {
                report.warnings.push(w.clone());
            }
        }
        return;
    }

    for finding in &mut report.findings {
        finding.attribution = attribute_one(finding, log);
    }
    report.attribution.target = log.target.clone();
    report.attribution.status = log.status;
    report.attribution.recount(&report.findings);
    report.attribution.caveat = attributed_caveat(&report.attribution);
    for w in &log.warnings {
        if !report.warnings.contains(w) {
            report.warnings.push(w.clone());
        }
    }
    if log.dropped_other_events > 0 {
        let w = format!(
            "Attribution dropped {} other-process events to stay within its memory cap.",
            log.dropped_other_events
        );
        if !report.warnings.contains(&w) {
            report.warnings.push(w);
        }
    }
}

fn attributed_caveat(sum: &AttributionSummary) -> String {
    let target = sum.target.as_deref().unwrap_or("the agent");
    format!(
        "Split by process: {} by {target}, {} by other apps, {} unattributed. Unattributed means no ETW event named a process, not that nothing happened.",
        sum.agent, sum.other, sum.unattributed
    )
}

fn summary_from_log(log: &TraceLog) -> AttributionSummary {
    AttributionSummary {
        status: log.status,
        target: log.target.clone(),
        caveat: SYSTEM_WIDE_CAVEAT.to_string(),
        ..AttributionSummary::default()
    }
}

fn attribute_one(finding: &Finding, log: &TraceLog) -> FindingAttribution {
    // Ports and leftover processes can be attributed by the pid already on the finding.
    if let Some(attr) = attribute_by_embedded_pid(finding, &log.tree) {
        return attr;
    }

    let file_hits = matching_file_events(finding, &log.file_events);
    let reg_hits = matching_registry_events(finding, &log.registry_events);

    if file_hits.is_empty() && reg_hits.is_empty() {
        return FindingAttribution {
            bucket: AttributionBucket::Unattributed,
            note: Some("No ETW event named a process for this change.".into()),
            ..FindingAttribution::default()
        };
    }

    let mut hits: Vec<(u32, u64)> = file_hits
        .iter()
        .map(|e| (e.pid, e.at_ms))
        .chain(reg_hits.iter().map(|e| (e.pid, e.at_ms)))
        .collect();
    // Prefer the latest event if several processes touched the same path.
    hits.sort_by_key(|(_, at)| *at);
    let mut agent_hit = None;
    let mut other_hit = None;
    let mut service_hit = None;
    for (pid, at) in hits.iter().copied() {
        let rec = log.tree.lookup(pid, at);
        let in_agent = log.tree.is_agent_at(pid, at);
        let service = is_kernel_pid(pid)
            || rec
                .map(|r| is_service_image(&r.image) || r.exe.as_deref().is_some_and(is_service_image))
                .unwrap_or(false);
        if in_agent {
            agent_hit = Some((pid, at));
        } else if service {
            service_hit = Some((pid, at));
        } else {
            other_hit = Some((pid, at));
        }
    }

    // If the agent touched it, say so, even if another app also did. Mention
    // the other pid in the note rather than guessing a single owner.
    if let Some((pid, at)) = agent_hit {
        let mut attr = describe(pid, at, &log.tree, AttributionBucket::Agent);
        if other_hit.is_some() {
            let extra = other_hit
                .and_then(|(opid, oat)| log.tree.lookup(opid, oat).map(|r| r.display_name()))
                .unwrap_or_else(|| "another app".into());
            let note = format!("Also touched by {extra} during the session.");
            attr.note = Some(match attr.note.take() {
                Some(n) => format!("{n} {note}"),
                None => note,
            });
        }
        return attr;
    }
    if let Some((pid, at)) = other_hit {
        return describe(pid, at, &log.tree, AttributionBucket::Other);
    }
    if let Some((pid, at)) = service_hit {
        let mut attr = describe(pid, at, &log.tree, AttributionBucket::Unattributed);
        let name = attr
            .process_name
            .clone()
            .unwrap_or_else(|| format!("pid {pid}"));
        attr.note = Some(format!("possibly via a service ({name})"));
        return attr;
    }

    FindingAttribution {
        bucket: AttributionBucket::Unattributed,
        note: Some("No ETW event named a process for this change.".into()),
        ..FindingAttribution::default()
    }
}

fn describe(pid: u32, at: u64, tree: &ProcessTree, bucket: AttributionBucket) -> FindingAttribution {
    let rec = tree.lookup(pid, at);
    FindingAttribution {
        bucket,
        process_name: rec.map(|r| r.display_name()),
        process_pid: Some(pid),
        process_chain: tree.chain(pid, at),
        note: None,
    }
}

fn matching_file_events<'a>(
    finding: &Finding,
    events: &'a [FileTraceEvent],
) -> Vec<&'a FileTraceEvent> {
    let keys = file_keys(finding);
    if keys.is_empty() {
        return Vec::new();
    }
    events
        .iter()
        .filter(|ev| {
            keys.iter().any(|k| {
                paths_match(&ev.path, k)
                    || ev
                        .old_path
                        .as_deref()
                        .is_some_and(|old| paths_match(old, k))
            })
        })
        .collect()
}

fn matching_registry_events<'a>(
    finding: &Finding,
    events: &'a [RegistryTraceEvent],
) -> Vec<&'a RegistryTraceEvent> {
    let keys = registry_keys(finding);
    if keys.is_empty() {
        return Vec::new();
    }
    events
        .iter()
        .filter(|ev| {
            keys.iter().any(|k| {
                registry_match(&ev.key, k)
                    || ev
                        .value
                        .as_deref()
                        .is_some_and(|v| registry_match(&format!("{}\\{v}", ev.key), k))
            })
        })
        .collect()
}

fn file_keys(finding: &Finding) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(p) = &finding.path {
        if looks_like_path(p) {
            keys.push(p.clone());
        }
    }
    if looks_like_path(&finding.detail) {
        keys.push(finding.detail.clone());
    }
    keys
}

fn looks_like_path(s: &str) -> bool {
    let t = s.trim();
    t.contains('\\')
        || t.contains('/')
        || (t.len() >= 3 && t.as_bytes()[1] == b':')
}

fn registry_keys(finding: &Finding) -> Vec<String> {
    match finding.category.as_str() {
        "registry" => {
            let mut keys = Vec::new();
            if let Some(p) = &finding.path {
                keys.push(p.clone());
            }
            if !finding.detail.is_empty() {
                keys.push(finding.detail.clone());
            }
            keys
        }
        "service" => {
            let mut keys = Vec::new();
            if let Some(name) = &finding.path {
                keys.push(format!(
                    r"HKLM\SYSTEM\CurrentControlSet\Services\{name}"
                ));
                keys.push(format!(
                    r"\REGISTRY\MACHINE\SYSTEM\CurrentControlSet\Services\{name}"
                ));
            }
            keys
        }
        "task" => {
            let mut keys = Vec::new();
            if let Some(p) = &finding.path {
                keys.push(p.clone());
                // Task Scheduler XML lives under %SYSTEM32%\Tasks and the registry tree.
                keys.push(format!(r"C:\Windows\System32\Tasks{p}"));
                keys.push(format!(
                    r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Schedule\TaskCache\Tree{p}"
                ));
            }
            keys
        }
        "env" => {
            let mut keys = Vec::new();
            if let Some(name) = &finding.path {
                keys.push(format!(
                    r"HKCU\Environment\{name}"
                ));
                keys.push(format!(
                    r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment\{name}"
                ));
            }
            keys
        }
        "package" | "shell profile" | "git config" | "ssh config" | "ssh authorized_keys"
        | "ssh key file" => file_keys(finding),
        _ => Vec::new(),
    }
}

fn attribute_by_embedded_pid(finding: &Finding, tree: &ProcessTree) -> Option<FindingAttribution> {
    if finding.category != "port" && finding.category != "process" {
        return None;
    }
    let pid = extract_pid(finding)?;
    // Use "now" as 0 so a live record with unknown times still matches.
    let at = 0;
    if tree.is_agent_at(pid, at) {
        return Some(describe(pid, at, tree, AttributionBucket::Agent));
    }
    if let Some(rec) = tree.lookup(pid, at) {
        if is_service_image(&rec.image) || is_kernel_pid(pid) {
            let mut attr = describe(pid, at, tree, AttributionBucket::Unattributed);
            attr.note = Some(format!("possibly via a service ({})", rec.display_name()));
            return Some(attr);
        }
        return Some(describe(pid, at, tree, AttributionBucket::Other));
    }
    None
}

fn extract_pid(finding: &Finding) -> Option<u32> {
    if let Some(path) = &finding.path {
        if let Ok(pid) = path.parse::<u32>() {
            return Some(pid);
        }
        // port ids look like "tcp:127.0.0.1:8080:1234" (pid last)
        if let Some(last) = path.rsplit(':').next() {
            if let Ok(pid) = last.parse::<u32>() {
                return Some(pid);
            }
        }
    }
    // "pid 1234" in the title or detail
    for text in [&finding.title, &finding.detail] {
        if let Some(pid) = text.split_whitespace().skip_while(|w| *w != "pid").nth(1) {
            let digits: String = pid.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(pid) = digits.parse::<u32>() {
                return Some(pid);
            }
        }
    }
    None
}
