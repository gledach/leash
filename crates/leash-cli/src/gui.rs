//! Double-click window: Start / Stop, session resume, and report list.

#![cfg(windows)]

use crate::ops::{
    coverage_sentence, is_elevated, load_engine, load_prefs, start_session, stop_session,
};
use crate::win_shell::{
    add_start_menu_shortcut, open_path, pick_folder, relaunch_elevated, start_menu_shortcut_path,
};
use chrono::Utc;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable as _, Theme, ThemeMode};
use gpui_kit::*;
use leash_filter::Severity;
use leash_snapshot::{
    format_elapsed_since, summary_finding_line, DiffReport, SessionInfo, SessionStatus,
    SessionStore, UiPrefs, UiTheme,
};
use std::path::PathBuf;
use std::time::Duration;

pub fn run() -> anyhow::Result<()> {
    let mut app = gpui_kit::application();
    app = app.with_assets(gpui_kit::assets::Assets);
    app.run(move |cx| {
        gpui_kit::init(cx);
        let bounds = Bounds::centered(None, size(px(760.), px(680.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("leash".into()),
                ..TitlebarOptions::default()
            }),
            ..WindowOptions::default()
        };
        gpui_kit::open_window(options, cx, |_, cx| cx.new(LeashApp::new))
            .expect("failed to open the leash window");
    });
    Ok(())
}

enum Phase {
    Idle,
    Starting,
    Running(SessionInfo),
    Stopping,
    Done {
        info: SessionInfo,
        report: Box<DiffReport>,
    },
    Error(String),
}

struct LeashApp {
    data_dir: PathBuf,
    store: SessionStore,
    prefs: UiPrefs,
    repo: PathBuf,
    phase: Phase,
    sessions: Vec<SessionInfo>,
    elevated: bool,
    start_menu_ok: bool,
    note: Option<String>,
}

impl LeashApp {
    fn new(cx: &mut Context<Self>) -> Self {
        let data_dir = leash_snapshot::default_data_dir();
        let _ = std::fs::create_dir_all(&data_dir);
        let store = SessionStore::new(&data_dir);
        let prefs = load_prefs(&data_dir);
        let theme = match prefs.theme {
            UiTheme::Dark => ThemeMode::Dark,
            UiTheme::Light => ThemeMode::Light,
        };
        Theme::change(theme, None, cx);

        let mut app = Self {
            data_dir: data_dir.clone(),
            store,
            prefs,
            repo: PathBuf::new(),
            phase: Phase::Idle,
            sessions: Vec::new(),
            elevated: is_elevated(),
            start_menu_ok: start_menu_shortcut_path()
                .map(|p| p.exists())
                .unwrap_or(false),
            note: None,
        };
        app.reload_from_disk();
        app.tick_elapsed(cx);
        app
    }

    fn reload_from_disk(&mut self) {
        self.sessions = self.store.list_sessions();
        if let Some(running) = self.store.running_session() {
            if let Some(repo) = &running.repo_path {
                self.repo = PathBuf::from(repo);
            }
            self.phase = Phase::Running(running);
            return;
        }
        if let Some(last) = self
            .sessions
            .iter()
            .find(|s| s.status == SessionStatus::Completed && s.has_html)
        {
            if let Ok(paths) = self.store.open(Some(&last.id)) {
                if let Ok(report) = self.store.read_report(&paths) {
                    self.phase = Phase::Done {
                        info: last.clone(),
                        report: Box::new(report),
                    };
                    if let Some(repo) = last.repo_path.as_ref() {
                        self.repo = PathBuf::from(repo);
                    }
                }
            }
        }
        if self.repo.as_os_str().is_empty() {
            if let Some(p) = &self.prefs.last_repo_path {
                self.repo = PathBuf::from(p);
            } else if let Ok(cwd) = std::env::current_dir() {
                self.repo = cwd;
            }
        }
    }

