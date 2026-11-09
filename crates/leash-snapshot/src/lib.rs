//! Snapshot model, session storage, diffs, and report rendering.
//!
//! All of this is platform-independent and is covered by unit tests on Linux.

mod diff;
mod model;
mod report;
mod store;

pub use diff::{diff_snapshots, DiffOptions, IgnoreTally};
pub use model::*;
pub use report::{render_json, render_markdown, render_text};
pub use store::{default_data_dir, SessionError, SessionPaths, SessionStore};

pub const SNAPSHOT_VERSION: u32 = 1;

#[cfg(test)]
mod tests;
