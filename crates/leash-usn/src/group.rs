use leash_filter::ChangeKind;
use leash_snapshot::FileEvent;
use std::collections::BTreeMap;

/// USN reason bits we care about in v1 (from `winioctl.h`).
pub const USN_OVERWRITE: u32 = 0x0000_0001;
pub const USN_EXTEND: u32 = 0x0000_0002;
pub const USN_TRUNCATION: u32 = 0x0000_0004;
pub const USN_CREATE: u32 = 0x0000_0100;
pub const USN_DELETE: u32 = 0x0000_0200;
pub const USN_RENAME_OLD: u32 = 0x0000_1000;
pub const USN_RENAME_NEW: u32 = 0x0000_2000;
pub const USN_CLOSE: u32 = 0x8000_0000;

/// A journal record reduced to the fields grouping needs. Kept free of
/// Windows types so tests run on Linux.
#[derive(Debug, Clone)]
pub struct RawUsnRecord {
    pub volume: String,
    pub usn: i64,
    pub file_id: String,
    pub name: String,
    pub path: Option<String>,
    pub reason: u32,
    pub is_dir: bool,
    pub timestamp: Option<String>,
}

pub fn classify_reason_bits(reason: u32) -> ChangeKind {
    if reason & USN_DELETE != 0 {
        ChangeKind::Delete
    } else if reason & USN_CREATE != 0 {
        ChangeKind::Create
    } else if reason & (USN_RENAME_OLD | USN_RENAME_NEW) != 0 {
        ChangeKind::Rename
    } else if reason & USN_OVERWRITE != 0 {
        ChangeKind::DataOverwrite
    } else if reason & USN_EXTEND != 0 {
        ChangeKind::DataExtend
    } else if reason & USN_TRUNCATION != 0 {
        ChangeKind::DataTruncation
    } else {
        ChangeKind::Other
    }
}

/// Combine per-file records (OR the reason bits) and pair rename old/new names.
pub fn group_records(records: Vec<RawUsnRecord>) -> Vec<FileEvent> {
    // Keyed by volume + file id. Rename old/new may use different names.
    let mut by_id: BTreeMap<(String, String), Acc> = BTreeMap::new();
    for rec in records {
        let key = (rec.volume.clone(), rec.file_id.clone());
        let acc = by_id.entry(key).or_insert_with(|| Acc {
            volume: rec.volume.clone(),
            file_id: rec.file_id.clone(),
            is_dir: rec.is_dir,
            ..Acc::default()
        });
        acc.reason |= rec.reason;
        acc.last_usn = rec.usn;
        acc.is_dir |= rec.is_dir;
        if rec.timestamp.is_some() {
            acc.timestamp = rec.timestamp.clone();
        }
        if rec.reason & USN_RENAME_OLD != 0 {
            acc.old_path = rec.path.clone().or(Some(rec.name.clone()));
            acc.old_name = Some(rec.name.clone());
        } else if rec.reason & USN_RENAME_NEW != 0 {
            acc.path = rec.path.clone();
            acc.name = rec.name;
        } else {
            if acc.name.is_empty() {
                acc.name = rec.name;
            }
            if rec.path.is_some() {
                acc.path = rec.path;
            }
        }
    }

    by_id
        .into_values()
        .filter(|acc| acc.reason & !USN_CLOSE != 0)
        .map(|acc| {
            let kind = classify_reason_bits(acc.reason);
            let name = if acc.name.is_empty() {
                acc.old_name.clone().unwrap_or_default()
            } else {
                acc.name.clone()
            };
            FileEvent {
                volume: Some(acc.volume),
                path: acc.path,
                name,
                old_path: acc.old_path,
                kind,
                reasons_raw: acc.reason,
                usn: Some(acc.last_usn),
                file_id: Some(acc.file_id),
                is_dir: acc.is_dir,
                timestamp: acc.timestamp,
            }
        })
        .collect()
}

#[derive(Default)]
struct Acc {
    volume: String,
    file_id: String,
    name: String,
    path: Option<String>,
    old_path: Option<String>,
    old_name: Option<String>,
    reason: u32,
    last_usn: i64,
    is_dir: bool,
    timestamp: Option<String>,
}
