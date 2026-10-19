//! macOS notification when a job ends, shown through `osascript`.
//!
//! The text (a title from Claude, an error reason) is passed as an `argv`
//! argument, never spliced into the script, so it cannot inject AppleScript.

use std::process::Command;

const OSASCRIPT: &str = "/usr/bin/osascript";
const MAX_REASON_CHARS: usize = 100;

pub enum Outcome<'a> {
    Done { title: &'a str },
    Failed { stage: &'a str, reason: &'a str },
}

pub fn message(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Done { title } => format!("Meeting summary ready: {title}"),
        Outcome::Failed { stage, reason } => {
            format!("Meeting processing failed: {stage} – {}", short(reason))
        }
    }
}

/// First line of `reason`, cut to `MAX_REASON_CHARS` characters (not bytes:
/// reasons can hold Bengali text).
fn short(reason: &str) -> String {
    let line = reason.lines().next().unwrap_or_default();
    match line.char_indices().nth(MAX_REASON_CHARS) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_string(),
    }
}

pub fn osascript_args(msg: &str) -> Vec<String> {
    [
        "-e",
        "on run argv",
        "-e",
        "display notification (item 1 of argv) with title \"Meeting Manager\"",
        "-e",
        "end run",
        msg,
    ]
    .map(String::from)
    .to_vec()
}

/// Failure is logged, never returned: a missing notification must not fail the job.
pub fn notify(msg: &str) {
    match Command::new(OSASCRIPT).args(osascript_args(msg)).output() {
        Ok(out) if out.status.success() => {}
        // The status only: the message can hold the meeting title.
        Ok(out) => tracing::warn!("notification failed: osascript exited with {}", out.status),
        Err(e) => tracing::warn!("notification failed: cannot run osascript: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn done_message() {
        let m = message(&Outcome::Done {
            title: "Weekly sync",
        });
        assert_eq!(m, "Meeting summary ready: Weekly sync");
    }

    #[test]
    fn failed_message() {
        let reason = "Deepgram rejected the API key (HTTP 401); run `meeting config set-key`";
        let m = message(&Outcome::Failed {
            stage: "transcribing",
            reason,
        });
        assert_eq!(
            m,
            format!("Meeting processing failed: transcribing – {reason}")
        );
    }

    #[test]
    fn empty_title_is_still_sent() {
        assert_eq!(
            message(&Outcome::Done { title: "" }),
            "Meeting summary ready: "
        );
    }

    #[test]
    fn long_reason_is_cut_with_ellipsis() {
        let s = short(&"a".repeat(300));
        assert_eq!(s, format!("{}…", "a".repeat(100)));
        assert_eq!(short(&"a".repeat(100)), "a".repeat(100));
    }

    #[test]
    fn only_first_line_of_reason_is_kept() {
        assert_eq!(short("first\nsecond"), "first");
        assert_eq!(short(""), "");
    }

    #[test]
    fn bengali_reason_is_cut_on_a_character_boundary() {
        let s = short(&"আ".repeat(150));
        assert_eq!(s, format!("{}…", "আ".repeat(100)));
    }

    #[test]
    fn args_are_the_argv_form() {
        assert_eq!(
            osascript_args("hi"),
            [
                "-e",
                "on run argv",
                "-e",
                "display notification (item 1 of argv) with title \"Meeting Manager\"",
                "-e",
                "end run",
                "hi"
            ]
        );
    }

    #[test]
    fn hostile_title_is_only_the_last_argument() {
        let title = "\"\\' \" & do shell script \"x";
        let args = osascript_args(title);
        assert_eq!(args.last().unwrap(), title);
        assert!(
            args[..args.len() - 1]
                .iter()
                .all(|a| !a.contains("do shell"))
        );
    }
}
