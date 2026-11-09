//! Filtering and grouping for the "Watch a running app" dropdown and `leash ps`.
//! Host-independent so it can be unit-tested with fixtures on Linux.

use crate::discover::{
    classify, count_tree, discover_candidates, AppKind, ProcView, WatchCandidate,
};
use crate::service::{is_kernel_pid, is_service_image};
use std::collections::{HashMap, HashSet};

/// What the user picked in the dropdown (or implicitly via Start).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchSelection {
    /// Plain system-wide session, no process-tree attribution.
    WholePc,
    /// Attach to these root pids (an editor, an agent, or every instance of an image).
    Roots {
        pids: Vec<u32>,
        label: String,
    },
}

/// One row in the picker or in `leash ps`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerRow {
    pub selection: WatchSelection,
    pub title: String,
    pub subtitle: String,
    pub depth: usize,
    pub search_text: String,
    pub summary: String,
}

impl PickerRow {
    pub fn matches(&self, query: &str) -> bool {
        let q = query.trim().to_ascii_lowercase();
        if q.is_empty() {
            return true;
        }
        self.search_text.to_ascii_lowercase().contains(&q)
    }
}

/// Grouped, unfiltered picker contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerModel {
    pub whole_pc: PickerRow,
    pub agents_and_editors: Vec<PickerRow>,
    pub other_apps: Vec<PickerRow>,
}

impl PickerModel {
    pub fn filter(&self, query: &str) -> Self {
        let q = query.trim();
        if q.is_empty() {
            return self.clone();
        }
        Self {
            whole_pc: self.whole_pc.clone(),
            agents_and_editors: self
                .agents_and_editors
                .iter()
                .filter(|r| r.matches(q))
                .cloned()
                .collect(),
            other_apps: expand_other_matches(&self.other_apps, q),
        }
    }

    pub fn row_by_selection(&self, sel: &WatchSelection) -> Option<&PickerRow> {
        if &self.whole_pc.selection == sel {
            return Some(&self.whole_pc);
        }
        self.agents_and_editors
            .iter()
            .chain(self.other_apps.iter())
            .find(|r| &r.selection == sel)
    }
}

fn expand_other_matches(others: &[PickerRow], query: &str) -> Vec<PickerRow> {
    others
        .iter()
        .filter(|r| r.matches(query))
        .cloned()
        .collect()
}

pub fn whole_pc_row() -> PickerRow {
    PickerRow {
        selection: WatchSelection::WholePc,
        title: "Whole PC (no attribution)".into(),
        subtitle: "Every change on this computer during the session, including other apps.".into(),
        depth: 0,
        search_text: "whole pc system-wide no attribution all processes".into(),
        summary: "Whole PC (no attribution) — every change on this computer during the session, not only an agent's."
            .into(),
    }
}

/// Build the dropdown model from a process snapshot.
///
/// `now_unix` is seconds since the epoch, used for "started … ago".
/// `home` is the user profile path, used to shorten working directories to `~\…`.
pub fn build_picker(procs: &[ProcView], now_unix: i64, home: Option<&str>) -> PickerModel {
    let by_pid: HashMap<u32, &ProcView> = procs.iter().map(|p| (p.pid, p)).collect();
    let candidates = discover_candidates(procs);
    let candidate_pids: HashSet<u32> = candidates.iter().map(|c| c.pid).collect();

    // Also hide editor/agent descendants from "Other apps" so Code.exe helpers
    // are not listed twice.
    let mut hidden = candidate_pids.clone();
    for c in &candidates {
        if c.depth == 0 && c.kind == AppKind::Editor {
            for pid in descendant_pids(c.pid, procs) {
                hidden.insert(pid);
            }
        }
        if c.kind == AppKind::Agent {
            hidden.insert(c.pid);
        }
    }

    let mut agents = Vec::new();
    for c in &candidates {
        let proc = by_pid.get(&c.pid).copied();
        agents.push(row_for_candidate(c, proc, procs, now_unix, home));
    }

    let other = group_other_apps(procs, &hidden, now_unix, home);

    PickerModel {
        whole_pc: whole_pc_row(),
        agents_and_editors: agents,
        other_apps: other,
    }
}

