use super::{FileCollection, ScanOptions};
use anyhow::{bail, Result};
use leash_snapshot::{FileMode, UsnVolumeState};

pub fn is_elevated() -> bool {
    false
}

pub fn query_volumes() -> Result<Vec<UsnVolumeState>> {
    bail!("the USN change journal is only available on Windows")
}

pub fn collect_before(scan: &ScanOptions) -> Result<FileCollection> {
    fallback_only(
        scan,
        "this binary is not a Windows build; using a local folder scan only",
    )
}

pub fn collect_after(_start: &[UsnVolumeState], scan: &ScanOptions) -> Result<FileCollection> {
    fallback_only(
        scan,
        "this binary is not a Windows build; using a local folder scan only",
    )
}

pub fn doctor_lines() -> Vec<String> {
    vec![
        "USN change journal: not available (this is not a Windows build).".into(),
        "Build with --target x86_64-pc-windows-gnu (or msvc) and run the .exe on Windows 11."
            .into(),
    ]
}

fn fallback_only(scan: &ScanOptions, warning: &str) -> Result<FileCollection> {
    let scan_result = crate::fallback::scan_priority_folders(scan)?;
    let mut warnings = vec![warning.to_string()];
    warnings.extend(scan_result.warnings);
    Ok(FileCollection {
        mode: if scan_result.files.is_empty() {
            FileMode::Unavailable
        } else {
            FileMode::FallbackScan
        },
        elevated: false,
        usn: Vec::new(),
        events: Vec::new(),
        fallback_files: scan_result.files,
        warnings,
    })
}
