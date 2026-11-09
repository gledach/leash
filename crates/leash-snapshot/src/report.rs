use crate::model::{DiffReport, Finding, SeverityCounts};
use leash_filter::{pretty_path, Severity};

pub fn render_text(report: &DiffReport) -> String {
    let mut out = String::new();
    out.push_str("leash — session report\n");
    out.push_str(&format!("session:  {}\n", report.session_id));
    out.push_str(&format!("before:   {}\n", report.before_at));
    out.push_str(&format!("after:    {}\n", report.after_at));
    if let Some(repo) = &report.repo_path {
        out.push_str(&format!("repo:     {repo}\n"));
    }
    out.push_str(&format!(
        "files:    {}{}\n",
        file_mode_label(report),
        if report.elevated {
            ""
        } else {
            "  (not elevated)"
        }
    ));
    write_attrib_preamble(&mut out, report, false);
    if !report.warnings.is_empty() {
        out.push_str("\nwarnings:\n");
        for w in &report.warnings {
            out.push_str(&format!("  - {w}\n"));
        }
    }
    out.push('\n');
    out.push_str(&counts_line(&report.counts, report.ignored.total));
    out.push('\n');

    write_section(&mut out, "HIGH", &filter_sev(report, Severity::High), false);
    write_section(
        &mut out,
        "MEDIUM",
        &filter_sev(report, Severity::Medium),
        false,
    );
    write_section(&mut out, "INFO", &filter_sev(report, Severity::Info), false);

    if report.ignored.total > 0 {
        out.push_str("\nIgnored file changes\n");
        out.push_str("────────────────────\n");
        out.push_str(&format!(
            "{} events were filtered as noise",
            report.ignored.total
        ));
        if !report.ignored.by_reason.is_empty() {
            out.push_str(":\n");
            for (reason, n) in &report.ignored.by_reason {
                out.push_str(&format!("  {n:>5}  {reason}\n"));
            }
        } else {
            out.push_str(".\n");
        }
        if !report.ignored.examples.is_empty() {
            out.push_str("examples:\n");
            for ex in &report.ignored.examples {
                out.push_str(&format!("  - {ex}\n"));
            }
        }
    }
    if report.findings.is_empty() && report.warnings.is_empty() {
        out.push_str(
            "\nNothing surprising showed up in this session, with the usual noise filtered out.\n",
        );
    }
    out
}

pub fn render_markdown(report: &DiffReport) -> String {
    let mut out = String::new();
    out.push_str("# leash session report\n\n");
    out.push_str(&format!("- **session:** {}\n", report.session_id));
    out.push_str(&format!("- **before:** {}\n", report.before_at));
    out.push_str(&format!("- **after:** {}\n", report.after_at));
    if let Some(repo) = &report.repo_path {
        out.push_str(&format!("- **repo:** `{repo}`\n"));
    }
    out.push_str(&format!(
        "- **files:** {}{}\n",
        file_mode_label(report),
        if report.elevated {
            ""
        } else {
            " (not elevated)"
        }
    ));
    write_attrib_preamble(&mut out, report, true);
    out.push_str(&format!(
        "- **counts:** {} high, {} medium, {} info; {} file events ignored\n",
        report.counts.high, report.counts.medium, report.counts.info, report.ignored.total
    ));
    if !report.warnings.is_empty() {
        out.push_str("\n## Warnings\n\n");
        for w in &report.warnings {
            out.push_str(&format!("- {w}\n"));
        }
    }
    write_md_section(&mut out, "High", &filter_sev(report, Severity::High));
    write_md_section(&mut out, "Medium", &filter_sev(report, Severity::Medium));
    write_md_section(&mut out, "Info", &filter_sev(report, Severity::Info));

    if report.ignored.total > 0 {
        out.push_str("\n## Ignored file changes\n\n");
        out.push_str(&format!(
            "{} events were filtered as noise.\n\n",
            report.ignored.total
        ));
        for (reason, n) in &report.ignored.by_reason {
            out.push_str(&format!("- {n} — {reason}\n"));
        }
    }
    if report.findings.is_empty() && report.warnings.is_empty() {
        out.push_str(
            "\nNothing surprising showed up in this session, with the usual noise filtered out.\n",
        );
    }
    out.push('\n');
    out
}

pub fn render_json(report: &DiffReport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}

