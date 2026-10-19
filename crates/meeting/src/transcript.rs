use anyhow::Context;
use serde::Deserialize;

use crate::Language;

#[derive(Deserialize)]
struct Response {
    metadata: Option<Metadata>,
    results: Results,
}

#[derive(Deserialize)]
struct Metadata {
    duration: Option<f64>,
}

#[derive(Deserialize)]
struct Results {
    #[serde(default)]
    utterances: Vec<Utterance>,
}

#[derive(Deserialize, Debug, PartialEq)]
pub struct Utterance {
    pub start: f64,
    pub speaker: u32,
    pub transcript: String,
}

#[derive(Debug, PartialEq)]
pub struct Transcript {
    pub duration_s: Option<f64>,
    pub utterances: Vec<Utterance>,
}

/// Reads the fields the tool uses from a Deepgram response; blank utterances are dropped.
pub fn parse(raw: &[u8]) -> anyhow::Result<Transcript> {
    let response: Response =
        serde_json::from_slice(raw).context("cannot read Deepgram response")?;
    let mut utterances = response.results.utterances;
    utterances.retain(|u| !u.transcript.trim().is_empty());
    Ok(Transcript {
        duration_s: response.metadata.and_then(|m| m.duration),
        utterances,
    })
}

/// The text of `<name>.transcript.md`.
pub fn render(t: &Transcript, language: Language) -> String {
    let mut speakers: Vec<u32> = t.utterances.iter().map(|u| u.speaker).collect();
    speakers.sort_unstable();
    speakers.dedup();
    let language = match language {
        Language::Bn => "bn",
        Language::En => "en",
    };
    let mut out = format!(
        "# Transcript\n\n- Language: {language}\n- Speakers: {}\n\n",
        speakers.len()
    );
    if t.utterances.is_empty() {
        out.push_str("No speech detected\n");
    }
    for u in &t.utterances {
        let secs = u.start as u64;
        let text = u
            .transcript
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        out.push_str(&format!(
            "[{:02}:{:02}:{:02}] Speaker {}: {text}\n",
            secs / 3600,
            secs % 3600 / 60,
            secs % 60,
            u.speaker
        ));
    }
    out
}

/// True when the rendered transcript has at least one utterance line.
pub fn has_speech(transcript_md: &str) -> bool {
    transcript_md.lines().any(|l| l.starts_with('['))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EN: &str = include_str!("../tests/fixtures/deepgram/en_two_speakers.json");
    const BN: &str = include_str!("../tests/fixtures/deepgram/bn_two_speakers.json");
    const ONE: &str = include_str!("../tests/fixtures/deepgram/one_speaker.json");
    const EMPTY: &str = include_str!("../tests/fixtures/deepgram/empty.json");

    fn rendered(raw: &str, language: Language) -> String {
        render(&parse(raw.as_bytes()).unwrap(), language)
    }

    #[test]
    fn renders_english_fixture_exactly() {
        assert_eq!(
            rendered(EN, Language::En),
            "# Transcript\n\n- Language: en\n- Speakers: 2\n\n\
             [00:00:01] Speaker 0: Hi, thanks for joining.\n\
             [00:00:04] Speaker 1: Thanks, Rahim.\n"
        );
    }

    #[test]
    fn reads_duration_and_ignores_unknown_fields() {
        assert_eq!(parse(EN.as_bytes()).unwrap().duration_s, Some(12.5));
    }

    #[test]
    fn preserves_bengali_text() {
        let md = rendered(BN, Language::Bn);
        assert!(md.contains("- Language: bn\n"));
        assert!(md.contains("[00:00:00] Speaker 0: আসসালামু আলাইকুম, কেমন আছেন?\n"));
    }

    #[test]
    fn one_speaker() {
        let md = rendered(ONE, Language::En);
        assert!(md.contains("- Speakers: 1\n"));
        assert!(!md.contains("Speaker 1"));
    }

    #[test]
    fn timestamp_rounds_down() {
        let t = Transcript {
            duration_s: None,
            utterances: vec![Utterance {
                start: 3725.9,
                speaker: 0,
                transcript: "x".into(),
            }],
        };
        assert!(render(&t, Language::En).contains("[01:02:05] Speaker 0: x\n"));
    }

    #[test]
    fn newlines_become_spaces_and_speaker_numbers_are_kept() {
        let raw = r#"{"results":{"utterances":[{"start":0,"speaker":2,"transcript":"a\nb"}]}}"#;
        let md = rendered(raw, Language::En);
        assert!(md.contains("[00:00:00] Speaker 2: a b\n"));
    }

    #[test]
    fn empty_cases_say_no_speech() {
        let blank = r#"{"results":{"utterances":[{"start":0,"speaker":0,"transcript":"  "}]}}"#;
        for raw in [EMPTY, r#"{"results":{}}"#, blank] {
            let md = rendered(raw, Language::En);
            assert!(
                md.ends_with("- Speakers: 0\n\nNo speech detected\n"),
                "{md}"
            );
            assert!(!has_speech(&md));
        }
    }

    #[test]
    fn has_speech_on_utterance_lines() {
        assert!(has_speech(&rendered(EN, Language::En)));
    }

    #[test]
    fn bad_bodies_fail() {
        for raw in ["not json", r#"{"metadata":{}}"#] {
            let err = parse(raw.as_bytes()).unwrap_err();
            assert!(err.to_string().starts_with("cannot read Deepgram response"));
        }
    }
}
