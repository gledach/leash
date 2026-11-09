//! Start / stop / snapshot collection shared by the CLI and the window.

use anyhow::{Context, Result};
use leash_filter::{context_from_env, FilterConfig, FilterContext, FilterEngine};
use leash_packages::collect_packages;
use leash_persist::{collect as collect_persist, PersistOptions};
use leash_snapshot::{
    default_data_dir, diff_snapshots, new_session_id, now_local_rfc3339, render_html,
    render_markdown, resolve_repo_path, DiffOptions, DiffReport, FileMode, SessionPaths,
    SessionStore, Snapshot, SnapshotLabel, UiPrefs,
};
use leash_usn::{collect_after, collect_before, ScanOptions};
use std::path::{Path, PathBuf};

pub struct StartResult {
    pub paths: SessionPaths,
    pub snap: Snapshot,
}

pub fn start_session(
    store: &SessionStore,
    repo: &Path,
    data_dir: &Path,
) -> Result<StartResult> {
    let id = new_session_id();
    let paths = store.create(&id)?;
    let snap = take_snapshot(SnapshotLabel::Before, &id, repo, data_dir, None)?;
    store.write_snapshot(&paths, &snap)?;
    Ok(StartResult { paths, snap })
}

pub fn stop_session(
    store: &SessionStore,
    session: Option<&str>,
    repo: &Path,
    data_dir: &Path,
    engine: &FilterEngine,
) -> Result<(SessionPaths, DiffReport)> {
    let paths = store.open(session)?;
    let before = store.read_before(&paths).with_context(|| {
        format!(
            "session {} has no before snapshot; run Start first",
            paths.id
        )
    })?;
    let after = take_snapshot(
        SnapshotLabel::After,
        &paths.id,
        repo,
        data_dir,
        Some(&before),
    )?;
    store.write_snapshot(&paths, &after)?;
    let report = write_diff(store, &paths, &before, &after, engine)?;
    Ok((paths, report))
}

pub fn write_diff(
    store: &SessionStore,
    paths: &SessionPaths,
    before: &Snapshot,
    after: &Snapshot,
    engine: &FilterEngine,
) -> Result<DiffReport> {
    let report = diff_snapshots(before, after, DiffOptions { engine });
    let md = render_markdown(&report);
    let html = render_html(&report);
    store.write_report(paths, &report, &md, &html)?;
    Ok(report)
}

pub fn load_or_build_report(
    store: &SessionStore,
    paths: &SessionPaths,
    pair: Option<(&Snapshot, &Snapshot)>,
    engine: &FilterEngine,
) -> Result<DiffReport> {
    if let Some((before, after)) = pair {
        return write_diff(store, paths, before, after, engine);
    }
    if paths.report_json().exists() {
        let report = store.read_report(paths)?;
        // Refresh html/md if a report already exists (keeps files in sync).
        let md = render_markdown(&report);
        let html = render_html(&report);
        store.write_report(paths, &report, &md, &html)?;
        return Ok(report);
    }
    let before = store.read_before(paths)?;
    let after = store.read_after(paths)?;
    write_diff(store, paths, &before, &after, engine)
}

pub fn take_snapshot(
    label: SnapshotLabel,
    session_id: &str,
    repo: &Path,
    data_dir: &Path,
    before: Option<&Snapshot>,
) -> Result<Snapshot> {
    let mut snap = Snapshot::new(label, session_id);
    snap.taken_at = now_local_rfc3339();
    snap.hostname = leash_persist::hostname();
    snap.username = leash_persist::username();
    snap.repo_path = Some(leash_filter::pretty_path(&repo.to_string_lossy()));
    snap.elevated = leash_usn::is_elevated();

    let scan = build_scan_options(repo, data_dir);
    let files = match label {
        SnapshotLabel::Before => collect_before(&scan),
        SnapshotLabel::After => {
            let cursors = before.map(|b| b.usn.as_slice()).unwrap_or(&[]);
            collect_after(cursors, &scan)
        }
    };
    match files {
        Ok(col) => {
            snap.file_mode = col.mode;
            snap.elevated = col.elevated;
            snap.usn = col.usn;
            snap.file_events = col.events;
            snap.fallback_files = col.fallback_files;
            snap.warnings.extend(col.warnings);
        }
        Err(err) => {
            snap.file_mode = FileMode::Unavailable;
            snap.warnings.push(format!("file collection failed: {err}"));
        }
    }

    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    let persist = collect_persist(&PersistOptions { home });
    snap.run_keys = persist.run_keys;
    snap.scheduled_tasks = persist.scheduled_tasks;
    snap.services = persist.services;
    snap.env_user = persist.env_user;
    snap.env_system = persist.env_system;
    snap.profiles = persist.profiles;
    snap.git_configs = persist.git_configs;
    snap.ssh = persist.ssh;
    snap.ports = persist.ports;
    snap.processes = persist.processes;
    snap.warnings.extend(persist.warnings);
    snap.packages = collect_packages();
    snap.warnings.extend(snap.packages.notes.iter().cloned());
    Ok(snap)
}

