//! Self-contained HTML report. No external requests, no CDN.

use crate::model::{AttributionBucket, DiffReport, Finding};
use leash_filter::Severity;
use std::collections::BTreeMap;

const CATEGORIES: &[(&str, &str, &[&str])] = &[
    ("file", "Files", &["file"]),
    ("registry", "Startup / Run keys", &["registry"]),
    ("task", "Scheduled tasks", &["task"]),
    ("service", "Services", &["service"]),
    ("env", "Env / PATH", &["env"]),
    ("package", "Packages", &["package"]),
    (
        "dotfiles",
        "Dotfiles / git / ssh",
        &[
            "shell profile",
            "git config",
            "ssh config",
            "ssh authorized_keys",
            "ssh key file",
        ],
    ),
    ("port", "Ports", &["port"]),
    ("process", "Leftover processes", &["process"]),
];

pub fn render_html(report: &DiffReport) -> String {
    let mut html = String::new();
    html.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    html.push_str("<meta charset=\"utf-8\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    html.push_str("<meta name=\"color-scheme\" content=\"light dark\">\n");
    html.push_str("<title>");
    html.push_str(&escape_html(&format!(
        "leash session {}",
        report.session_id
    )));
    html.push_str("</title>\n<style>\n");
    html.push_str(CSS);
    html.push_str("\n</style>\n</head>\n<body>\n");
    html.push_str("<main class=\"page\">\n");
    render_header(&mut html, report);
    render_summary_strip(&mut html, report);
    render_chips(&mut html, report);
    if report.attribution.status.has_events() {
        render_bucket(&mut html, report, AttributionBucket::Agent, true);
        render_bucket(&mut html, report, AttributionBucket::Other, false);
        render_bucket(&mut html, report, AttributionBucket::Unattributed, true);
    } else {
        render_file_tree(&mut html, report, None);
        render_category_cards(&mut html, report, None);
    }
    render_footer(&mut html, report);
    html.push_str("</main>\n<script>\n");
    html.push_str(JS);
    html.push_str("\n</script>\n</body>\n</html>\n");
    html
}

/// Escape text for HTML text and quoted attributes. Ampersand first.
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

pub fn verdict_line(report: &DiffReport) -> String {
    let c = &report.counts;
    if c.high == 0 && c.medium == 0 && c.info == 0 {
        "Nothing outside your project changed".into()
    } else if c.high == 1 {
        "1 thing needs a look".into()
    } else if c.high > 1 {
        format!("{} things need a look", c.high)
    } else if c.medium == 1 {
        "1 thing to be aware of, nothing High".into()
    } else if c.medium > 1 {
        format!("{} things to be aware of, nothing High", c.medium)
    } else {
        "Only low-priority changes; worth a skim".into()
    }
}

fn render_header(html: &mut String, report: &DiffReport) {
    html.push_str("<header class=\"head\">\n");
    html.push_str("<p class=\"kicker\">leash session</p>\n");
    html.push_str(&format!("<h1>{}</h1>\n", escape_html(&report.session_id)));
    html.push_str("<dl class=\"meta\">\n");
    meta_row(html, "From", &report.before_at);
    meta_row(html, "To", &report.after_at);
    if let Some(dur) = duration_label(&report.before_at, &report.after_at) {
        meta_row(html, "Duration", &dur);
    }
    if let Some(repo) = &report.repo_path {
        meta_row(html, "Repo", repo);
    }
    meta_row(html, "Files", &file_mode_label(report));
    if report.attribution.status.has_events() {
        if let Some(target) = &report.attribution.target {
            meta_row(html, "Watching", target);
        }
        meta_row(
            html,
            "Attribution",
            &format!(
                "{} by the agent, {} by other apps, {} unattributed",
                report.attribution.agent, report.attribution.other, report.attribution.unattributed
            ),
        );
    }
    html.push_str("</dl>\n");
    let caveat = if report.attribution.caveat.is_empty() {
        crate::SYSTEM_WIDE_CAVEAT
    } else {
        report.attribution.caveat.as_str()
    };
    html.push_str("<p class=\"caveat\">");
    html.push_str(&escape_html(caveat));
    html.push_str("</p>\n</header>\n");
}

fn meta_row(html: &mut String, key: &str, value: &str) {
    html.push_str("<div><dt>");
    html.push_str(&escape_html(key));
    html.push_str("</dt><dd>");
    html.push_str(&escape_html(value));
    html.push_str("</dd></div>\n");
}

