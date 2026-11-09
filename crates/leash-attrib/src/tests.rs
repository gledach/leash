use crate::join::apply_attribution;
use crate::model::*;
use crate::paths::{names_equal, normalize_path, normalize_registry, paths_match, registry_match};
use crate::service::is_service_image;
use crate::tree::{ProcessRecord, ProcessTree};
use leash_filter::Severity;
use leash_snapshot::{AttributionBucket, DiffReport, FileMode, Finding, FindingAttribution};

fn rec(pid: u32, ppid: Option<u32>, start: u64, image: &str) -> ProcessRecord {
    ProcessRecord {
        pid,
        parent_pid: ppid,
        start_ms: start,
        end_ms: None,
        image: image.into(),
        exe: Some(format!(r"C:\tools\{image}")),
        command_line: Some(format!("{image} --foo")),
        in_agent_tree: false,
    }
}

fn finding(category: &str, path: &str) -> Finding {
    Finding {
        category: category.into(),
        severity: Severity::High,
        title: format!("changed {path}"),
        detail: path.into(),
        reason: "test".into(),
        path: Some(path.into()),
        attribution: FindingAttribution::default(),
    }
}

fn empty_report() -> DiffReport {
    DiffReport {
        session_id: "s1".into(),
        before_at: "2026-11-09T18:00:00Z".into(),
        after_at: "2026-11-09T19:00:00Z".into(),
        repo_path: Some(r"C:\src\app".into()),
        file_mode: FileMode::Usn,
        elevated: true,
        warnings: Vec::new(),
        findings: Vec::new(),
        ignored: Default::default(),
        counts: Default::default(),
        attribution: Default::default(),
    }
}

fn file_ev(path: &str, pid: u32, at: u64) -> FileTraceEvent {
    FileTraceEvent {
        path: path.into(),
        old_path: None,
        op: TraceOp::Write,
        pid,
        at_ms: at,
    }
}

#[test]
fn normalize_strips_nt_prefixes_and_case() {
    assert_eq!(
        normalize_path(r"\\?\C:\Users\Alek\file.TXT"),
        r"c:\users\alek\file.txt"
    );
    assert_eq!(
        normalize_path(r"C:/Users/Alek/file.TXT"),
        r"c:\users\alek\file.txt"
    );
}

#[test]
fn paths_match_across_device_prefix() {
    assert!(paths_match(
        r"\Device\HarddiskVolume3\Users\Alek\AppData\Roaming\foo.dll",
        r"C:\Users\Alek\AppData\Roaming\foo.dll",
    ));
    assert!(!paths_match(
        r"C:\Users\Alek\foo.dll",
        r"C:\Users\Alek\bar.dll",
    ));
}

#[test]
fn registry_user_hive_matches_hkcu() {
    let etw = r"\REGISTRY\USER\S-1-5-21-1-2-3-1001\Software\Microsoft\Windows\CurrentVersion\Run\Hook";
    let finding = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Hook";
    assert_eq!(
        normalize_registry(finding),
        r"hkcu\software\microsoft\windows\currentversion\run\hook"
    );
    assert!(registry_match(etw, finding));
}

#[test]
fn image_names_equal_ignores_exe_suffix() {
    assert!(names_equal("Cursor.exe", "cursor"));
    assert!(names_equal(r"C:\Users\Alek\App\claude.exe", "CLAUDE"));
    assert!(!names_equal("cursor.exe", "code"));
}

#[test]
fn service_images_are_recognised() {
    assert!(is_service_image("svchost.exe"));
    assert!(is_service_image(r"C:\Windows\System32\msiexec.exe"));
    assert!(is_service_image("dockerd.exe"));
    assert!(!is_service_image("claude.exe"));
}

#[test]
fn tree_marks_descendants_and_handles_pid_reuse() {
    let mut tree = ProcessTree::new();
    tree.add_root(rec(100, Some(10), 1_000, "claude.exe"));
    tree.on_start(rec(200, Some(100), 1_100, "node.exe"));
    tree.on_start(rec(300, Some(1), 1_200, "chrome.exe"));
    assert!(tree.is_agent_at(100, 1_050));
    assert!(tree.is_agent_at(200, 1_150));
    assert!(!tree.is_agent_at(300, 1_250));
    assert_eq!(
        tree.chain(200, 1_150).as_deref(),
        Some("node.exe ← claude.exe")
    );

    // PID 200 exits, then a later unrelated process reuses it.
    tree.on_exit(200, 2_000);
    tree.on_start(rec(200, Some(1), 3_000, "steam.exe"));
    assert!(tree.is_agent_at(200, 1_500));
    assert!(!tree.is_agent_at(200, 3_100));
}

