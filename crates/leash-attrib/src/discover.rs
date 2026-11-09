//! Find editors and coding agents in a process list.
//!
//! Matching is host-independent so it can be unit-tested with fixtures on Linux.
//! A query such as `vscode` must match `Code.exe`, and `claude` must match a
//! `node.exe` whose command line points at `@anthropic-ai\claude-code\cli.js`.

use crate::paths::{image_basename, names_equal};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// What kind of known app a process was recognised as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppKind {
    Editor,
    Agent,
}

/// A process as the matcher sees it. Built from a live snapshot or a fixture.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcView {
    pub pid: u32,
    pub ppid: Option<u32>,
    pub name: String,
    pub exe: Option<String>,
    pub command_line: Option<String>,
    pub window_title: Option<String>,
    pub cwd: Option<String>,
    pub create_time: Option<i64>,
}

impl ProcView {
    pub fn fixture(
        pid: u32,
        ppid: Option<u32>,
        name: &str,
        command_line: Option<&str>,
        window_title: Option<&str>,
    ) -> Self {
        let exe = Some(format!(r"C:\Program Files\{name}"));
        Self {
            pid,
            ppid,
            name: name.to_string(),
            exe,
            command_line: command_line.map(|s| s.to_string()),
            window_title: window_title.map(|s| s.to_string()),
            cwd: None,
            create_time: None,
        }
    }
}

/// Why a process matched a query or a known app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchReason {
    pub field: String,
    pub detail: String,
}

impl MatchReason {
    pub fn describe(&self) -> String {
        format!("{}: {}", self.field, self.detail)
    }
}

/// A recognised editor or agent, ready to show in `leash ps` or the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchCandidate {
    pub pid: u32,
    pub ppid: Option<u32>,
    pub name: String,
    pub exe: Option<String>,
    pub command_line: Option<String>,
    pub window_title: Option<String>,
    pub kind: AppKind,
    pub app_id: String,
    pub reason: String,
    pub parent_name: Option<String>,
    /// e.g. `Code.exe > cmd.exe > node.exe (claude)`
    pub lineage: String,
    /// 0 for a listed root (editor root or standalone agent), 1+ when nested.
    pub depth: usize,
    pub is_attach_root: bool,
}

impl WatchCandidate {
    pub fn list_label(&self) -> String {
        let indent = if self.depth > 0 { "  " } else { "" };
        format!(
            "{indent}{}  ({})  {}",
            self.name, self.pid, self.app_id
        )
    }
}

/// Roots to attach to for a user query, plus a human summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachResolution {
    pub query: String,
    pub app_id: Option<String>,
    pub kind: Option<AppKind>,
    pub roots: Vec<WatchCandidate>,
    /// Every process in any root's descendant tree (including the roots).
    pub tree_pids: Vec<u32>,
}

/// Known editors and agents, with the aliases a user is likely to type.
#[derive(Clone, Copy)]
struct KnownApp {
    id: &'static str,
    kind: AppKind,
    aliases: &'static [&'static str],
    /// Image stem after stripping `.exe`, compared exactly (case-insensitive).
    image_stems: &'static [&'static str],
    /// Distinctive command-line fragments (backslashes; matched case-insensitively).
    cmdline_needles: &'static [&'static str],
    title_needles: &'static [&'static str],
}