fn file_mode_label(report: &DiffReport) -> String {
    let mode = match report.file_mode {
        crate::FileMode::Usn => "USN change journal",
        crate::FileMode::FallbackScan => "fallback folder scan (mtime/hash)",
        crate::FileMode::Mixed => "USN journal + fallback folder scan",
        crate::FileMode::Unavailable => "file changes not collected",
    };
    if report.elevated {
        format!("{mode} · elevated")
    } else {
        format!("{mode} · not elevated")
    }
}

fn duration_label(before: &str, after: &str) -> Option<String> {
    let b = chrono::DateTime::parse_from_rfc3339(before).ok()?;
    let a = chrono::DateTime::parse_from_rfc3339(after).ok()?;
    let secs = a.signed_duration_since(b).num_seconds();
    if secs < 0 {
        return None;
    }
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    Some(if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    })
}

fn render_summary_strip(html: &mut String, report: &DiffReport) {
    html.push_str("<section class=\"strip\" aria-label=\"Summary\">\n");
    html.push_str(&format!(
        "<p class=\"verdict\">{}</p>\n",
        escape_html(&verdict_line(report))
    ));
    html.push_str("<ul class=\"counts\">\n");
    count_pill(html, "high", "High", report.counts.high);
    count_pill(html, "medium", "Medium", report.counts.medium);
    count_pill(html, "info", "Info", report.counts.info);
    html.push_str(&format!(
        "<li class=\"count muted\">{} ignored</li>\n",
        report.ignored.total
    ));
    html.push_str("</ul>\n");
    if !report.warnings.is_empty() {
        html.push_str("<ul class=\"warnings\">\n");
        for w in &report.warnings {
            html.push_str("<li>");
            html.push_str(&escape_html(w));
            html.push_str("</li>\n");
        }
        html.push_str("</ul>\n");
    }
    html.push_str("</section>\n");
}

fn count_pill(html: &mut String, class: &str, label: &str, n: usize) {
    html.push_str(&format!(
        "<li class=\"count {class}\"><strong>{}</strong> {}</li>\n",
        n,
        escape_html(label)
    ));
}

fn render_chips(html: &mut String, report: &DiffReport) {
    html.push_str("<nav class=\"chips\" aria-label=\"Filter findings\">\n");
    html.push_str("<p class=\"chip-label\">Severity</p>\n<div class=\"chip-row\">\n");
    sev_chip(html, "high", "High", report.counts.high);
    sev_chip(html, "medium", "Medium", report.counts.medium);
    sev_chip(html, "info", "Info", report.counts.info);
    html.push_str("</div>\n<p class=\"chip-label\">Category</p>\n<div class=\"chip-row\">\n");
    for (id, label, keys) in CATEGORIES {
        let n = report
            .findings
            .iter()
            .filter(|f| keys.iter().any(|k| f.category.eq_ignore_ascii_case(k)))
            .count();
        if n == 0 {
            continue;
        }
        html.push_str(&format!(
            "<button type=\"button\" class=\"chip is-on\" data-filter-cat=\"{}\" aria-pressed=\"true\">{} <span>{}</span></button>\n",
            escape_html(id),
            escape_html(label),
            n
        ));
    }
    html.push_str("</div>\n</nav>\n");
}

fn sev_chip(html: &mut String, id: &str, label: &str, n: usize) {
    html.push_str(&format!(
        "<button type=\"button\" class=\"chip is-on {id}\" data-filter-sev=\"{}\" aria-pressed=\"true\">{} <span>{}</span></button>\n",
        escape_html(id),
        escape_html(label),
        n
    ));
}

fn render_file_tree(html: &mut String, report: &DiffReport, bucket: Option<AttributionBucket>) {
    let files: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|f| f.category == "file")
        .filter(|f| bucket.is_none_or(|b| f.attribution.bucket == b))
        .collect();
    if files.is_empty() {
        return;
    }
    html.push_str("<section class=\"card\" data-category=\"file\">\n");
    html.push_str("<h2>Files</h2>\n<p class=\"lead\">Grouped by folder. Open a directory to see what changed.</p>\n");
    let mut root = DirNode::default();
    for f in &files {
        let path = f.path.as_deref().unwrap_or(f.detail.as_str());
        let parts = path_components(path);
        if parts.is_empty() {
            continue;
        }
        root.insert(&parts, f);
    }
    html.push_str("<div class=\"tree\">\n");
    root.render(html, true);
    html.push_str("</div>\n</section>\n");
}