#[test]
fn tree_ingest_picks_up_grandchild_in_one_snapshot() {
    let mut tree = ProcessTree::new();
    tree.add_root(rec(10, Some(1), 100, "cursor.exe"));
    let snap = vec![
        rec(30, Some(20), 120, "rg.exe"),
        rec(20, Some(10), 110, "node.exe"),
        rec(10, Some(1), 100, "cursor.exe"),
    ];
    tree.ingest_snapshot(&snap, 200);
    assert!(tree.is_agent_at(20, 200));
    assert!(tree.is_agent_at(30, 200));
}

#[test]
fn join_splits_agent_other_service_and_unknown() {
    let mut tree = ProcessTree::new();
    tree.add_root(rec(100, Some(10), 1_000, "claude.exe"));
    tree.on_start(rec(200, Some(100), 1_100, "node.exe"));
    tree.on_start(rec(300, Some(1), 1_200, "chrome.exe"));
    tree.on_start(rec(4, None, 0, "System"));
    let mut log = TraceLog {
        status: AttributionStatus::Active,
        target: Some("claude.exe (pid 100)".into()),
        tree,
        file_events: vec![
            file_ev(r"C:\Users\Alek\.ssh\config", 200, 1_500),
            file_ev(r"C:\Users\Alek\AppData\Roaming\foo", 300, 1_600),
            file_ev(r"C:\Windows\System32\tasks\evil.xml", 4, 1_700),
        ],
        registry_events: vec![RegistryTraceEvent {
            key: r"\REGISTRY\USER\S-1-5-21-1-2-3-1001\Software\Microsoft\Windows\CurrentVersion\Run\Hook".into(),
            value: Some("Hook".into()),
            op: TraceOp::SetValue,
            pid: 200,
            at_ms: 1_800,
        }],
        warnings: Vec::new(),
        dropped_other_events: 0,
        error: None,
    };

    let mut report = empty_report();
    report.findings = vec![
        finding("file", r"C:\Users\Alek\.ssh\config"),
        finding("file", r"C:\Users\Alek\AppData\Roaming\foo"),
        finding("file", r"C:\Windows\System32\tasks\evil.xml"),
        finding(
            "registry",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Hook",
        ),
        finding("file", r"C:\mystery\no-etw.dll"),
    ];

    apply_attribution(&mut report, &log);

    assert_eq!(report.findings[0].attribution.bucket, AttributionBucket::Agent);
    assert_eq!(
        report.findings[0].attribution.process_chain.as_deref(),
        Some("node.exe ← claude.exe")
    );
    assert_eq!(report.findings[1].attribution.bucket, AttributionBucket::Other);
    assert_eq!(
        report.findings[1].attribution.process_name.as_deref(),
        Some("chrome.exe")
    );
    assert_eq!(
        report.findings[2].attribution.bucket,
        AttributionBucket::Unattributed
    );
    assert!(report.findings[2]
        .attribution
        .note
        .as_deref()
        .unwrap_or("")
        .contains("possibly via a service"));
    assert_eq!(report.findings[3].attribution.bucket, AttributionBucket::Agent);
    assert_eq!(
        report.findings[4].attribution.bucket,
        AttributionBucket::Unattributed
    );
    assert_eq!(report.attribution.agent, 2);
    assert_eq!(report.attribution.other, 1);
    assert_eq!(report.attribution.unattributed, 2);
    assert!(report.attribution.caveat.contains("Split by process"));

    // Unused mut
    let _ = &mut log;
}

#[test]
fn join_leaves_system_wide_when_attribution_is_off() {
    let mut report = empty_report();
    report.findings = vec![finding("file", r"C:\Users\Alek\.ssh\config")];
    apply_attribution(&mut report, &TraceLog::off());
    assert_eq!(report.findings[0].attribution.bucket, AttributionBucket::None);
    assert!(report.attribution.caveat.contains("all changes on this PC"));
}

