//! leash — read-only before/after snapshots of what an agent changed
//! on a Windows PC, outside the watched repo.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use leash_filter::{context_from_env, FilterConfig, FilterContext, FilterEngine};
use leash_packages::collect_packages;
use leash_persist::{collect as collect_persist, PersistOptions};
use leash_snapshot::{
    default_data_dir, diff_snapshots, render_html, render_json, render_markdown, render_summary,
    render_text, DiffOptions, DiffReport, FileMode, SessionStore, Snapshot, SnapshotLabel,
};
use leash_usn::{collect_after, collect_before, ScanOptions};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod open;

#[derive(Parser, Debug)]
#[command(
    name = "leash",
    version,
    about = "Watch what an AI coding agent changes on this Windows PC, outside the repo. Read-only: it never deletes, kills, or blocks anything.",
    after_help = "Typical session:\n  leash start --path .     # in an elevated PowerShell, from the repo\n  # …run the agent…\n  leash stop               # short summary + paths; open the HTML with: leash open\n  leash last               # reprint the short summary\n  leash open               # open report.html"
)]
struct Cli {
    /// Path of the watched repo (changes inside it are ignored, except .env and similar).
    #[arg(long, global = true)]
    path: Option<PathBuf>,

    /// Alias for --path.
    #[arg(long, global = true)]
    repo: Option<PathBuf>,

