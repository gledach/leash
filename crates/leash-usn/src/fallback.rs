use anyhow::Result;
use leash_snapshot::FileInventoryEntry;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const MAX_FILES: usize = 40_000;
const MAX_HASH_BYTES: u64 = 8 * 1024 * 1024;
const MAX_DEPTH: usize = 8;

const SKIP_DIR_NAMES: &[&str] = &[
    "node_modules",
    ".git",
    "__pycache__",
    ".pytest_cache",
    ".cache",
    "target",
    "Cache",
    "CachedData",
    "Code Cache",
    "GPUCache",
    "ShaderCache",
    "Temp",
    "tmp",
    "Logs",
    "logs",
    "INetCache",
    "WebCache",
    "$Recycle.Bin",
    "System Volume Information",
    "Windows",
    "Prefetch",
];

#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    /// Recurse (depth-capped, with skip lists).
    pub roots: Vec<PathBuf>,
    /// Only record the folder itself and its immediate children (for %USERPROFILE% dotfiles).
    pub shallow_roots: Vec<PathBuf>,
    /// Files always hashed even if they are not under a root walk (profiles, ssh, …).
    pub extra_files: Vec<PathBuf>,
}

#[derive(Debug, Default)]
pub struct FallbackScan {
    pub files: Vec<FileInventoryEntry>,
    pub warnings: Vec<String>,
    pub truncated: bool,
}

/// Walk priority folders, recording size, mtime, and a hash for smaller files.
pub fn scan_priority_folders(opts: &ScanOptions) -> Result<FallbackScan> {
    let mut out = FallbackScan::default();
    let mut seen = std::collections::BTreeSet::new();
    for root in &opts.shallow_roots {
        if !root.exists() {
            continue;
        }
        walk(root, 0, &mut out, &mut seen, 1)?;
        if out.truncated {
            break;
        }
    }
    for root in &opts.roots {
        if !root.exists() {
            continue;
        }
        walk(root, 0, &mut out, &mut seen, MAX_DEPTH)?;
        if out.truncated {
            break;
        }
    }
    for extra in &opts.extra_files {
        if out.files.len() >= MAX_FILES {
            out.truncated = true;
            break;
        }
        if let Some(entry) = inventory_one(extra) {
            let key = entry.path.to_lowercase();
            if seen.insert(key) {
                out.files.push(entry);
            }
        }
    }
    if out.truncated {
        out.warnings.push(format!(
            "fallback folder scan stopped after {MAX_FILES} files; later changes may be missing"
        ));
    }
    Ok(out)
}

fn walk(
    path: &Path,
    depth: usize,
    out: &mut FallbackScan,
    seen: &mut std::collections::BTreeSet<String>,
    max_depth: usize,
) -> Result<()> {
    if out.files.len() >= MAX_FILES {
        out.truncated = true;
        return Ok(());
    }
    if depth > max_depth {
        return Ok(());
    }
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return Ok(()),
    };
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_file() {
        if let Some(entry) = inventory_from(path, &meta) {
            let key = entry.path.to_lowercase();
            if seen.insert(key) {
                out.files.push(entry);
            }
        }
        return Ok(());
    }
    if !meta.is_dir() {
        return Ok(());
    }
    if depth > 0 {
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if SKIP_DIR_NAMES.iter().any(|s| s.eq_ignore_ascii_case(name)) {
                return Ok(());
            }
        }
        let norm = path
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        if norm.contains("/appdata/local/")
            || norm.ends_with("/appdata/local")
            || norm.contains("/appdata/locallow/")
            || norm.ends_with("/appdata/locallow")
        {
            return Ok(());
        }
    }
    // Record the directory itself so deletions of empty dirs can be seen.
    if let Some(entry) = inventory_from(path, &meta) {
        let key = entry.path.to_lowercase();
        if seen.insert(key) {
            out.files.push(entry);
        }
    }
    let rd = match fs::read_dir(path) {
        Ok(rd) => rd,
        Err(_) => return Ok(()),
    };
    for ent in rd.flatten() {
        walk(&ent.path(), depth + 1, out, seen, max_depth)?;
        if out.truncated {
            break;
        }
    }
    Ok(())
}

fn inventory_one(path: &Path) -> Option<FileInventoryEntry> {
    let meta = fs::symlink_metadata(path).ok()?;
    inventory_from(path, &meta)
}

fn inventory_from(path: &Path, meta: &fs::Metadata) -> Option<FileInventoryEntry> {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let is_dir = meta.is_dir();
    let size = if is_dir { 0 } else { meta.len() };
    let sha256 = if !is_dir && size <= MAX_HASH_BYTES {
        hash_file(path)
    } else {
        None
    };
    Some(FileInventoryEntry {
        path: path.to_string_lossy().into_owned(),
        size,
        mtime,
        sha256,
        is_dir,
    })
}

pub fn hash_file(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).ok()?;
    Some(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp() -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("leash-scan-{n}"));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn scan_hashes_files_and_skips_node_modules() {
        let root = tmp();
        fs::write(root.join("notes.txt"), b"hello").unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::write(root.join("node_modules/pkg/index.js"), b"nope").unwrap();
        let mut extra = root.clone();
        extra.push("extra.env");
        let mut f = fs::File::create(&extra).unwrap();
        f.write_all(b"SECRET=1").unwrap();
        let scan = scan_priority_folders(&ScanOptions {
            roots: vec![root.clone()],
            shallow_roots: Vec::new(),
            extra_files: vec![extra.clone()],
        })
        .unwrap();
        let paths: Vec<_> = scan.files.iter().map(|e| e.path.clone()).collect();
        assert!(paths.iter().any(|p| p.ends_with("notes.txt")));
        assert!(paths.iter().any(|p| p.ends_with("extra.env")));
        assert!(!paths.iter().any(|p| p.contains("node_modules")));
        let notes = scan
            .files
            .iter()
            .find(|e| e.path.ends_with("notes.txt"))
            .unwrap();
        assert_eq!(
            notes.sha256.as_deref(),
            Some("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
        );
        let _ = fs::remove_dir_all(&root);
    }
}