#[test]
fn join_does_not_guess_when_unelevated() {
    let mut report = empty_report();
    report.findings = vec![finding("file", r"C:\Users\Alek\.ssh\config")];
    apply_attribution(
        &mut report,
        &TraceLog::unavailable("not running as administrator"),
    );
    assert_eq!(report.findings[0].attribution.bucket, AttributionBucket::None);
    assert!(report.attribution.caveat.contains("Attribution is off"));
}

#[test]
fn split_command_line_respects_quotes() {
    assert_eq!(
        crate::split_command_line(r#""C:\Program Files\app.exe" --flag "my file""#),
        vec!["C:\\Program Files\\app.exe", "--flag", "my file"]
    );
}

/// Build a fixture report with a mixed attribution split, used for the sample screenshot.
pub fn sample_attrib_report() -> DiffReport {
    let mut tree = ProcessTree::new();
    tree.add_root(rec(100, Some(10), 1_000, "claude.exe"));
    tree.on_start(rec(200, Some(100), 1_100, "node.exe"));
    tree.on_start(rec(300, Some(1), 1_200, "chrome.exe"));
    tree.on_start(rec(4, None, 0, "System"));
    let log = TraceLog {
        status: AttributionStatus::Active,
        target: Some("claude.exe (pid 100)".into()),
        tree,
        file_events: vec![
            file_ev(r"C:\Users\Alek\.ssh\config", 200, 1_500),
            file_ev(r"C:\Users\Alek\AppData\Roaming\npm\claude-hook.cmd", 200, 1_550),
            file_ev(r"C:\Users\Alek\AppData\Local\Temp\chrome-cache\x", 300, 1_600),
            file_ev(r"C:\Windows\System32\tasks\Updater.xml", 4, 1_700),
        ],
        registry_events: vec![RegistryTraceEvent {
            key: r"\REGISTRY\USER\S-1-5-21-1-2-3-1001\Software\Microsoft\Windows\CurrentVersion\Run\Hook".into(),
            value: Some("Hook".into()),
            op: TraceOp::SetValue,
            pid: 200,
            at_ms: 1_800,
        }],
        warnings: Vec::new(),
        dropped_other_events: 0,
        error: None,
    };
    let mut report = empty_report();
    report.session_id = "20261109-231500".into();
    report.before_at = "2026-11-09T22:00:00+01:00".into();
    report.after_at = "2026-11-09T23:12:00+01:00".into();
    report.repo_path = Some(r"C:\Users\Alek\src\myapp".into());
    report.findings = vec![
        finding("file", r"C:\Users\Alek\.ssh\config"),
        finding("file", r"C:\Users\Alek\AppData\Roaming\npm\claude-hook.cmd"),
        finding(
            "registry",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Hook",
        ),
        finding("file", r"C:\Users\Alek\AppData\Roaming\Chrome\User Data\Default\Preferences"),
        finding("file", r"C:\Windows\System32\tasks\Updater.xml"),
        finding("file", r"C:\mystery\no-etw.dll"),
    ];
    // Re-point the chrome finding at a path the event will match.
    report.findings[3].path = Some(r"C:\Users\Alek\AppData\Local\Temp\chrome-cache\x".into());
    report.findings[3].detail = r"C:\Users\Alek\AppData\Local\Temp\chrome-cache\x".into();
    report.findings[3].title = "File data overwrite".into();
    report.findings[3].severity = Severity::Info;
    report.findings[0].title = "File data overwrite".into();
    report.findings[0].severity = Severity::High;
    report.findings[1].title = "File create".into();
    report.findings[1].severity = Severity::High;
    report.findings[2].title = "New Run key: Hook".into();
    report.findings[2].severity = Severity::High;
    report.findings[4].title = "File create".into();
    report.findings[4].severity = Severity::Medium;
    report.findings[5].title = "File create".into();
    report.findings[5].severity = Severity::Medium;
    report.counts = leash_snapshot::SeverityCounts::tally(&report.findings);
    apply_attribution(&mut report, &log);
    report
}

#[test]
fn attrib_sample_html_has_three_sections() {
    let html = leash_snapshot::render_html(&sample_attrib_report());
    assert!(html.contains("Your agent did this"));
    assert!(html.contains("Other apps changed this meanwhile"));
    assert!(html.contains("Unattributed"));
    assert!(html.contains("node.exe"));
    assert!(html.contains("possibly via a service"));
    assert!(html.contains("class=\"caveat\""));
    if let Ok(path) = std::env::var("LEASH_ATTRIB_HTML") {
        std::fs::write(&path, html).expect("write attrib sample html");
    }
}
