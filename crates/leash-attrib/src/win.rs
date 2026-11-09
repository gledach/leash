//! Windows ETW collector and process helpers.

use crate::logutil::log;
use crate::model::{
    attrib_path, read_watch, request_stop, stop_requested, write_trace, AttributionStatus,
    FileTraceEvent, RegistryTraceEvent, TraceLog, TraceOp, WatchTarget,
};
use crate::paths::image_basename;
use crate::tree::{InstantMs, ProcessRecord, ProcessTree};
use crate::process_entry_to_record;
use anyhow::{Context, Result};
use ferrisetw::parser::{Parser, Pointer};
use ferrisetw::provider::Provider;
use ferrisetw::schema_locator::SchemaLocator;
use ferrisetw::trace::{stop_trace_by_name, UserTrace};
use ferrisetw::EventRecord;
use leash_snapshot::ProcessEntry;
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows::core::{PCWSTR, HRESULT};
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, WAIT_OBJECT_0, WIN32_ERROR};
use windows::Win32::System::Threading::{CreateEventW, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE};

const MAX_AGENT_EVENTS: usize = 80_000;
const MAX_OTHER_EVENTS: usize = 20_000;
const MAX_FILE_OBJECTS: usize = 40_000;

const KERNEL_PROCESS: &str = "22fb2cd6-0e7b-422b-a0c7-2fad1fd0e716";
const KERNEL_FILE: &str = "edd08927-9cc4-4e65-b970-c2560fb5c289";
const KERNEL_REGISTRY: &str = "70eb4f03-c1de-4f73-a051-33d13d5413bd";

/// Filename | Create | Write | DeletePath | RenameSetLinkPath
const FILE_KEYWORDS: u64 = 0xE90;
/// Process start/stop
const PROCESS_KEYWORDS: u64 = 0x10;
/// CreateKey | DeleteKey | SetValue | DeleteValue
const REGISTRY_KEYWORDS: u64 = 0x35;

pub fn list_processes() -> Vec<ProcessEntry> {
    leash_persist::list_processes()
}

