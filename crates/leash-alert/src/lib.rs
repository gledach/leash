//! Reserved for v1.5 live alerts.
//!
//! Planned hooks:
//! - Windows toast + sound via `tauri-winrt-notification` 0.8
//! - Optional user-configured ntfy / Telegram HTTP posts
//!
//! v1 does not send notifications and does not make any network call.

/// Always `false` in v1: no alert backend is compiled in.
pub fn alerts_available() -> bool {
    false
}
