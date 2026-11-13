//! The last thing the recorder has to tell the user (D33): `recorder_status.notice`. Kinds mirror
//! the extension's notices (`extension/src/shared/messages.ts`) plus `recovered`.

use std::path::Path;

use serde::Serialize;

pub const SAVED: &str = "Saved, sent for processing";
pub const TOO_SHORT: &str = "Under 10 seconds, so it was saved but not processed.";
pub const SETUP: &str = "Processing tool not found — run `meeting install`";
pub const DISK_FULL: &str = "The disk is full, so recording stopped. The audio so far is kept: free some space, then retry.";

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Saved,
    TooShort,
    Setup,
    Error,
    /// A recording a crash left behind was converted on launch (#149).
    Recovered,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Notice {
    pub kind: Kind,
    pub message: String,
    /// The meeting folder, only when a retry makes sense (a failed conversion, a full disk).
    pub folder: Option<String>,
}

impl Notice {
    pub fn new(kind: Kind, message: impl Into<String>) -> Notice {
        Notice {
            kind,
            message: message.into(),
            folder: None,
        }
    }

    /// An `error` notice the user can retry from `folder`.
    pub fn retry(message: impl Into<String>, folder: &Path) -> Notice {
        Notice {
            kind: Kind::Error,
            message: message.into(),
            folder: Some(folder.display().to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn notices_serialise_as_the_contract_says() {
        assert_eq!(
            serde_json::to_value(Notice::new(Kind::TooShort, TOO_SHORT)).unwrap(),
            json!({"kind": "too_short", "message": TOO_SHORT, "folder": null})
        );
        assert_eq!(
            serde_json::to_value(Notice::retry("ffmpeg failed: x", Path::new("/r/m"))).unwrap(),
            json!({"kind": "error", "message": "ffmpeg failed: x", "folder": "/r/m"})
        );
        for (kind, name) in [
            (Kind::Saved, "saved"),
            (Kind::Setup, "setup"),
            (Kind::Recovered, "recovered"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), name);
        }
    }
}
