//! The macOS calendar: access, the upcoming events and the hidden calendars.

use serde_json::Value;
use tauri::{AppHandle, Runtime};

use crate::env::Env;
use crate::reply;

pub struct Calendar;

impl Calendar {
    pub fn new(_env: &Env) -> Calendar {
        Calendar
    }
}

/// Called once at startup.
pub fn on_launch<R: Runtime>(_app: &AppHandle<R>) {}

#[tauri::command(rename_all = "snake_case")]
pub fn calendar_status() -> Value {
    reply::not_implemented("calendar_status")
}

#[tauri::command(rename_all = "snake_case")]
pub fn calendar_events() -> Value {
    reply::not_implemented("calendar_events")
}

#[tauri::command(rename_all = "snake_case")]
pub fn calendar_event() -> Value {
    reply::not_implemented("calendar_event")
}

#[tauri::command(rename_all = "snake_case")]
pub fn calendars() -> Value {
    reply::not_implemented("calendars")
}

#[tauri::command(rename_all = "snake_case")]
pub fn set_hidden_calendars() -> Value {
    reply::not_implemented("set_hidden_calendars")
}