    fn save_prefs(&mut self) {
        self.prefs.remember_repo(&self.repo);
        let _ = self.prefs.save(&self.data_dir);
    }

    fn tick_elapsed(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_secs(1))
                .await;
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        })
        .detach();
    }

    fn browse(&mut self, cx: &mut Context<Self>) {
        match pick_folder(None) {
            Ok(Some(path)) => {
                self.repo = path;
                self.save_prefs();
                self.note = None;
            }
            Ok(None) => {}
            Err(err) => self.note = Some(format!("Could not open the folder picker: {err}")),
        }
        cx.notify();
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        if matches!(self.phase, Phase::Starting | Phase::Running(_) | Phase::Stopping) {
            return;
        }
        if self.repo.as_os_str().is_empty() || !self.repo.exists() {
            self.note = Some("Choose a project folder first.".into());
            cx.notify();
            return;
        }
        self.save_prefs();
        self.phase = Phase::Starting;
        self.note = Some("Taking the before snapshot…".into());
        let repo = self.repo.clone();
        let data_dir = self.data_dir.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let store = SessionStore::new(&data_dir);
                    start_session(&store, &repo, &data_dir)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(started) => {
                        this.note = None;
                        let info = this
                            .store
                            .running_session()
                            .unwrap_or_else(|| SessionInfo::from_paths(&started.paths));
                        this.sessions = this.store.list_sessions();
                        this.phase = Phase::Running(info);
                    }
                    Err(err) => {
                        this.phase = Phase::Error(err.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.phase, Phase::Running(_)) {
            return;
        }
        self.phase = Phase::Stopping;
        self.note = Some("Taking the after snapshot and writing the report…".into());
        let repo = self.repo.clone();
        let data_dir = self.data_dir.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let store = SessionStore::new(&data_dir);
                    let engine = load_engine(None, &data_dir, &repo)?.0;
                    stop_session(&store, None, &repo, &data_dir, &engine)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok((paths, report)) => {
                        this.sessions = this.store.list_sessions();
                        let info = SessionInfo::from_paths(&paths);
                        this.note = None;
                        this.phase = Phase::Done { info, report: Box::new(report) };
                    }
                    Err(err) => this.phase = Phase::Error(err.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_report(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Ok(paths) = self.store.open(Some(id)) {
            if let Err(err) = open_path(&paths.report_html()) {
                self.note = Some(err.to_string());
            }
        }
        cx.notify();
    }

    fn elevate(&mut self, cx: &mut Context<Self>) {
        self.save_prefs();
        match relaunch_elevated() {
            Ok(()) => cx.quit(),
            Err(err) => {
                self.note = Some(format!("Could not restart as administrator: {err}"));
                cx.notify();
            }
        }
    }

    fn pin_start_menu(&mut self, cx: &mut Context<Self>) {
        match add_start_menu_shortcut() {
            Ok(path) => {
                self.start_menu_ok = true;
                self.note = Some(format!("Added to the Start menu: {}", path.display()));
            }
            Err(err) => self.note = Some(format!("Could not create the shortcut: {err}")),
        }
        cx.notify();
    }

    fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        self.prefs.theme = self.prefs.theme.toggle();
        let mode = match self.prefs.theme {
            UiTheme::Dark => ThemeMode::Dark,
            UiTheme::Light => ThemeMode::Light,
        };
        Theme::change(mode, None, cx);
        self.save_prefs();
        cx.notify();
    }
}

impl Render for LeashApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .p_5()
            .gap_4()
            .child(self.header(cx, &theme))
            .child(self.folder_row(cx, &theme))
            .child(self.action_row(cx, &theme))
            .child(self.status_block(cx, &theme))
            .child(self.result_block(cx, &theme))
            .child(self.sessions_block(cx, &theme))
            .child(self.footer(cx, &theme))
    }
}