/// Short console summary: counts, then up to three High findings (one line each).
/// The CLI prints report file paths after this.
pub fn render_summary(report: &DiffReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "leash session {}: {} high, {} medium, {} info ({} file events ignored)\n",
        report.session_id,
        report.counts.high,
        report.counts.medium,
        report.counts.info,
        report.ignored.total
    ));
    if !report.attribution.caveat.is_empty() {
        out.push_str(&format!("{}\n", report.attribution.caveat));
    } else {
        out.push_str(&format!("{}\n", crate::SYSTEM_WIDE_CAVEAT));
    }
    let highs: Vec<&Finding> = filter_sev(report, Severity::High);
    for f in highs.iter().take(3) {
        out.push_str(&format!("  {}\n", summary_finding_line(f)));
    }
    if highs.len() > 3 {
        out.push_str(&format!(
            "  … and {} more High findings in the report\n",
            highs.len() - 3
        ));
    }
    out
}

pub fn summary_finding_line(f: &Finding) -> String {
    let detail = if f.detail.is_empty() {
        String::new()
    } else {
        pretty_path(&f.detail)
    };
    let path = f.path.as_deref().map(pretty_path);
    let body = if !detail.is_empty() {
        if detail.starts_with(&f.title) || f.title.contains(&detail) {
            detail
        } else {
            format!("{}  {}", f.title, detail)
        }
    } else if let Some(path) = path {
        format!("{}  {}", f.title, path)
    } else {
        f.title.clone()
    };
    let mut line = format!("[{}] {}", f.category, body);
    if let Some(chain) = &f.attribution.process_chain {
        line.push_str(&format!("  ({chain})"));
    } else if let Some(name) = &f.attribution.process_name {
        line.push_str(&format!("  ({name})"));
    } else if let Some(note) = &f.attribution.note {
        if f.attribution.bucket != crate::AttributionBucket::None {
            line.push_str(&format!("  ({note})"));
        }
    }
    line
}

/// Top-of-window summary: cluster file findings that share a parent or a
/// well-known editor state folder (`workspaceStorage: 42 changes`) so a
/// burst of similar events does not flood the list.
pub fn aggregate_finding_lines(findings: &[Finding], limit: usize) -> Vec<String> {
    use std::collections::BTreeMap;

    let mut by_bucket: BTreeMap<String, Vec<&Finding>> = BTreeMap::new();
    let mut ungrouped: Vec<&Finding> = Vec::new();
    for f in findings {
        if let Some(bucket) = finding_bucket(f) {
            by_bucket.entry(bucket).or_default().push(f);
        } else {
            ungrouped.push(f);
        }
    }

    struct Row {
        rank: u8,
        count: usize,
        line: String,
    }
    let mut rows: Vec<Row> = Vec::new();
    for (bucket, items) in by_bucket {
        if items.len() >= 2 {
            let rank = items.iter().map(|f| severity_rank(f.severity)).min().unwrap_or(2);
            rows.push(Row {
                rank,
                count: items.len(),
                line: format!("{bucket}: {} changes", items.len()),
            });
        } else {
            for f in items {
                rows.push(Row {
                    rank: severity_rank(f.severity),
                    count: 1,
                    line: summary_finding_line(f),
                });
            }
        }
    }
    for f in ungrouped {
        rows.push(Row {
            rank: severity_rank(f.severity),
            count: 1,
            line: summary_finding_line(f),
        });
    }
    rows.sort_by(|a, b| a.rank.cmp(&b.rank).then(b.count.cmp(&a.count)));
    rows.into_iter().take(limit).map(|r| r.line).collect()
}

fn severity_rank(severity: Severity) -> u8 {
    match severity {
        Severity::High => 0,
        Severity::Medium => 1,
        Severity::Info => 2,
    }
}

fn finding_bucket(f: &Finding) -> Option<String> {
    let raw = f.path.as_deref().filter(|s| !s.is_empty())?;
    let pretty = pretty_path(raw);
    let slashy = pretty.replace('\\', "/");
    let parts: Vec<&str> = slashy
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }
    const KNOWN: &[&str] = &[
        "workspacestorage",
        "globalstorage",
        "cacheddata",
        "cachedextensions",
        "shell-snapshots",
        "shadercache",
        "gpucache",
    ];
    for part in &parts {
        if KNOWN.iter().any(|k| part.eq_ignore_ascii_case(k)) {
            return Some((*part).to_string());
        }
    }
    // Parent folder name (the file's containing directory).
    if parts.len() >= 2 {
        Some(parts[parts.len() - 2].to_string())
    } else {
        None
    }
}

