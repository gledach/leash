//! Open a report file or session folder with the Windows shell.
//! Uses ShellExecuteW — no PowerShell.

use anyhow::{bail, Result};
use std::path::Path;

/// Open `path` with the association for that file type (or Explorer, for a folder).
pub fn open_with_default_app(path: &Path) -> Result<()> {
    if !path.exists() {
        bail!("nothing to open: {}", path.display());
    }
    #[cfg(windows)]
    {
        windows_shell_execute(path)
    }
    #[cfg(not(windows))]
    {
        bail!(
            "opening files is only supported on Windows (ShellExecuteW). Path: {}",
            path.display()
        )
    }
}

#[cfg(windows)]
fn windows_shell_execute(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let file: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let verb: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `file` and `verb` are null-terminated wide strings that live
    // for the duration of the call. HWND::default() is the desktop; no
    // parameters or working-directory override are passed.
    let rc = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    let code = rc.0 as isize;
    if code <= 32 {
        bail!(
            "ShellExecuteW failed to open {} (status {code})",
            path.display()
        );
    }
    Ok(())
}
