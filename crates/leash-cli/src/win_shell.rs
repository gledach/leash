//! Windows shell helpers: attach a parent console, folder picker,
//! Start-menu shortcut, and "run as administrator".

#![cfg(windows)]

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Attach to the console of the process that launched us (typically `cmd.exe`
/// or PowerShell) so CLI output still appears there. A WINDOWS-subsystem
/// exe otherwise has no stdout when started from a terminal.
///
/// If stdin/stdout/stderr are already redirected to a pipe or file, they are
/// left alone so `leash report --json > file` keeps working.
pub fn attach_parent_console() {
    use windows::Win32::Storage::FileSystem::GetFileType;
    use windows::Win32::System::Console::{
        AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
        STD_OUTPUT_HANDLE,
    };

    unsafe {
        if already_redirected(GetStdHandle(STD_OUTPUT_HANDLE).ok().map(|h| GetFileType(h)))
            || already_redirected(GetStdHandle(STD_ERROR_HANDLE).ok().map(|h| GetFileType(h)))
        {
            return;
        }
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            return;
        }
        let _ = GetStdHandle(STD_INPUT_HANDLE);
        reopen_stdio();
    }
}

fn already_redirected(kind: Option<windows::Win32::Storage::FileSystem::FILE_TYPE>) -> bool {
    use windows::Win32::Storage::FileSystem::{FILE_TYPE_DISK, FILE_TYPE_PIPE, FILE_TYPE_REMOTE};
    matches!(
        kind,
        Some(FILE_TYPE_DISK) | Some(FILE_TYPE_PIPE) | Some(FILE_TYPE_REMOTE)
    )
}

fn acrt_iob(ix: u32) -> *mut libc::FILE {
    extern "C" {
        fn __acrt_iob_func(index: u32) -> *mut libc::FILE;
    }
    unsafe { __acrt_iob_func(ix) }
}

fn reopen_stdio() {
    // rustc's std handles are still the old NUL ones; point them at CONOUT$/CONIN$.
    let out = std::ffi::CString::new("CONOUT$").unwrap();
    let inp = std::ffi::CString::new("CONIN$").unwrap();
    let w = std::ffi::CString::new("w").unwrap();
    let r = std::ffi::CString::new("r").unwrap();
    unsafe {
        // On Windows/UCRT, stdout/stderr/stdin are accessed via __acrt_iob_func.
        libc::freopen(out.as_ptr(), w.as_ptr(), acrt_iob(1));
        libc::freopen(out.as_ptr(), w.as_ptr(), acrt_iob(2));
        libc::freopen(inp.as_ptr(), r.as_ptr(), acrt_iob(0));
    }
}

/// Prompt for a folder with the common item dialog (no PowerShell).
pub fn pick_folder(owner: Option<isize>) -> Result<Option<PathBuf>> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
    };
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IFileOpenDialog, IShellItem, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST,
        FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
    };

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        let options = dialog.GetOptions()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST;
        dialog.SetOptions(options)?;
        let title: Vec<u16> = "Choose the project folder leash should ignore\0"
            .encode_utf16()
            .collect();
        dialog.SetTitle(PCWSTR(title.as_ptr()))?;
        let hwnd = HWND(owner.unwrap_or(0) as *mut _);
        let show = dialog.Show(Some(hwnd));
        if show.is_err() {
            CoUninitialize();
            return Ok(None);
        }
        let item: IShellItem = dialog.GetResult()?;
        let raw = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = PathBuf::from(raw.to_string().unwrap_or_default());
        windows::Win32::System::Com::CoTaskMemFree(Some(raw.0 as _));
        CoUninitialize();
        if path.as_os_str().is_empty() {
            Ok(None)
        } else {
            Ok(Some(path))
        }
    }
}

/// Relaunch this exe elevated (UAC). Does not wait. The unelevated process
/// should exit after a successful call so the user is not left with two windows.
pub fn relaunch_elevated() -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let exe = std::env::current_exe()?;
    let mut file: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut verb: Vec<u16> = "runas".encode_utf16().chain(std::iter::once(0)).collect();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_mut_ptr()),
        lpFile: PCWSTR(file.as_mut_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut info)?;
    }
    Ok(())
}

/// Create `%APPDATA%\Microsoft\Windows\Start Menu\Programs\leash.lnk`.
pub fn add_start_menu_shortcut() -> Result<PathBuf> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::{Interface, PCWSTR};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED, IPersistFile, COINIT_DISABLE_OLE1DDE,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

    let appdata = std::env::var_os("APPDATA").ok_or_else(|| anyhow::anyhow!("APPDATA is not set"))?;
    let dir = PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs");
    std::fs::create_dir_all(&dir)?;
    let lnk = dir.join("leash.lnk");
    let exe = std::env::current_exe()?;

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        let mut exe_w: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        link.SetPath(PCWSTR(exe_w.as_mut_ptr()))?;
        if let Some(parent) = exe.parent() {
            let mut dir_w: Vec<u16> = parent
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            link.SetWorkingDirectory(PCWSTR(dir_w.as_mut_ptr()))?;
        }
        let mut desc: Vec<u16> = "leash — see what an AI agent changed outside the project\0"
            .encode_utf16()
            .collect();
        link.SetDescription(PCWSTR(desc.as_mut_ptr()))?;
        let persist: IPersistFile = link.cast()?;
        let mut lnk_w: Vec<u16> = lnk.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        persist.Save(PCWSTR(lnk_w.as_mut_ptr()), true)?;
        CoUninitialize();
    }
    Ok(lnk)
}

pub fn start_menu_shortcut_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(
        PathBuf::from(appdata)
            .join(r"Microsoft\Windows\Start Menu\Programs")
            .join("leash.lnk"),
    )
}

/// Open a file or folder with the associated app (same as `leash open`).
pub fn open_path(path: &Path) -> Result<()> {
    crate::open::open_with_default_app(path)
}