const APPS: &[KnownApp] = &[
    // Agents first so a node.exe running Claude is not also labelled as an editor.
    KnownApp {
        id: "claude",
        kind: AppKind::Agent,
        aliases: &["claude", "claude code", "claudecode"],
        image_stems: &["claude"],
        cmdline_needles: &[
            r"@anthropic-ai\claude",
            r"\claude-code\",
            r"claude-code\cli.js",
            r"\claude.cmd",
            r"\claude.ps1",
        ],
        title_needles: &["claude code"],
    },
    KnownApp {
        id: "codex",
        kind: AppKind::Agent,
        aliases: &["codex", "openai codex"],
        image_stems: &["codex"],
        cmdline_needles: &[
            r"@openai\codex",
            r"\codex.cmd",
            r"\codex.ps1",
            r"openai-codex",
            r"\codex-cli\",
        ],
        title_needles: &["codex"],
    },
    KnownApp {
        id: "gemini",
        kind: AppKind::Agent,
        aliases: &["gemini", "gemini cli", "gemini-cli"],
        image_stems: &["gemini"],
        cmdline_needles: &[
            r"@google\gemini-cli",
            r"\gemini-cli\",
            r"\gemini.cmd",
            r"\gemini.ps1",
        ],
        title_needles: &["gemini"],
    },
    KnownApp {
        id: "aider",
        kind: AppKind::Agent,
        aliases: &["aider"],
        image_stems: &["aider"],
        cmdline_needles: &[r"\aider.cmd", r"\aider.ps1", r"\aider.exe", r" -m aider", r"\scripts\aider"],
        title_needles: &["aider"],
    },
    KnownApp {
        id: "vscode",
        kind: AppKind::Editor,
        aliases: &[
            "vscode",
            "vs code",
            "visual studio code",
            "code",
            "code - insiders",
            "code insiders",
            "insiders",
        ],
        image_stems: &["code", "code - insiders"],
        cmdline_needles: &[r"\code.exe", r"\code - insiders.exe"],
        title_needles: &["visual studio code", "visual studio code - insiders"],
    },
    KnownApp {
        id: "cursor",
        kind: AppKind::Editor,
        aliases: &["cursor"],
        image_stems: &["cursor"],
        cmdline_needles: &[r"\cursor.exe"],
        title_needles: &["cursor"],
    },
    KnownApp {
        id: "windsurf",
        kind: AppKind::Editor,
        aliases: &["windsurf"],
        image_stems: &["windsurf"],
        cmdline_needles: &[r"\windsurf.exe"],
        title_needles: &["windsurf"],
    },
];

fn canon_query(q: &str) -> String {
    let mut s = q.trim().to_ascii_lowercase();
    if let Some(stripped) = s.strip_suffix(".exe") {
        s = stripped.trim().to_string();
    }
    s
}

fn image_stem(name: &str) -> String {
    let mut n = image_basename(name);
    if let Some(stripped) = n
        .to_ascii_lowercase()
        .strip_suffix(".exe")
        .map(|s| s.to_string())
    {
        n = stripped;
    } else {
        n = n.to_ascii_lowercase();
        if let Some(stripped) = n.strip_suffix(".exe") {
            n = stripped.to_string();
        }
    }
    n
}

fn norm_hay(s: &str) -> String {
    s.to_ascii_lowercase().replace('/', r"\")
}

fn image_stem_of(p: &ProcView) -> String {
    if let Some(exe) = &p.exe {
        let stem = image_stem(exe);
        if !stem.is_empty() {
            return stem;
        }
    }
    image_stem(&p.name)
}
fn app_for_query(query: &str) -> Option<&'static KnownApp> {
    let q = canon_query(query);
    if q.is_empty() {
        return None;
    }
    APPS.iter().find(|app| {
        app.id == q
            || app.aliases.iter().any(|a| *a == q)
            || app.image_stems.iter().any(|s| *s == q)
    })
}

fn field_matches_app(p: &ProcView, app: &KnownApp) -> Option<MatchReason> {
    let stem = image_stem_of(p);
    if app.image_stems.iter().any(|s| *s == stem) {
        return Some(MatchReason {
            field: "image".into(),
            detail: format!("{} is alias {}", p.name, app.id),
        });
    }
    if let Some(cmd) = &p.command_line {
        let hay = norm_hay(cmd);
        if let Some(n) = app
            .cmdline_needles
            .iter()
            .find(|n| hay.contains(&n.to_ascii_lowercase()))
        {
            return Some(MatchReason {
                field: "command line".into(),
                detail: format!("contains `{n}`"),
            });
        }
    }
    if let Some(title) = &p.window_title {
        let hay = title.to_ascii_lowercase();
        if let Some(n) = app
            .title_needles
            .iter()
            .find(|n| hay.contains(*n))
        {
            return Some(MatchReason {
                field: "window title".into(),
                detail: format!("contains `{n}`"),
            });
        }
    }
    None
}

/// Classify a process as a known editor or agent, if it looks like one.
pub fn classify(p: &ProcView) -> Option<(&'static str, AppKind, MatchReason)> {
    for app in APPS {
        if let Some(reason) = field_matches_app(p, app) {
            return Some((app.id, app.kind, reason));
        }
    }
    None
}

