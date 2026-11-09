//! USN change-journal wrapper around `usn-journal-rs`, plus a fallback
//! hash/mtime scan used when the process is not elevated.
//!
//! Record grouping lives here and is tested on Linux. The Windows journal
//! reader is compiled only for Windows targets. This crate never creates or
//! deletes a USN journal — it only queries an existing one.

mod fallback;
mod group;

#[cfg(not(windows))]
mod stub;
#[cfg(windows)]
mod win;

pub use fallback::{scan_priority_folders, FallbackScan, ScanOptions};
pub use group::{
    classify_reason_bits, group_records, RawUsnRecord, USN_CLOSE, USN_CREATE, USN_DELETE,
    USN_EXTEND, USN_OVERWRITE, USN_RENAME_NEW, USN_RENAME_OLD, USN_TRUNCATION,
};

use anyhow::Result;
use leash_snapshot::{FileEvent, FileMode, UsnVolumeState};

/// Outcome of a file-change collection pass.
#[derive(Debug, Default)]
pub struct FileCollection {
    pub mode: FileMode,
    pub elevated: bool,
    pub usn: Vec<UsnVolumeState>,
    pub events: Vec<FileEvent>,
    pub fallback_files: Vec<leash_snapshot::FileInventoryEntry>,
    pub warnings: Vec<String>,
}

pub fn is_elevated() -> bool {
    #[cfg(windows)]
    {
        win::is_elevated()
    }
    #[cfg(not(windows))]
    {
        stub::is_elevated()
    }
}

/// Query journal cursors on fixed NTFS/ReFS volumes (no records are read).
pub fn query_volumes() -> Result<Vec<UsnVolumeState>> {
    #[cfg(windows)]
    {
        win::query_volumes()
    }
    #[cfg(not(windows))]
    {
        stub::query_volumes()
    }
}

/// Read USN records from the cursors captured at `start`, resolve paths, and
/// group them into file events. Falls back to a folder scan when not elevated
/// or when no journal is available.
pub fn collect_after(start: &[UsnVolumeState], scan: &ScanOptions) -> Result<FileCollection> {
    #[cfg(windows)]
    {
        win::collect_after(start, scan)
    }
    #[cfg(not(windows))]
    {
        stub::collect_after(start, scan)
    }
}

/// Capture start cursors (elevated) and/or a fallback inventory.
pub fn collect_before(scan: &ScanOptions) -> Result<FileCollection> {
    #[cfg(windows)]
    {
        win::collect_before(scan)
    }
    #[cfg(not(windows))]
    {
        stub::collect_before(scan)
    }
}

/// Free-form doctor lines about USN availability.
pub fn doctor_lines() -> Vec<String> {
    #[cfg(windows)]
    {
        win::doctor_lines()
    }
    #[cfg(not(windows))]
    {
        stub::doctor_lines()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leash_filter::ChangeKind;

    fn rec(usn: i64, fid: u128, name: &str, reason: u32) -> RawUsnRecord {
        RawUsnRecord {
            volume: "C:".into(),
            usn,
            file_id: format!("{fid:x}"),
            name: name.into(),
            path: Some(format!(r"C:\Users\Alek\{name}")),
            reason,
            is_dir: false,
            timestamp: None,
        }
    }

    #[test]
    fn classify_reason_prefers_delete_create_rename() {
        assert_eq!(
            classify_reason_bits(USN_CREATE | USN_CLOSE),
            ChangeKind::Create
        );
        assert_eq!(
            classify_reason_bits(USN_DELETE | USN_CLOSE),
            ChangeKind::Delete
        );
        assert_eq!(
            classify_reason_bits(USN_RENAME_OLD | USN_RENAME_NEW | USN_CLOSE),
            ChangeKind::Rename
        );
        assert_eq!(
            classify_reason_bits(USN_OVERWRITE | USN_CLOSE),
            ChangeKind::DataOverwrite
        );
        assert_eq!(classify_reason_bits(USN_EXTEND), ChangeKind::DataExtend);
        assert_eq!(
            classify_reason_bits(USN_TRUNCATION),
            ChangeKind::DataTruncation
        );
        assert_eq!(classify_reason_bits(USN_CLOSE), ChangeKind::Other);
    }

    #[test]
    fn group_ors_reasons_and_pairs_renames() {
        let ev = group_records(vec![
            rec(1, 10, "a.txt", USN_CREATE),
            rec(2, 10, "a.txt", USN_OVERWRITE | USN_CLOSE),
            rec(3, 11, "old.txt", USN_RENAME_OLD),
            rec(4, 11, "new.txt", USN_RENAME_NEW | USN_CLOSE),
        ]);
        assert_eq!(ev.len(), 2);
        let create = ev.iter().find(|e| e.name == "a.txt").unwrap();
        assert_eq!(create.kind, ChangeKind::Create);
        let rename = ev.iter().find(|e| e.kind == ChangeKind::Rename).unwrap();
        assert!(rename.path.as_deref().unwrap().contains("new.txt"));
        assert!(rename.old_path.as_deref().unwrap().contains("old.txt"));
    }

    #[test]
    fn group_keeps_delete_even_without_resolved_path() {
        let ev = group_records(vec![RawUsnRecord {
            volume: "C:".into(),
            usn: 9,
            file_id: "abc".into(),
            name: "gone.txt".into(),
            path: None,
            reason: USN_DELETE | USN_CLOSE,
            is_dir: false,
            timestamp: None,
        }]);
        assert_eq!(ev[0].kind, ChangeKind::Delete);
        assert_eq!(ev[0].name, "gone.txt");
    }
}
