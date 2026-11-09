//! Shared library for the leash CLI and the double-click window.

pub mod open;
pub mod ops;

#[cfg(windows)]
pub mod gui;
#[cfg(windows)]
pub mod win_shell;