fn render_category_cards(html: &mut String, report: &DiffReport, bucket: Option<AttributionBucket>) {
    for (id, label, keys) in CATEGORIES {
        if *id == "file" {
            continue; // already rendered as a tree
        }
        let items: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|f| keys.iter().any(|k| f.category.eq_ignore_ascii_case(k)))
            .filter(|f| bucket.is_none_or(|b| f.attribution.bucket == b))
            .collect();
        if items.is_empty() {
            continue;
        }
        html.push_str(&format!(
            "<section class=\"card\" data-category=\"{}\">\n<h2>{}</h2>\n<ul class=\"items\">\n",
            escape_html(id),
            escape_html(label)
        ));
        for f in items {
            html.push_str(&format!(
                "<li class=\"item\" data-severity=\"{}\" data-category=\"{}\">\n",
                escape_html(f.severity.as_str()),
                escape_html(id)
            ));
            html.push_str(&format!(
                "<p class=\"item-title\"><span class=\"badge {}\">{}</span> {}</p>\n",
                escape_html(f.severity.as_str()),
                escape_html(f.severity.as_str()),
                escape_html(&f.title)
            ));
            if !f.detail.is_empty() {
                html.push_str("<p class=\"item-detail\">");
                html.push_str(&escape_html(&f.detail));
                html.push_str("</p>\n");
            }
            if !f.reason.is_empty() {
                html.push_str("<p class=\"item-why\">");
                html.push_str(&escape_html(&f.reason));
                html.push_str("</p>\n");
            }
            render_attrib_line(html, f);
            html.push_str("</li>\n");
        }
        html.push_str("</ul>\n</section>\n");
    }
}



fn attrib_label(f: &Finding) -> Option<String> {
    let mut bits: Vec<String> = Vec::new();
    if let Some(chain) = &f.attribution.process_chain {
        bits.push(chain.clone());
    } else if let Some(name) = &f.attribution.process_name {
        if let Some(pid) = f.attribution.process_pid {
            bits.push(format!("{name} (pid {pid})"));
        } else {
            bits.push(name.clone());
        }
    }
    if let Some(note) = &f.attribution.note {
        bits.push(note.clone());
    }
    if bits.is_empty() {
        None
    } else {
        Some(bits.join(" · "))
    }
}

fn render_attrib_line(html: &mut String, f: &Finding) {
    if f.attribution.bucket == AttributionBucket::None
        && f.attribution.process_chain.is_none()
        && f.attribution.note.is_none()
    {
        return;
    }
    let mut bits: Vec<String> = Vec::new();
    if let Some(chain) = &f.attribution.process_chain {
        bits.push(chain.clone());
    } else if let Some(name) = &f.attribution.process_name {
        if let Some(pid) = f.attribution.process_pid {
            bits.push(format!("{name} (pid {pid})"));
        } else {
            bits.push(name.clone());
        }
    }
    if let Some(note) = &f.attribution.note {
        bits.push(note.clone());
    }
    if bits.is_empty() {
        return;
    }
    html.push_str("<p class=\"item-proc\">");
    html.push_str(&escape_html(&bits.join(" · ")));
    html.push_str("</p>\n");
}

fn render_bucket(
    html: &mut String,
    report: &DiffReport,
    bucket: AttributionBucket,
    _open: bool,
) {
    let count = report
        .findings
        .iter()
        .filter(|f| f.attribution.bucket == bucket)
        .count();
    if count == 0 {
        return;
    }
    let (class, collapsed) = match bucket {
        AttributionBucket::Other => ("bucket bucket-other", true),
        AttributionBucket::Unattributed => ("bucket bucket-unknown", false),
        _ => ("bucket bucket-agent", false),
    };
    if collapsed {
        html.push_str("<details class=\"");
        html.push_str(class);
        html.push_str("\">\n<summary><h2>");
        html.push_str(&escape_html(bucket.heading()));
        html.push_str(&format!(" <span class=\"muted\">{count}</span></h2></summary>\n"));
    } else {
        html.push_str("<section class=\"");
        html.push_str(class);
        html.push_str("\">\n<h2>");
        html.push_str(&escape_html(bucket.heading()));
        html.push_str(&format!(" <span class=\"muted\">{count}</span></h2>\n"));
    }
    let lead = match bucket {
        AttributionBucket::Agent => "The process that made the change was in the agent's tree.",
        AttributionBucket::Other => "These changed during the session, but the process was not in the agent's tree.",
        AttributionBucket::Unattributed => "No ETW event named a process for these, or the work went through a service. That is not a guess that the agent did them.",
        AttributionBucket::None => "",
    };
    if !lead.is_empty() {
        html.push_str("<p class=\"lead\">");
        html.push_str(lead);
        html.push_str("</p>\n");
    }
    render_file_tree(html, report, Some(bucket));
    render_category_cards(html, report, Some(bucket));
    if collapsed {
        html.push_str("</details>\n");
    } else {
        html.push_str("</section>\n");
    }
}

