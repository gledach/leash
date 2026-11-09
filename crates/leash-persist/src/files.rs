use leash_snapshot::{HashedFile, SshSnapshot};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Default)]
pub struct UserFiles {
    pub profiles: Vec<HashedFile>,
    pub git_configs: Vec<HashedFile>,
    pub ssh: SshSnapshot,
}

pub fn collect_user_files(home: &Path) -> UserFiles {
    let mut out = UserFiles::default();
    for path in profile_candidates(home) {
        out.profiles.push(hash_file(&path));
    }
    for path in git_candidates(home) {
        out.git_configs.push(hash_file(&path));
    }
    let ssh_dir = home.join(".ssh");
    out.ssh.config = Some(hash_file(&ssh_dir.join("config")));
    let auth = ssh_dir.join("authorized_keys");
    let auth2 = ssh_dir.join("authorized_keys2");
    out.ssh.authorized_keys = Some(if auth.exists() {
        hash_file(&auth)
    } else {
        hash_file(&auth2)
    });
    if ssh_dir.is_dir() {
        if let Ok(rd) = fs::read_dir(&ssh_dir) {
            for ent in rd.flatten() {
                let path = ent.path();
                if !path.is_file() {
                    continue;
                }
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.eq_ignore_ascii_case("config")
                    || name.eq_ignore_ascii_case("authorized_keys")
                    || name.eq_ignore_ascii_case("authorized_keys2")
                    || name.eq_ignore_ascii_case("known_hosts")
                    || name.eq_ignore_ascii_case("known_hosts.old")
                    || name.ends_with(".ppk")
                {
                    // config / authorized_keys already recorded; skip known_hosts (not a key)
                    if name.eq_ignore_ascii_case("config")
                        || name.to_ascii_lowercase().starts_with("authorized_keys")
                        || name.to_ascii_lowercase().starts_with("known_hosts")
                    {
                        continue;
                    }
                }
                // Hash every other file in .ssh (private keys, .pub, certs). Content is never stored.
                out.ssh.key_files.push(hash_file(&path));
            }
        }
    }
    out
}

pub fn profile_candidates(home: &Path) -> Vec<PathBuf> {
    let mut paths = vec![
        home.join(".bashrc"),
        home.join(".bash_profile"),
        home.join(".zshrc"),
        home.join(".zprofile"),
        home.join(".profile"),
        home.join(".config/powershell/Microsoft.PowerShell_profile.ps1"),
        home.join(".config/powershell/profile.ps1"),
    ];
    for docs in [
        home.join("Documents"),
        home.join("OneDrive").join("Documents"),
        home.join("OneDrive").join("Documentos"),
    ] {
        paths.push(docs.join("PowerShell/Microsoft.PowerShell_profile.ps1"));
        paths.push(docs.join("PowerShell/profile.ps1"));
        paths.push(docs.join("WindowsPowerShell/Microsoft.PowerShell_profile.ps1"));
        paths.push(docs.join("WindowsPowerShell/profile.ps1"));
    }
    paths
}

pub fn git_candidates(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".gitconfig"),
        home.join(".config/git/config"),
        PathBuf::from(r"C:\Program Files\Git\etc\gitconfig"),
        PathBuf::from(r"C:\ProgramData\Git\config"),
        PathBuf::from("/etc/gitconfig"),
        PathBuf::from("/usr/local/etc/gitconfig"),
    ]
}

pub fn hash_file(path: &Path) -> HashedFile {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64);
            let sha = fs::File::open(path).ok().and_then(|mut f| {
                let mut hasher = Sha256::new();
                std::io::copy(&mut f, &mut hasher).ok()?;
                Some(format!("{:x}", hasher.finalize()))
            });
            HashedFile {
                path: path.to_string_lossy().into_owned(),
                exists: true,
                sha256: sha,
                size: Some(meta.len()),
                mtime,
            }
        }
        _ => HashedFile {
            path: path.to_string_lossy().into_owned(),
            exists: false,
            sha256: None,
            size: None,
            mtime: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn hash_file_records_digest_and_missing() {
        let dir = std::env::temp_dir().join(format!(
            "leash-hash-{}",
            std::time::SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("id_ed25519");
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(b"not-a-real-key").unwrap();
        let hashed = hash_file(&path);
        assert!(hashed.exists);
        assert!(hashed.sha256.as_ref().is_some_and(|h| h.len() == 64));
        let again = hash_file(&path);
        assert_eq!(hashed.sha256, again.sha256);
        assert_eq!(hashed.size, Some(b"not-a-real-key".len() as u64));
        let missing = hash_file(&dir.join("nope"));
        assert!(!missing.exists);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_user_files_lists_ssh_keys_without_content() {
        let dir = std::env::temp_dir().join(format!(
            "leash-ssh-{}",
            std::time::SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let ssh = dir.join(".ssh");
        fs::create_dir_all(&ssh).unwrap();
        fs::write(ssh.join("config"), b"Host *\n").unwrap();
        fs::write(ssh.join("authorized_keys"), b"ssh-ed25519 AAAA comment\n").unwrap();
        fs::write(
            ssh.join("id_ed25519"),
            b"-----BEGIN OPENSSH PRIVATE KEY-----\n",
        )
        .unwrap();
        let files = collect_user_files(&dir);
        assert!(files.ssh.config.as_ref().unwrap().exists);
        assert!(files.ssh.authorized_keys.as_ref().unwrap().exists);
        assert!(files
            .ssh
            .key_files
            .iter()
            .any(|f| f.path.ends_with("id_ed25519") && f.sha256.is_some()));
        // Never store the key bytes themselves.
        for key in &files.ssh.key_files {
            assert_ne!(
                key.sha256.as_deref(),
                Some("-----BEGIN OPENSSH PRIVATE KEY-----\n")
            );
            assert!(key.sha256.as_ref().is_some_and(|h| !h.contains("BEGIN")));
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