/// Whether `p` matches a user query (`vscode`, `claude`, a pid-less name, …).
pub fn matches_query(p: &ProcView, query: &str) -> Option<MatchReason> {
    let q = canon_query(query);
    if q.is_empty() {
        return None;
    }
    if let Some(app) = app_for_query(&q) {
        return field_matches_app(p, app);
    }
    // Generic: image stem, exe basename, window title or command line.
    if image_stem_of(p) == q || names_equal(&p.name, &q) {
        return Some(MatchReason {
            field: "image".into(),
            detail: p.name.clone(),
        });
    }
    if let Some(exe) = &p.exe {
        if names_equal(exe, &q) || image_stem(exe) == q {
            return Some(MatchReason {
                field: "image".into(),
                detail: exe.clone(),
            });
        }
        let hay = norm_hay(exe);
        if hay.contains(&q) {
            return Some(MatchReason {
                field: "exe path".into(),
                detail: exe.clone(),
            });
        }
    }
    if let Some(title) = &p.window_title {
        if title.to_ascii_lowercase().contains(&q) {
            return Some(MatchReason {
                field: "window title".into(),
                detail: title.clone(),
            });
        }
    }
    if let Some(cmd) = &p.command_line {
        if norm_hay(cmd).contains(&q) {
            return Some(MatchReason {
                field: "command line".into(),
                detail: truncate(cmd, 180),
            });
        }
    }
    None
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

fn by_pid(procs: &[ProcView]) -> HashMap<u32, &ProcView> {
    procs.iter().map(|p| (p.pid, p)).collect()
}

fn children_map(procs: &[ProcView]) -> HashMap<u32, Vec<u32>> {
    let mut m: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in procs {
        if let Some(pp) = p.ppid {
            m.entry(pp).or_default().push(p.pid);
        }
    }
    m
}

/// Number of processes in the descendant tree of `root`, including `root`.
pub fn count_tree(root: u32, procs: &[ProcView]) -> usize {
    let kids = children_map(procs);
    let mut seen = HashSet::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some(cs) = kids.get(&pid) {
            stack.extend(cs);
        }
    }
    seen.len()
}

fn descendants(root: u32, procs: &[ProcView]) -> Vec<u32> {
    let kids = children_map(procs);
    let mut seen = Vec::new();
    let mut stack = vec![root];
    let mut used = HashSet::new();
    while let Some(pid) = stack.pop() {
        if !used.insert(pid) {
            continue;
        }
        seen.push(pid);
        if let Some(cs) = kids.get(&pid) {
            stack.extend(cs);
        }
    }
    seen
}fn parent_name_of(p: &ProcView, index: &HashMap<u32, &ProcView>) -> Option<String> {
    p.ppid.and_then(|id| index.get(&id).map(|x| x.name.clone()))
}

fn lineage_of(p: &ProcView, index: &HashMap<u32, &ProcView>) -> String {
    let mut names = Vec::new();
    let mut cur = Some(p);
    let mut guard = HashSet::new();
    while let Some(node) = cur {
        if !guard.insert(node.pid) {
            break;
        }
        let label = match classify(node) {
            Some((id, _, _)) if !names_equal(&node.name, id) => {
                format!("{} ({id})", node.name)
            }
            _ => node.name.clone(),
        };
        names.push(label);
        cur = node.ppid.and_then(|id| index.get(&id).copied());
        if names.len() > 8 {
            break;
        }
    }
    names.reverse();
    names.join(" > ")
}