fn descendant_pids(root: u32, procs: &[ProcView]) -> Vec<u32> {
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in procs {
        if let Some(pp) = p.ppid {
            kids.entry(pp).or_default().push(p.pid);
        }
    }
    let mut out = Vec::new();
    let mut stack = vec![root];
    let mut seen = HashSet::new();
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if pid != root {
            out.push(pid);
        }
        if let Some(cs) = kids.get(&pid) {
            stack.extend(cs);
        }
    }
    out
}

fn row_for_candidate(
    c: &WatchCandidate,
    proc: Option<&ProcView>,
    procs: &[ProcView],
    now_unix: i64,
    home: Option<&str>,
) -> PickerRow {
    let title = friendly_title(c, proc);
    let cwd = proc.and_then(|p| p.cwd.as_deref()).map(|p| short_path(p, home));
    let started = proc.and_then(|p| format_started(p.create_time, now_unix));
    let tail = command_tail(proc.and_then(|p| p.command_line.as_deref()));
    let subtitle = second_line(cwd.as_deref(), started.as_deref(), c.pid, tail.as_deref());
    let summary = watch_summary(c, procs);
    let mut search = vec![
        title.clone(),
        c.name.clone(),
        c.app_id.clone(),
        c.pid.to_string(),
        c.lineage.clone(),
    ];
    if let Some(p) = proc {
        if let Some(cmd) = &p.command_line {
            search.push(cmd.clone());
        }
        if let Some(cwd) = &p.cwd {
            search.push(cwd.clone());
        }
        if let Some(t) = &p.window_title {
            search.push(t.clone());
        }
        if let Some(exe) = &p.exe {
            search.push(exe.clone());
        }
    }
    PickerRow {
        selection: WatchSelection::Roots {
            pids: vec![c.pid],
            label: title.clone(),
        },
        title,
        subtitle,
        depth: c.depth,
        search_text: search.join(" "),
        summary,
    }
}

fn group_other_apps(
    procs: &[ProcView],
    hidden: &HashSet<u32>,
    now_unix: i64,
    home: Option<&str>,
) -> Vec<PickerRow> {
    let mut groups: HashMap<String, Vec<&ProcView>> = HashMap::new();
    for p in procs {
        if is_kernel_pid(p.pid) || is_service_image(&p.name) || hidden.contains(&p.pid) {
            continue;
        }
        let key = p.name.to_ascii_lowercase();
        groups.entry(key).or_default().push(p);
    }
    let mut keys: Vec<String> = groups.keys().cloned().collect();
    keys.sort();
    let mut out = Vec::new();
    for key in keys {
        let mut members = groups.remove(&key).unwrap();
        members.sort_by_key(|p| p.pid);
        let name = members[0].name.clone();
        let pids: Vec<u32> = members.iter().map(|p| p.pid).collect();
        let n = pids.len();
        let title = if n == 1 {
            format!("{}  ({})", name, pids[0])
        } else {
            format!("{name} ({n})")
        };
        // For a group, prefer the earliest start and a shared cwd if any.
        let started = members
            .iter()
            .filter_map(|p| format_started(p.create_time, now_unix))
            .min();
        let cwd = members
            .iter()
            .find_map(|p| p.cwd.as_deref())
            .map(|p| short_path(p, home));
        let tail = if n == 1 {
            command_tail(members[0].command_line.as_deref())
        } else {
            Some(format!("{n} processes"))
        };
        let subtitle = if n == 1 {
            second_line(cwd.as_deref(), started.as_deref(), pids[0], tail.as_deref())
        } else {
            let mut bits = Vec::new();
            bits.push(format!("{n} processes"));
            bits.push(format!(
                "pids {}",
                pids.iter()
                    .take(6)
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
                    + if n > 6 { ", …" } else { "" }
            ));
            bits.join("  ·  ")
        };
        let mut search = vec![name.clone(), title.clone()];
        for p in &members {
            search.push(p.pid.to_string());
            if let Some(cmd) = &p.command_line {
                search.push(cmd.clone());
            }
            if let Some(cwd) = &p.cwd {
                search.push(cwd.clone());
            }
            if let Some(t) = &p.window_title {
                search.push(t.clone());
            }
        }
        let summary = if n == 1 {
            format!("{name} (pid {}) — that process and its children.", pids[0])
        } else {
            format!("{name} ({n} processes) — attaches to all of them and their children.")
        };
        // If the query wants a single member, we also keep per-pid text in search_text.
        out.push(PickerRow {
            selection: WatchSelection::Roots {
                pids,
                label: name,
            },
            title,
            subtitle,
            depth: 0,
            search_text: search.join(" "),
            summary,
        });
    }
    out
}

