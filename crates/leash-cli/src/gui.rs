//! Double-click window: Start / Stop, session resume, and report list.

#![cfg(windows)]

use crate::examples::{self, EXAMPLES_URL};
use crate::ops::{
    coverage_sentence, is_elevated, load_engine, load_prefs, resolve_attach_pid, start_session,
    start_session_watching, stop_session,
};
use crate::app_mode::{elevation_user_message, ElevationResult};
use crate::debug_log;
use crate::win_shell::{
    add_start_menu_shortcut_async, open_path_async, open_url_async, pick_folder_async,
    relaunch_elevated, start_menu_shortcut_path,
};
use chrono::Utc;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::searchable_list::{SearchableGroup, SearchableListItem, SearchableVec};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::IndexPath;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable as _, Theme, ThemeMode};
use gpui_kit::*;
use leash_attrib::{build_picker, PickerModel, PickerRow, WatchSelection, WatchTarget};
use leash_snapshot::{
    aggregate_finding_lines, format_elapsed_since, DiffReport, SessionInfo, SessionStatus,
    SessionStore, UiPrefs, UiTheme, SYSTEM_WIDE_CAVEAT,
};
use std::path::PathBuf;
use std::time::Duration;

pub fn run() -> anyhow::Result<()> {
    let data_dir = leash_snapshot::default_data_dir();
    debug_log::init(&data_dir);
    debug_log::log(format!(
        "gui start version={} exe={:?}",
        env!("CARGO_PKG_VERSION"),
        std::env::current_exe()
    ));
    let mut app = gpui_kit::application();
    app = app.with_assets(gpui_kit::assets::Assets);
    app.run(move |cx| {
        gpui_kit::init(cx);
        let bounds = Bounds::centered(None, size(px(760.), px(820.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("leash".into()),
                ..TitlebarOptions::default()
            }),
            ..WindowOptions::default()
        };
        if let Err(err) = gpui_kit::open_window(options, cx, |_, cx| cx.new(LeashApp::new)) {
            debug_log::log(format!("failed to open the leash window: {err:?}"));
        }
    });
    Ok(())
}

fn window_hwnd(window: &Window) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window).ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

#[derive(Clone)]
struct GuiPickerItem(PickerRow);

impl SearchableListItem for GuiPickerItem {
    type Value = WatchSelection;

    fn title(&self) -> SharedString {
        SharedString::from(self.0.title.clone())
    }

    fn value(&self) -> &Self::Value {
        &self.0.selection
    }

    fn matches(&self, query: &str) -> bool {
        self.0.matches(query)
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let indent = if self.0.depth > 0 { px(14.) } else { px(0.) };
        v_flex()
            .pl(indent)
            .gap_0()
            .child(div().text_size(px(12.)).child(self.0.title.clone()))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(cx.theme().muted_foreground)
                    .child(self.0.subtitle.clone()),
            )
    }
}

type PickerState = SelectState<SearchableVec<SearchableGroup<GuiPickerItem>>>;

