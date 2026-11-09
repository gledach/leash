//! Windows shell helpers: attach a parent console, folder picker,
//! Start-menu shortcut, and "run as administrator".
//!
//! COM is never initialized or uninitialized on the GPUI UI thread.
//! The folder dialog and shortcut creation run on a dedicated STA thread.

#![cfg(windows)]

use crate::app_mode::{
    classify_elevation, elevation_user_message, is_user_cancelled,
    ElevationResult,
};
use crate::debug_log;
use anyhow::{anyhow, Result};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

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
    // These literals cannot contain interior NULs; a failed CString would be a bug.
    let out = c_literal(b"CONOUT$\0");
    let inp = c_literal(b"CONIN$\0");
    let w = c_literal(b"w\0");
    let r = c_literal(b"r\0");
    unsafe {
        libc::freopen(out, w, acrt_iob(1));
        libc::freopen(out, w, acrt_iob(2));
        libc::freopen(inp, r, acrt_iob(0));
    }
}

fn c_literal(bytes: &'static [u8]) -> *const libc::c_char {
    bytes.as_ptr().cast()
}

struct ComGuard {
    uninit: bool,
}

impl ComGuard {
    fn init_sta() -> Self {
        use windows::Win32::System::Com::{
            CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
        };
        let result =
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        // S_OK / S_FALSE → we must later CoUninitialize. RPC_E_CHANGED_MODE → do not.
        Self {
            uninit: result.is_ok(),
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.uninit {
            unsafe {
                windows::Win32::System::Com::CoUninitialize();
            }
        }
    }
}

/// Run `work` on a fresh STA thread and return its result. The GPUI thread
/// is not used for COM init or for the folder dialog.
fn run_on_sta<T, F>(name: &'static str, work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let join = thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _com = ComGuard::init_sta();
            let out = work();
            let _ = tx.send(out);
        })
        .map_err(|err| anyhow!("could not start {name} thread: {err}"))?;
    let out = rx
        .recv_timeout(Duration::from_secs(15 * 60))
        .map_err(|_| anyhow!("{name} thread did not return a result"))?;
    match join.join() {
        Ok(()) => {}
        Err(payload) => {
            debug_log::log(format!("{name} thread panicked: {payload:?}"));
            return Err(anyhow!("{name} thread panicked"));
        }
    }
    Ok(out)
}

/// Prompt for a folder with the common item dialog (no PowerShell).
/// Cancel returns `Ok(None)` and never fails the process.
///
/// Runs on a dedicated STA thread so we never `CoInitialize`/`CoUninitialize`
/// on GPUI's UI thread (doing so can take the whole window down when the
/// dialog closes).
pub fn pick_folder(owner: Option<isize>) -> Result<Option<PathBuf>> {
    match run_on_sta("leash-folder-dialog", move || pick_folder_sta(owner)) {
        Ok(inner) => inner,
        Err(err) => {
            debug_log::log(format!("folder picker thread error: {err}"));
            Err(err)
        }
    }
}

/// Same picker, but intended to be started from an async task so the GPUI
/// message loop is not blocked waiting on the user.
pub fn pick_folder_async(owner: Option<isize>) -> mpsc::Receiver<Result<Option<PathBuf>>> {
    let (tx, rx) = mpsc::channel();
    let _ = thread::Builder::new()
        .name("leash-folder-dialog".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _com = ComGuard::init_sta();
                pick_folder_sta(owner)
            }))
            .unwrap_or_else(|_| {
                Err(anyhow!(
                    "the folder picker hit an unexpected error (see crash.log if present)"
                ))
            });
            if tx.send(result).is_err() {
                debug_log::log("folder picker: UI no longer listening for the result");
            }
        });
    rx
}

fn pick_folder_sta(owner: Option<isize>) -> Result<Option<PathBuf>> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IFileOpenDialog, IShellItem, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST,
        FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
    };

    debug_log::log(format!("folder picker: opening (owner hwnd={owner:?})"));
    let result = (|| -> Result<Option<PathBuf>> {
        let dialog: IFileOpenDialog =
            unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) }
                .map_err(|err| anyhow!("could not create the folder dialog: {err}"))?;
        let options = unsafe { dialog.GetOptions() }.unwrap_or_default()
            | FOS_PICKFOLDERS
            | FOS_FORCEFILESYSTEM
            | FOS_PATHMUSTEXIST;
        if let Err(err) = unsafe { dialog.SetOptions(options) } {
            return Err(anyhow!("could not configure the folder dialog: {err}"));
        }
        let title: Vec<u16> = "Choose the project folder\0".encode_utf16().collect();
        let _ = unsafe { dialog.SetTitle(PCWSTR(title.as_ptr())) };
        let hwnd = HWND(owner.unwrap_or(0) as *mut _);
        let show = unsafe { dialog.Show(Some(hwnd)) };
        if let Err(err) = show {
            let code = err.code().0 as u32;
            if is_user_cancelled(code) {
                debug_log::log(format!("folder picker: cancelled (code {code:#010x})"));
                return Ok(None);
            }
            // Some shells still return a plain failure HRESULT for Cancel.
            // Treat any Show() error as "no folder chosen", and only surface
            // unexpected codes in the log.
            debug_log::log(format!(
                "folder picker: Show failed ({code:#010x}): {err}; treating as cancel/no-op"
            ));
            return Ok(None);
        }
        let item: IShellItem = unsafe { dialog.GetResult() }
            .map_err(|err| anyhow!("could not read the chosen folder: {err}"))?;
        let raw = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }
            .map_err(|err| anyhow!("could not read the chosen folder path: {err}"))?;
        let path = PathBuf::from(unsafe { raw.to_string() }.unwrap_or_default());
        unsafe { CoTaskMemFree(Some(raw.0 as _)) };
        if path.as_os_str().is_empty() {
            Ok(None)
        } else {
            debug_log::log(format!("folder picker: chose {}", path.display()));
            Ok(Some(path))
        }
    })();
    if let Err(err) = &result {
        debug_log::log(format!("folder picker: error: {err}"));
    }
    result
}