fn friendly_title(c: &WatchCandidate, proc: Option<&ProcView>) -> String {
    let folder = proc.and_then(|p| editor_folder(p.window_title.as_deref(), &c.app_id));
    match c.kind {
        AppKind::Editor => {
            let app = app_display_name(&c.app_id);
            if let Some(folder) = folder {
                format!("{app} window: {folder}")
            } else {
                app
            }
        }
        AppKind::Agent => {
            let agent = app_display_name(&c.app_id);
            if c.depth > 0 {
                format!("{} ({agent})", c.name)
            } else {
                agent
            }
        }
    }
}

fn app_display_name(id: &str) -> String {
    match id {
        "vscode" => "VS Code".into(),
        "cursor" => "Cursor".into(),
        "windsurf" => "Windsurf".into(),
        "claude" => "claude".into(),
        "codex" => "Codex".into(),
        "gemini" => "Gemini".into(),
        "aider" => "aider".into(),
        other => other.to_string(),
    }
}

fn editor_folder(title: Option<&str>, app_id: &str) -> Option<String> {
    let title = title?.trim();
    if title.is_empty() {
        return None;
    }
    let suffixes = match app_id {
        "vscode" => &[
            " - Visual Studio Code - Insiders",
            " - Visual Studio Code",
        ][..],
        "cursor" => &[" - Cursor"][..],
        "windsurf" => &[" - Windsurf"][..],
        _ => &[][..],
    };
    let mut rest = title;
    for s in suffixes {
        if let Some(stripped) = rest.strip_suffix(s) {
            rest = stripped.trim();
            break;
        }
    }
    if rest.is_empty() || rest.eq_ignore_ascii_case(title) && suffixes.iter().any(|s| title.ends_with(s.trim_start_matches(" - "))) {
        // title was only the app name
        if suffixes.iter().any(|s| title == s.trim_start_matches(" - ")) {
            return None;
        }
    }
    if rest.eq_ignore_ascii_case("Visual Studio Code")
        || rest.eq_ignore_ascii_case("Visual Studio Code - Insiders")
        || rest.eq_ignore_ascii_case("Cursor")
        || rest.eq_ignore_ascii_case("Windsurf")
    {
        return None;
    }
    // "file.rs - myrepo" → myrepo (VS Code often puts the folder last).
    if let Some((_, folder)) = rest.rsplit_once(" - ") {
        let folder = folder.trim();
        if !folder.is_empty() {
            return Some(folder.to_string());
        }
    }
    Some(rest.to_string())
}