fn is_same_app_child(
    p: &ProcView,
    app_id: &str,
    classified: &HashMap<u32, (&'static str, AppKind)>,
) -> bool {
    p.ppid.is_some_and(|ppid| {
        classified
            .get(&ppid)
            .is_some_and(|(id, _)| *id == app_id)
    })
}

/// Editors and agents, grouped so a VS Code tree shows Claude running inside it.
pub fn discover_candidates(procs: &[ProcView]) -> Vec<WatchCandidate> {
    let index = by_pid(procs);
    let mut classified: HashMap<u32, (&'static str, AppKind, MatchReason)> = HashMap::new();
    for p in procs {
        if let Some((id, kind, reason)) = classify(p) {
            classified.insert(p.pid, (id, kind, reason));
        }
    }
    let kind_index: HashMap<u32, (&'static str, AppKind)> = classified
        .iter()
        .map(|(k, (id, kind, _))| (*k, (*id, *kind)))
        .collect();

    let mut editor_roots: Vec<&ProcView> = procs
        .iter()
        .filter(|p| {
            classified
                .get(&p.pid)
                .is_some_and(|(id, kind, _)| {
                    *kind == AppKind::Editor && !is_same_app_child(p, id, &kind_index)
                })
        })
        .collect();
    editor_roots.sort_by_key(|p| (p.name.to_ascii_lowercase(), p.pid));

    let mut used_agents: HashSet<u32> = HashSet::new();
    let mut out = Vec::new();

    for root in editor_roots {
        let (id, kind, reason) = classified.get(&root.pid).unwrap();
        out.push(to_candidate(
            root, id, *kind, reason, &index, 0, true,
        ));
        let mut agents_in_tree: Vec<&ProcView> = descendants(root.pid, procs)
            .into_iter()
            .filter(|pid| *pid != root.pid)
            .filter_map(|pid| {
                let p = index.get(&pid)?;
                let (aid, akind, _) = classified.get(&pid)?;
                (*akind == AppKind::Agent).then_some((*p, *aid))
            })
            .map(|(p, _)| p)
            .collect();
        agents_in_tree.sort_by_key(|p| p.pid);
        for agent in agents_in_tree {
            let (id, kind, reason) = classified.get(&agent.pid).unwrap();
            used_agents.insert(agent.pid);
            out.push(to_candidate(
                agent, id, *kind, reason, &index, 1, true,
            ));
        }
    }

    let mut standalone: Vec<&ProcView> = procs
        .iter()
        .filter(|p| {
            classified
                .get(&p.pid)
                .is_some_and(|(_, kind, _)| *kind == AppKind::Agent)
                && !used_agents.contains(&p.pid)
        })
        .collect();
    standalone.sort_by_key(|p| (p.name.to_ascii_lowercase(), p.pid));
    for p in standalone {
        let (id, kind, reason) = classified.get(&p.pid).unwrap();
        out.push(to_candidate(p, id, *kind, reason, &index, 0, true));
    }
    out
}

fn to_candidate(
    p: &ProcView,
    app_id: &str,
    kind: AppKind,
    reason: &MatchReason,
    index: &HashMap<u32, &ProcView>,
    depth: usize,
    is_attach_root: bool,
) -> WatchCandidate {
    WatchCandidate {
        pid: p.pid,
        ppid: p.ppid,
        name: p.name.clone(),
        exe: p.exe.clone(),
        command_line: p.command_line.clone(),
        window_title: p.window_title.clone(),
        kind,
        app_id: app_id.to_string(),
        reason: reason.describe(),
        parent_name: parent_name_of(p, index),
        lineage: lineage_of(p, index),
        depth,
        is_attach_root,
    }
}

/// Resolve `--attach <query>` against a process list.
///
/// Editors attach at the top-level editor process so terminals and agents
/// inside them are included. Agents attach at the matching process only.
pub fn resolve_attach(procs: &[ProcView], query: &str) -> AttachResolution {
    let q = query.trim();
    let app = app_for_query(q);
    let matches: Vec<(&ProcView, MatchReason)> = procs
        .iter()
        .filter_map(|p| matches_query(p, q).map(|r| (p, r)))
        .collect();

    let kind = app.map(|a| a.kind).or_else(|| {
        matches
            .first()
            .and_then(|(p, _)| classify(p).map(|(_, k, _)| k))
    });
    let app_id = app.map(|a| a.id.to_string()).or_else(|| {
        matches
            .first()
            .and_then(|(p, _)| classify(p).map(|(id, _, _)| id.to_string()))
    });

    let index = by_pid(procs);
    let match_ids: HashSet<u32> = matches.iter().map(|(p, _)| p.pid).collect();

    let roots: Vec<WatchCandidate> = if kind == Some(AppKind::Editor) {
        matches
            .iter()
            .filter(|(p, _)| {
                p.ppid
                    .and_then(|ppid| index.get(&ppid))
                    .is_none_or(|parent| !match_ids.contains(&parent.pid))
            })
            .map(|(p, reason)| {
                let id = app_id.as_deref().unwrap_or("editor");
                to_candidate(p, id, AppKind::Editor, reason, &index, 0, true)
            })
            .collect()
    } else {
        matches
            .iter()
            .map(|(p, reason)| {
                let (id, k) = classify(p)
                    .map(|(id, k, _)| (id.to_string(), k))
                    .unwrap_or_else(|| {
                        (
                            app_id.clone().unwrap_or_else(|| canon_query(q)),
                            AppKind::Agent,
                        )
                    });
                to_candidate(p, &id, k, reason, &index, 0, true)
            })
            .collect()
    };

    let mut tree = Vec::new();
    let mut seen = HashSet::new();
    for r in &roots {
        for pid in descendants(r.pid, procs) {
            if seen.insert(pid) {
                tree.push(pid);
            }
        }
    }

    AttachResolution {
        query: q.to_string(),
        app_id,
        kind,
        roots,
        tree_pids: tree,
    }
}

impl AttachResolution {
    pub fn format_summary(&self, procs: &[ProcView]) -> String {
        if self.roots.is_empty() {
            return format!("no running process matched `{}`", self.query);
        }
        let app = self.app_id.as_deref().unwrap_or(self.query.as_str());
        let mut lines = Vec::new();
        lines.push(format!(
            "matched {app} at {} root process(es); watching {} process(es) in the tree:",
            self.roots.len(),
            self.tree_pids.len()
        ));
        for r in &self.roots {
            let n = count_tree(r.pid, procs);
            lines.push(format!(
                "  {} pid {} — {n} process(es) — {}",
                r.name, r.pid, r.reason
            ));
        }
        if self.kind == Some(AppKind::Editor) {
            lines.push(
                "attaching to the editor includes terminals, extension hosts and agents started inside them."
                    .into(),
            );
        }
        lines.join("\n")
    }

    pub fn no_match_message(&self, procs: &[ProcView]) -> String {
        let mut s = format!(
            "no running process matched `{}`.\n\nCandidates (`leash ps`):\n",
            self.query
        );
        let listed = format_ps(&discover_candidates(procs));
        if listed.trim().is_empty() {
            s.push_str("  (none — no known editors or agents were visible)\n");
        } else {
            s.push_str(&listed);
        }
        s.push_str(
            "\nIf the app is running but missing here, try `leash ps` from an elevated window\n\
             (command lines can be hidden otherwise), or `leash start --pid <pid>`.",
        );
        s
    }
}

/// Text listing for `leash ps` / `leash list`.
pub fn format_ps(candidates: &[WatchCandidate]) -> String {
    if candidates.is_empty() {
        return String::new();
    }
    let mut lines = Vec::new();
    for c in candidates {
        let indent = if c.depth > 0 { "  " } else { "" };
        let kind = match c.kind {
            AppKind::Editor => "editor",
            AppKind::Agent => "agent ",
        };
        lines.push(format!(
            "{indent}{:<6} {:<22} {kind}  {:<8}  {}",
            c.pid,
            truncate(&c.name, 22),
            c.app_id,
            c.reason
        ));
        let mut extras = Vec::new();
        if let Some(parent) = &c.parent_name {
            extras.push(format!(
                "parent {} {}",
                c.ppid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                parent
            ));
        }
        if !c.lineage.is_empty() && c.lineage != c.name {
            extras.push(format!("tree {}", c.lineage));
        }
        if let Some(title) = &c.window_title {
            extras.push(format!("title {}", truncate(title, 80)));
        }
        if !extras.is_empty() {
            lines.push(format!("{indent}       {}", extras.join("  ·  ")));
        }
    }
    lines.join("\n")
}

/// Build views from snapshot entries (no window titles).
pub fn views_from_entries(entries: &[leash_snapshot::ProcessEntry]) -> Vec<ProcView> {
    entries
        .iter()
        .map(|p| ProcView {
            pid: p.pid,
            ppid: p.ppid,
            name: p.name.clone(),
            exe: p.exe.clone(),
            command_line: p.command_line.clone(),
            window_title: None,
            cwd: p.cwd.clone(),
            create_time: p.create_time,
        })
        .collect()
}

/// Merge window titles into views.
pub fn apply_window_titles(views: &mut [ProcView], titles: &HashMap<u32, String>) {
    for v in views {
        if let Some(t) = titles.get(&v.pid) {
            if !t.is_empty() {
                v.window_title = Some(t.clone());
            }
        }
    }
}

/// Live snapshot used by the CLI and the window. Empty on non-Windows hosts.
pub fn live_views() -> Vec<ProcView> {
    let entries = crate::list_processes();
    log_process_info_failures();
    let mut views = views_from_entries(&entries);
    let titles = leash_persist::list_window_titles();
    apply_window_titles(&mut views, &titles);
    views
}

fn log_process_info_failures() {
    let failures = leash_persist::take_process_info_failures();
    if failures.is_empty() {
        return;
    }
    crate::log(format!(
        "{} process info failure(s) (command line or image often need elevation; not silently dropped):",
        failures.len()
    ));
    for f in failures.iter().take(40) {
        crate::log(f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vscode_tree() -> Vec<ProcView> {
        // explorer → Code.exe (main) → Code.exe (renderer)
        //                             → cmd.exe → node.exe (claude-code cli.js)
        //                             → node.exe (extension host, not an agent)
        vec![
            ProcView::fixture(100, Some(1), "explorer.exe", None, None),
            ProcView {
                pid: 200,
                ppid: Some(100),
                name: "Code.exe".into(),
                exe: Some(r"C:\Users\a\AppData\Local\Programs\Microsoft VS Code\Code.exe".into()),
                command_line: Some(r#""C:\Users\a\AppData\Local\Programs\Microsoft VS Code\Code.exe""#.into()),
                window_title: Some("leash - Visual Studio Code".into()),
                cwd: None,
                create_time: None,
            },
            ProcView {
                pid: 201,
                ppid: Some(200),
                name: "Code.exe".into(),
                exe: Some(r"C:\Users\a\AppData\Local\Programs\Microsoft VS Code\Code.exe".into()),
                command_line: Some(r#""C:\Users\a\AppData\Local\Programs\Microsoft VS Code\Code.exe" --type=renderer"#.into()),
                window_title: None,
                cwd: None,
                create_time: None,
            },
            ProcView::fixture(
                210,
                Some(200),
                "cmd.exe",
                Some(r"C:\Windows\System32\cmd.exe /d /c claude"),
                None,
            ),
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
                cwd: None,
                create_time: None,
            },
            ProcView {
                pid: 230,
                ppid: Some(200),
                name: "node.exe".into(),
                exe: Some(r"C:\Program Files\nodejs\node.exe".into()),
                command_line: Some(
                    r#""C:\Program Files\nodejs\node.exe" "c:\vscode\resources\app\out\bootstrap-fork" --type=extensionHost"#
                        .into(),
                ),
                window_title: None,
                cwd: None,
                create_time: None,
            },
            // Noise that must not appear as a watch candidate.
            ProcView::fixture(50, Some(1), "chrome.exe", None, Some("Gmail - Google Chrome")),
        ]
    }

    fn cursor_and_codex() -> Vec<ProcView> {
        let mut v = vscode_tree();
        v.push(ProcView {
            pid: 300,
            ppid: Some(100),
            name: "Cursor.exe".into(),
            exe: Some(r"C:\Users\a\AppData\Local\Programs\cursor\Cursor.exe".into()),
            command_line: Some(r#"C:\Users\a\AppData\Local\Programs\cursor\Cursor.exe"#.into()),
            window_title: Some("leash - Cursor".into()),
            cwd: None,
            create_time: None,
        });
        v.push(ProcView {
            pid: 400,
            ppid: Some(100),
            name: "node.exe".into(),
            exe: Some(r"C:\Program Files\nodejs\node.exe".into()),
            command_line: Some(
                r#"C:\Program Files\nodejs\node.exe C:\Users\a\AppData\Roaming\npm\node_modules\@openai\codex\bin.js"#
                    .into(),
            ),
            window_title: None,
            cwd: None,
            create_time: None,
        });
        v.push(ProcView::fixture(
            500,
            Some(100),
            "codex.exe",
            Some(r"C:\Users\a\AppData\Local\Programs\codex\codex.exe"),
            None,
        ));
        v.push(ProcView {
            pid: 600,
            ppid: Some(100),
            name: "node.exe".into(),
            exe: Some(r"C:\Program Files\nodejs\node.exe".into()),
            command_line: Some(
                r#"node C:\Users\a\AppData\Roaming\npm\node_modules\@google\gemini-cli\dist\index.js"#
                    .into(),
            ),
            window_title: None,
            cwd: None,
            create_time: None,
        });
        v.push(ProcView::fixture(
            700,
            Some(100),
            "python.exe",
            Some(r"C:\Python\python.exe -m aider --yes-always"),
            None,
        ));
        v.push(ProcView {
            pid: 800,
            ppid: Some(100),
            name: "Windsurf.exe".into(),
            exe: Some(r"C:\Users\a\AppData\Local\Programs\Windsurf\Windsurf.exe".into()),
            command_line: Some(r"C:\Users\a\AppData\Local\Programs\Windsurf\Windsurf.exe".into()),
            window_title: Some("Windsurf".into()),
            cwd: None,
            create_time: None,
        });
        v
    }

    #[test]
    fn vscode_aliases_match_code_exe() {
        let p = &vscode_tree()[1];
        assert!(names_equal("Code.exe", "vscode") == false, "the old matcher cannot see vscode");
        for q in ["vscode", "VS Code", "code", "Code.exe", "visual studio code"] {
            let m = matches_query(p, q).unwrap_or_else(|| panic!("expected match for {q}"));
            assert_eq!(m.field, "image", "{q}");
        }
    }

    #[test]
    fn insiders_matches() {
        let p = ProcView {
            pid: 1,
            ppid: None,
            name: "Code - Insiders.exe".into(),
            exe: Some(r"C:\Users\a\AppData\Local\Programs\Microsoft VS Code Insiders\Code - Insiders.exe".into()),
            command_line: None,
            window_title: Some("Visual Studio Code - Insiders".into()),
            cwd: None,
            create_time: None,
        };
        assert!(matches_query(&p, "vscode").is_some());
        assert!(matches_query(&p, "code - insiders").is_some());
        assert!(matches_query(&p, "insiders").is_some());
        let c = classify(&p).unwrap();
        assert_eq!(c.0, "vscode");
    }

    #[test]
    fn claude_via_node_command_line() {
        let node = vscode_tree()
            .into_iter()
            .find(|p| p.pid == 220)
            .unwrap();
        assert!(names_equal("node.exe", "claude") == false);
        let m = matches_query(&node, "claude").expect("claude in command line");
        assert_eq!(m.field, "command line");
        let c = classify(&node).unwrap();
        assert_eq!(c.0, "claude");
        assert_eq!(c.1, AppKind::Agent);
    }

    #[test]
    fn native_claude_exe() {
        let p = ProcView::fixture(9, None, "claude.exe", Some(r"C:\Program Files\Claude\claude.exe"), None);
        assert!(matches_query(&p, "claude").is_some());
        assert_eq!(classify(&p).unwrap().0, "claude");
    }

    #[test]
    fn extension_host_is_not_claude() {
        let p = vscode_tree()
            .into_iter()
            .find(|p| p.pid == 230)
            .unwrap();
        assert!(matches_query(&p, "claude").is_none());
        assert!(classify(&p).is_none());
    }

    #[test]
    fn attach_vscode_uses_editor_root_and_includes_claude() {
        let procs = vscode_tree();
        let res = resolve_attach(&procs, "vscode");
        let roots: Vec<u32> = res.roots.iter().map(|r| r.pid).collect();
        assert_eq!(roots, vec![200], "only the top-level Code.exe, not the renderer");
        assert!(res.tree_pids.contains(&220), "claude node is in the tree");
        assert!(res.tree_pids.contains(&201));
        assert!(res.tree_pids.contains(&210));
        assert_eq!(res.tree_pids.len(), count_tree(200, &procs));
        assert_eq!(count_tree(200, &procs), 5); // 200,201,210,220,230
        let text = res.format_summary(&procs);
        assert!(text.contains("Code.exe pid 200"), "{text}");
        assert!(text.contains("5 process"), "{text}");
        assert!(text.contains("terminals"), "{text}");
    }

    #[test]
    fn attach_claude_is_just_the_node_subtree() {
        let procs = vscode_tree();
        let res = resolve_attach(&procs, "claude");
        assert_eq!(res.roots.iter().map(|r| r.pid).collect::<Vec<_>>(), vec![220]);
        assert_eq!(res.tree_pids, vec![220]);
        assert!(!res.tree_pids.contains(&200));
    }

    #[test]
    fn candidates_group_claude_under_code() {
        let list = discover_candidates(&vscode_tree());
        let labels: Vec<String> = list
            .iter()
            .map(|c| format!("{}:{}:{}:{}", c.depth, c.pid, c.name, c.app_id))
            .collect();
        assert!(
            labels.iter().any(|s| s == "0:200:Code.exe:vscode"),
            "{labels:?}"
        );
        assert!(
            labels.iter().any(|s| s == "1:220:node.exe:claude"),
            "{labels:?}"
        );
        assert!(
            !labels.iter().any(|s| s.contains("chrome")),
            "chrome must not be a watch candidate: {labels:?}"
        );
        assert!(
            !labels.iter().any(|s| s.contains(":201:")),
            "renderer Code.exe should be folded under the root: {labels:?}"
        );
        let claude = list.iter().find(|c| c.pid == 220).unwrap();
        assert!(
            claude.lineage.contains("Code.exe") && claude.lineage.contains("node.exe (claude)"),
            "lineage was {}",
            claude.lineage
        );
        assert_eq!(claude.parent_name.as_deref(), Some("cmd.exe"));
    }

    #[test]
    fn cursor_codex_gemini_aider_windsurf() {
        let procs = cursor_and_codex();
        assert!(matches_query(procs.iter().find(|p| p.pid == 300).unwrap(), "cursor").is_some());
        assert!(matches_query(procs.iter().find(|p| p.pid == 400).unwrap(), "codex").is_some());
        assert!(matches_query(procs.iter().find(|p| p.pid == 500).unwrap(), "codex").is_some());
        assert!(matches_query(procs.iter().find(|p| p.pid == 600).unwrap(), "gemini").is_some());
        assert!(matches_query(procs.iter().find(|p| p.pid == 700).unwrap(), "aider").is_some());
        assert!(matches_query(procs.iter().find(|p| p.pid == 800).unwrap(), "windsurf").is_some());

        let list = discover_candidates(&procs);
        let ids: Vec<(&str, u32)> = list.iter().map(|c| (c.app_id.as_str(), c.pid)).collect();
        assert!(ids.contains(&("cursor", 300)), "{ids:?}");
        assert!(ids.contains(&("codex", 400)), "{ids:?}");
        assert!(ids.contains(&("codex", 500)), "{ids:?}");
        assert!(ids.contains(&("gemini", 600)), "{ids:?}");
        assert!(ids.contains(&("aider", 700)), "{ids:?}");
        assert!(ids.contains(&("windsurf", 800)), "{ids:?}");

        let ps = format_ps(&list);
        assert!(ps.contains("220"), "{ps}");
        assert!(ps.contains("claude"), "{ps}");
        assert!(ps.contains("Cursor.exe"), "{ps}");
    }

    #[test]
    fn no_match_lists_candidates() {
        let procs = vscode_tree();
        let res = resolve_attach(&procs, "not-an-app");
        assert!(res.roots.is_empty());
        let msg = res.no_match_message(&procs);
        assert!(msg.contains("no running process matched `not-an-app`"), "{msg}");
        assert!(msg.contains("Code.exe"), "{msg}");
        assert!(msg.contains("claude"), "{msg}");
    }

    #[test]
    fn case_insensitive_command_line() {
        let p = ProcView {
            pid: 1,
            ppid: None,
            name: "NODE.EXE".into(),
            exe: Some(r"C:\Program Files\nodejs\NODE.EXE".into()),
            command_line: Some(
                r"C:\Program Files\nodejs\NODE.EXE C:\Users\a\node_modules\@Anthropic-AI\Claude-Code\CLI.js"
                    .into(),
            ),
            window_title: None,
            cwd: None,
            create_time: None,
        };
        assert!(matches_query(&p, "CLAUDE").is_some());
    }
}