fn picker_groups(model: &PickerModel) -> SearchableVec<SearchableGroup<GuiPickerItem>> {
    let mut groups = Vec::new();
    groups.push(
        SearchableGroup::new("Session").item(GuiPickerItem(model.whole_pc.clone())),
    );
    if !model.agents_and_editors.is_empty() {
        groups.push(
            SearchableGroup::new("Agents & editors")
                .items(model.agents_and_editors.iter().cloned().map(GuiPickerItem)),
        );
    }
    if !model.other_apps.is_empty() {
        groups.push(
            SearchableGroup::new("Other apps")
                .items(model.other_apps.iter().cloned().map(GuiPickerItem)),
        );
    }
    SearchableVec::new(groups)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn user_home() -> Option<String> {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()
}

fn current_picker_model() -> PickerModel {
    let views = leash_attrib::live_views();
    build_picker(&views, now_unix(), user_home().as_deref())
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
    picker_model: PickerModel,
    watch_sel: WatchSelection,
    picker: Option<Entity<PickerState>>,
    _picker_subs: Vec<Subscription>,
    picker_stale: bool,
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
            picker_model: PickerModel {
                whole_pc: leash_attrib::build_picker(&[], 0, None).whole_pc,
                agents_and_editors: Vec::new(),
                other_apps: Vec::new(),
            },
            watch_sel: WatchSelection::WholePc,
            picker: None,
            _picker_subs: Vec::new(),
            picker_stale: true,
        };
        app.reload_from_disk();
        app.tick_elapsed(cx);
        app
    }

    fn reload_from_disk(&mut self) {
        self.sessions = self.store.list_sessions();
        if let Some(running) = self.store.running_session() {
            if let Some(repo) = &running.repo_path {
                self.repo = PathBuf::from(leash_filter::pretty_path(repo));
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
                        self.repo = PathBuf::from(leash_filter::pretty_path(repo));
                    }
                }
            }
        }
        if self.repo.as_os_str().is_empty() {
            if let Some(p) = &self.prefs.last_repo_path {
                self.repo = PathBuf::from(leash_filter::pretty_path(p));
            } else if let Ok(cwd) = std::env::current_dir() {
                self.repo = leash_filter::pretty_pathbuf(&cwd);
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

    /// Wait for a worker-thread Win32 call (ShellExecute, folder dialog, UAC)
    /// and apply the result on the GPUI thread once the original listener has
    /// released its App borrow. Never call those APIs directly from a click
    /// handler: they pump a nested message loop and re-enter GPUI (see
    /// `win_shell` module docs).
    fn after_shell_work<T, F>(&mut self, cx: &mut Context<Self>, rx: std::sync::mpsc::Receiver<T>, apply: F)
    where
        T: Send + 'static,
        F: FnOnce(&mut Self, T) + Send + 'static,
    {
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { rx.recv() })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(value) = result {
                    apply(this, value);
                } else {
                    this.note = Some("The Windows action did not return a result.".into());
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn browse(&mut self, cx: &mut Context<Self>, window: &Window) {
        let owner = window_hwnd(window);
        debug_log::log(format!("browse clicked (hwnd={owner:?})"));
        let rx = pick_folder_async(owner);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { rx.recv() })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(Some(path))) => {
                        this.repo = leash_filter::pretty_pathbuf(&path);
                        this.save_prefs();
                        this.note = None;
                    }
                    Ok(Ok(None)) => {
                        // Cancel or empty choice: leave the current folder alone.
                    }
                    Ok(Err(err)) => {
                        this.note = Some(format!("Could not open the folder picker: {err}"));
                    }
                    Err(_) => {
                        this.note = Some("Could not open the folder picker.".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
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
        debug_log::log(format!("start snapshot repo={}", self.repo.display()));
        let summary = self
            .picker_model
            .row_by_selection(&self.watch_sel)
            .map(|r| r.summary.clone())
            .unwrap_or_else(|| "Whole PC (no attribution)".into());
        self.note = Some(format!("Taking the before snapshot… {summary}"));
        let repo = self.repo.clone();
        let data_dir = self.data_dir.clone();
        let selection = self.watch_sel.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let store = SessionStore::new(&data_dir);
                    match selection {
                        WatchSelection::WholePc => start_session(&store, &repo, &data_dir),
                        WatchSelection::Roots { pids, label } => {
                            let target = if pids.len() == 1 {
                                resolve_attach_pid(pids[0])?
                            } else {
                                WatchTarget::Names {
                                    names: vec![label],
                                    pids,
                                }
                            };
                            start_session_watching(&store, &repo, &data_dir, target)
                        }
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(started) => {
                        this.note = if matches!(this.watch_sel, WatchSelection::WholePc) {
                            None
                        } else if started.snap.elevated {
                            this.picker_model
                                .row_by_selection(&this.watch_sel)
                                .map(|r| format!("Watching {}. Press Stop when you are done.", r.summary))
                        } else {
                            Some("Attribution is off because this window is not running as administrator. The report will be system-wide.".into())
                        };
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
        debug_log::log("stop snapshot");
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
        match self.store.open(Some(id)) {
            Ok(paths) => {
                let path = paths.report_html();
                debug_log::log(format!(
                    "open_path: dispatched to worker thread ({})",
                    path.display()
                ));
                let rx = open_path_async(path);
                self.after_shell_work(cx, rx, |this, result| match result {
                    Ok(()) => {}
                    Err(err) => this.note = Some(err.to_string()),
                });
            }
            Err(err) => {
                self.note = Some(err.to_string());
                cx.notify();
            }
        }
    }

    fn elevate(&mut self, cx: &mut Context<Self>, window: &Window) {
        // ShellExecuteExW("runas") shows a UAC prompt, which pumps a nested
        // Win32 message loop. If we call it from this click handler, GPUI still
        // holds the App RefCell (gpui-pre 0.3.12 window.rs:2245 dispatch_event
        // → async_context.rs:66 update_entity borrow_mut). A WM_PAINT / input
        // during UAC then panics: "RefCell already borrowed".
        // Run ShellExecuteExW on a worker thread and only quit after the
        // result is posted back, via cx.defer, once the click handler has
        // released the borrow.
        self.save_prefs();
        let owner = window_hwnd(window);
        debug_log::log(format!("restart-as-admin clicked (hwnd={owner:?})"));
        self.note = Some("Asking Windows for administrator permission…".into());
        cx.notify();
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("leash-elevate".into())
            .spawn(move || {
                let result = relaunch_elevated(owner);
                let _ = tx.send(result);
            });
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { rx.recv() })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(ElevationResult::Started) => {
                        this.note = Some(elevation_user_message(&ElevationResult::Started));
                        cx.notify();
                        cx.defer(|cx| cx.quit());
                    }
                    Ok(other) => {
                        this.note = Some(elevation_user_message(&other));
                        cx.notify();
                    }
                    Err(_) => {
                        this.note = Some("Windows refused to elevate: the request was interrupted.".into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn pin_start_menu(&mut self, cx: &mut Context<Self>) {
        let rx = add_start_menu_shortcut_async();
        self.after_shell_work(cx, rx, |this, result| match result {
            Ok(path) => {
                this.start_menu_ok = true;
                this.note = Some(format!("Added to the Start menu: {}", path.display()));
            }
            Err(err) => this.note = Some(format!("Could not create the shortcut: {err}")),
        });
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

    fn open_examples(&mut self, cx: &mut Context<Self>) {
        debug_log::log("open_url: dispatched to worker thread");
        let rx = open_url_async(EXAMPLES_URL.to_string());
        self.after_shell_work(cx, rx, |this, result| match result {
            Ok(()) => this.note = Some(format!("Opened {EXAMPLES_URL}")),
            Err(err) => this.note = Some(err.to_string()),
        });
    }
}

impl Render for LeashApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_picker(window, cx);
        let theme = cx.theme().clone();
        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .p_5()
            .gap_4()
            .child(self.header(cx, &theme))
            .child(self.folder_row(cx, &theme))
            .child(self.watch_row(window, cx, &theme))
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
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme.muted_foreground)
                            .child(SYSTEM_WIDE_CAVEAT),
                    ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("help-examples")
                            .ghost()
                            .label("Examples")
                            .on_click(cx.listener(|this, _, _, cx| this.open_examples(cx))),
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
                    ),
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
                            .on_click(cx.listener(|this, _, window, cx| this.browse(cx, window))),
                    ),
            )
    }


    fn refresh_candidates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.snapshot_picker(window, cx);
        cx.notify();
    }

    fn snapshot_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        debug_log::log("watch picker: taking a process snapshot");
        self.picker_model = current_picker_model();
        self.picker_stale = false;
        if let Some(state) = self.picker.clone() {
            let groups = picker_groups(&self.picker_model);
            let keep = self.watch_sel.clone();
            state.update(cx, |state, cx| {
                state.set_items(groups, window, cx);
                state.set_selected_value(&keep, window, cx);
            });
        }
    }

    fn ensure_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.is_some() {
            return;
        }
        if self.picker_stale {
            self.picker_model = current_picker_model();
            self.picker_stale = false;
        }
        let groups = picker_groups(&self.picker_model);
        let initial = Some(IndexPath::default().section(0).row(0));
        let state = cx.new(|cx| SelectState::new(groups, initial, window, cx).searchable(true));
        let confirm = cx.subscribe(&state, |this, _, ev: &SelectEvent<SearchableVec<SearchableGroup<GuiPickerItem>>>, cx| {
            if let SelectEvent::Confirm(Some(value)) = ev {
                this.watch_sel = value.clone();
                cx.notify();
            }
        });
        let dismiss = cx.subscribe(&state, |this, _, _: &DismissEvent, _cx| {
            this.picker_stale = true;
        });
        self._picker_subs = vec![confirm, dismiss];
        self.picker = Some(state);
        self.watch_sel = WatchSelection::WholePc;
    }

    fn watch_row(&mut self, _window: &mut Window, cx: &mut Context<Self>, theme: &Theme) -> impl IntoElement {
        let busy = matches!(self.phase, Phase::Starting | Phase::Running(_) | Phase::Stopping);
        let featured = examples::featured(3);
        let summary = self
            .picker_model
            .row_by_selection(&self.watch_sel)
            .map(|r| r.summary.clone())
            .unwrap_or_else(|| "Whole PC (no attribution)".into());
        let picker = self.picker.clone();
        v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child("WATCH A RUNNING APP"),
                    )
                    .child(
                        Button::new("refresh-apps")
                            .ghost()
                            .label("Refresh")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| this.refresh_candidates(window, cx))),
                    ),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme.muted_foreground)
                    .child("Most reliable: start your agent through leash, e.g. leash run -- claude"),
            )
            .child(
                div()
                    .w_full()
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| {
                        if this.picker_stale {
                            this.snapshot_picker(window, cx);
                        }
                    }))
                    .child(if let Some(state) = picker {
                        Select::new(&state)
                            .placeholder("Pick an app to watch")
                            .search_placeholder("Filter by name, folder, pid or command line")
                            .menu_max_h(px(280.))
                            .menu_width(px(560.))
                            .disabled(busy)
                            .into_any_element()
                    } else {
                        div().child("Pick an app to watch").into_any_element()
                    }),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme.muted_foreground)
                    .child(summary),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme.muted_foreground)
                    .child("Attaching is the fallback when an agent is already running in an editor. Attribution needs administrator. Whole PC stays system-wide."),
            )
            .children(featured.into_iter().map(|r| {
                div()
                    .text_size(px(11.))
                    .font_family("Consolas")
                    .text_color(theme.muted_foreground)
                    .child(format!("{}  —  {}", r.code, r.title))
            }))
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
            Phase::Idle => lines.push(("Ready. Choose a project folder and press Start before you run the agent, or watch a running app below. Start alone is system-wide.".into(), false)),
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
                            .label("Restart as admin for full coverage and attribution")
                            .on_click(cx.listener(|this, _, window, cx| this.elevate(cx, window))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(12.))
                            .text_color(theme.muted_foreground)
                            .child("Administrator is needed for the disk change log and for per-process attribution."),
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
        let top = aggregate_finding_lines(&report.findings, 5);
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
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme.muted_foreground)
                    .child(if report.attribution.caveat.is_empty() {
                        SYSTEM_WIDE_CAVEAT.to_string()
                    } else {
                        report.attribution.caveat.clone()
                    }),
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