/// Path of this process, from `GetModuleFileNameW` (more reliable for
/// ShellExecute than `std::env::current_exe` on some rustc/Windows pairs).
pub fn current_module_path() -> Result<PathBuf> {
    use windows::Win32::Foundation::GetLastError;
    use windows::Win32::System::LibraryLoader::GetModuleFileNameW;

    let mut buf = vec![0u16; 1024];
    let n = unsafe { GetModuleFileNameW(None, &mut buf) };
    if n == 0 || n as usize >= buf.len() {
        let code = unsafe { GetLastError() }.0;
        return Err(anyhow!(
            "GetModuleFileNameW failed ({})",
            crate::app_mode::win32_error_text(code)
        ));
    }
    let wide = &buf[..n as usize];
    let raw = String::from_utf16_lossy(wide);
    Ok(leash_filter::pretty_pathbuf(&PathBuf::from(&raw)))
}

/// Relaunch this exe elevated (UAC) with `--gui` so the new process opens
/// the window. Must be called on the UI thread that received the click.
///
/// Only returns [`ElevationResult::Started`] when `hProcess` is a real handle.
pub fn relaunch_elevated(owner: Option<isize>) -> ElevationResult {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, HWND};
    use windows::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let exe = match current_module_path() {
        Ok(p) => p,
        Err(err) => {
            debug_log::log(format!("elevate: cannot resolve exe path: {err}"));
            return ElevationResult::Failed {
                code: 0,
                message: format!("Windows refused to elevate: {err}"),
            };
        }
    };
    debug_log::log(format!(
        "elevate: ShellExecuteExW runas file={} params=--gui hwnd={owner:?}",
        exe.display()
    ));

    let mut file: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut verb: Vec<u16> = "runas".encode_utf16().chain(std::iter::once(0)).collect();
    let mut params: Vec<u16> = "--gui".encode_utf16().chain(std::iter::once(0)).collect();

    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        hwnd: HWND(owner.unwrap_or(0) as *mut _),
        lpVerb: PCWSTR(verb.as_mut_ptr()),
        lpFile: PCWSTR(file.as_mut_ptr()),
        lpParameters: PCWSTR(params.as_mut_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };

    // Call the raw export so we can read GetLastError and hProcess ourselves.
    // The windows-rs wrapper maps a FALSE return through `.ok()`, which is
    // fine, but we still want the raw codes in the log.
    let call = unsafe { ShellExecuteExW(&mut info) };
    let last_error = unsafe { GetLastError() }.0;
    let handle: HANDLE = info.hProcess;
    let handle_valid = !handle.is_invalid() && !handle.0.is_null();
    let success = call.is_ok();
    let hinst = info.hInstApp.0 as isize;

    debug_log::log(format!(
        "elevate: ShellExecuteExW success={success} last_error={last_error} hInstApp={hinst} hProcess_valid={handle_valid}"
    ));

    let classified = classify_elevation(success, last_error, handle_valid);
    // If the wrapper returned an error with a more specific HRESULT, prefer it
    // when GetLastError was left at 0.
    let classified = match (&classified, &call) {
        (ElevationResult::Failed { code: 0, .. }, Err(err)) => {
            let code = err.code().0 as u32;
            classify_elevation(false, code, false)
        }
        _ => classified,
    };

    debug_log::log(format!(
        "elevate: classified={classified:?} message={}",
        elevation_user_message(&classified)
    ));

    if handle_valid {
        let _ = unsafe { CloseHandle(handle) };
    }
    classified
}

/// Create `%APPDATA%\Microsoft\Windows\Start Menu\Programs\leash.lnk`.
pub fn add_start_menu_shortcut() -> Result<PathBuf> {
    run_on_sta("leash-start-menu", add_start_menu_shortcut_sta)?
}

fn add_start_menu_shortcut_sta() -> Result<PathBuf> {
    use windows::core::{Interface, PCWSTR};
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER, IPersistFile};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

    let appdata = std::env::var_os("APPDATA").ok_or_else(|| anyhow!("APPDATA is not set"))?;
    let dir = PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs");
    std::fs::create_dir_all(&dir)?;
    let lnk = dir.join("leash.lnk");
    let exe = current_module_path().or_else(|_| std::env::current_exe().map_err(|e| anyhow!(e)))?;

    let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }?;
    let mut exe_w: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    unsafe { link.SetPath(PCWSTR(exe_w.as_mut_ptr())) }?;
    if let Some(parent) = exe.parent() {
        let mut dir_w: Vec<u16> = parent
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe { link.SetWorkingDirectory(PCWSTR(dir_w.as_mut_ptr())) }?;
    }
    let mut args: Vec<u16> = "--gui".encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { link.SetArguments(PCWSTR(args.as_mut_ptr())) }?;
    let mut desc: Vec<u16> = "leash — see what an AI agent changed outside the project\0"
        .encode_utf16()
        .collect();
    unsafe { link.SetDescription(PCWSTR(desc.as_mut_ptr())) }?;
    let persist: IPersistFile = link.cast()?;
    let mut lnk_w: Vec<u16> = lnk.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    unsafe { persist.Save(PCWSTR(lnk_w.as_mut_ptr()), true) }?;
    debug_log::log(format!("start menu shortcut written to {}", lnk.display()));
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
    debug_log::log(format!("open {}", path.display()));
    crate::open::open_with_default_app(path)
}
