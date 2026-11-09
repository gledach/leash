//! Snapshot model, session storage, diffs, and report rendering.
//!
//! All of this is platform-independent and is covered by unit tests on Linux.

mod diff;
mod html;
mod model;
mod report;
mod prefs;
mod session_list;
mod store;

pub use diff::{diff_snapshots, DiffOptions, IgnoreTally};
pub use html::{escape_html, render_html, verdict_line};
pub use model::*;
pub use report::{aggregate_finding_lines, render_json, render_markdown, render_summary, render_text, summary_finding_line};
pub use prefs::{resolve_repo_path, PrefsError, UiPrefs, UiTheme};
pub use session_list::{
    format_elapsed, format_elapsed_since, new_session_id, now_local_rfc3339, parse_rfc3339,
    SessionInfo, SessionStatus,
};
pub use store::{default_data_dir, SessionError, SessionPaths, SessionStore};

pub const SNAPSHOT_VERSION: u32 = 1;

#[cfg(test)]
mod tests;
