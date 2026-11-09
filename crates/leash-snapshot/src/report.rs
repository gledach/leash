use crate::model::{DiffReport, Finding, SeverityCounts};
use leash_filter::Severity;

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
