//! Resolve a command the way Windows does, including PATHEXT and `.cmd` shims.
//!
//! `leash run -- claude` must find `claude.cmd` on PATH and launch it through
//! `cmd.exe /c`, because CreateProcess does not honour PATHEXT on its own.

use std::path::{Path, PathBuf};

/// How the resolved program should be launched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchKind {
    Exe,
    Batch,
    PowerShell,
}

/// A concrete process image and argument vector for `Command::new`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    pub program: String,
    pub args: Vec<String>,
    pub kind: LaunchKind,
    /// The file that was found on PATH (script or exe), if any.
    pub resolved: Option<String>,
}

impl LaunchPlan {
    fn as_is(argv: &[String]) -> Self {
        Self {
            program: argv[0].clone(),
            args: argv[1..].to_vec(),
            kind: kind_from_path(&argv[0]),
            resolved: None,
        }
    }
}

fn kind_from_path(path: &str) -> LaunchKind {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".cmd") || lower.ends_with(".bat") {
        LaunchKind::Batch
    } else if lower.ends_with(".ps1") {
        LaunchKind::PowerShell
    } else {
        LaunchKind::Exe
    }
}

fn split_path(path: &str) -> Vec<String> {
    path.split(';')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn split_pathext(pathext: &str) -> Vec<String> {
    let exts: Vec<String> = pathext
        .split(';')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.starts_with('.') {
                s
            } else {
                format!(".{s}")
            }
        })
        .collect();
    if exts.is_empty() {
        vec![
            ".com".into(),
            ".exe".into(),
            ".bat".into(),
            ".cmd".into(),
        ]
    } else {
        exts
    }
}

fn has_known_extension(name: &str, exts: &[String]) -> bool {
    let lower = name.to_ascii_lowercase();
    exts.iter().any(|e| lower.ends_with(e.as_str()))
}

fn looks_like_path(name: &str) -> bool {
    name.contains('\\')
        || name.contains('/')
        || Path::new(name).components().count() > 1
        || (name.len() >= 2 && name.as_bytes()[1] == b':')
}

/// Search PATH + PATHEXT the way cmd.exe does, using `exists` so tests can
/// supply a fixture file tree.
pub fn search_on_path(
    name: &str,
    path: &str,
    pathext: &str,
    cwd: &str,
    exists: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let name = name.trim().trim_matches('"');
    if name.is_empty() {
        return None;
    }
    let exts = split_pathext(pathext);
    let mut dirs = Vec::new();
    if !cwd.is_empty() {
        dirs.push(cwd.to_string());
    }
    dirs.extend(split_path(path));

    let candidates_in = |dir: &str| -> Vec<String> {
        let mut out = Vec::new();
        if has_known_extension(name, &exts) || looks_like_path(name) {
            out.push(join_dir(dir, name));
        } else {
            for ext in &exts {
                out.push(join_dir(dir, &format!("{name}{ext}")));
            }
            // Bare name last, in case it is already an extensionless file.
            out.push(join_dir(dir, name));
        }
        out
    };

    if looks_like_path(name) {
        if exists(name) {
            return Some(name.to_string());
        }
        // Relative path: also try under cwd, but do not walk PATH.
        if !cwd.is_empty() {
            let joined = join_dir(cwd, name);
            if exists(&joined) {
                return Some(joined);
            }
        }
        return None;
    }

    for dir in &dirs {
        for cand in candidates_in(dir) {
            if exists(&cand) {
                return Some(cand);
            }
        }
    }
    None
}

fn join_dir(dir: &str, name: &str) -> String {
    let p = Path::new(dir).join(name);
    p.to_string_lossy().replace('/', r"\")
}

fn wrap_plan(
    resolved: &str,
    rest: &[String],
    comspec: &str,
    powershell: &str,
) -> LaunchPlan {
    match kind_from_path(resolved) {
        LaunchKind::Batch => {
            let mut args = vec!["/c".into(), resolved.to_string()];
            args.extend(rest.iter().cloned());
            LaunchPlan {
                program: comspec.to_string(),
                args,
                kind: LaunchKind::Batch,
                resolved: Some(resolved.to_string()),
            }
        }
        LaunchKind::PowerShell => {
            let mut args = vec![
                "-NoProfile".into(),
                "-File".into(),
                resolved.to_string(),
            ];
            args.extend(rest.iter().cloned());
            LaunchPlan {
                program: powershell.to_string(),
                args,
                kind: LaunchKind::PowerShell,
                resolved: Some(resolved.to_string()),
            }
        }
        LaunchKind::Exe => LaunchPlan {
            program: resolved.to_string(),
            args: rest.to_vec(),
            kind: LaunchKind::Exe,
            resolved: Some(resolved.to_string()),
        },
    }
}

/// Plan a launch given environment-like inputs. Never fails: if nothing is
/// found, the original argv is used as-is so the OS error is unchanged.
pub fn plan_launch(
    argv: &[String],
    path: &str,
    pathext: &str,
    cwd: &str,
    comspec: &str,
    powershell: &str,
    exists: &dyn Fn(&str) -> bool,
) -> LaunchPlan {
    if argv.is_empty() {
        return LaunchPlan {
            program: String::new(),
            args: Vec::new(),
            kind: LaunchKind::Exe,
            resolved: None,
        };
    }
    let name = &argv[0];
    if let Some(found) = search_on_path(name, path, pathext, cwd, exists) {
        wrap_plan(&found, &argv[1..], comspec, powershell)
    } else if looks_like_path(name) && exists(name) {
        wrap_plan(name, &argv[1..], comspec, powershell)
    } else {
        // If the user already typed a .cmd path that exists, wrap it.
        if exists(name) {
            wrap_plan(name, &argv[1..], comspec, powershell)
        } else {
            LaunchPlan::as_is(argv)
        }
    }
}

pub fn default_pathext() -> &'static str {
    ".COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC;.CPL"
}

