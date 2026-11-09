//! Reconstruct session status from files on disk so a GUI can be closed
//! and reopened without losing a running Start.

use crate::model::{FileMode, SeverityCounts, Snapshot};
use crate::store::{SessionPaths, SessionStore};
use chrono::{DateTime, FixedOffset, Local, Utc};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    /// `before.json` exists, no after/report — Stop can still run.
    Running,
    /// `after.json` exists but the report was not written (unusual).
    Partial,
    /// `report.json` (and usually report.html) exists.
    Completed,
}

impl SessionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Partial => "partial",
            Self::Completed => "completed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: String,
    pub dir: PathBuf,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub repo_path: Option<String>,
    pub elevated: Option<bool>,
    pub file_mode: Option<FileMode>,
    pub status: SessionStatus,
    pub counts: Option<SeverityCounts>,
    pub has_html: bool,
}

impl SessionInfo {
    pub fn from_paths(paths: &SessionPaths) -> Self {
        let before = read_snap_opt(&paths.before());
        let after = read_snap_opt(&paths.after());
        let report = read_report_direct(&paths.report_json());

        let status = if report.is_some() || paths.report_json().exists() {
            SessionStatus::Completed
        } else if after.is_some() {
            SessionStatus::Partial
        } else {
            SessionStatus::Running
        };

        let started_at = before.as_ref().map(|s| s.taken_at.clone());
        let finished_at = report
            .as_ref()
            .map(|r| r.after_at.clone())
            .or_else(|| after.as_ref().map(|s| s.taken_at.clone()));
        let repo_path = report
            .as_ref()
            .and_then(|r| r.repo_path.clone())
            .or_else(|| before.as_ref().and_then(|s| s.repo_path.clone()));
        let elevated = report
            .as_ref()
            .map(|r| r.elevated)
            .or_else(|| before.as_ref().map(|s| s.elevated));
        let file_mode = report
            .as_ref()
            .map(|r| r.file_mode)
            .or_else(|| before.as_ref().map(|s| s.file_mode));
        let counts = report.as_ref().map(|r| r.counts.clone());

        SessionInfo {
            id: paths.id.clone(),
            dir: paths.dir.clone(),
            started_at,
            finished_at,
            repo_path,
            elevated,
            file_mode,
            status,
            counts,
            has_html: paths.report_html().exists(),
        }
    }
}

fn read_snap_opt(path: &std::path::Path) -> Option<Snapshot> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn read_report_direct(path: &std::path::Path) -> Option<crate::DiffReport> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

impl SessionStore {
    /// All sessions under `sessions/`, newest first (id is a timestamp).
    /// Directories without `before.json` are skipped.
    pub fn list_sessions(&self) -> Vec<SessionInfo> {
        let mut out = Vec::new();
        let dir = self.sessions_dir();
        let Ok(rd) = fs::read_dir(&dir) else {
            return out;
        };
        for ent in rd.flatten() {
            let path = ent.path();
            if !path.is_dir() {
                continue;
            }
            let Some(id) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let paths = SessionPaths {
                id: id.to_string(),
                dir: path,
            };
            if !paths.before().exists() {
                continue;
            }
            out.push(SessionInfo::from_paths(&paths));
        }
        out.sort_by(|a, b| b.id.cmp(&a.id));
        out
    }

    /// The current session if it was started and has not been stopped yet.
    pub fn running_session(&self) -> Option<SessionInfo> {
        let paths = self.open(None).ok()?;
        let info = SessionInfo::from_paths(&paths);
        if info.status == SessionStatus::Running {
            Some(info)
        } else {
            None
        }
    }
}

/// Human elapsed time between a snapshot `taken_at` (RFC 3339) and `now`.
pub fn format_elapsed_since(taken_at: &str, now: DateTime<Utc>) -> Option<String> {
    let start = parse_rfc3339(taken_at)?;
    let secs = now.signed_duration_since(start).num_seconds();
    if secs < 0 {
        return Some("0s".into());
    }
    Some(format_elapsed(Duration::from_secs(secs as u64)))
}

