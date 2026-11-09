//! Panic hook: write `%LOCALAPPDATA%\leash\crash.log` (and a MessageBox in GUI mode).
//! The hook flushes the file so it still works if the profile uses `panic = "abort"`.

use leash_snapshot::default_data_dir;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::panic::{self, PanicHookInfo};
use std::sync::atomic::{AtomicBool, Ordering};

static GUI_MODE: AtomicBool = AtomicBool::new(false);
static IN_HOOK: AtomicBool = AtomicBool::new(false);

pub fn install(gui_mode: bool) {
    GUI_MODE.store(gui_mode, Ordering::SeqCst);
    panic::set_hook(Box::new(panic_hook));
}

fn panic_hook(info: &PanicHookInfo<'_>) {
    if IN_HOOK.swap(true, Ordering::SeqCst) {
        return;
    }
    let backtrace = std::backtrace::Backtrace::force_capture();
    let body = format!(
        "leash crashed\n\ntime: {}\nversion: {}\n{info}\n\nbacktrace:\n{backtrace}\n",
        chrono::Local::now().to_rfc3339(),
        env!("CARGO_PKG_VERSION"),
    );

    let dir = default_data_dir();
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("crash.log");
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)
    {
        let _ = file.write_all(body.as_bytes());
        let _ = file.flush();
        let _ = file.sync_all();
    }
    crate::debug_log::log(format!("panic: {info} (wrote {})", path.display()));

    if GUI_MODE.load(Ordering::SeqCst) {
        // Never MessageBox on the panicking (often GPUI) thread: it pumps a
        // nested message loop and can re-enter App::borrow_mut.
        let path_for_ui = path.clone();
        let summary = info.to_string();
        if let Ok(handle) = std::thread::Builder::new()
            .name("leash-panic-ui".into())
            .spawn(move || show_panic_dialog(&path_for_ui, &summary))
        {
            let _ = handle.join();
        }
    } else {
        eprintln!("{body}");
        eprintln!("details written to {}", path.display());
    }
}

fn show_panic_dialog(path: &std::path::Path, summary: &str) {
    #[cfg(windows)]
    {
        use windows::core::PCWSTR;
        use windows::Win32::UI::WindowsAndMessaging::{
            MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
        };

        let text = format!(
            "leash hit an unexpected error and has to close.\n\n{summary}\n\nA report was written to:\n{}",
            path.display()
        );
        let mut text_w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let mut title: Vec<u16> = "leash".encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let _ = MessageBoxW(
                None,
                PCWSTR(text_w.as_mut_ptr()),
                PCWSTR(title.as_mut_ptr()),
                MB_OK | MB_ICONERROR | MB_TOPMOST | MB_SETFOREGROUND,
            );
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (path, summary);
    }
}
