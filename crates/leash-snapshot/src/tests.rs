use crate::diff::{diff_snapshots, DiffOptions};
use crate::model::*;
use crate::report::{render_markdown, render_text};
use crate::SNAPSHOT_VERSION;
use leash_filter::{context_from_env, ChangeKind, FilterEngine, Severity};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn engine() -> FilterEngine {
    FilterEngine::with_defaults(context_from_env(
        Some(Path::new(r"C:\Users\Alek\src\myapp")),
        Some(Path::new(r"C:\Users\Alek\AppData\Local\leash")),
        Some(Path::new(r"C:\Users\Alek")),
        Some(Path::new(r"C:\Users\Alek\AppData\Local")),
        Some(Path::new(r"C:\Users\Alek\AppData\Roaming")),
        Some(Path::new(r"C:\Users\Alek\AppData\Local\Temp")),
        vec![PathBuf::from(r"C:\Users\Alek\src\other")],
    ))
}

fn base_snap(label: SnapshotLabel) -> Snapshot {
    let mut s = Snapshot::new(label, "s1");
    s.taken_at = match label {
        SnapshotLabel::Before => "2026-11-09T18:00:00Z".into(),
        SnapshotLabel::After => "2026-11-09T19:00:00Z".into(),
    };
    s.repo_path = Some(r"C:\Users\Alek\src\myapp".into());
    s.file_mode = FileMode::Usn;
    s.elevated = true;
    s
}

fn file_event(path: &str, kind: ChangeKind) -> FileEvent {
    FileEvent {
        volume: Some("C:".into()),
        path: Some(path.into()),
        name: path.rsplit(['\\', '/']).next().unwrap_or(path).into(),
        old_path: None,
        kind,
        reasons_raw: 0,
        usn: Some(1),
        file_id: None,
        is_dir: false,
        timestamp: None,
    }
}

fn report(before: &Snapshot, after: &Snapshot) -> DiffReport {
    let eng = engine();
    diff_snapshots(before, after, DiffOptions { engine: &eng })
}

#[test]
fn json_round_trip_preserves_version_and_fields() {
    let mut s = base_snap(SnapshotLabel::Before);
    s.run_keys.push(RunKeyEntry {
        hive: "HKCU".into(),
        key: "Run".into(),
        name: "One".into(),
        value: "a.exe".into(),
    });
    let json = serde_json::to_string(&s).unwrap();
    let back: Snapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(back.version, SNAPSHOT_VERSION);
    assert_eq!(back.run_keys[0].name, "One");
}

#[test]
fn repo_and_temp_file_events_are_filtered() {
    let before = base_snap(SnapshotLabel::Before);
    let mut after = base_snap(SnapshotLabel::After);
    after.file_events = vec![
        file_event(
            r"C:\Users\Alek\src\myapp\src\lib.rs",
            ChangeKind::DataOverwrite,
        ),
        file_event(
            r"C:\Users\Alek\AppData\Local\Temp\x.tmp",
            ChangeKind::Create,
        ),
        file_event(r"C:\Users\Alek\src\other\secret.env", ChangeKind::Create),
        file_event(r"C:\Users\Alek\Desktop\notes.txt", ChangeKind::Delete),
    ];
    // secret.env may be classified high via .env rule? name is secret.env - looks_secret because contains "secret"
    let r = report(&before, &after);
    assert!(
        r.ignored.total >= 2,
        "repo and temp should be ignored: {:?}",
        r.ignored
    );
    assert!(
        r.counts.high >= 2,
        "desktop delete and secret file should be high: {:?}",
        r.findings
    );
    assert!(r
        .findings
        .iter()
        .any(|f| f.detail.contains("notes.txt") && f.severity == Severity::High));
}

