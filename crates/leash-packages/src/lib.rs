//! Best-effort listings of global npm, pnpm, pip, and cargo packages.
//!
//! Missing tools are recorded as notes, never as a hard failure.

use leash_snapshot::{Package, PackageSets};
use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

const TIMEOUT_SECS: u64 = 25;

pub fn collect_packages() -> PackageSets {
    let mut sets = PackageSets::default();
    match collect_npm() {
        Ok(pkgs) => sets.npm = pkgs,
        Err(note) => sets.notes.push(note),
    }
    match collect_pnpm() {
        Ok(pkgs) => sets.pnpm = pkgs,
        Err(note) => sets.notes.push(note),
    }
    match collect_pip() {
        Ok(pkgs) => sets.pip = pkgs,
        Err(note) => sets.notes.push(note),
    }
    match collect_cargo() {
        Ok(pkgs) => sets.cargo = pkgs,
        Err(note) => sets.notes.push(note),
    }
    sets
}

pub fn doctor_lines() -> Vec<String> {
    let tools = ["npm", "pnpm", "pip", "pip3", "cargo"];
    tools
        .iter()
        .map(|t| {
            if which(t) {
                format!("{t}: found on PATH")
            } else {
                format!("{t}: not on PATH (this is fine; that list will be skipped)")
            }
        })
        .collect()
}

fn which(tool: &str) -> bool {
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("where");
        c.arg(tool);
        c
    } else {
        let mut c = Command::new("which");
        c.arg(tool);
        c
    };
    hide_window(&mut cmd);
    cmd.stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn collect_npm() -> Result<Vec<Package>, String> {
    let json = run_tool("npm", &["list", "-g", "--depth=0", "--json"])?;
    parse_npm_like(&json, "npm")
}

fn collect_pnpm() -> Result<Vec<Package>, String> {
    let json = run_tool("pnpm", &["list", "-g", "--depth=0", "--json"])?;
    parse_npm_like(&json, "pnpm")
}

fn collect_pip() -> Result<Vec<Package>, String> {
    let json = match run_tool("pip", &["list", "--format=json"]) {
        Ok(j) => j,
        Err(_) => run_tool("pip3", &["list", "--format=json"])?,
    };
    parse_pip_list(&json)
}

fn collect_cargo() -> Result<Vec<Package>, String> {
    let text = run_tool("cargo", &["install", "--list"])?;
    Ok(parse_cargo_list(&text))
}

fn run_tool(tool: &str, args: &[&str]) -> Result<String, String> {
    if !which(tool) {
        return Err(format!("{tool} is not on PATH; skipped"));
    }
    let mut cmd = Command::new(tool);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_window(&mut cmd);
    let output = run_with_timeout(cmd, Duration::from_secs(TIMEOUT_SECS))
        .map_err(|e| format!("{tool} could not be started: {e}"))?;
    if !output.status.success() && output.stdout.is_empty() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{tool} exited with status {:?} ({})",
            output.status.code(),
            err.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run_with_timeout(mut cmd: Command, timeout: Duration) -> std::io::Result<std::process::Output> {
    let mut child = cmd.spawn()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait()? {
            Some(_) => return child.wait_with_output(),
            None if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::other("timed out"));
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn hide_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let _ = cmd;
}

/// Parse `npm list --json` / `pnpm list --json` (dependencies map, or an array).
pub fn parse_npm_like(json: &str, tool: &str) -> Result<Vec<Package>, String> {
    let value: Value = serde_json::from_str(json).map_err(|e| format!("{tool} json: {e}"))?;
    let mut pkgs = Vec::new();
    if let Some(arr) = value.as_array() {
        for item in arr {
            collect_dep_map(item.get("dependencies"), &mut pkgs);
        }
        if pkgs.is_empty() {
            collect_dep_map(Some(&value), &mut pkgs);
        }
    } else {
        collect_dep_map(value.get("dependencies"), &mut pkgs);
    }
    pkgs.sort();
    pkgs.dedup();
    Ok(pkgs)
}

fn collect_dep_map(deps: Option<&Value>, pkgs: &mut Vec<Package>) {
    let Some(Value::Object(map)) = deps else {
        return;
    };
    for (name, meta) in map {
        let version = meta
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        pkgs.push(Package {
            name: name.clone(),
            version,
        });
    }
}

pub fn parse_pip_list(json: &str) -> Result<Vec<Package>, String> {
    let value: Value = serde_json::from_str(json).map_err(|e| format!("pip json: {e}"))?;
    let mut pkgs = Vec::new();
    if let Some(arr) = value.as_array() {
        for item in arr {
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let version = item
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            if !name.is_empty() {
                pkgs.push(Package { name, version });
            }
        }
    }
    pkgs.sort();
    Ok(pkgs)
}

pub fn parse_cargo_list(text: &str) -> Vec<Package> {
    let mut pkgs = Vec::new();
    for line in text.lines() {
        if line.is_empty() || line.starts_with(char::is_whitespace) {
            continue;
        }
        let line = line.trim();
        // "ripgrep v14.1.0:" or "ripgrep 14.1.0:"
        let line = line.trim_end_matches(':').trim();
        let mut parts = line.split_whitespace();
        let Some(name) = parts.next() else { continue };
        let version = parts
            .next()
            .unwrap_or("unknown")
            .trim_start_matches('v')
            .to_string();
        pkgs.push(Package {
            name: name.to_string(),
            version,
        });
    }
    pkgs.sort();
    pkgs.dedup();
    pkgs
}

// Keep Write in scope for possible future stdin; silence via a tiny helper.
#[allow(dead_code)]
fn _sink() {
    let _ = std::io::sink().write(&[]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_json_dependencies() {
        let json = r#"{
            "dependencies": {
                "corepack": { "version": "0.29.0" },
                "npm": { "version": "10.8.2" }
            }
        }"#;
        let pkgs = parse_npm_like(json, "npm").unwrap();
        assert_eq!(pkgs.len(), 2);
        assert!(pkgs
            .iter()
            .any(|p| p.name == "npm" && p.version == "10.8.2"));
    }

    #[test]
    fn pip_json_array() {
        let json = r#"[{"name":"pip","version":"24.0"},{"name":"wheel","version":"0.44.0"}]"#;
        let pkgs = parse_pip_list(json).unwrap();
        assert_eq!(pkgs[0].name, "pip");
        assert_eq!(pkgs[1].version, "0.44.0");
    }

    #[test]
    fn cargo_install_list() {
        let text = "ripgrep v14.1.0:\n    rg\nfd-find v10.2.0:\n    fd\n";
        let pkgs = parse_cargo_list(text);
        assert_eq!(pkgs.len(), 2);
        assert!(pkgs
            .iter()
            .any(|p| p.name == "ripgrep" && p.version == "14.1.0"));
        assert!(pkgs.iter().any(|p| p.name == "fd-find"));
    }

    #[test]
    fn npm_bad_json_is_an_error() {
        assert!(parse_npm_like("not-json", "npm").is_err());
    }
}
