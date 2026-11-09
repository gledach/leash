//! Shared library for the leash CLI and the double-click window.

pub mod app_mode;
pub mod crash;
pub mod debug_log;
pub mod open;
pub mod ops;

#[cfg(windows)]
pub mod gui;
#[cfg(windows)]
pub mod win_shell;
