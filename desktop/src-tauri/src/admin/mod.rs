//! Housekeeping: rescan and backfill, health, archive, trash and the settings page's facts.

use serde_json::Value;
use tauri::{AppHandle, Runtime};

use crate::env::Env;
use crate::reply;

pub struct Admin;

impl Admin {
    pub fn new(_env: &Env) -> Admin {
        Admin
    }
}

/// Called once at startup.
pub fn on_launch<R: Runtime>(_app: &AppHandle<R>) {}

#[tauri::command(rename_all = "snake_case")]
pub fn rescan() -> Value {
    reply::not_implemented("rescan")
}

#[tauri::command(rename_all = "snake_case")]
pub fn rescan_status() -> Value {
    reply::not_implemented("rescan_status")
}

#[tauri::command(rename_all = "snake_case")]
pub fn health() -> Value {
    reply::not_implemented("health")
}

#[tauri::command(rename_all = "snake_case")]
pub fn note_archive() -> Value {
    reply::not_implemented("note_archive")
}

#[tauri::command(rename_all = "snake_case")]
pub fn note_trash() -> Value {
    reply::not_implemented("note_trash")
}

#[tauri::command(rename_all = "snake_case")]
pub fn settings_info() -> Value {
    reply::not_implemented("settings_info")
}