pub fn format_elapsed(dur: Duration) -> String {
    let secs = dur.as_secs();
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h {m:02}m {s:02}s")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

pub fn parse_rfc3339(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw.trim())
        .map(|d| d.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            DateTime::<FixedOffset>::parse_from_str(raw.trim(), "%Y-%m-%dT%H:%M:%S%.f%z")
                .ok()
                .map(|d| d.with_timezone(&Utc))
        })
        .or_else(|| {
            // Local-offset strings from chrono::Local::to_rfc3339() already
            // match the first parser. This is a last-ditch UTC-naive form.
            chrono::NaiveDateTime::parse_from_str(raw.trim(), "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(|n| n.and_utc())
        })
}

pub fn now_local_rfc3339() -> String {
    Local::now().to_rfc3339()
}

pub fn new_session_id() -> String {
    Local::now().format("%Y%m%d-%H%M%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        AttributionSummary, DiffReport, IgnoreSummary, Snapshot, SnapshotLabel, SeverityCounts,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp() -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("leash-sessions-{n}"))
    }

    fn write_before(store: &SessionStore, id: &str, repo: &str, at: &str) -> SessionPaths {
        let paths = store.create(id).unwrap();
        let mut snap = Snapshot::new(SnapshotLabel::Before, id);
        snap.taken_at = at.into();
        snap.repo_path = Some(repo.into());
        snap.elevated = true;
        snap.file_mode = FileMode::Usn;
        store.write_snapshot(&paths, &snap).unwrap();
        paths
    }

    #[test]
    fn running_session_survives_reopen() {
        let root = tmp();
        let store = SessionStore::new(&root);
        write_before(&store, "20261109-210000", r"C:\src\app", "2026-11-09T20:00:00+01:00");

        // Simulate closing and reopening the app: new SessionStore, same dir.
        let store2 = SessionStore::new(&root);
        let running = store2.running_session().expect("should still be running");
        assert_eq!(running.id, "20261109-210000");
        assert_eq!(running.status, SessionStatus::Running);
        assert_eq!(running.repo_path.as_deref(), Some(r"C:\src\app"));
        assert_eq!(running.elevated, Some(true));
        assert!(!running.has_html);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn list_sessions_newest_first_and_classifies() {
        let root = tmp();
        let store = SessionStore::new(&root);
        let older = write_before(&store, "20261109-180000", r"C:\old", "2026-11-09T18:00:00Z");
        let _newer = write_before(&store, "20261109-210000", r"C:\new", "2026-11-09T21:00:00Z");

        let mut after = Snapshot::new(SnapshotLabel::After, "20261109-180000");
        after.taken_at = "2026-11-09T18:10:00Z".into();
        store.write_snapshot(&older, &after).unwrap();
        let report = DiffReport {
            session_id: "20261109-180000".into(),
            before_at: "2026-11-09T18:00:00Z".into(),
            after_at: "2026-11-09T18:10:00Z".into(),
            repo_path: Some(r"C:\old".into()),
            file_mode: FileMode::Usn,
            elevated: true,
            warnings: vec![],
            findings: vec![],
            ignored: IgnoreSummary::default(),
            counts: SeverityCounts {
                high: 2,
                medium: 1,
                info: 0,
            },
            attribution: AttributionSummary::off(),
        };
        store
            .write_report(&older, &report, "# md", "<html></html>")
            .unwrap();

        let list = store.list_sessions();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "20261109-210000");
        assert_eq!(list[0].status, SessionStatus::Running);
        assert_eq!(list[1].id, "20261109-180000");
        assert_eq!(list[1].status, SessionStatus::Completed);
        assert_eq!(list[1].counts.as_ref().unwrap().high, 2);
        assert!(list[1].has_html);

        // Current pointer is the last create(), so running is the newer one.
        let running = store.running_session().unwrap();
        assert_eq!(running.id, "20261109-210000");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn format_elapsed_buckets() {
        assert_eq!(format_elapsed(Duration::from_secs(7)), "7s");
        assert_eq!(format_elapsed(Duration::from_secs(65)), "1m 05s");
        assert_eq!(format_elapsed(Duration::from_secs(3723)), "1h 02m 03s");
    }

    #[test]
    fn elapsed_since_rfc3339() {
        let start = "2026-11-09T20:00:00Z";
        let now = DateTime::parse_from_rfc3339("2026-11-09T20:05:07Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(format_elapsed_since(start, now).as_deref(), Some("5m 07s"));
    }
}
