//! Recording: start, pause, resume, stop, and the stops macOS forces (sleep, quit).

use serde_json::Value;
use tauri::{AppHandle, Runtime};

use crate::env::Env;
use crate::reply;

pub struct Recorder;

impl Recorder {
    pub fn new(_env: &Env) -> Recorder {
        Recorder
    }
}

/// Called once at startup.
pub fn on_launch<R: Runtime>(_app: &AppHandle<R>) {}

/// Called once at startup; registers for the Mac going to sleep.
pub fn on_sleep<R: Runtime>(_app: &AppHandle<R>) {}

/// Called when the app is asked to quit. `false` keeps it running.
pub fn on_exit_requested<R: Runtime>(_app: &AppHandle<R>) -> bool {
    true
}

#[tauri::command(rename_all = "snake_case")]
pub fn recorder_status() -> Value {
    reply::not_implemented("recorder_status")
}

#[tauri::command(rename_all = "snake_case")]
pub fn recorder_start() -> Value {
    reply::not_implemented("recorder_start")
}

#[tauri::command(rename_all = "snake_case")]
pub fn recorder_pause() -> Value {
    reply::not_implemented("recorder_pause")
}

#[tauri::command(rename_all = "snake_case")]
pub fn recorder_resume() -> Value {
    reply::not_implemented("recorder_resume")
}

#[tauri::command(rename_all = "snake_case")]
pub fn recorder_stop() -> Value {
    reply::not_implemented("recorder_stop")
}

#[tauri::command(rename_all = "snake_case")]
pub fn recorder_retry() -> Value {
    reply::not_implemented("recorder_retry")
}