/// Plan a launch using the current process environment (Windows).
pub fn plan_launch_from_env(argv: &[String], cwd: &Path) -> LaunchPlan {
    let path = std::env::var("PATH").unwrap_or_default();
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| default_pathext().into());
    let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into());
    let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let powershell = PathBuf::from(&system_root)
        .join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
        .to_string_lossy()
        .into_owned();
    let cwd_s = cwd.to_string_lossy().into_owned();
    plan_launch(
        argv,
        &path,
        &pathext,
        &cwd_s,
        &comspec,
        &powershell,
        &|p| Path::new(p).is_file(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn exists_set<'a>(files: &'a [&'a str]) -> impl Fn(&str) -> bool + 'a {
        let set: HashSet<String> = files
            .iter()
            .map(|s| s.replace('/', r"\").to_ascii_lowercase())
            .collect();
        move |p: &str| set.contains(&p.replace('/', r"\").to_ascii_lowercase())
    }

    const PATH: &str = r"C:\Windows\System32;C:\Users\a\AppData\Roaming\npm;C:\tools";
    const EXT: &str = ".COM;.EXE;.BAT;.CMD;.PS1";
    const CMD: &str = r"C:\Windows\System32\cmd.exe";
    const PS: &str = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
    const CWD: &str = r"C:\repo";

    fn plan(argv: &[&str], files: &[&str]) -> LaunchPlan {
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        plan_launch(&argv, PATH, EXT, CWD, CMD, PS, &exists_set(files))
    }

    #[test]
    fn claude_cmd_shim_runs_through_cmd_exe() {
        let files = &[
            r"C:\Users\a\AppData\Roaming\npm\claude.cmd",
            r"C:\tools\claude.exe", // later on PATH, must not win if .CMD is... wait, PATHEXT has .EXE before .CMD
        ];
        // .EXE comes before .CMD in PATHEXT, but claude.exe is in a later PATH dir.
        // Search is PATH-dir major, PATHEXT minor: npm\claude.cmd is found before tools\claude.exe
        // only if npm is searched first — npm is before tools, and claude.cmd exists there, but
        // for each dir we try .COM .EXE .BAT .CMD. npm has no claude.exe, then claude.cmd hits.
        let p = plan(&["claude", "--dangerously-skip-permissions"], files);
        assert_eq!(p.kind, LaunchKind::Batch);
        assert_eq!(p.program, CMD);
        assert_eq!(
            p.args,
            vec![
                "/c".to_string(),
                r"C:\Users\a\AppData\Roaming\npm\claude.cmd".into(),
                "--dangerously-skip-permissions".into(),
            ]
        );
    }

    #[test]
    fn exe_on_path_is_used_directly() {
        let files = &[r"C:\tools\cursor.exe"];
        let p = plan(&["cursor", "."], files);
        assert_eq!(p.kind, LaunchKind::Exe);
        assert_eq!(p.program, r"C:\tools\cursor.exe");
        assert_eq!(p.args, vec![".".to_string()]);
    }

    #[test]
    fn pathext_prefers_exe_in_the_same_directory() {
        let files = &[r"C:\tools\claude.exe", r"C:\tools\claude.cmd"];
        let p = plan(&["claude"], files);
        assert_eq!(p.kind, LaunchKind::Exe);
        assert_eq!(p.program, r"C:\tools\claude.exe");
    }

    #[test]
    fn ps1_runs_through_powershell() {
        let files = &[r"C:\tools\doit.ps1"];
        let p = plan(&["doit", "-x"], files);
        assert_eq!(p.kind, LaunchKind::PowerShell);
        assert_eq!(p.program, PS);
        assert_eq!(
            p.args,
            vec![
                "-NoProfile".to_string(),
                "-File".into(),
                r"C:\tools\doit.ps1".into(),
                "-x".into(),
            ]
        );
    }

    #[test]
    fn explicit_cmd_path_is_wrapped() {
        let files = &[r"C:\Users\a\AppData\Roaming\npm\claude.cmd"];
        let p = plan(&[r"C:\Users\a\AppData\Roaming\npm\claude.cmd", "x"], files);
        assert_eq!(p.kind, LaunchKind::Batch);
        assert_eq!(p.program, CMD);
        assert_eq!(p.args[1], r"C:\Users\a\AppData\Roaming\npm\claude.cmd");
        assert_eq!(p.args[2], "x");
    }

    #[test]
    fn missing_command_left_as_is() {
        let p = plan(&["definitely-not-installed", "--help"], &[]);
        assert_eq!(p.program, "definitely-not-installed");
        assert_eq!(p.args, vec!["--help".to_string()]);
        assert_eq!(p.resolved, None);
    }

    #[test]
    fn cwd_is_searched_first() {
        let files = &[r"C:\repo\claude.cmd", r"C:\tools\claude.cmd"];
        let p = plan(&["claude"], files);
        assert_eq!(
            p.resolved.as_deref(),
            Some(r"C:\repo\claude.cmd")
        );
    }
}