impl LeashApp {
    fn header(&self, cx: &mut Context<Self>, theme: &Theme) -> impl IntoElement {
        h_flex()
            .w_full()
            .justify_between()
            .items_start()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(22.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("leash"),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(theme.muted_foreground)
                            .child("See what an AI agent changed outside your project. Read-only — nothing is deleted, killed, or blocked."),
                    ),
            )
            .child(
                Button::new("theme")
                    .ghost()
                    .label(if self.prefs.theme == UiTheme::Dark {
                        "Light"
                    } else {
                        "Dark"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_theme(cx))),
            )
    }

    fn folder_row(&self, cx: &mut Context<Self>, theme: &Theme) -> impl IntoElement {
        let path = if self.repo.as_os_str().is_empty() {
            "No project folder chosen".to_string()
        } else {
            self.repo.display().to_string()
        };
        v_flex()
            .w_full()
            .gap_1()
            .child(
                div()
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child("PROJECT FOLDER"),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.secondary)
                            .font_family("Consolas")
                            .text_size(px(13.))
                            .child(path),
                    )
                    .child(
                        Button::new("browse")
                            .label("Browse")
                            .on_click(cx.listener(|this, _, _, cx| this.browse(cx))),
                    ),
            )
    }

    fn action_row(&self, cx: &mut Context<Self>, _theme: &Theme) -> impl IntoElement {
        let (label, enabled, is_start) = match &self.phase {
            Phase::Starting => ("Starting…", false, true),
            Phase::Stopping => ("Stopping…", false, false),
            Phase::Running(_) => ("Stop", true, false),
            Phase::Idle | Phase::Done { .. } | Phase::Error(_) => ("Start", true, true),
        };
        let start = is_start;
        h_flex()
            .gap_2()
            .child(
                Button::new("start-stop")
                    .primary()
                    .label(label)
                    .disabled(!enabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if start {
                            this.start(cx);
                        } else {
                            this.stop(cx);
                        }
                    })),
            )
    }

    fn status_block(&self, cx: &mut Context<Self>, theme: &Theme) -> impl IntoElement {
        let mut lines: Vec<(String, bool)> = Vec::new();
        match &self.phase {
            Phase::Idle => lines.push(("Ready. Choose a project folder and press Start before you run the agent.".into(), false)),
            Phase::Starting => lines.push(("Taking the before snapshot. This can take a few seconds.".into(), false)),
            Phase::Stopping => lines.push(("Taking the after snapshot and writing the report.".into(), false)),
            Phase::Running(info) => {
                let elapsed = info
                    .started_at
                    .as_deref()
                    .and_then(|t| format_elapsed_since(t, Utc::now()))
                    .unwrap_or_else(|| "a moment".into());
                lines.push((format!("Watching for {elapsed}."), false));
                lines.push((
                    coverage_sentence(info.elevated.unwrap_or(self.elevated), info.file_mode),
                    false,
                ));
                if let Some(id) = Some(&info.id) {
                    lines.push((format!("Session {id} is saved on disk, so you can close this window and press Stop later."), false));
                }
            }
            Phase::Done { info, .. } => {
                lines.push((
                    coverage_sentence(info.elevated.unwrap_or(self.elevated), info.file_mode),
                    false,
                ));
            }
            Phase::Error(err) => lines.push((err.clone(), true)),
        }
        if let Some(note) = &self.note {
            lines.push((note.clone(), false));
        }

        let show_elevate = !self.elevated;
        v_flex()
            .w_full()
            .gap_2()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .children(lines.into_iter().map(|(text, danger)| {
                div()
                    .text_size(px(13.))
                    .text_color(if danger {
                        theme.danger
                    } else {
                        theme.muted_foreground
                    })
                    .child(text)
            }))
            .children(show_elevate.then(|| {
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("elevate")
                            .label("Restart as admin for full coverage")
                            .on_click(cx.listener(|this, _, _, cx| this.elevate(cx))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(12.))
                            .text_color(theme.muted_foreground)
                            .child("Windows only lets leash read the full disk change log as administrator."),
                    )
            }))
    }

    fn result_block(&self, cx: &mut Context<Self>, theme: &Theme) -> impl IntoElement {
        let Phase::Done { info, report } = &self.phase else {
            return div().into_any_element();
        };
        let counts = format!(
            "{} high,  {} medium,  {} info    ({} file events ignored)",
            report.counts.high,
            report.counts.medium,
            report.counts.info,
            report.ignored.total
        );
        let mut findings: Vec<_> = report.findings.iter().collect();
        findings.sort_by_key(|f| match f.severity {
            Severity::High => 0,
            Severity::Medium => 1,
            Severity::Info => 2,
        });
        let top: Vec<String> = findings
            .iter()
            .take(5)
            .map(|f| summary_finding_line(f))
            .collect();
        let id = info.id.clone();

        v_flex()
            .w_full()
            .gap_2()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .child(
                div()
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child("LAST REPORT"),
            )
            .child(
                div()
                    .text_size(px(16.))
                    .font_weight(FontWeight::MEDIUM)
                    .child(counts),
            )
            .children(top.into_iter().map(|line| {
                div()
                    .text_size(px(13.))
                    .font_family("Consolas")
                    .child(line)
            }))
            .children(report.findings.is_empty().then(|| {
                div()
                    .text_size(px(13.))
                    .text_color(theme.muted_foreground)
                    .child("Nothing surprising showed up, with the usual noise filtered out.")
            }))
            .child(
                Button::new("open-last")
                    .label("Open report")
                    .on_click(cx.listener(move |this, _, _, cx| this.open_report(&id, cx))),
            )
            .into_any_element()
    }

    fn sessions_block(&self, cx: &mut Context<Self>, theme: &Theme) -> impl IntoElement {
        v_flex()
            .w_full()
            .flex_1()
            .gap_2()
            .child(
                div()
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child("PAST SESSIONS"),
            )
            .children(self.sessions.is_empty().then(|| {
                div()
                    .text_size(px(13.))
                    .text_color(theme.muted_foreground)
                    .child("No sessions yet.")
            }))
            .children(self.sessions.iter().take(8).map(|s| {
                let id = s.id.clone();
                let can_open = s.has_html;
                let repo = s
                    .repo_path
                    .as_deref()
                    .unwrap_or("(no folder recorded)")
                    .to_string();
                let counts = s.counts.as_ref().map(|c| {
                    format!("{} high, {} medium, {} info", c.high, c.medium, c.info)
                });
                let label = match s.status {
                    SessionStatus::Running => format!("{}  ·  watching  ·  {repo}", s.id),
                    SessionStatus::Partial => format!("{}  ·  incomplete  ·  {repo}", s.id),
                    SessionStatus::Completed => {
                        let extra = counts.unwrap_or_default();
                        format!("{}  ·  {extra}  ·  {repo}", s.id)
                    }
                };
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(12.))
                            .font_family("Consolas")
                            .child(label),
                    )
                    .child(
                        Button::new(SharedString::from(format!("open-{id}")))
                            .ghost()
                            .label("Open")
                            .disabled(!can_open)
                            .on_click(cx.listener(move |this, _, _, cx| this.open_report(&id, cx))),
                    )
            }))
    }

    fn footer(&self, cx: &mut Context<Self>, theme: &Theme) -> impl IntoElement {
        h_flex()
            .w_full()
            .justify_between()
            .items_center()
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme.muted_foreground)
                    .child(if self.start_menu_ok {
                        "leash is in the Start menu."
                    } else {
                        "Optional: add a Start-menu shortcut so you do not have to find the exe."
                    }),
            )
            .children((!self.start_menu_ok).then(|| {
                Button::new("start-menu")
                    .ghost()
                    .label("Add to Start menu")
                    .on_click(cx.listener(|this, _, _, cx| this.pin_start_menu(cx)))
            }))
    }
}
