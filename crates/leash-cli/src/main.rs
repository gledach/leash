//! leash — read-only before/after snapshots of what an agent changed
//! on a Windows PC, outside the watched repo.
//!
//! No arguments (double-click in Explorer) opens the window.
//! Any argument keeps the existing CLI.

#![cfg_attr(windows, windows_subsystem = "windows")]

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use leash_cli::ops::{
    data_dir_or_default, load_engine, load_or_build_report, load_prefs, mode_label, resolve_repo,
    session_repo_from_store, start_session, stop_session, take_snapshot,
};
use leash_cli::open;
use leash_snapshot::{
    render_json, render_summary, render_text, SessionStore, SnapshotLabel,
};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser, Debug)]
#[command(
    name = "leash",
    version,
    about = "Watch what an AI coding agent changes on this Windows PC, outside the repo. Read-only: it never deletes, kills, or blocks anything.",
    after_help = "With no arguments, leash opens a window (double-click from Explorer).\nTypical CLI session:\n  leash start --path .     # in an elevated PowerShell, from the repo\n  # …run the agent…\n  leash stop               # short summary + paths; open the HTML with: leash open\n  leash last               # reprint the short summary\n  leash open               # open report.html"
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
    if std::env::args_os().len() <= 1 {
        return run_gui_or_hint();
    }
    attach_parent_console();
    match run_cli() {
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

fn run_gui_or_hint() -> ExitCode {
    #[cfg(windows)]
    {
        match leash_cli::gui::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                // Last-ditch: if we can attach a console, print the error.
                attach_parent_console();
                eprintln!("leash: {err}");
                ExitCode::from(2)
            }
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("leash: the window is Windows-only. Try `leash --help` for the CLI.");
        ExitCode::from(2)
    }
}

fn attach_parent_console() {
    #[cfg(windows)]
    {
        leash_cli::win_shell::attach_parent_console();
    }
}

