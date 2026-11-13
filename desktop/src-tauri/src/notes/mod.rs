//! Notes: the list, one note, edits, reruns and opening a note outside the app.

use serde_json::Value;

use crate::env::Env;
use crate::reply;

pub struct Notes;

impl Notes {
    pub fn new(_env: &Env) -> Notes {
        Notes
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn notes_list() -> Value {
    reply::not_implemented("notes_list")
}

#[tauri::command(rename_all = "snake_case")]
pub fn note_get() -> Value {
    reply::not_implemented("note_get")
}

#[tauri::command(rename_all = "snake_case")]
pub fn note_update() -> Value {
    reply::not_implemented("note_update")
}

#[tauri::command(rename_all = "snake_case")]
pub fn note_rerun() -> Value {
    reply::not_implemented("note_rerun")
}

#[tauri::command(rename_all = "snake_case")]
pub fn note_open() -> Value {
    reply::not_implemented("note_open")
}