/// `C:\Users\a\src\myapp` + home `C:\Users\a` → `~\src\myapp`.
pub fn short_path(path: &str, home: Option<&str>) -> String {
    let trimmed = path.trim().trim_end_matches(['\\', '/']);
    let norm = trimmed.replace('/', r"\");
    if let Some(home) = home {
        let home_n = home.trim().trim_end_matches(['\\', '/']).replace('/', r"\");
        if !home_n.is_empty()
            && norm.len() >= home_n.len()
            && norm[..home_n.len()].eq_ignore_ascii_case(&home_n)
        {
            let rest = &norm[home_n.len()..];
            let rest = rest.trim_start_matches('\\');
            if rest.is_empty() {
                return "~".into();
            }
            return format!(r"~\{rest}");
        }
    }
    // Keep at most the last three components if it is still long.
    if norm.len() > 48 {
        let parts: Vec<&str> = norm.split('\\').filter(|s| !s.is_empty()).collect();
        if parts.len() > 3 {
            return format!(r"…\{}", parts[parts.len() - 3..].join(r"\"));
        }
    }
    norm
}

/// "started 23:41, 12 min ago"
pub fn format_started(create_unix: Option<i64>, now_unix: i64) -> Option<String> {
    let created = create_unix?;
    if created <= 0 {
        return None;
    }
    let dt = chrono::DateTime::from_timestamp(created, 0)?;
    let local = dt.with_timezone(&chrono::Local);
    let clock = local.format("%H:%M").to_string();
    let delta = now_unix.saturating_sub(created).max(0);
    let ago = if delta < 60 {
        "just now".to_string()
    } else if delta < 3600 {
        let m = delta / 60;
        format!("{m} min ago")
    } else if delta < 86400 {
        let h = delta / 3600;
        if h == 1 {
            "1 h ago".into()
        } else {
            format!("{h} h ago")
        }
    } else {
        let d = delta / 86400;
        if d == 1 {
            "1 day ago".into()
        } else {
            format!("{d} days ago")
        }
    };
    Some(format!("started {clock}, {ago}"))
}

/// Strip the executable and keep a short tail of the command line.
pub fn command_tail(cmd: Option<&str>) -> Option<String> {
    let cmd = cmd?.trim();
    if cmd.is_empty() {
        return None;
    }
    let rest = if let Some(s) = cmd.strip_prefix('"') {
        s.split_once('"').map(|(_, r)| r.trim()).unwrap_or("")
    } else {
        cmd.split_once(char::is_whitespace)
            .map(|(_, r)| r.trim())
            .unwrap_or("")
    };
    let mut t = if rest.is_empty() { cmd.to_string() } else { rest.to_string() };
    const MAX: usize = 80;
    if t.len() > MAX {
        let start = t.len() - MAX;
        t = format!("…{}", &t[start..]);
    }
    Some(t)
}

fn second_line(
    cwd: Option<&str>,
    started: Option<&str>,
    pid: u32,
    tail: Option<&str>,
) -> String {
    let mut bits = Vec::new();
    if let Some(cwd) = cwd {
        bits.push(cwd.to_string());
    }
    if let Some(started) = started {
        bits.push(started.to_string());
    }
    bits.push(format!("pid {pid}"));
    if let Some(tail) = tail {
        if !tail.is_empty() {
            bits.push(tail.to_string());
        }
    }
    bits.join("  ·  ")
}

fn watch_summary(c: &WatchCandidate, procs: &[ProcView]) -> String {
    let n = count_tree(c.pid, procs);
    let children = n.saturating_sub(1);
    let name = app_display_name(&c.app_id);
    let mut inc: Vec<String> = Vec::new();
    if c.kind == AppKind::Editor {
        for p in procs {
            if p.pid == c.pid {
                continue;
            }
            // in tree?
            // cheap: walk later
        }
        let tree: HashSet<u32> = {
            let mut s = HashSet::new();
            s.insert(c.pid);
            for d in descendant_pids(c.pid, procs) {
                s.insert(d);
            }
            s
        };
        for p in procs {
            if tree.contains(&p.pid) {
                if let Some((id, kind, _)) = classify(p) {
                    if kind == AppKind::Agent && !inc.iter().any(|x| x == id) {
                        inc.push(app_display_name(id));
                    }
                }
            }
        }
    }
    let child_bit = if children == 0 {
        "no child processes".to_string()
    } else if children == 1 {
        "1 child process".to_string()
    } else {
        format!("{children} child processes")
    };
    if inc.is_empty() {
        if c.kind == AppKind::Editor {
            format!("{name} and {child_bit}")
        } else {
            format!("{name} (pid {}) and {child_bit}", c.pid)
        }
    } else {
        format!(
            "{name} and {child_bit}, including {}",
            inc.join(", ")
        )
    }
}

/// Text listing for `leash ps`, with the same distinguishing fields as the dropdown.
pub fn format_ps_rows(model: &PickerModel) -> String {
    let mut lines = Vec::new();
    if !model.agents_and_editors.is_empty() {
        lines.push("Agents & editors".into());
        for r in &model.agents_and_editors {
            let indent = if r.depth > 0 { "  " } else { "" };
            lines.push(format!("{indent}{}", r.title));
            if !r.subtitle.is_empty() {
                lines.push(format!("{indent}  {}", r.subtitle));
            }
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::ProcView;

    fn fixture_now() -> i64 {
        // 2026-11-10 00:00:00 UTC
        1_783_929_600
    }

    fn vscode_tree() -> Vec<ProcView> {
        let now = fixture_now();
        let started = now - 12 * 60;
        vec![
            ProcView {
                pid: 100,
                ppid: Some(1),
                name: "explorer.exe".into(),
                exe: Some(r"C:\Windows\explorer.exe".into()),
                command_line: None,
                window_title: None,
                cwd: None,
                create_time: None,
            },
            ProcView {
                pid: 200,
                ppid: Some(100),
                name: "Code.exe".into(),
                exe: Some(r"C:\Users\a\AppData\Local\Programs\Microsoft VS Code\Code.exe".into()),
                command_line: Some(r#""C:\Users\a\AppData\Local\Programs\Microsoft VS Code\Code.exe""#.into()),
                window_title: Some("gui.rs - myrepo - Visual Studio Code".into()),
                cwd: Some(r"C:\Users\a\src\myrepo".into()),
                create_time: Some(started),
            },
            ProcView {
                pid: 201,
                ppid: Some(200),
                name: "Code.exe".into(),
                exe: Some(r"C:\Users\a\AppData\Local\Programs\Microsoft VS Code\Code.exe".into()),
                command_line: Some(r#""C:\Users\a\AppData\Local\Programs\Microsoft VS Code\Code.exe" --type=renderer"#.into()),
                window_title: None,
                cwd: Some(r"C:\Users\a\src\myrepo".into()),
                create_time: Some(started),
            },
            ProcView {
                pid: 210,
                ppid: Some(200),
                name: "cmd.exe".into(),
                exe: Some(r"C:\Windows\System32\cmd.exe".into()),
                command_line: Some(r"C:\Windows\System32\cmd.exe /d /c claude".into()),
                window_title: None,
                cwd: Some(r"C:\Users\a\src\myrepo".into()),
                create_time: Some(started),
            },
            ProcView {
                pid: 220,
                ppid: Some(210),
                name: "node.exe".into(),
                exe: Some(r"C:\Program Files\nodejs\node.exe".into()),
                command_line: Some(
                    r#"C:\Program Files\nodejs\node.exe C:\Users\a\AppData\Roaming\npm\node_modules\@anthropic-ai\claude-code\cli.js"#
                        .into(),
                ),
                window_title: None,
                cwd: Some(r"C:\Users\a\src\myrepo".into()),
                create_time: Some(now - 5 * 60),
            },
            ProcView {
                pid: 50,
                ppid: Some(1),
                name: "chrome.exe".into(),
                exe: Some(r"C:\Program Files\Google\Chrome\chrome.exe".into()),
                command_line: None,
                window_title: Some("Gmail - Google Chrome".into()),
                cwd: None,
                create_time: Some(started),
            },
            ProcView {
                pid: 51,
                ppid: Some(50),
                name: "chrome.exe".into(),
                exe: Some(r"C:\Program Files\Google\Chrome\chrome.exe".into()),
                command_line: None,
                window_title: None,
                cwd: None,
                create_time: Some(started),
            },
        ]
    }

    fn add_more_chrome(mut procs: Vec<ProcView>) -> Vec<ProcView> {
        for i in 0..22 {
            procs.push(ProcView {
                pid: 1000 + i,
                ppid: Some(50),
                name: "chrome.exe".into(),
                exe: Some(r"C:\Program Files\Google\Chrome\chrome.exe".into()),
                command_line: None,
                window_title: None,
                cwd: None,
                create_time: None,
            });
        }
        procs
    }

    #[test]
    fn short_path_uses_home_tilde() {
        assert_eq!(
            short_path(r"C:\Users\a\src\myapp", Some(r"C:\Users\a")),
            r"~\src\myapp"
        );
        assert_eq!(short_path(r"C:\Users\a", Some(r"C:\Users\a")), "~");
        assert_eq!(
            short_path(r"c:\users\a\src\myapp\", Some(r"C:\Users\a")),
            r"~\src\myapp"
        );
    }

    #[test]
    fn format_started_includes_clock_and_relative() {
        let now = fixture_now();
        let s = format_started(Some(now - 12 * 60), now).unwrap();
        assert!(s.starts_with("started "), "{s}");
        assert!(s.contains("12 min ago"), "{s}");
    }

    #[test]
    fn command_tail_strips_exe() {
        let t = command_tail(Some(
            r#"C:\Program Files\nodejs\node.exe C:\Users\a\AppData\Roaming\npm\node_modules\@anthropic-ai\claude-code\cli.js"#,
        ))
        .unwrap();
        assert!(t.contains("claude-code\\cli.js"), "{t}");
        assert!(!t.starts_with(r"C:\Program Files\nodejs\node.exe"), "{t}");
    }

    #[test]
    fn groups_editors_first_and_dedupes_chrome() {
        let procs = add_more_chrome(vscode_tree());
        let model = build_picker(&procs, fixture_now(), Some(r"C:\Users\a"));
        assert_eq!(model.whole_pc.selection, WatchSelection::WholePc);
        let titles: Vec<&str> = model.agents_and_editors.iter().map(|r| r.title.as_str()).collect();
        assert!(
            titles.iter().any(|t| t.contains("VS Code window: myrepo")),
            "{titles:?}"
        );
        assert!(
            titles.iter().any(|t| t.contains("claude")),
            "{titles:?}"
        );
        let claude = model
            .agents_and_editors
            .iter()
            .find(|r| r.title.contains("claude"))
            .unwrap();
        assert_eq!(claude.depth, 1);
        assert!(claude.subtitle.contains("pid 220"), "{}", claude.subtitle);
        assert!(claude.subtitle.contains(r"~\src\myrepo"), "{}", claude.subtitle);
        assert!(claude.subtitle.contains("cli.js"), "{}", claude.subtitle);
        assert!(claude.summary.contains("claude"), "{}", claude.summary);

        let vs = model
            .agents_and_editors
            .iter()
            .find(|r| r.title.contains("VS Code"))
            .unwrap();
        assert!(vs.summary.contains("child"), "{}", vs.summary);
        assert!(vs.summary.contains("claude"), "{}", vs.summary);
        assert!(vs.subtitle.contains("12 min ago"), "{}", vs.subtitle);

        let others: Vec<&str> = model.other_apps.iter().map(|r| r.title.as_str()).collect();
        assert!(
            others.iter().any(|t| t.contains("chrome.exe (24)") || t.contains("chrome.exe (2")),
            "expected a grouped chrome row, got {others:?}"
        );
        // renderer Code.exe must not reappear as other
        assert!(
            !others.iter().any(|t| t.to_ascii_lowercase().contains("code.exe")),
            "{others:?}"
        );
    }

    #[test]
    fn filter_is_case_insensitive_over_name_pid_and_cmdline() {
        let model = build_picker(&vscode_tree(), fixture_now(), Some(r"C:\Users\a"));
        let by_pid = model.filter("220");
        assert!(
            by_pid.agents_and_editors.iter().any(|r| r.title.contains("claude")),
            "{:?}",
            by_pid.agents_and_editors
        );
        let by_cmd = model.filter("CLAUDE-CODE");
        assert_eq!(by_cmd.agents_and_editors.len(), 1);
        assert!(by_cmd.agents_and_editors[0].title.contains("claude"));
        let by_folder = model.filter("myrepo");
        assert!(by_folder.agents_and_editors.iter().any(|r| r.title.contains("VS Code")));
        // Whole PC stays present so the user can always pick it.
        assert_eq!(model.filter("zzz-no-match").whole_pc.selection, WatchSelection::WholePc);
        assert!(model.filter("zzz-no-match").agents_and_editors.is_empty());
    }

    #[test]
    fn chrome_group_attaches_to_all_pids() {
        let model = build_picker(&add_more_chrome(vscode_tree()), fixture_now(), None);
        let chrome = model
            .other_apps
            .iter()
            .find(|r| r.title.to_ascii_lowercase().contains("chrome"))
            .unwrap();
        match &chrome.selection {
            WatchSelection::Roots { pids, .. } => assert_eq!(pids.len(), 24),
            other => panic!("{other:?}"),
        }
        assert!(chrome.summary.contains("24"), "{}", chrome.summary);
        // Searching for a specific pid still finds the group (search_text includes pids).
        let filtered = model.filter("1010");
        assert!(
            filtered.other_apps.iter().any(|r| r.title.to_ascii_lowercase().contains("chrome")),
            "{:?}",
            filtered.other_apps
        );
    }

    #[test]
    fn ps_listing_distinguishes_rows() {
        let model = build_picker(&vscode_tree(), fixture_now(), Some(r"C:\Users\a"));
        let text = format_ps_rows(&model);
        assert!(text.contains("VS Code window: myrepo"), "{text}");
        assert!(text.contains("claude"), "{text}");
        assert!(text.contains("pid 220"), "{text}");
        assert!(text.contains(r"~\src\myrepo"), "{text}");
    }
}