fn attribution_footer_line(report: &DiffReport) -> String {
    if report.attribution.status.has_events() {
        "Attribution names a process only when an ETW event joins to the finding. Work done only inside a service is labelled unattributed, not guessed. Short-lived helpers may already be gone at stop.".into()
    } else {
        "This preview records every change on the PC during the session, not only the agent's. Other apps running at the same time appear too. Per-process attribution needs an elevated `leash run` (or attach). Short-lived helpers may already be gone at stop.".into()
    }
}

fn render_footer(html: &mut String, report: &DiffReport) {
    let attrib = attribution_footer_line(report);
    html.push_str("<footer class=\"blind\">\n<h2>Blind spots</h2>\n");
    html.push_str("<p>This report is an observation, not a guarantee. leash never deletes, kills, or blocks anything.</p>\n<ul>\n");
    for line in [
        "Changes inside the watched repo are ignored by design, except for a few high-priority names such as .env.",
        "File reads are invisible. The journal records creates, deletes, renames, and writes — not opens.",
        attrib.as_str(),
        "Without Administrator, file coverage is a fallback scan of priority folders, not the whole disk.",
        "The USN journal can wrap or be recreated; early events may be missing, and the report will warn if so.",
        "Default filters drop node_modules, .git, temp, and caches. Something planted there will not show up.",
        "Network-only activity with no new file, Run key, or listen port is out of scope.",
    ] {
        html.push_str("<li>");
        html.push_str(line);
        html.push_str("</li>\n");
    }
    html.push_str("</ul>\n</footer>\n");
}

#[derive(Default)]
struct DirNode {
    dirs: BTreeMap<String, DirNode>,
    files: Vec<FileLeaf>,
}

struct FileLeaf {
    name: String,
    badge: &'static str,
    severity: Severity,
    detail: String,
    proc: Option<String>,
}

impl DirNode {
    fn insert(&mut self, parts: &[String], finding: &Finding) {
        if parts.is_empty() {
            return;
        }
        if parts.len() == 1 {
            self.files.push(FileLeaf {
                name: parts[0].clone(),
                badge: file_badge(&finding.title),
                severity: finding.severity,
                detail: finding.detail.clone(),
                proc: attrib_label(finding),
            });
            return;
        }
        self.dirs
            .entry(parts[0].clone())
            .or_default()
            .insert(&parts[1..], finding);
    }

    fn render(&self, html: &mut String, top: bool) {
        for (name, child) in &self.dirs {
            let (created, deleted, renamed, modified) = child.tally();
            let mut badges = Vec::new();
            if created > 0 {
                badges.push(format!("{created} created"));
            }
            if deleted > 0 {
                badges.push(format!("{deleted} deleted"));
            }
            if renamed > 0 {
                badges.push(format!("{renamed} renamed"));
            }
            if modified > 0 {
                badges.push(format!("{modified} modified"));
            }
            html.push_str("<details class=\"folder\"");
            if top {
                html.push_str(" open");
            }
            html.push_str(">\n<summary>");
            html.push_str(&escape_html(name));
            if !badges.is_empty() {
                html.push_str(" <span class=\"dir-badges\">");
                html.push_str(&escape_html(&badges.join(" · ")));
                html.push_str("</span>");
            }
            html.push_str("</summary>\n");
            child.render(html, false);
            html.push_str("</details>\n");
        }
        if !self.files.is_empty() {
            html.push_str("<ul class=\"leaves\">\n");
            for leaf in &self.files {
                html.push_str(&format!(
                    "<li class=\"item leaf\" data-severity=\"{}\" data-category=\"file\">",
                    escape_html(leaf.severity.as_str())
                ));
                html.push_str(&format!(
                    "<span class=\"fbadge {}\">{}</span> <span class=\"fname\">{}</span>",
                    escape_html(leaf.badge),
                    escape_html(leaf.badge),
                    escape_html(&leaf.name)
                ));
                if !leaf.detail.is_empty() && leaf.detail != leaf.name {
                    html.push_str(" <span class=\"fdetail\">");
                    html.push_str(&escape_html(&leaf.detail));
                    html.push_str("</span>");
                }
                if let Some(proc) = &leaf.proc {
                    html.push_str(" <span class=\"item-proc\">");
                    html.push_str(&escape_html(proc));
                    html.push_str("</span>");
                }
                html.push_str("</li>\n");
            }
            html.push_str("</ul>\n");
        }
    }