    /// Filter config TOML (defaults to %LOCALAPPDATA%\leash\config.toml if present).
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Override the data directory (default: %LOCALAPPDATA%\leash).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Take a baseline snapshot and print a session id.
    Start,
    /// Take the second snapshot and write a report (short summary on the console).
    Stop {
        /// Session id from `leash start` (default: the current session).
        session: Option<String>,
        /// Print the report as JSON instead of the short summary.
        #[arg(long)]
        json: bool,
        /// Print the full text report on the console.
        #[arg(long)]
        verbose: bool,
        /// Open the session folder in Explorer.
        #[arg(long)]
        open: bool,
        /// Also write a markdown report to this path (a copy is always stored in the session folder).
        #[arg(long)]
        markdown: Option<PathBuf>,
    },
    /// Re-print the last report (short summary by default).
    Report {
        session: Option<String>,
        /// Print report.json to stdout.
        #[arg(long)]
        json: bool,
        /// Print the full text report on the console.
        #[arg(long)]
        verbose: bool,
        /// Open the session folder in Explorer.
        #[arg(long)]
        open: bool,
    },
    /// Print a concise summary of the latest (or named) session.
    Last { session: Option<String> },
    /// Open the latest HTML report in the default app.
    Open {
        /// Open report.md instead of report.html.
        #[arg(long, conflicts_with_all = ["json", "folder"])]
        md: bool,
        /// Open report.json instead of report.html.
        #[arg(long, conflicts_with = "folder")]
        json: bool,
        /// Open the session folder in Explorer.
        #[arg(long)]
        folder: bool,
        session: Option<String>,
    },
    /// Check privileges, USN availability, and collector health.
    Doctor,
    /// Write a named snapshot (`before` or `after`) without printing a report.
    Snap { which: SnapWhich },
    /// Compare the last before/after snapshots (same as `stop` without taking a new snapshot).
    Diff {
        session: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        verbose: bool,
        #[arg(long)]
        open: bool,
        #[arg(long)]
        markdown: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SnapWhich {
    Before,
    After,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("leash: {err}");
            for cause in err.chain().skip(1) {
                eprintln!("  caused by: {cause}");
            }
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    let data_dir = cli.data_dir.clone().unwrap_or_else(default_data_dir);
    let store = SessionStore::new(&data_dir);
    let repo = resolve_repo(cli.path.as_deref().or(cli.repo.as_deref()))?;
    let (engine, exit_on_high) = load_engine(&cli, &data_dir, &repo)?;

    match cli.command {
        Commands::Doctor => {
            run_doctor(&data_dir, &repo);
            Ok(ExitCode::SUCCESS)
        }
        Commands::Start => {
            let id = new_session_id();
            let paths = store.create(&id)?;
            let snap = take_snapshot(SnapshotLabel::Before, &id, &repo, &data_dir, None)?;
            let written = store.write_snapshot(&paths, &snap)?;
            println!("leash session {id}");
            println!("repo:    {}", repo.display());
            println!("files:   {}", mode_label(snap.file_mode, snap.elevated));
            println!("saved:   {}", written.display());
            if !snap.elevated {
                println!();
                println!("Note: this process is not elevated. File changes use a fallback scan of priority folders, not the USN journal.");
                println!("For a fuller picture, open an elevated PowerShell and run `leash start` again.");
            }
            for w in &snap.warnings {
                eprintln!("warning: {w}");
            }
            println!();
            println!("When the agent is done, run:  leash stop");
            Ok(ExitCode::SUCCESS)
        }
        Commands::Snap { which } => {
            let (label, paths) = match which {
                SnapWhich::Before => {
                    let id = new_session_id();
                    let paths = store.create(&id)?;
                    (SnapshotLabel::Before, paths)
                }
                SnapWhich::After => {
                    let paths = store.open(None)?;
                    (SnapshotLabel::After, paths)
                }
            };
            let before = if matches!(label, SnapshotLabel::After) {
                Some(store.read_before(&paths)?)
            } else {
                None
            };
            let snap = take_snapshot(label, &paths.id, &repo, &data_dir, before.as_ref())?;
            let written = store.write_snapshot(&paths, &snap)?;
            println!(
                "session {}  {}  {}",
                paths.id,
                snap.taken_at,
                written.display()
            );
            for w in &snap.warnings {
                eprintln!("warning: {w}");
            }
            Ok(ExitCode::SUCCESS)
        }
        Commands::Stop {
            session,
            json,
            verbose,
            open,
            markdown,
        } => {
            let paths = store.open(session.as_deref())?;
            let before = store.read_before(&paths).with_context(|| {
                format!(
                    "session {} has no before snapshot; run `leash start` first",
                    paths.id
                )
            })?;
            let after = take_snapshot(
                SnapshotLabel::After,
                &paths.id,
                &repo,
                &data_dir,
                Some(&before),
            )?;
            store.write_snapshot(&paths, &after)?;
            finish_report(
                &store,
                &paths,
                Some((&before, &after)),
                &engine,
                &ReportPrint {
                    json,
                    verbose,
                    open_folder: open,
                    markdown_copy: markdown,
                    hint_open: true,
                    exit_on_high,
                },
            )
        }
        Commands::Diff {
            session,
            json,
            verbose,
            open,
            markdown,
        } => {
            let paths = store.open(session.as_deref())?;
            let before = store.read_before(&paths)?;
            let after = store.read_after(&paths)?;
            finish_report(
                &store,
                &paths,
                Some((&before, &after)),
                &engine,
                &ReportPrint {
                    json,
                    verbose,
                    open_folder: open,
                    markdown_copy: markdown,
                    hint_open: false,
                    exit_on_high,
                },
            )
        }
        Commands::Report {
            session,
            json,
            verbose,
            open,
        } => {
            let paths = store.open(session.as_deref())?;
            finish_report(
                &store,
                &paths,
                None,
                &engine,
                &ReportPrint {
                    json,
                    verbose,
                    open_folder: open,
                    markdown_copy: None,
                    hint_open: false,
                    exit_on_high,
                },
            )
        }
        Commands::Last { session } => {
            let paths = store.open(session.as_deref())?;
            finish_report(
                &store,
                &paths,
                None,
                &engine,
                &ReportPrint {
                    json: false,
                    verbose: false,
                    open_folder: false,
                    markdown_copy: None,
                    hint_open: false,
                    exit_on_high,
                },
            )
        }
        Commands::Open {
            md,
            json,
            folder,
            session,
        } => {
            let paths = store.open(session.as_deref())?;
            let target = if folder {
                paths.dir.clone()
            } else if json {
                paths.report_json()
            } else if md {
                paths.report_md()
            } else {
                paths.report_html()
            };
            open::open_with_default_app(&target)?;
            println!("opened {}", target.display());
            Ok(ExitCode::SUCCESS)
        }
    }
}

struct ReportPrint {
    json: bool,
    verbose: bool,
    open_folder: bool,
    markdown_copy: Option<PathBuf>,
    hint_open: bool,
    exit_on_high: bool,
}

fn finish_report(
    store: &SessionStore,
    paths: &leash_snapshot::SessionPaths,
    pair: Option<(&Snapshot, &Snapshot)>,
    engine: &FilterEngine,
    opts: &ReportPrint,
) -> Result<ExitCode> {
    let report = load_or_build_report(store, paths, pair, engine)?;
    let md = render_markdown(&report);
    let html = render_html(&report);
    store.write_report(paths, &report, &md, &html)?;
    if let Some(extra) = &opts.markdown_copy {
        if let Some(parent) = extra.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(extra, &md)?;
        eprintln!("markdown copy: {}", extra.display());
    }

    if opts.json {
        println!("{}", render_json(&report)?);
    } else if opts.verbose {
        print!("{}", render_text(&report));
        print_report_paths(paths);
    } else {
        print!("{}", render_summary(&report));
        print_report_paths(paths);
    }
    if opts.hint_open && !opts.json {
        println!("Open it: leash open");
    }
    if opts.open_folder {
        open::open_with_default_app(&paths.dir)?;
        eprintln!("opened {}", paths.dir.display());
    }
    if opts.exit_on_high && report.counts.high > 0 {
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

fn print_report_paths(paths: &leash_snapshot::SessionPaths) {
    println!("report.html  {}", paths.report_html().display());
    println!("report.json  {}", paths.report_json().display());
    println!("report.md    {}", paths.report_md().display());
}

fn load_or_build_report(
    store: &SessionStore,
    paths: &leash_snapshot::SessionPaths,
    pair: Option<(&Snapshot, &Snapshot)>,
    engine: &FilterEngine,
) -> Result<DiffReport> {
    if let Some((before, after)) = pair {
        return Ok(diff_snapshots(before, after, DiffOptions { engine }));
    }
    if paths.report_json().exists() {
        return Ok(store.read_report(paths)?);
    }
    let before = store.read_before(paths)?;
    let after = store.read_after(paths)?;
    Ok(diff_snapshots(&before, &after, DiffOptions { engine }))
}

fn take_snapshot(
    label: SnapshotLabel,
    session_id: &str,
    repo: &Path,
    data_dir: &Path,
    before: Option<&Snapshot>,
) -> Result<Snapshot> {
    let mut snap = Snapshot::new(label, session_id);
    snap.taken_at = now_rfc3339();
    snap.hostname = leash_persist::hostname();
    snap.username = leash_persist::username();
    snap.repo_path = Some(repo.to_string_lossy().into_owned());
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

fn build_scan_options(repo: &Path, data_dir: &Path) -> ScanOptions {
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
    // Sibling projects of the watched repo.
    if let Some(parent) = repo.parent() {
        if parent != Path::new("") {
            roots.push(parent.to_path_buf());
        }
    }
    // Walking the whole user profile is too large; the walker has a depth cap and skip list.
    // Still, prefer not to walk the entire home recursively as a first root if it is huge:
    // keep it, but the skip list drops AppData\Local noise only when named Temp/Cache/etc.
    // Limit home itself to depth via walker MAX_DEPTH.
    let _ = data_dir;
    // Dedup
    roots.sort();
    roots.dedup();
    ScanOptions {
        roots,
        shallow_roots,
        extra_files,
    }
}

fn load_engine(cli: &Cli, data_dir: &Path, repo: &Path) -> Result<(FilterEngine, bool)> {
    let config_path = cli.config.clone().or_else(|| {
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

fn build_context(repo: &Path, data_dir: &Path) -> FilterContext {
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

fn resolve_repo(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()));
    }
    let cwd = std::env::current_dir().context("current directory")?;
    Ok(std::fs::canonicalize(&cwd).unwrap_or(cwd))
}

fn new_session_id() -> String {
    let now = chrono::Local::now();
    now.format("%Y%m%d-%H%M%S").to_string()
}

fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339()
}

fn mode_label(mode: FileMode, elevated: bool) -> String {
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

fn run_doctor(data_dir: &Path, repo: &Path) {
    println!("leash doctor");
    println!("  version:    {}", env!("CARGO_PKG_VERSION"));
    println!("  os:         {}", std::env::consts::OS);
    println!("  arch:       {}", std::env::consts::ARCH);
    println!("  repo:       {}", repo.display());
    println!("  data dir:   {}", data_dir.display());
    match std::fs::create_dir_all(data_dir) {
        Ok(()) => println!("  data dir writable: yes"),
        Err(err) => println!("  data dir writable: no ({err})"),
    }
    println!();
    for line in leash_usn::doctor_lines() {
        println!("  {line}");
    }
    println!();
    for line in leash_persist::doctor_lines() {
        println!("  {line}");
    }
    println!();
    for line in leash_packages::doctor_lines() {
        println!("  {line}");
    }
    println!();
    println!(
        "leash never deletes, kills, or blocks anything. It does not open a network connection."
    );
    println!("v1 records what changed, not which process did it.");
}
