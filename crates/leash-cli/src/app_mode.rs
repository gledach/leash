//! Decide whether this process should open the window or run the CLI.

use std::ffi::OsStr;

/// True when the process was started with no arguments, or with `--gui`
/// and no CLI subcommand. Elevated relaunch passes `--gui` so the window
/// still opens even if Windows adds extra argv noise.
pub fn wants_gui<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let rest: Vec<_> = args.into_iter().skip(1).collect();
    if rest.is_empty() {
        return true;
    }
    if rest.iter().any(|a| is_help_or_version(a.as_ref())) {
        return false;
    }
    let has_gui = rest.iter().any(|a| a.as_ref() == "--gui");
    if !has_gui {
        return false;
    }
    !rest.iter().any(|a| is_cli_subcommand(a.as_ref()))
}

fn is_cli_subcommand(arg: &OsStr) -> bool {
    matches!(
        arg.to_str(),
        Some("start" | "stop" | "report" | "last" | "open" | "doctor" | "snap" | "diff" | "help")
    )
}

fn is_help_or_version(arg: &OsStr) -> bool {
    matches!(
        arg.to_str(),
        Some("--help" | "-h" | "--version" | "-V")
    )
}

/// Win32 `ERROR_CANCELLED` and the HRESULT `HRESULT_FROM_WIN32` form.
pub const ERROR_CANCELLED: u32 = 1223;
pub const HRESULT_ERROR_CANCELLED: u32 = 0x8007_04C7;

pub fn is_user_cancelled(code: u32) -> bool {
    if code == ERROR_CANCELLED || code == HRESULT_ERROR_CANCELLED {
        return true;
    }
    // HRESULT_FROM_WIN32(ERROR_CANCELLED) is 0x800704C7; also accept a
    // packed Win32 facility code with the same low word.
    (code & 0xFFFF) == ERROR_CANCELLED && (code & 0xFFFF_0000) == 0x8007_0000
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElevationResult {
    Started,
    Cancelled,
    Failed { code: u32, message: String },
}

/// Classify a `ShellExecuteExW("runas")` outcome without calling Win32.
/// `last_error` is `GetLastError` (or an HRESULT if that is all we have).
pub fn classify_elevation(
    success: bool,
    last_error: u32,
    process_handle_valid: bool,
) -> ElevationResult {
    if success && process_handle_valid {
        return ElevationResult::Started;
    }
    let code = if last_error == 0 && !(success && process_handle_valid) {
        // Success bit set but no process handle, or a bare failure.
        last_error
    } else {
        last_error
    };
    if is_user_cancelled(code) {
        return ElevationResult::Cancelled;
    }
    if success && !process_handle_valid {
        return ElevationResult::Failed {
            code,
            message: "Windows refused to elevate: no process handle was returned".into(),
        };
    }
    ElevationResult::Failed {
        code,
        message: format!("Windows refused to elevate: {msg}", msg = win32_error_text(code)),
    }
}

pub fn elevation_user_message(result: &ElevationResult) -> String {
    match result {
        ElevationResult::Started => "Restarting as administrator…".into(),
        ElevationResult::Cancelled => {
            "Admin was cancelled, still running without admin".into()
        }
        ElevationResult::Failed { message, .. } => message.clone(),
    }
}

pub fn win32_error_text(code: u32) -> String {
    let win = if is_hresult(code) { code & 0xFFFF } else { code };
    match win {
        0 => format!("unknown error (code {code:#010x})"),
        2 => "file not found (2)".into(),
        5 => "access denied (5)".into(),
        1223 => "the operation was cancelled (1223)".into(),
        other => format!("error {other} ({code:#010x})"),
    }
}

fn is_hresult(code: u32) -> bool {
    code & 0x8000_0000 != 0
}

/// Strip a Windows `\\?\` prefix so ShellExecute gets a normal path.
pub fn shell_friendly_path(path: &str) -> &str {
    path.strip_prefix(r"\\?\")
        .map(|rest| {
            // `\\?\UNC\server\share` → `\server\share` is wrong; leave those alone.
            if rest.starts_with(r"UNC\") {
                path
            } else {
                rest
            }
        })
        .unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_args_opens_gui() {
        assert!(wants_gui(["leash.exe"]));
        assert!(wants_gui(["/path/to/leash"]));
    }

    #[test]
    fn gui_flag_opens_gui() {
        assert!(wants_gui(["leash.exe", "--gui"]));
        assert!(wants_gui(["leash.exe", "--gui"]));
    }

    #[test]
    fn cli_subcommands_stay_cli() {
        for cmd in [
            "start", "stop", "report", "last", "open", "doctor", "snap", "diff", "help",
        ] {
            assert!(!wants_gui(["leash.exe", cmd]), "{cmd}");
        }
        assert!(!wants_gui(["leash.exe", "--help"]));
        assert!(!wants_gui(["leash.exe", "-h"]));
        assert!(!wants_gui(["leash.exe", "--version"]));
        assert!(!wants_gui(["leash.exe", "start", "--gui"]));
        assert!(!wants_gui(["leash.exe", "--gui", "stop"]));
    }

    #[test]
    fn cancelled_codes_are_detected() {
        assert!(is_user_cancelled(ERROR_CANCELLED));
        assert!(is_user_cancelled(HRESULT_ERROR_CANCELLED));
        assert!(!is_user_cancelled(0));
        assert!(!is_user_cancelled(5));
    }

    #[test]
    fn classify_elevation_outcomes() {
        assert_eq!(
            classify_elevation(true, 0, true),
            ElevationResult::Started
        );
        assert_eq!(
            classify_elevation(false, 1223, false),
            ElevationResult::Cancelled
        );
        assert_eq!(
            classify_elevation(false, HRESULT_ERROR_CANCELLED, false),
            ElevationResult::Cancelled
        );
        match classify_elevation(true, 0, false) {
            ElevationResult::Failed { .. } => {}
            other => panic!("expected failed, got {other:?}"),
        }
        let failed = classify_elevation(false, 5, false);
        let msg = elevation_user_message(&failed);
        assert!(msg.contains("Windows refused to elevate"), "{msg}");
        assert!(msg.contains("access denied"), "{msg}");
        assert_eq!(
            elevation_user_message(&ElevationResult::Cancelled),
            "Admin was cancelled, still running without admin"
        );
    }

    #[test]
    fn shell_friendly_strips_dos_device_prefix() {
        assert_eq!(
            shell_friendly_path(r"\\?\C:\Tools\leash.exe"),
            r"C:\Tools\leash.exe"
        );
        assert_eq!(shell_friendly_path(r"C:\Tools\leash.exe"), r"C:\Tools\leash.exe");
        assert_eq!(
            shell_friendly_path(r"\\?\UNC\server\share\leash.exe"),
            r"\\?\UNC\server\share\leash.exe"
        );
    }
}