pub fn build_scan_options(repo: &Path, data_dir: &Path) -> ScanOptions {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
    let mut roots = Vec::new();
    let mut extra_files = Vec::new();
    let mut shallow_roots = Vec::new();
    if let Some(home) = &home {
        shallow_roots.push(home.clone());
        let user_files = leash_persist::collect_user_files(home);
        extra_files.extend(user_files.profiles.iter().map(|f| PathBuf::from(&f.path)));
        extra_files.extend(
            user_files
                .git_configs
                .iter()
                .map(|f| PathBuf::from(&f.path)),
        );
        if let Some(cfg) = user_files.ssh.config.as_ref() {
            extra_files.push(PathBuf::from(&cfg.path));
        }
        for name in [
            "Desktop",
            "Documents",
            "src",
            "source",
            "projects",
            "dev",
            "code",
            "work",
            "git",
            "repos",
        ] {
            let p = home.join(name);
            if p.is_dir() {
                roots.push(p);
            }
        }
    }
    if let Some(app) = &appdata {
        roots.push(app.clone());
        roots.push(app.join("Microsoft/Windows/Start Menu/Programs/Startup"));
    }
    if let Some(program_data) = std::env::var_os("PROGRAMDATA").map(PathBuf::from) {
        roots.push(program_data.join("Microsoft/Windows/Start Menu/Programs/StartUp"));
    }
    if let Some(parent) = repo.parent() {
        if parent != Path::new("") {
            roots.push(parent.to_path_buf());
        }
    }
    let _ = data_dir;
    roots.sort();
    roots.dedup();
    ScanOptions {
        roots,
        shallow_roots,
        extra_files,
    }
}

pub fn load_engine(
    config: Option<&Path>,
    data_dir: &Path,
    repo: &Path,
) -> Result<(FilterEngine, bool)> {
    let config_path = config.map(Path::to_path_buf).or_else(|| {
        let p = data_dir.join("config.toml");
        p.exists().then_some(p)
    });
    let config = if let Some(path) = config_path {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        FilterConfig::load_overlay(&text).with_context(|| format!("parsing {}", path.display()))?
    } else {
        FilterConfig::default_rules()
    };
    let exit_on_high = config.exit_on_high;
    let context = build_context(repo, data_dir);
    let engine = FilterEngine::new(config, context)?;
    Ok((engine, exit_on_high))
}

pub fn build_context(repo: &Path, data_dir: &Path) -> FilterContext {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
    let temp = std::env::var_os("TEMP")
        .or_else(|| std::env::var_os("TMP"))
        .map(PathBuf::from);
    let mut other = Vec::new();
    if let Some(parent) = repo.parent() {
        if let Ok(rd) = std::fs::read_dir(parent) {
            for ent in rd.flatten() {
                let p = ent.path();
                if p == repo {
                    continue;
                }
                if leash_filter::looks_like_project_root(&p) {
                    other.push(p);
                }
            }
        }
    }
    context_from_env(
        Some(repo),
        Some(data_dir),
        home.as_deref(),
        local.as_deref(),
        appdata.as_deref(),
        temp.as_deref(),
        other,
    )
}

/// Resolve the watched repo. `session_repo` is the path stored on the before
/// snapshot (used when Stop runs after the window was closed).
pub fn resolve_repo(
    explicit: Option<&Path>,
    session_repo: Option<&str>,
    last_pref: Option<&str>,
) -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("current directory")?;
    let raw = resolve_repo_path(explicit, session_repo, last_pref, &cwd);
    Ok(std::fs::canonicalize(&raw).unwrap_or(raw))
}

pub fn session_repo_from_store(store: &SessionStore, session: Option<&str>) -> Option<String> {
    let paths = store.open(session).ok()?;
    store
        .read_before(&paths)
        .ok()
        .and_then(|s| s.repo_path)
}

pub fn data_dir_or_default(override_dir: Option<&Path>) -> PathBuf {
    override_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(default_data_dir)
}

pub fn load_prefs(data_dir: &Path) -> UiPrefs {
    UiPrefs::load(data_dir)
}

pub fn mode_label(mode: FileMode, elevated: bool) -> String {
    let base = match mode {
        FileMode::Usn => "USN change journal",
        FileMode::FallbackScan => "fallback folder scan (mtime/hash)",
        FileMode::Mixed => "USN journal + fallback folder scan",
        FileMode::Unavailable => "not collected",
    };
    if elevated {
        base.to_string()
    } else {
        format!("{base}; not elevated")
    }
}

pub fn is_elevated() -> bool {
    leash_usn::is_elevated()
}

pub fn coverage_sentence(elevated: bool, file_mode: Option<FileMode>) -> String {
    if elevated && matches!(file_mode, Some(FileMode::Usn) | Some(FileMode::Mixed) | None) {
        "Full coverage: this window is running as administrator, so file changes come from the Windows change log."
            .into()
    } else if elevated {
        format!(
            "Running as administrator ({}).",
            file_mode
                .map(|m| mode_label(m, true))
                .unwrap_or_else(|| "file mode unknown".into())
        )
    } else {
        "Not administrator: leash can still see startup entries, packages and ports, but file coverage is a fallback scan of key folders, not the full disk change log.".into()
    }
}
