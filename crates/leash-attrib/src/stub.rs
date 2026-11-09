//! Non-Windows placeholders.

use crate::model::TraceLog;
use std::path::Path;

#[allow(dead_code)]
pub fn signal_stop_event(_session_dir: &Path) {}

#[allow(dead_code)]
pub fn run_collector(_session_dir: &Path, _log: &mut TraceLog) -> anyhow::Result<()> {
    anyhow::bail!("ETW attribution is only available on Windows")
}
