use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};

use crate::Language;

/// Deepgram pre-recorded API client. `base_url` and `retry_delays` are fields so tests can
/// point it at a local mock server and skip the waits.
pub struct Deepgram {
    pub base_url: String,
    pub retry_delays: Vec<Duration>,
}

impl Default for Deepgram {
    fn default() -> Self {
        Self {
            base_url: "https://api.deepgram.com".into(),
            retry_delays: vec![
                Duration::from_secs(5),
                Duration::from_secs(20),
                Duration::from_secs(60),
            ],
        }
    }
}

impl Deepgram {
    /// Sends the audio for transcription and returns Deepgram's raw JSON response.
    pub fn transcribe(
        &self,
        audio: &Path,
        language: Language,
        key: &str,
    ) -> anyhow::Result<Vec<u8>> {
        let bytes = fs::read(audio).with_context(|| format!("cannot read {}", audio.display()))?;
        let lang = match language {
            Language::Bn => "bn",
            Language::En => "en",
        };
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(15 * 60)))
                .http_status_as_error(false)
                .build(),
        );
        let url = format!("{}/v1/listen", self.base_url);
        let attempts = self.retry_delays.len() + 1;

        for attempt in 1..=attempts {
            let started = Instant::now();
            let mut response = agent
                .post(&url)
                .query("model", "nova-3")
                .query("language", lang)
                .query("diarize", "true")
                .query("utterances", "true")
                .query("punctuate", "true")
                .query("smart_format", "true")
                .header("Authorization", format!("Token {key}"))
                .header("Content-Type", "audio/ogg")
                .send(&bytes[..])
                .map_err(|e| anyhow!("cannot reach Deepgram: {e}"))?;
            let status = response.status().as_u16();
            tracing::info!(
                "deepgram attempt {attempt}/{attempts}: HTTP {status} in {:?}",
                started.elapsed()
            );

            // Default read limit is 10 MB; a long meeting's word timings exceed it.
            let body = response
                .body_mut()
                .with_config()
                .limit(u64::MAX)
                .read_to_vec();
            match status {
                200..=299 => return body.context("cannot read the Deepgram response"),
                401 | 403 => bail!(
                    "Deepgram rejected the API key (HTTP {status}); run `meeting config set-key`"
                ),
                429 | 500..=599 => {
                    if let Some(delay) = self.retry_delays.get(attempt - 1) {
                        std::thread::sleep(*delay);
                    }
                }
                _ => {
                    let err_msg = body.ok().and_then(|b| {
                        serde_json::from_slice::<serde_json::Value>(&b)
                            .ok()?
                            .get("err_msg")?
                            .as_str()
                            .map(str::to_owned)
                    });
                    match err_msg {
                        Some(m) => bail!("Deepgram returned HTTP {status}: {m}"),
                        None => bail!("Deepgram returned HTTP {status}"),
                    }
                }
            }
            if attempt == attempts {
                bail!("Deepgram failed after {attempts} attempts (HTTP {status})");
            }
        }
        unreachable!("the last attempt returns above")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::mpsc;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("meeting-test-{}-{name}", std::process::id()))
    }

    /// Answers one scripted `(status, body)` per connection and sends each request's
    /// head (request line plus headers) down the returned channel.
    fn mock(responses: Vec<(u16, &'static str)>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut head = String::new();
                let mut len = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                    head.push_str(&line);
                }
                std::io::copy(&mut reader.take(len), &mut std::io::sink()).unwrap();
                tx.send(head).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        (base, rx)
    }

    fn run(responses: Vec<(u16, &'static str)>) -> (anyhow::Result<Vec<u8>>, Vec<String>) {
        let (base_url, rx) = mock(responses);
        let audio = temp("audio.ogg");
        fs::write(&audio, b"audio-bytes").unwrap();
        let client = Deepgram {
            base_url,
            retry_delays: vec![Duration::ZERO; 3],
        };
        let result = client.transcribe(&audio, Language::Bn, "test-key");
        (result, rx.try_iter().collect())
    }

    #[test]
    fn success_returns_body_and_sends_exact_request() {
        let (result, requests) = run(vec![(200, r#"{"ok":true}"#)]);
        assert_eq!(result.unwrap(), br#"{"ok":true}"#);
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with(
            "POST /v1/listen?model=nova-3&language=bn&diarize=true&utterances=true&punctuate=true&smart_format=true HTTP/1.1"
        ));
        let head = requests[0].to_lowercase();
        assert!(head.contains("authorization: token test-key"));
        assert!(head.contains("content-type: audio/ogg"));
    }

    #[test]
    fn bad_key_fails_after_one_request() {
        let (result, requests) = run(vec![(401, "{}")]);
        assert_eq!(
            result.unwrap_err().to_string(),
            "Deepgram rejected the API key (HTTP 401); run `meeting config set-key`"
        );
        assert_eq!(requests.len(), 1);
    }

    #[test]
    fn rate_limit_then_success() {
        let (result, requests) = run(vec![(429, ""), (200, "done")]);
        assert_eq!(result.unwrap(), b"done");
        assert_eq!(requests.len(), 2);
    }

    #[test]
    fn server_errors_stop_after_four_attempts() {
        let (result, requests) = run(vec![(503, ""); 4]);
        assert_eq!(
            result.unwrap_err().to_string(),
            "Deepgram failed after 4 attempts (HTTP 503)"
        );
        assert_eq!(requests.len(), 4);
    }

    #[test]
    fn other_errors_are_not_retried_and_carry_err_msg() {
        let (result, requests) = run(vec![(400, r#"{"err_msg":"Bad language"}"#)]);
        assert_eq!(
            result.unwrap_err().to_string(),
            "Deepgram returned HTTP 400: Bad language"
        );
        assert_eq!(requests.len(), 1);
    }

    #[test]
    fn other_errors_without_json_body_omit_err_msg() {
        let (result, _) = run(vec![(404, "nope")]);
        assert_eq!(
            result.unwrap_err().to_string(),
            "Deepgram returned HTTP 404"
        );
    }
}