fn file_mode_label(report: &DiffReport) -> String {
    match report.file_mode {
        crate::FileMode::Usn => "USN change journal".into(),
        crate::FileMode::FallbackScan => "fallback folder scan (mtime/hash)".into(),
        crate::FileMode::Mixed => "USN journal + fallback folder scan".into(),
        crate::FileMode::Unavailable => "file changes not collected".into(),
    }
}

fn counts_line(counts: &SeverityCounts, ignored: usize) -> String {
    format!(
        "{} high,  {} medium,  {} info    ({} file events ignored)\n",
        counts.high, counts.medium, counts.info, ignored
    )
}

fn filter_sev(report: &DiffReport, sev: Severity) -> Vec<&Finding> {
    report
        .findings
        .iter()
        .filter(|f| f.severity == sev)
        .collect()
}

fn write_section(out: &mut String, heading: &str, items: &[&Finding], _md: bool) {
    out.push('\n');
    out.push_str(heading);
    out.push('\n');
    out.push_str(&"─".repeat(heading.len()));
    out.push('\n');
    if items.is_empty() {
        out.push_str("(none)\n");
        return;
    }
    for f in items {
        out.push_str(&format!("[{}] {}\n", f.category, f.title));
        if !f.detail.is_empty() {
            out.push_str(&format!("    {}\n", f.detail));
        }
        out.push_str(&format!("    why: {}\n", f.reason));
    }
}

fn write_md_section(out: &mut String, heading: &str, items: &[&Finding]) {
    out.push_str(&format!("\n## {heading}\n\n"));
    if items.is_empty() {
        out.push_str("_none_\n");
        return;
    }
    for f in items {
        out.push_str(&format!("- **[{}] {}**\n", f.category, f.title));
        if !f.detail.is_empty() {
            out.push_str(&format!("  - {}\n", f.detail));
        }
        out.push_str(&format!("  - why: {}\n", f.reason));
    }
}



fn write_attrib_preamble(out: &mut String, report: &DiffReport, markdown: bool) {
    let caveat = if report.attribution.caveat.is_empty() {
        crate::SYSTEM_WIDE_CAVEAT
    } else {
        report.attribution.caveat.as_str()
    };
    if markdown {
        out.push_str(&format!("- **attribution:** {caveat}\n"));
        if report.attribution.status.has_events() {
            out.push_str(&format!(
                "- **split:** {} agent, {} other, {} unattributed\n",
                report.attribution.agent, report.attribution.other, report.attribution.unattributed
            ));
        }
    } else {
        out.push_str(&format!("note:     {caveat}\n"));
        if report.attribution.status.has_events() {
            out.push_str(&format!(
                "split:    {} by the agent, {} by other apps, {} unattributed\n",
                report.attribution.agent, report.attribution.other, report.attribution.unattributed
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Finding;
    use leash_filter::Severity;

    fn finding(sev: Severity, path: &str) -> Finding {
        Finding {
            category: "files".into(),
            severity: sev,
            title: "File data overwrite".into(),
            detail: path.into(),
            reason: "test".into(),
            path: Some(path.into()),
            attribution: Default::default(),
        }
    }

    #[test]
    fn aggregates_known_state_directory() {
        let mut findings = Vec::new();
        for i in 0..42 {
            findings.push(finding(
                Severity::High,
                &format!(r"C:\Users\Alek\AppData\Roaming\Code\User\workspaceStorage\hash{i}\state.vscdb"),
            ));
        }
        findings.push(finding(
            Severity::High,
            r"C:\Users\Alek\AppData\Roaming\Code\User\settings.json",
        ));
        let lines = aggregate_finding_lines(&findings, 5);
        assert!(
            lines.iter().any(|l| l == "workspaceStorage: 42 changes"),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("settings.json") && !l.contains("workspaceStorage")),
            "{lines:?}"
        );
        assert!(lines.len() <= 5);
    }

    #[test]
    fn single_finding_is_not_aggregated() {
        let findings = vec![finding(Severity::High, r"C:\Users\Alek\.ssh\config")];
        let lines = aggregate_finding_lines(&findings, 5);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains(".ssh") || lines[0].contains("config"), "{lines:?}");
        assert!(!lines[0].contains("changes"));
    }

    #[test]
    fn summary_line_strips_verbatim_prefix() {
        let f = finding(
            Severity::High,
            r"\\?\C:\Users\Alek\.ssh\config",
        );
        let line = summary_finding_line(&f);
        assert!(!line.contains("\\\\?\\"), "{line}");
        assert!(line.contains(r"C:\Users\Alek\.ssh\config"), "{line}");
    }
}
