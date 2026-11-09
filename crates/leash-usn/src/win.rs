use super::group::{
    group_records, RawUsnRecord, USN_CREATE, USN_DELETE, USN_EXTEND, USN_OVERWRITE, USN_RENAME_NEW,
    USN_RENAME_OLD, USN_TRUNCATION,
};
use super::{FileCollection, ScanOptions};
use anyhow::{Context, Result};
use leash_snapshot::{FileMode, UsnVolumeState};
use usn_journal_rs::errors::UsnError;
use usn_journal_rs::journal::JournalIterOptions;
use usn_journal_rs::volume::Volume;
use usn_journal_rs::{Usn, UsnReason};
use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW};
use windows::Win32::System::WindowsProgramming::DRIVE_FIXED;

pub fn is_elevated() -> bool {
    usn_journal_rs::privilege::is_elevated().unwrap_or(false)
}

pub fn query_volumes() -> Result<Vec<UsnVolumeState>> {
    let letters = fixed_ntfs_letters()?;
    let mut out = Vec::new();
    for letter in letters {
        out.push(query_one(letter));
    }
    Ok(out)
}

pub fn collect_before(scan: &ScanOptions) -> Result<FileCollection> {
    let elevated = is_elevated();
    let mut col = FileCollection {
        elevated,
        ..FileCollection::default()
    };
    if elevated {
        match query_volumes() {
            Ok(states) => {
                if states.iter().all(|s| !s.query_ok) {
                    col.warnings.push(
                        "Administrator rights are present, but no USN journal could be opened. Using the fallback folder scan."
                            .into(),
                    );
                    for s in &states {
                        if let Some(err) = &s.error {
                            col.warnings.push(format!("{}: {err}", s.volume));
                        }
                    }
                    col.usn = states;
                    fill_fallback(&mut col, scan);
                    col.mode = FileMode::FallbackScan;
                } else {
                    for s in &states {
                        if !s.query_ok {
                            col.warnings.push(format!(
                                "USN journal on {} was not readable: {}",
                                s.volume,
                                s.error.as_deref().unwrap_or("unknown error")
                            ));
                        }
                    }
                    col.usn = states;
                    col.mode = FileMode::Usn;
                    // Still snapshot priority folders so mixed volumes have a baseline.
                    fill_fallback(&mut col, scan);
                    if !col.fallback_files.is_empty() {
                        col.mode = FileMode::Mixed;
                    }
                }
            }
            Err(err) => {
                col.warnings
                    .push(format!("could not enumerate volumes: {err}"));
                fill_fallback(&mut col, scan);
                col.mode = FileMode::FallbackScan;
            }
        }
    } else {
        col.warnings.push(
            "leash is not running as Administrator, so it cannot read the NTFS USN change journal. Falling back to a hash/mtime scan of priority folders only (user profile, AppData\\Roaming, Startup, nearby projects). Re-run from an elevated PowerShell for a full file diff."
                .into(),
        );
        fill_fallback(&mut col, scan);
        col.mode = FileMode::FallbackScan;
    }
    Ok(col)
}

pub fn collect_after(start: &[UsnVolumeState], scan: &ScanOptions) -> Result<FileCollection> {
    let elevated = is_elevated();
    let mut col = FileCollection {
        elevated,
        ..FileCollection::default()
    };
    if elevated && !start.is_empty() {
        let mut raw = Vec::new();
        for cursor in start {
            if !cursor.query_ok {
                col.usn.push(cursor.clone());
                continue;
            }
            match read_delta(cursor, &mut raw) {
                Ok(state) => col.usn.push(state),
                Err(err) => {
                    let mut state = cursor.clone();
                    state.query_ok = false;
                    state.error = Some(err.to_string());
                    col.warnings.push(format!(
                        "reading USN journal on {} failed: {err}",
                        cursor.volume
                    ));
                    col.usn.push(state);
                }
            }
        }
        col.events = group_records(raw);
        col.mode = FileMode::Usn;
        fill_fallback(&mut col, scan);
        if start.iter().all(|s| !s.query_ok) {
            col.mode = FileMode::FallbackScan;
        } else if !col.fallback_files.is_empty() {
            col.mode = FileMode::Mixed;
        }
    } else {
        if !elevated {
            col.warnings.push(
                "leash is not running as Administrator, so it cannot read the NTFS USN change journal. Falling back to a hash/mtime scan of priority folders only."
                    .into(),
            );
        }
        fill_fallback(&mut col, scan);
        col.mode = FileMode::FallbackScan;
    }
    Ok(col)
}