fn run_cli() -> Result<ExitCode> {
    let cli = Cli::parse();
    let data_dir = data_dir_or_default(cli.data_dir.as_deref());
    let store = SessionStore::new(&data_dir);
    let prefs = load_prefs(&data_dir);
    let explicit = cli.path.as_deref().or(cli.repo.as_deref());

    match &cli.command {
        Commands::Doctor => {
            let repo = resolve_repo(explicit, None, prefs.last_repo_path.as_deref())?;
            run_doctor(&data_dir, &repo);
            Ok(ExitCode::SUCCESS)
        }
        Commands::Start => {
            let repo = resolve_repo(explicit, None, prefs.last_repo_path.as_deref())?;
            let started = start_session(&store, &repo, &data_dir)?;
            println!("leash session {}", started.paths.id);
            println!("repo:    {}", repo.display());
            println!(
                "files:   {}",
                mode_label(started.snap.file_mode, started.snap.elevated)
            );
            println!("saved:   {}", started.paths.before().display());
            if !started.snap.elevated {
                println!();
                println!("Note: this process is not elevated. File changes use a fallback scan of priority folders, not the USN journal.");
                println!("For a fuller picture, open an elevated PowerShell and run `leash start` again, or use Restart as admin in the window.");
            }
            for w in &started.snap.warnings {
                eprintln!("warning: {w}");
            }
            println!();
            println!("When the agent is done, run:  leash stop");
            Ok(ExitCode::SUCCESS)
        }
        Commands::Snap { which } => {
            let (label, paths) = match which {
                SnapWhich::Before => {
                    let repo = resolve_repo(explicit, None, prefs.last_repo_path.as_deref())?;
                    let started = start_session(&store, &repo, &data_dir)?;
                    println!(
                        "session {}  {}  {}",
                        started.paths.id,
                        started.snap.taken_at,
                        started.paths.before().display()
                    );
                    for w in &started.snap.warnings {
                        eprintln!("warning: {w}");
                    }
                    return Ok(ExitCode::SUCCESS);
                }
                SnapWhich::After => {
                    let paths = store.open(None)?;
                    (SnapshotLabel::After, paths)
                }
            };
            let session_repo = store.read_before(&paths).ok().and_then(|s| s.repo_path);
            let repo = resolve_repo(
                explicit,
                session_repo.as_deref(),
                prefs.last_repo_path.as_deref(),
            )?;
            let before = store.read_before(&paths).ok();
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
            let session_repo = session_repo_from_store(&store, session.as_deref());
            let repo = resolve_repo(
                explicit,
                session_repo.as_deref(),
                prefs.last_repo_path.as_deref(),
            )?;
            let (engine, exit_on_high) = load_engine(cli.config.as_deref(), &data_dir, &repo)?;
            let (paths, report) =
                stop_session(&store, session.as_deref(), &repo, &data_dir, &engine)?;
            print_report(
                &paths,
                &report,
                *json,
                *verbose,
                *open,
                markdown.as_ref(),
                true,
                exit_on_high,
            )
        }
        Commands::Diff {
            session,
            json,
            verbose,
            open,
            markdown,
        } => {
            let session_repo = session_repo_from_store(&store, session.as_deref());
            let repo = resolve_repo(
                explicit,
                session_repo.as_deref(),
                prefs.last_repo_path.as_deref(),
            )?;
            let (engine, exit_on_high) = load_engine(cli.config.as_deref(), &data_dir, &repo)?;
            let paths = store.open(session.as_deref())?;
            let before = store.read_before(&paths)?;
            let after = store.read_after(&paths)?;
            let report = load_or_build_report(&store, &paths, Some((&before, &after)), &engine)?;
            print_report(
                &paths,
                &report,
                *json,
                *verbose,
                *open,
                markdown.as_ref(),
                false,
                exit_on_high,
            )
        }
        Commands::Report {
            session,
            json,
            verbose,
            open,
        } => {
            let session_repo = session_repo_from_store(&store, session.as_deref());
            let repo = resolve_repo(
                explicit,
                session_repo.as_deref(),
                prefs.last_repo_path.as_deref(),
            )?;
            let (engine, exit_on_high) = load_engine(cli.config.as_deref(), &data_dir, &repo)?;
            let paths = store.open(session.as_deref())?;
            let report = load_or_build_report(&store, &paths, None, &engine)?;
            print_report(
                &paths,
                &report,
                *json,
                *verbose,
                *open,
                None,
                false,
                exit_on_high,
            )
        }
        Commands::Last { session } => {
            let session_repo = session_repo_from_store(&store, session.as_deref());
            let repo = resolve_repo(
                explicit,
                session_repo.as_deref(),
                prefs.last_repo_path.as_deref(),
            )?;
            let (engine, exit_on_high) = load_engine(cli.config.as_deref(), &data_dir, &repo)?;
            let paths = store.open(session.as_deref())?;
            let report = load_or_build_report(&store, &paths, None, &engine)?;
            print_report(&paths, &report, false, false, false, None, false, exit_on_high)
        }
        Commands::Open {
            md,
            json,
            folder,
            session,
        } => {
            let paths = store.open(session.as_deref())?;
            let target = if *folder {
                paths.dir.clone()
            } else if *json {
                paths.report_json()
            } else if *md {
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

#[allow(clippy::too_many_arguments)]
fn print_report(
    paths: &leash_snapshot::SessionPaths,
    report: &leash_snapshot::DiffReport,
    json: bool,
    verbose: bool,
    open_folder: bool,
    markdown_copy: Option<&PathBuf>,
    hint_open: bool,
    exit_on_high: bool,
) -> Result<ExitCode> {
    if let Some(extra) = markdown_copy {
        if let Some(parent) = extra.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let md = leash_snapshot::render_markdown(report);
        std::fs::write(extra, md)?;
        eprintln!("markdown copy: {}", extra.display());
    }

    if json {
        println!("{}", render_json(report)?);
    } else if verbose {
        print!("{}", render_text(report));
        print_report_paths(paths);
    } else {
        print!("{}", render_summary(report));
        print_report_paths(paths);
    }
    if hint_open && !json {
        println!("Open it: leash open");
    }
    if open_folder {
        open::open_with_default_app(&paths.dir)?;
        eprintln!("opened {}", paths.dir.display());
    }
    if exit_on_high && report.counts.high > 0 {
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

fn run_doctor(data_dir: &std::path::Path, repo: &std::path::Path) {
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
