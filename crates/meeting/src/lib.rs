//! The meeting pipeline as a library: the `meeting` binary and the desktop app both link it.

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

pub mod calendar;
pub mod calendar_fetch;
pub mod config;
pub mod context;
pub mod context_block;
pub mod deepgram;
pub mod doctor;
pub mod extract;
pub mod ical;
pub mod import;
pub mod install;
pub mod jobs;
pub mod lock;
pub mod logging;
pub mod meta;
pub mod native;
pub mod notify;
pub mod pipeline;
pub mod plan;
pub mod secret;
pub mod store;
pub mod summarise;
pub mod summary_md;
#[cfg(test)]
mod test_support;
pub mod transcript;
pub mod vault;

#[derive(ValueEnum, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Bn,
    En,
}