pub fn doctor_lines() -> Vec<String> {
    let mut lines = Vec::new();
    let elevated = is_elevated();
    lines.push(format!(
        "Administrator: {}",
        if elevated { "yes" } else { "no" }
    ));
    if !elevated {
        lines.push("  USN journal reads need an elevated process. Right-click PowerShell → Run as administrator, or start a terminal as admin.".into());
        lines.push("  Without elevation, `leash start` still works: it scans priority folders by hash and mtime.".into());
        return lines;
    }
    match query_volumes() {
        Ok(states) if states.is_empty() => {
            lines.push("USN: no fixed NTFS/ReFS volumes were found.".into());
        }
        Ok(states) => {
            for s in states {
                if s.query_ok {
                    lines.push(format!(
                        "USN {}: ok  journal_id=0x{:x}  next_usn={}  lowest_valid={}",
                        s.volume, s.journal_id, s.next_usn, s.lowest_valid_usn
                    ));
                } else {
                    lines.push(format!(
                        "USN {}: not readable — {}",
                        s.volume,
                        s.error.as_deref().unwrap_or("unknown error")
                    ));
                }
            }
        }
        Err(err) => lines.push(format!("USN: could not enumerate volumes: {err}")),
    }
    lines
}

fn fill_fallback(col: &mut FileCollection, scan: &ScanOptions) {
    match crate::fallback::scan_priority_folders(scan) {
        Ok(result) => {
            col.warnings.extend(result.warnings);
            col.fallback_files = result.files;
        }
        Err(err) => col
            .warnings
            .push(format!("fallback folder scan failed: {err}")),
    }
}

fn query_one(letter: char) -> UsnVolumeState {
    let volume_name = format!("{letter}:");
    match Volume::from_drive_letter(letter) {
        Err(err) => UsnVolumeState {
            volume: volume_name,
            journal_id: 0,
            start_usn: 0,
            next_usn: 0,
            first_usn: 0,
            lowest_valid_usn: 0,
            query_ok: false,
            error: Some(describe_usn_error(&err)),
        },
        Ok(volume) => match volume.journal().query() {
            Ok(data) => UsnVolumeState {
                volume: volume_name,
                journal_id: data.journal_id,
                start_usn: data.next_usn.get(),
                next_usn: data.next_usn.get(),
                first_usn: data.first_usn.get(),
                lowest_valid_usn: data.lowest_valid_usn.get(),
                query_ok: true,
                error: None,
            },
            Err(err) => UsnVolumeState {
                volume: volume_name,
                journal_id: 0,
                start_usn: 0,
                next_usn: 0,
                first_usn: 0,
                lowest_valid_usn: 0,
                query_ok: false,
                error: Some(describe_usn_error(&err)),
            },
        },
    }
}