pub fn now_ms() -> InstantMs {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn filetime_to_unix_ms(ft: i64) -> InstantMs {
    if ft <= 0 {
        return now_ms();
    }
    // FILETIME is 100-ns ticks since 1601-01-01.
    let unix_ms = (ft / 10_000) - 11_644_473_600_000;
    if unix_ms > 0 {
        unix_ms as u64
    } else {
        now_ms()
    }
}

pub fn event_name(session_id: &str) -> String {
    let safe: String = session_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
        .collect();
    format!("Local\\leash-stop-{safe}")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn signal_stop_event(session_dir: &Path) {
    let Some(id) = session_dir.file_name().and_then(|s| s.to_str()) else {
        return;
    };
    let name = event_name(id);
    let w = wide(&name);
    unsafe {
        if let Ok(handle) = OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(w.as_ptr())) {
            let _ = SetEvent(handle);
            let _ = CloseHandle(handle);
        }
    }
}

fn create_stop_event(session_id: &str) -> Option<HANDLE> {
    let name = event_name(session_id);
    let w = wide(&name);
    unsafe { CreateEventW(None, true, false, PCWSTR(w.as_ptr())).ok() }
}

struct CollectorState {
    tree: ProcessTree,
    file_events: Vec<FileTraceEvent>,
    other_files: VecDeque<FileTraceEvent>,
    registry_events: Vec<RegistryTraceEvent>,
    other_registry: VecDeque<RegistryTraceEvent>,
    file_objects: HashMap<u64, String>,
    dropped_other: usize,
    warnings: Vec<String>,
}

impl CollectorState {
    fn new() -> Self {
        Self {
            tree: ProcessTree::new(),
            file_events: Vec::new(),
            other_files: VecDeque::new(),
            registry_events: Vec::new(),
            other_registry: VecDeque::new(),
            file_objects: HashMap::new(),
            dropped_other: 0,
            warnings: Vec::new(),
        }
    }

    fn remember_file_object(&mut self, fo: u64, path: String) {
        if fo == 0 || path.is_empty() {
            return;
        }
        if self.file_objects.len() >= MAX_FILE_OBJECTS {
            if let Some(key) = self.file_objects.keys().next().copied() {
                self.file_objects.remove(&key);
            }
        }
        self.file_objects.insert(fo, path);
    }

    fn push_file(&mut self, ev: FileTraceEvent) {
        let at = ev.at_ms;
        if self.tree.is_agent_at(ev.pid, at) {
            if self.file_events.len() < MAX_AGENT_EVENTS {
                self.file_events.push(ev);
            }
        } else if self.other_files.len() < MAX_OTHER_EVENTS {
            self.other_files.push_back(ev);
        } else {
            self.other_files.pop_front();
            self.other_files.push_back(ev);
            self.dropped_other += 1;
        }
    }

    fn push_registry(&mut self, ev: RegistryTraceEvent) {
        let at = ev.at_ms;
        if self.tree.is_agent_at(ev.pid, at) {
            if self.registry_events.len() < MAX_AGENT_EVENTS {
                self.registry_events.push(ev);
            }
        } else if self.other_registry.len() < MAX_OTHER_EVENTS {
            self.other_registry.push_back(ev);
        } else {
            self.other_registry.pop_front();
            self.other_registry.push_back(ev);
            self.dropped_other += 1;
        }
    }

    fn into_log(self, status: AttributionStatus, target: Option<String>, error: Option<String>) -> TraceLog {
        let mut file_events = self.file_events;
        file_events.extend(self.other_files);
        let mut registry_events = self.registry_events;
        registry_events.extend(self.other_registry);
        TraceLog {
            status,
            target,
            tree: self.tree,
            file_events,
            registry_events,
            warnings: self.warnings,
            dropped_other_events: self.dropped_other,
            error,
        }
    }
}

fn parse_u32(parser: &Parser, names: &[&str]) -> Option<u32> {
    for name in names {
        if let Ok(v) = parser.try_parse::<u32>(name) {
            return Some(v);
        }
        if let Ok(v) = parser.try_parse::<u64>(name) {
            return Some(v as u32);
        }
    }
    None
}

fn parse_string(parser: &Parser, names: &[&str]) -> Option<String> {
    for name in names {
        if let Ok(v) = parser.try_parse::<String>(name) {
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

fn parse_pointer(parser: &Parser, names: &[&str]) -> Option<u64> {
    for name in names {
        if let Ok(v) = parser.try_parse::<u64>(name) {
            return Some(v);
        }
        if let Ok(v) = parser.try_parse::<Pointer>(name) {
            return Some(*v as u64);
        }
    }
    None
}

fn on_process_event(record: &EventRecord, locator: &SchemaLocator, state: &Arc<Mutex<CollectorState>>) {
    let schema = match locator.event_schema(record) {
        Ok(s) => s,
        Err(_) => return,
    };
    let parser = Parser::create(record, &schema);
    let at = filetime_to_unix_ms(record.raw_timestamp());
    let event_id = record.event_id();
    let pid = parse_u32(&parser, &["ProcessID", "ProcessId", "PID"]).unwrap_or_else(|| record.process_id());
    match event_id {
        1 => {
            let rec = ProcessRecord {
                pid,
                parent_pid: parse_u32(&parser, &["ParentProcessID", "ParentProcessId", "ParentId"]),
                start_ms: parse_u32(&parser, &[]).and(None).unwrap_or(at),
                end_ms: None,
                image: parse_string(&parser, &["ImageName", "ImageFileName"])
                    .map(|s| image_basename(&s))
                    .unwrap_or_default(),
                exe: parse_string(&parser, &["ImageName", "ImageFileName"]),
                command_line: parse_string(&parser, &["CommandLine", "Commandline"]),
                in_agent_tree: false,
            };
            if let Ok(mut g) = state.lock() {
                g.tree.on_start(rec);
            }
        }
        2 => {
            if let Ok(mut g) = state.lock() {
                g.tree.on_exit(pid, at);
            }
        }
        _ => {}
    }
}

// The file event handler above got messy with moves. A clean version follows
// in `handle_file`, used by the real callback.

fn handle_file(record: &EventRecord, locator: &SchemaLocator, state: &Arc<Mutex<CollectorState>>) {
    let event_id = record.event_id();
    if matches!(event_id, 14 | 24 | 25) {
        return;
    }
    let Ok(schema) = locator.event_schema(record) else {
        return;
    };
    let parser = Parser::create(record, &schema);
    let at = filetime_to_unix_ms(record.raw_timestamp());
    let pid = record.process_id();
    let fo = parse_pointer(&parser, &["FileObject", "FileKey"]);
    let named = parse_string(&parser, &["FileName", "FilePath", "Name", "OpenPath"]);
    let old = parse_string(&parser, &["OldName", "OldPath", "InputPath"]);
    let newp = parse_string(&parser, &["NewName", "NewPath"]);

    let Ok(mut g) = state.lock() else {
        return;
    };
    if let (Some(fo), Some(p)) = (fo, named.as_ref()) {
        if matches!(event_id, 10 | 12 | 27) {
            g.remember_file_object(fo, p.clone());
        }
    }
    if matches!(event_id, 17 | 18) {
        if let Some(fo) = fo {
            g.file_objects.remove(&fo);
        }
        return;
    }
    let path = named
        .or_else(|| fo.and_then(|fo| g.file_objects.get(&fo).cloned()));
    let Some(path) = path else {
        return;
    };
    let op = match event_id {
        12 | 10 => TraceOp::Create,
        16 | 15 => TraceOp::Write,
        26 | 11 => TraceOp::Delete,
        27 => TraceOp::Rename,
        30 | 32 => TraceOp::SetInfo,
        _ => return,
    };
    let (path, old_path) = if op == TraceOp::Rename {
        (newp.unwrap_or(path.clone()), old.or(Some(path)))
    } else {
        (path, old)
    };
    g.push_file(FileTraceEvent {
        path,
        old_path,
        op,
        pid,
        at_ms: at,
    });
}

fn handle_registry(record: &EventRecord, locator: &SchemaLocator, state: &Arc<Mutex<CollectorState>>) {
    let event_id = record.event_id();
    // Keep create/delete key and set/delete value. Drop open/query/enum.
    let op = match event_id {
        1 => TraceOp::CreateKey,
        3 => TraceOp::DeleteKey,
        5 => TraceOp::SetValue,
        6 => TraceOp::DeleteValue,
        _ => return,
    };
    let Ok(schema) = locator.event_schema(record) else {
        return;
    };
    let parser = Parser::create(record, &schema);
    let at = filetime_to_unix_ms(record.raw_timestamp());
    let pid = record.process_id();
    let Some(key) = parse_string(&parser, &["KeyName", "KeyPath", "Name", "RelativeName", "Path"])
    else {
        return;
    };
    let value = parse_string(&parser, &["ValueName", "Value"]);
    if let Ok(mut g) = state.lock() {
        g.push_registry(RegistryTraceEvent {
            key,
            value,
            op,
            pid,
            at_ms: at,
        });
    }
}

fn seed_tree(tree: &mut ProcessTree, target: &WatchTarget, at: InstantMs) {
    let snapshot: Vec<ProcessRecord> = list_processes()
        .iter()
        .map(process_entry_to_record)
        .collect();
    match target {
        WatchTarget::Command { pid: Some(pid), argv, .. } => {
            let mut rec = snapshot
                .iter()
                .find(|p| p.pid == *pid)
                .cloned()
                .unwrap_or_else(|| ProcessRecord {
                    pid: *pid,
                    parent_pid: None,
                    start_ms: at,
                    end_ms: None,
                    image: argv
                        .first()
                        .map(|s| image_basename(s))
                        .unwrap_or_else(|| format!("pid {pid}")),
                    exe: argv.first().cloned(),
                    command_line: Some(argv.join(" ")),
                    in_agent_tree: true,
                });
            rec.in_agent_tree = true;
            tree.add_root(rec);
            tree.ingest_snapshot(&snapshot, at);
        }
        WatchTarget::Command { pid: None, .. } => {
            tree.ingest_snapshot(&snapshot, at);
        }
        WatchTarget::Pid { pid, name } => {
            let mut rec = snapshot
                .iter()
                .find(|p| p.pid == *pid)
                .cloned()
                .unwrap_or_else(|| ProcessRecord {
                    pid: *pid,
                    parent_pid: None,
                    start_ms: at,
                    end_ms: None,
                    image: name.clone().unwrap_or_else(|| format!("pid {pid}")),
                    exe: None,
                    command_line: None,
                    in_agent_tree: true,
                });
            rec.in_agent_tree = true;
            tree.add_root(rec);
            tree.ingest_snapshot(&snapshot, at);
        }
        WatchTarget::Names { names, .. } => {
            for name in names {
                tree.add_roots_named(name, &snapshot, at);
            }
            tree.ingest_snapshot(&snapshot, at);
        }
    }
}

fn start_etw(
    session_id: &str,
    state: Arc<Mutex<CollectorState>>,
) -> Result<UserTrace, String> {
    let name = format!("leash-attrib-{session_id}");
    let _ = stop_trace_by_name(&name);

    let proc_state = Arc::clone(&state);
    let process = Provider::by_guid(KERNEL_PROCESS)
        .any(PROCESS_KEYWORDS)
        .level(5)
        .add_callback(move |record: &EventRecord, loc: &SchemaLocator| {
            on_process_event(record, loc, &proc_state);
        })
        .build();

    let file_state = Arc::clone(&state);
    let file = Provider::by_guid(KERNEL_FILE)
        .any(FILE_KEYWORDS)
        .level(5)
        .add_callback(move |record: &EventRecord, loc: &SchemaLocator| {
            handle_file(record, loc, &file_state);
        })
        .build();

    let reg_state = Arc::clone(&state);
    let registry = Provider::by_guid(KERNEL_REGISTRY)
        .any(REGISTRY_KEYWORDS)
        .level(5)
        .add_callback(move |record: &EventRecord, loc: &SchemaLocator| {
            handle_registry(record, loc, &reg_state);
        })
        .build();

    UserTrace::new()
        .named(name)
        .enable(process)
        .enable(file)
        .enable(registry)
        .start_and_process()
        .map_err(|e| format!("{e:?}"))
}

/// Run the collector until a stop is requested, then write `attrib.json`.
pub fn run_collector(session_dir: &Path, target: &WatchTarget, elevated: bool) -> Result<TraceLog> {
    let session_id = session_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("session")
        .to_string();
    crate::model::clear_stop(session_dir);

    let state = Arc::new(Mutex::new(CollectorState::new()));
    {
        let mut g = state.lock().expect("collector lock");
        seed_tree(&mut g.tree, target, now_ms());
    }

    if !elevated {
        let mut log_out = match state.lock() {
            Ok(g) => CollectorState {
                tree: g.tree.clone(),
                warnings: g.warnings.clone(),
                ..CollectorState::new()
            },
            Err(_) => CollectorState::new(),
        }
        .into_log(
            AttributionStatus::Unavailable,
            Some(target.label()),
            Some("not running as administrator".into()),
        );
        log_out.warnings.push(
            "Attribution is off because this process is not running as administrator. The report is system-wide."
                .into(),
        );
        let _ = write_trace(session_dir, &log_out);
        return Ok(log_out);
    }

    let mut etw_error = None;
    let mut status = AttributionStatus::Active;
    let trace = match start_etw(&session_id, Arc::clone(&state)) {
        Ok(t) => Some(t),
        Err(err) => {
            log(format!("ETW start failed: {err}"));
            etw_error = Some(err);
            status = AttributionStatus::Partial;
            None
        }
    };

    let stop_handle = create_stop_event(&session_id);
    let started = Instant::now();
    loop {
        if stop_requested(session_dir) {
            break;
        }
        if let Some(h) = stop_handle {
            let wait = unsafe { WaitForSingleObject(h, 200) };
            if wait == WAIT_OBJECT_0 {
                break;
            }
        } else {
            std::thread::sleep(Duration::from_millis(200));
        }
        if started.elapsed() > Duration::from_millis(0)
            && started.elapsed().as_millis() % 2000 < 250
        {
            let snapshot: Vec<ProcessRecord> = list_processes()
                .iter()
                .map(process_entry_to_record)
                .collect();
            if let Ok(mut g) = state.lock() {
                g.tree.ingest_snapshot(&snapshot, now_ms());
            }
        }
        // Safety cap: a forgotten watcher should not run forever unattended
        // beyond a long session; 24 hours is plenty for a coding session.
        if started.elapsed() > Duration::from_secs(24 * 3600) {
            if let Ok(mut g) = state.lock() {
                g.warnings
                    .push("Attribution watcher stopped after 24 hours.".into());
            }
            break;
        }
    }
    if let Some(trace) = trace {
        let _ = trace.stop();
    }
    if let Some(h) = stop_handle {
        unsafe {
            let _ = CloseHandle(h);
        }
    }
    let mut collected = match Arc::try_unwrap(state) {
        Ok(m) => m.into_inner().unwrap_or_else(|e| e.into_inner()),
        Err(arc) => match arc.lock() {
            Ok(g) => CollectorState {
                tree: g.tree.clone(),
                file_events: g.file_events.clone(),
                other_files: g.other_files.clone(),
                registry_events: g.registry_events.clone(),
                other_registry: g.other_registry.clone(),
                file_objects: HashMap::new(),
                dropped_other: g.dropped_other,
                warnings: g.warnings.clone(),
            },
            Err(_) => CollectorState::new(),
        },
    };
    if status == AttributionStatus::Partial {
        collected
            .warnings
            .push("File and registry ETW could not be started; only the process tree was recorded. Findings stay unattributed unless a leftover process or port names a pid.".into());
    } else if collected.file_events.is_empty() && collected.other_files.is_empty() {
        // Still Active: ETW started, just nothing happened or events used an unexpected layout.
        collected.warnings.push(
            "No file ETW events were recorded. Attribution may be incomplete (a known gap if the kernel provider is filtered by policy)."
                .into(),
        );
    }
    let log_out = collected.into_log(status, Some(target.label()), etw_error);
    write_trace(session_dir, &log_out)?;
    Ok(log_out)
}

/// Watcher process entry: read watch.json, collect, exit.
pub fn watch_main(session_dir: &Path, elevated: bool) -> Result<()> {
    let watch = read_watch(session_dir)?.context("watch.json missing; start the session first")?;
    log(format!(
        "watch start session={} target={} elevated={elevated}",
        watch.session_id,
        watch.target.label()
    ));
    let _ = run_collector(session_dir, &watch.target, elevated)?;
    log("watch stop, attrib.json written");
    Ok(())
}

pub fn spawn_watcher(exe: &Path, data_dir: &Path, session_id: &str) -> Result<u32> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    let child = std::process::Command::new(exe)
        .args([
            "__watch",
            "--session",
            session_id,
            "--data-dir",
            &data_dir.to_string_lossy(),
        ])
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("spawning the attribution watcher")?;
    Ok(child.id())
}

pub fn spawn_command(argv: &[String], cwd: &Path) -> Result<std::process::Child> {
    anyhow::ensure!(!argv.is_empty(), "no command given");
    let mut cmd = std::process::Command::new(&argv[0]);
    if argv.len() > 1 {
        cmd.args(&argv[1..]);
    }
    cmd.current_dir(cwd)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .with_context(|| format!("starting {}", argv[0]))
}

pub fn wait_for_watcher(session_dir: &Path, timeout: Duration) -> Result<Option<TraceLog>> {
    let deadline = Instant::now() + timeout;
    // Ask it to stop first.
    let _ = request_stop(session_dir);
    while Instant::now() < deadline {
        if attrib_path(session_dir).exists() {
            // Give it a moment to finish writing.
            std::thread::sleep(Duration::from_millis(50));
            return crate::model::read_trace(session_dir);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    crate::model::read_trace(session_dir)
}

// Silence unused windows imports we keep around for GetLastError diagnostics.
#[allow(dead_code)]
fn last_error() -> WIN32_ERROR {
    unsafe { GetLastError() }
}

#[allow(dead_code)]
fn unused_hresult(_h: HRESULT) {}
