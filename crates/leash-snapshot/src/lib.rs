//! Snapshot model, session storage, diffs, and report rendering.
//!
//! All of this is platform-independent and is covered by unit tests on Linux.

mod diff;
mod html;
mod model;
mod report;
mod store;

pub use diff::{diff_snapshots, DiffOptions, IgnoreTally};
pub use html::{escape_html, render_html, verdict_line};
pub use model::*;
pub use report::{render_json, render_markdown, render_summary, render_text};
pub use store::{default_data_dir, SessionError, SessionPaths, SessionStore};

pub const SNAPSHOT_VERSION: u32 = 1;

#[cfg(test)]
mod tests;