#[test]
fn env_dotfile_in_repo_is_high_not_ignored() {
    let before = base_snap(SnapshotLabel::Before);
    let mut after = base_snap(SnapshotLabel::After);
    after.file_events = vec![file_event(
        r"C:\Users\Alek\src\myapp\.env",
        ChangeKind::DataOverwrite,
    )];
    let r = report(&before, &after);
    assert_eq!(r.counts.high, 1);
    assert_eq!(r.ignored.total, 0);
}

#[test]
fn run_key_and_task_and_service_are_medium() {
    let mut before = base_snap(SnapshotLabel::Before);
    before.services.push(ServiceEntry {
        name: "OldSvc".into(),
        display_name: Some("Old".into()),
        start_type: Some("auto".into()),
        state: Some("running".into()),
        binary_path: Some(r"C:\svc\old.exe".into()),
        account: Some("LocalSystem".into()),
    });
    let mut after = base_snap(SnapshotLabel::After);
    after.run_keys.push(RunKeyEntry {
        hive: "HKCU".into(),
        key: "Run".into(),
        name: "SuperUpdater".into(),
        value: r"C:\Users\Alek\updater.exe".into(),
    });
    after.scheduled_tasks.push(ScheduledTask {
        path: r"\Odd\Agent".into(),
        name: "Agent".into(),
        enabled: Some(true),
        command: Some(r"C:\tools\agent.exe".into()),
        arguments: None,
        user: Some(r"Alek".into()),
    });
    after.services = before.services.clone();
    after.services[0].binary_path = Some(r"C:\svc\new.exe".into());
    after.services.push(ServiceEntry {
        name: "NewSvc".into(),
        display_name: Some("New".into()),
        start_type: Some("auto".into()),
        state: Some("stopped".into()),
        binary_path: Some(r"C:\svc\brand.exe".into()),
        account: Some("LocalSystem".into()),
    });
    let r = report(&before, &after);
    assert!(r
        .findings
        .iter()
        .any(|f| f.category == "registry" && f.severity == Severity::Medium));
    assert!(r
        .findings
        .iter()
        .any(|f| f.category == "task" && f.title.contains("New")));
    assert!(r
        .findings
        .iter()
        .any(|f| f.category == "service" && f.title.contains("New")));
    assert!(r
        .findings
        .iter()
        .any(|f| f.category == "service" && f.title.contains("configuration")));
}

#[test]
fn service_state_only_is_info() {
    let mut before = base_snap(SnapshotLabel::Before);
    before.services.push(ServiceEntry {
        name: "Spooler".into(),
        display_name: None,
        start_type: Some("auto".into()),
        state: Some("running".into()),
        binary_path: Some("spoolsv.exe".into()),
        account: None,
    });
    let mut after = before.clone();
    after.label = SnapshotLabel::After;
    after.services[0].state = Some("stopped".into());
    let r = report(&before, &after);
    assert_eq!(r.counts.medium, 0);
    assert_eq!(r.counts.info, 1);
}

#[test]
fn path_entries_are_split_and_diffed() {
    let mut before = base_snap(SnapshotLabel::Before);
    before.env_user.insert(
        "PATH".into(),
        r"C:\Windows\system32;C:\Windows;C:\Tools".into(),
    );
    let mut after = base_snap(SnapshotLabel::After);
    after.env_user.insert(
        "PATH".into(),
        r"C:\Windows\system32;C:\Windows;C:\Users\Alek\AppData\Roaming\npm;C:\Tools".into(),
    );
    after.env_user.insert("FOO".into(), "bar".into());
    let r = report(&before, &after);
    assert!(r
        .findings
        .iter()
        .any(|f| f.title.contains("PATH") && f.detail.contains("npm")));
    assert!(r
        .findings
        .iter()
        .any(|f| f.title.contains("environment variable") && f.detail.starts_with("FOO=")));
}

