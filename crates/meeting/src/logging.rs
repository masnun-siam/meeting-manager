//! File logging to `~/Library/Logs/meeting-manager/meeting.log`.
//!
//! Never log the Deepgram key or transcript text. Never log to stdout: in
//! `native-host` mode stdout is Chrome's message channel.

use std::fs::{self, OpenOptions};
use std::sync::Mutex;

use crate::config::home;

pub fn init() {
    let file = home().and_then(|home| {
        let dir = home.join("Library/Logs/meeting-manager");
        fs::create_dir_all(&dir)?;
        Ok(OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("meeting.log"))?)
    });
    match file {
        Ok(file) => tracing_subscriber::fmt()
            .with_writer(Mutex::new(file))
            .with_ansi(false)
            .with_target(false)
            .with_max_level(tracing::Level::INFO)
            .init(),
        // A missing log must not stop a recording from being processed.
        Err(e) => eprintln!("warning: file logging is off: {e:#}"),
    }
}
