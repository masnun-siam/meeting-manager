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

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    // The desktop app's store needs FTS5 (PRD D22); `bundled` must compile it in.
    #[test]
    fn bundled_sqlite_has_fts5() {
        let db = Connection::open_in_memory().unwrap();
        let used: i64 = db
            .query_row("SELECT sqlite_compileoption_used('ENABLE_FTS5')", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(used, 1);
        db.execute_batch(
            "CREATE VIRTUAL TABLE t USING fts5(a, tokenize='unicode61 remove_diacritics 2')",
        )
        .unwrap();
    }
}