#[test]
fn split_path_var_handles_windows_and_unix() {
    assert_eq!(
        split_path_var(r"C:\Windows;C:\Tools\"),
        vec!["C:\\Windows".to_string(), "C:\\Tools".to_string()]
    );
    assert_eq!(
        split_path_var("/usr/bin:/usr/local/bin"),
        vec!["/usr/bin".to_string(), "/usr/local/bin".to_string()]
    );
}

#[test]
fn packages_ports_processes_and_ssh_hash() {
    let mut before = base_snap(SnapshotLabel::Before);
    before.packages.npm.push(Package {
        name: "old".into(),
        version: "1.0.0".into(),
    });
    before.ssh.authorized_keys = Some(HashedFile {
        path: r"C:\Users\Alek\.ssh\authorized_keys".into(),
        exists: true,
        sha256: Some("aaa".into()),
        size: Some(10),
        mtime: Some(1),
    });
    before.processes.push(ProcessEntry {
        pid: 10,
        ppid: Some(1),
        name: "explorer.exe".into(),
        exe: None,
        command_line: None,
        create_time: Some(100),
    });
    let mut after = base_snap(SnapshotLabel::After);
    after.packages.npm = vec![Package {
        name: "old".into(),
        version: "1.1.0".into(),
    }];
    after.packages.npm.push(Package {
        name: "left-pad".into(),
        version: "1.3.0".into(),
    });
    after.ssh.authorized_keys = Some(HashedFile {
        path: r"C:\Users\Alek\.ssh\authorized_keys".into(),
        exists: true,
        sha256: Some("bbb".into()),
        size: Some(12),
        mtime: Some(2),
    });
    after.ports.push(ListeningPort {
        protocol: "tcp".into(),
        local_addr: "127.0.0.1".into(),
        local_port: 8765,
        pid: 4242,
        process_name: Some("python.exe".into()),
        state: Some("LISTEN".into()),
    });
    after.processes = before.processes.clone();
    after.processes.push(ProcessEntry {
        pid: 99,
        ppid: Some(10),
        name: "odd.exe".into(),
        exe: Some(r"C:\Users\Alek\odd.exe".into()),
        command_line: Some(r"C:\Users\Alek\odd.exe --serve".into()),
        create_time: Some(200),
    });
    let r = report(&before, &after);
    assert!(r
        .findings
        .iter()
        .any(|f| f.title.contains("Updated global npm")));
    assert!(r
        .findings
        .iter()
        .any(|f| f.title.contains("New global npm") && f.detail.contains("left-pad")));
    assert!(r
        .findings
        .iter()
        .any(|f| f.category.contains("authorized") && f.severity == Severity::High));
    assert!(r
        .findings
        .iter()
        .any(|f| f.category == "port" && f.detail.contains("8765")));
    assert!(r
        .findings
        .iter()
        .any(|f| f.category == "process" && f.detail.contains("odd.exe")));
}

#[test]
fn journal_wrap_and_id_change_emit_warnings() {
    let mut before = base_snap(SnapshotLabel::Before);
    before.usn.push(UsnVolumeState {
        volume: "C:".into(),
        journal_id: 1,
        start_usn: 100,
        next_usn: 100,
        first_usn: 10,
        lowest_valid_usn: 10,
        query_ok: true,
        error: None,
    });
    let mut after = base_snap(SnapshotLabel::After);
    after.usn.push(UsnVolumeState {
        volume: "C:".into(),
        journal_id: 2,
        start_usn: 50,
        next_usn: 80,
        first_usn: 50,
        lowest_valid_usn: 50,
        query_ok: true,
        error: None,
    });
    // also a wrapped same-id case on D:
    before.usn.push(UsnVolumeState {
        volume: "D:".into(),
        journal_id: 9,
        start_usn: 10,
        next_usn: 10,
        first_usn: 1,
        lowest_valid_usn: 1,
        query_ok: true,
        error: None,
    });
    after.usn.push(UsnVolumeState {
        volume: "D:".into(),
        journal_id: 9,
        start_usn: 10,
        next_usn: 500,
        first_usn: 200,
        lowest_valid_usn: 200,
        query_ok: true,
        error: None,
    });
    let r = report(&before, &after);
    assert!(r.warnings.iter().any(|w| w.contains("recreated")));
    assert!(r.warnings.iter().any(|w| w.contains("wrapped")));
}

#[test]
fn fallback_inventory_create_delete_hash_change() {
    let mut before = base_snap(SnapshotLabel::Before);
    before.file_mode = FileMode::FallbackScan;
    before.fallback_files = vec![
        FileInventoryEntry {
            path: r"C:\Users\Alek\.bashrc".into(),
            size: 10,
            mtime: 1,
            sha256: Some("aaa".into()),
            is_dir: false,
        },
        FileInventoryEntry {
            path: r"C:\Users\Alek\Desktop\gone.txt".into(),
            size: 2,
            mtime: 1,
            sha256: Some("g".into()),
            is_dir: false,
        },
    ];
    let mut after = base_snap(SnapshotLabel::After);
    after.file_mode = FileMode::FallbackScan;
    after.fallback_files = vec![
        FileInventoryEntry {
            path: r"C:\Users\Alek\.bashrc".into(),
            size: 11,
            mtime: 2,
            sha256: Some("bbb".into()),
            is_dir: false,
        },
        FileInventoryEntry {
            path: r"C:\Users\Alek\Desktop\new.txt".into(),
            size: 3,
            mtime: 3,
            sha256: Some("n".into()),
            is_dir: false,
        },
    ];
    let r = report(&before, &after);
    assert!(r
        .findings
        .iter()
        .any(|f| f.path.as_deref().unwrap().contains(".bashrc") && f.severity == Severity::High));
    assert!(r
        .findings
        .iter()
        .any(|f| f.detail.contains("gone.txt") && f.title.contains("delete")));
    assert!(r
        .findings
        .iter()
        .any(|f| f.detail.contains("new.txt") && f.title.contains("create")));
}

#[test]
fn findings_are_sorted_high_first_and_report_mentions_them() {
    let mut before = base_snap(SnapshotLabel::Before);
    before.env_user.insert("X".into(), "1".into());
    let mut after = base_snap(SnapshotLabel::After);
    after.env_user.insert("X".into(), "2".into());
    after.file_events.push(file_event(
        r"C:\Users\Alek\Desktop\x.txt",
        ChangeKind::Create,
    ));
    let r = report(&before, &after);
    assert_eq!(r.findings[0].severity, Severity::High);
    let text = render_text(&r);
    assert!(text.contains("HIGH"));
    assert!(text.contains("x.txt"));
    assert!(text.contains("why:"));
    let md = render_markdown(&r);
    assert!(md.contains("## High"));
    assert!(md.contains("session"));
}

#[test]
fn empty_diff_has_a_calm_empty_message() {
    let before = base_snap(SnapshotLabel::Before);
    let after = base_snap(SnapshotLabel::After);
    let r = report(&before, &after);
    assert!(r.findings.is_empty());
    let text = render_text(&r);
    assert!(text.contains("Nothing surprising"));
}

#[test]
fn path_var_split_empty_and_env_map_independent() {
    assert!(split_path_var("").is_empty());
    let mut m: BTreeMap<String, String> = BTreeMap::new();
    m.insert("A".into(), "b".into());
    assert_eq!(m.get("A").unwrap(), "b");
}

#[test]
fn rename_detail_shows_old_and_new() {
    let before = base_snap(SnapshotLabel::Before);
    let mut after = base_snap(SnapshotLabel::After);
    after.file_events.push(FileEvent {
        volume: Some("C:".into()),
        path: Some(r"C:\Users\Alek\Desktop\b.txt".into()),
        name: "b.txt".into(),
        old_path: Some(r"C:\Users\Alek\Desktop\a.txt".into()),
        kind: ChangeKind::Rename,
        reasons_raw: 0,
        usn: None,
        file_id: None,
        is_dir: false,
        timestamp: None,
    });
    let r = report(&before, &after);
    assert!(r.findings.iter().any(|f| f.detail.contains('→')));
}