    fn tally(&self) -> (usize, usize, usize, usize) {
        let mut c = (0, 0, 0, 0);
        for f in &self.files {
            match f.badge {
                "created" => c.0 += 1,
                "deleted" => c.1 += 1,
                "renamed" => c.2 += 1,
                _ => c.3 += 1,
            }
        }
        for d in self.dirs.values() {
            let t = d.tally();
            c.0 += t.0;
            c.1 += t.1;
            c.2 += t.2;
            c.3 += t.3;
        }
        c
    }
}

fn file_badge(title: &str) -> &'static str {
    let t = title.to_ascii_lowercase();
    if t.contains("delete") {
        "deleted"
    } else if t.contains("create") {
        "created"
    } else if t.contains("rename") {
        "renamed"
    } else {
        "modified"
    }
}

fn path_components(path: &str) -> Vec<String> {
    let raw = path.replace('\\', "/");
    let mut parts: Vec<String> = raw
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    if parts.is_empty() {
        return parts;
    }
    // Keep a drive letter attached as the first component, then drop the
    // C:\Users\<name>\ prefix so the tree starts at Desktop, AppData, etc.
    if parts.len() >= 4 && parts[0].ends_with(':') && parts[1].eq_ignore_ascii_case("Users") {
        parts = parts[3..].to_vec();
    }
    parts
}

