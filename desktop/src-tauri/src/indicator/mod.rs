//! The floating recording indicator (FR-6 to FR-11): a small always-on-top `indicator` window
//! and its setting. #236 fills this module; until then the launch hook does nothing and each
//! command answers the contract's shape without touching a window or the store.

use serde_json::{Value, json};
use tauri::{AppHandle, Runtime};

use crate::reply;

/// Called once at startup, after the tray and the link hook. Does nothing until #236.
pub fn on_launch<R: Runtime>(_app: &AppHandle<R>) {}

/// Stub until #236: the indicator is on by default.
#[tauri::command(rename_all = "snake_case")]
pub async fn indicator_settings() -> Value {
    reply::ok(json!({"enabled": true}))
}

/// Stub until #236: answers with `enabled` and stores nothing.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_indicator_enabled(enabled: bool) -> Value {
    reply::ok(json!({"enabled": enabled}))
}

/// Stub until #236: hides nothing yet.
#[tauri::command(rename_all = "snake_case")]
pub async fn indicator_dismiss() -> Value {
    reply::ok(json!({}))
}

/// Stub until #236: shows nothing yet.
#[tauri::command(rename_all = "snake_case")]
pub async fn window_open_main() -> Value {
    reply::ok(json!({}))
}
