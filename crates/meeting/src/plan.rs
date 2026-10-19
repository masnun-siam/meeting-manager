use std::time::SystemTime;

use anyhow::bail;

use crate::Language;

/// Modification times of a recording's files; `None` when the file is absent.
pub struct Files {
    pub webm: Option<SystemTime>,
    pub audio: Option<SystemTime>,
    pub raw: Option<SystemTime>,
    pub transcript: Option<SystemTime>,
}

/// Which stages `meeting process` runs. Summarise and write always run, so they are not here.
#[derive(Debug, PartialEq)]
pub struct Plan {
    pub language: Language,
    pub delete_transcript_and_raw: bool,
    pub extract: bool,
    pub transcribe: bool,
    pub render: bool,
}

/// An output is fresh when it exists and is not older than its input. An absent input
/// counts as fresh: inputs are deleted when a job succeeds (FR-16).
fn fresh(out: Option<SystemTime>, input: Option<SystemTime>) -> bool {
    match (out, input) {
        (Some(out), Some(input)) => out >= input,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

pub fn plan(
    files: &Files,
    stored: Option<Language>,
    retranscribe: bool,
    language: Option<Language>,
) -> anyhow::Result<Plan> {
    let Some(lang) = language.or(stored) else {
        bail!("no language stored for this recording; pass --language bn|en");
    };
    let force = retranscribe || language.is_some();
    let (raw, transcript) = if force {
        (None, None)
    } else {
        (files.raw, files.transcript)
    };
    let (extract, transcribe, render) = if fresh(transcript, raw) {
        (false, false, false)
    } else if fresh(raw, files.audio) {
        (false, false, true)
    } else if fresh(files.audio, files.webm) {
        (false, true, true)
    } else {
        (true, true, true)
    };
    Ok(Plan {
        language: lang,
        delete_transcript_and_raw: force,
        extract,
        transcribe,
        render,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn t(n: u64) -> Option<SystemTime> {
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(n))
    }

    fn files(
        webm: Option<SystemTime>,
        audio: Option<SystemTime>,
        raw: Option<SystemTime>,
        transcript: Option<SystemTime>,
    ) -> Files {
        Files {
            webm,
            audio,
            raw,
            transcript,
        }
    }

    fn expected(
        language: Language,
        delete: bool,
        extract: bool,
        transcribe: bool,
        render: bool,
    ) -> Plan {
        Plan {
            language,
            delete_transcript_and_raw: delete,
            extract,
            transcribe,
            render,
        }
    }

    #[test]
    fn fresh_recording_runs_everything() {
        let p = plan(
            &files(t(1), None, None, None),
            None,
            false,
            Some(Language::Bn),
        )
        .unwrap();
        // --language forces, which deletes nothing that exists but is still flagged.
        assert_eq!(p, expected(Language::Bn, true, true, true, true));
    }

    #[test]
    fn fresh_recording_with_stored_language_deletes_nothing() {
        let p = plan(
            &files(t(1), None, None, None),
            Some(Language::Bn),
            false,
            None,
        )
        .unwrap();
        assert_eq!(p, expected(Language::Bn, false, true, true, true));
    }

    #[test]
    fn resume_at_summarising_runs_no_stage() {
        let f = files(t(1), t(2), t(3), t(4));
        let p = plan(&f, Some(Language::En), false, None).unwrap();
        assert_eq!(p, expected(Language::En, false, false, false, false));
    }

    #[test]
    fn rerun_of_done_recording_runs_no_stage() {
        let p = plan(
            &files(t(1), None, None, t(5)),
            Some(Language::Bn),
            false,
            None,
        )
        .unwrap();
        assert_eq!(p, expected(Language::Bn, false, false, false, false));
    }

    #[test]
    fn resume_at_transcribing_skips_extract() {
        let p = plan(
            &files(t(1), t(2), None, None),
            Some(Language::Bn),
            false,
            None,
        )
        .unwrap();
        assert_eq!(p, expected(Language::Bn, false, false, true, true));
    }

    #[test]
    fn raw_newer_than_audio_renders_only() {
        let p = plan(
            &files(t(1), t(2), t(3), None),
            Some(Language::Bn),
            false,
            None,
        )
        .unwrap();
        assert_eq!(p, expected(Language::Bn, false, false, false, true));
    }

    #[test]
    fn stale_transcript_renders_again_without_deepgram() {
        let p = plan(
            &files(t(1), t(2), t(4), t(3)),
            Some(Language::Bn),
            false,
            None,
        )
        .unwrap();
        assert_eq!(p, expected(Language::Bn, false, false, false, true));
    }

    #[test]
    fn audio_older_than_webm_is_extracted_again() {
        let p = plan(
            &files(t(5), t(2), None, None),
            Some(Language::Bn),
            false,
            None,
        )
        .unwrap();
        assert_eq!(p, expected(Language::Bn, false, true, true, true));
    }

    #[test]
    fn equal_timestamps_are_fresh() {
        let p = plan(
            &files(t(2), t(2), None, None),
            Some(Language::Bn),
            false,
            None,
        )
        .unwrap();
        assert_eq!(p, expected(Language::Bn, false, false, true, true));
    }

    #[test]
    fn retranscribe_on_done_recording_extracts_again() {
        let p = plan(
            &files(t(1), None, None, t(5)),
            Some(Language::Bn),
            true,
            None,
        )
        .unwrap();
        assert_eq!(p, expected(Language::Bn, true, true, true, true));
    }

    #[test]
    fn retranscribe_keeps_fresh_audio() {
        let f = files(t(1), t(2), t(3), t(4));
        let p = plan(&f, Some(Language::Bn), true, None).unwrap();
        assert_eq!(p, expected(Language::Bn, true, false, true, true));
    }

    #[test]
    fn language_override_acts_like_retranscribe() {
        let p = plan(
            &files(t(1), None, None, t(5)),
            Some(Language::Bn),
            false,
            Some(Language::En),
        )
        .unwrap();
        assert_eq!(p, expected(Language::En, true, true, true, true));
    }

    #[test]
    fn language_equal_to_stored_still_forces() {
        let p = plan(
            &files(t(1), None, None, t(5)),
            Some(Language::Bn),
            false,
            Some(Language::Bn),
        )
        .unwrap();
        assert_eq!(p, expected(Language::Bn, true, true, true, true));
    }

    #[test]
    fn missing_language_is_an_error() {
        let e = plan(&files(t(1), None, None, None), None, false, None).unwrap_err();
        assert_eq!(
            e.to_string(),
            "no language stored for this recording; pass --language bn|en"
        );
    }
}