fn read_delta(cursor: &UsnVolumeState, raw: &mut Vec<RawUsnRecord>) -> Result<UsnVolumeState> {
    let letter = cursor
        .volume
        .chars()
        .next()
        .context("volume label missing a drive letter")?;
    let volume = Volume::from_drive_letter(letter)?;
    let journal = volume.journal();
    let current = journal.query()?;
    let mut state = UsnVolumeState {
        volume: cursor.volume.clone(),
        journal_id: current.journal_id,
        start_usn: cursor.start_usn,
        next_usn: current.next_usn.get(),
        first_usn: current.first_usn.get(),
        lowest_valid_usn: current.lowest_valid_usn.get(),
        query_ok: true,
        error: None,
    };
    if current.journal_id != cursor.journal_id && cursor.journal_id != 0 {
        // Still try to read; the diff layer will warn about the id change.
    }
    let start = if cursor.start_usn < current.lowest_valid_usn.get()
        && current.lowest_valid_usn.get() != 0
    {
        current.lowest_valid_usn
    } else {
        Usn::new(cursor.start_usn)
    };
    let reason_mask = UsnReason::from_bits_retain(
        USN_OVERWRITE
            | USN_EXTEND
            | USN_TRUNCATION
            | USN_CREATE
            | USN_DELETE
            | USN_RENAME_OLD
            | USN_RENAME_NEW,
    );
    let opts = JournalIterOptions::builder()
        .start_usn(start)
        .reason_mask(reason_mask)
        .wait_for_more(false)
        .build()?;
    let resolver = volume.path_resolver_with_cache();
    for item in journal.try_iter_with_options(opts)? {
        let entry = match item {
            Ok(e) => e,
            Err(err) => {
                state
                    .error
                    .get_or_insert_with(|| format!("some records failed: {err}"));
                continue;
            }
        };
        let path = resolver.resolve_path(&entry).map(|p| strip_nt_prefix(&p));
        let ts = entry.time.to_system_time().and_then(system_time_rfc3339);
        raw.push(RawUsnRecord {
            volume: cursor.volume.clone(),
            usn: entry.usn.get(),
            file_id: format!("{:x}", entry.fid.as_u128()),
            name: entry.file_name.to_string_lossy().into_owned(),
            path,
            reason: entry.reason.bits(),
            is_dir: entry.is_dir(),
            timestamp: ts,
        });
    }
    Ok(state)
}

fn system_time_rfc3339(st: std::time::SystemTime) -> Option<String> {
    let secs = st.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let hour = rem / 3600;
    let min = (rem % 3600) / 60;
    let sec = rem % 60;
    let (y, m, d) = civil_from_days(days as i64);
    Some(format!("{y:04}-{m:02}-{d:02}T{hour:02}:{min:02}:{sec:02}Z"))
}

fn civil_from_days(mut z: i64) -> (i32, u32, u32) {
    // Howard Hinnant civil-from-days. Unix epoch is 1970-01-01.
    z += 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

fn strip_nt_prefix(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    s.strip_prefix(r"\\?\")
        .or_else(|| s.strip_prefix("//?/"))
        .unwrap_or(&s)
        .to_string()
}

fn describe_usn_error(err: &UsnError) -> String {
    if err.is_permission_denied() {
        "access denied — Administrator privileges are required to open the volume's USN journal"
            .into()
    } else {
        err.to_string()
    }
}

fn fixed_ntfs_letters() -> Result<Vec<char>> {
    let mask = unsafe { GetLogicalDrives() };
    let mut letters = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = char::from(b'A' + i as u8);
        let root = format!(r"{letter}:\");
        let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
        let drive_type = unsafe { GetDriveTypeW(windows::core::PCWSTR(wide.as_ptr())) };
        if drive_type != DRIVE_FIXED {
            continue;
        }
        let mut fs_name = [0u16; 32];
        let ok = unsafe {
            GetVolumeInformationW(
                windows::core::PCWSTR(wide.as_ptr()),
                None,
                None,
                None,
                None,
                Some(&mut fs_name),
            )
        };
        if ok.is_err() {
            continue;
        }
        let end = fs_name
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(fs_name.len());
        let fs = String::from_utf16_lossy(&fs_name[..end]).to_ascii_uppercase();
        if fs == "NTFS" || fs == "REFS" {
            letters.push(letter);
        }
    }
    Ok(letters)
}