const CSS: &str = r#"
:root {
  --bg: #f3f1eb;
  --ink: #1c1916;
  --muted: #5c574f;
  --rule: #d4cfc4;
  --card: #fffdf8;
  --chip: #ece8df;
  --high: #9f1f1f;
  --medium: #8a5510;
  --info: #243f5c;
  --created: #1d5a38;
  --deleted: #9f1f1f;
  --renamed: #5c3d8a;
  --modified: #243f5c;
  --focus: #1c1916;
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #141311;
    --ink: #efece6;
    --muted: #b2aca2;
    --rule: #3a372f;
    --card: #1c1a17;
    --chip: #2a2722;
    --high: #f0a3a0;
    --medium: #e6c07a;
    --info: #9db8d1;
    --created: #8dcaa8;
    --deleted: #f0a3a0;
    --renamed: #cbb4ea;
    --modified: #9db8d1;
    --focus: #efece6;
  }
}
* { box-sizing: border-box; }
html { color-scheme: light dark; }
body {
  margin: 0;
  background: var(--bg);
  color: var(--ink);
  font: 16px/1.5 "Segoe UI", system-ui, -apple-system, "Helvetica Neue", Arial, sans-serif;
}
.page {
  max-width: 52rem;
  margin: 0 auto;
  padding: 2.5rem 1.25rem 4rem;
}
.head { border-bottom: 1px solid var(--rule); padding-bottom: 1.5rem; margin-bottom: 1.5rem; }
.kicker {
  margin: 0 0 0.3rem;
  letter-spacing: 0.14em;
  text-transform: uppercase;
  font-size: 0.72rem;
  color: var(--muted);
}
h1 { margin: 0 0 1rem; font-size: 1.85rem; font-weight: 650; letter-spacing: -0.02em; }
h2 { margin: 0 0 0.7rem; font-size: 1.1rem; font-weight: 650; }
.meta { display: grid; gap: 0.45rem 1.5rem; margin: 0; grid-template-columns: repeat(auto-fit, minmax(14rem, 1fr)); }
.meta div { margin: 0; }
.meta dt { font-size: 0.72rem; letter-spacing: 0.08em; text-transform: uppercase; color: var(--muted); }
.meta dd { margin: 0; word-break: break-word; }
.strip { margin-bottom: 1.5rem; }
.verdict { font-size: 1.25rem; margin: 0 0 0.8rem; font-weight: 600; letter-spacing: -0.01em; }
.counts { display: flex; flex-wrap: wrap; gap: 0.6rem 1.1rem; list-style: none; padding: 0; margin: 0; }
.count { font-size: 0.95rem; }
.count strong { font-size: 1.15rem; margin-right: 0.2rem; }
.count.high strong { color: var(--high); }
.count.medium strong { color: var(--medium); }
.count.info strong { color: var(--info); }
.count.muted { color: var(--muted); }
.warnings { margin: 0.8rem 0 0; padding-left: 1.1rem; color: var(--muted); font-size: 0.92rem; }
.chips { margin: 0 0 1.75rem; }
.chip-label { margin: 0.6rem 0 0.35rem; font-size: 0.72rem; letter-spacing: 0.1em; text-transform: uppercase; color: var(--muted); }
.chip-row { display: flex; flex-wrap: wrap; gap: 0.4rem; }
.chip {
  border: 1px solid var(--rule);
  background: var(--chip);
  color: var(--ink);
  border-radius: 999px;
  padding: 0.2rem 0.7rem;
  font: inherit;
  font-size: 0.88rem;
  cursor: pointer;
}
.chip span { color: var(--muted); margin-left: 0.15rem; }
.chip.is-on { background: var(--ink); color: var(--bg); }
.chip.is-on span { color: inherit; opacity: 0.75; }
.chip:focus-visible { outline: 2px solid var(--focus); outline-offset: 2px; }
.card {
  background: var(--card);
  border: 1px solid var(--rule);
  border-radius: 6px;
  padding: 1.1rem 1.15rem 1.2rem;
  margin-bottom: 1rem;
}
.lead { margin: -0.3rem 0 0.8rem; color: var(--muted); font-size: 0.92rem; }
.items { list-style: none; padding: 0; margin: 0; }
.item { padding: 0.7rem 0; border-top: 1px solid var(--rule); }
.item:first-child { border-top: 0; padding-top: 0; }
.item-title { margin: 0 0 0.2rem; }
.item-detail, .item-why, .fdetail { margin: 0; color: var(--muted); font-size: 0.92rem; word-break: break-word; }
.item-why { margin-top: 0.15rem; }
.badge, .fbadge {
  display: inline-block;
  font-size: 0.72rem;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  margin-right: 0.35rem;
}
.badge.high, .fbadge.deleted { color: var(--high); }
.badge.medium { color: var(--medium); }
.badge.info, .fbadge.modified { color: var(--info); }
.fbadge.created { color: var(--created); }
.fbadge.renamed { color: var(--renamed); }
.tree details { margin: 0.15rem 0 0.15rem 0.2rem; }
.tree > details { margin-left: 0; }
.folder > summary { cursor: pointer; font-weight: 600; padding: 0.2rem 0; }
.dir-badges { font-weight: 400; color: var(--muted); font-size: 0.85rem; }
.leaves { list-style: none; margin: 0.15rem 0 0.3rem; padding: 0 0 0 1rem; }
.leaf { border: 0; padding: 0.18rem 0; }
.fname { word-break: break-word; }
.item.is-hidden, .card.is-hidden { display: none; }
.blind { margin-top: 2.2rem; padding-top: 1.2rem; border-top: 1px solid var(--rule); color: var(--muted); font-size: 0.92rem; }
.blind h2 { color: var(--ink); }
.blind ul { padding-left: 1.15rem; }
.blind li { margin: 0.3rem 0; }
"#;

const JS: &str = r#"
(function () {
  const sevBtns = [].slice.call(document.querySelectorAll("[data-filter-sev]"));
  const catBtns = [].slice.call(document.querySelectorAll("[data-filter-cat]"));
  function on(btn) { return btn.classList.contains("is-on"); }
  function apply() {
    const sev = {};
    sevBtns.forEach(function (b) { if (on(b)) sev[b.getAttribute("data-filter-sev")] = true; });
    const cat = {};
    catBtns.forEach(function (b) { if (on(b)) cat[b.getAttribute("data-filter-cat")] = true; });
    document.querySelectorAll(".item").forEach(function (el) {
      const s = el.getAttribute("data-severity");
      const c = el.getAttribute("data-category");
      const show = (!s || sev[s]) && (!c || cat[c]);
      el.classList.toggle("is-hidden", !show);
    });
    document.querySelectorAll(".card").forEach(function (card) {
      const visible = card.querySelector(".item:not(.is-hidden)");
      const c = card.getAttribute("data-category");
      const catOn = !c || cat[c];
      card.classList.toggle("is-hidden", !catOn || !visible);
    });
  }
  function bind(btns) {
    btns.forEach(function (btn) {
      btn.addEventListener("click", function () {
        btn.classList.toggle("is-on");
        btn.setAttribute("aria-pressed", on(btn) ? "true" : "false");
        apply();
      });
    });
  }
  bind(sevBtns);
  bind(catBtns);
})();
"#;
