//! Audio extraction: mono 16 kHz Opus (~32 kbps) from a recording, via ffmpeg.

use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, anyhow, bail};

/// Writes `audio` only after ffmpeg exits 0, so a killed run never leaves a
/// partial file that a later run would reuse.
pub fn extract(ffmpeg: &Path, recording: &Path, audio: &Path) -> anyhow::Result<()> {
    let part = part_path(audio);
    tracing::info!(recording = %recording.display(), "extracting audio");
    let started = Instant::now();
    let output = Command::new(ffmpeg)
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(recording)
        .args([
            "-vn", "-ac", "1", "-ar", "16000", "-c:a", "libopus", "-b:a", "32k",
        ])
        // The `.part` extension doesn't tell ffmpeg the container.
        .args(["-f", "ogg"])
        .arg(&part)
        .output()
        .map_err(|e| match e.kind() {
            ErrorKind::NotFound => anyhow!(
                "ffmpeg not found at {}; run `meeting doctor`",
                ffmpeg.display()
            ),
            _ => anyhow!(e).context(format!("cannot run {}", ffmpeg.display())),
        })?;
    if !output.status.success() {
        let _ = fs::remove_file(&part);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let last = stderr.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("");
        bail!("ffmpeg failed: {}", last.trim());
    }
    fs::rename(&part, audio).with_context(|| format!("cannot write {}", audio.display()))?;
    tracing::info!(elapsed_s = started.elapsed().as_secs(), "audio extracted");
    Ok(())
}

fn part_path(audio: &Path) -> PathBuf {
    let mut name = OsString::from(audio.as_os_str());
    name.push(".part");
    name.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ffmpeg -f lavfi -i sine=frequency=440:duration=3 -f lavfi -i color=c=blue:size=320x240:duration=3 -c:v libvpx -b:v 100k -c:a libopus -shortest crates/meeting/tests/fixtures/short.webm
    const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/short.webm");

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("meeting-test-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ffmpeg() -> PathBuf {
        let out = Command::new("/usr/bin/which")
            .arg("ffmpeg")
            .output()
            .unwrap();
        assert!(out.status.success(), "install ffmpeg (brew install ffmpeg)");
        String::from_utf8(out.stdout).unwrap().trim().into()
    }

    #[test]
    fn extracts_mono_16khz_opus() {
        let dir = temp("extract-ok");
        let audio = dir.join("short.audio.ogg");
        extract(&ffmpeg(), Path::new(FIXTURE), &audio).unwrap();
        let bytes = fs::read(&audio).unwrap();
        let head = &bytes[..bytes.len().min(200)];
        let at = head.windows(8).position(|w| w == b"OpusHead").unwrap();
        let rate = u32::from_le_bytes(head[at + 12..at + 16].try_into().unwrap());
        let part_left = part_path(&audio).exists();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(head[at + 9], 1, "channels");
        assert_eq!(rate, 16000);
        assert!(!part_left);
    }

    #[test]
    fn corrupt_input_fails_and_leaves_nothing() {
        let dir = temp("extract-bad");
        let bad = dir.join("bad.webm");
        fs::write(&bad, "not a video").unwrap();
        let audio = dir.join("bad.audio.ogg");
        let err = extract(&ffmpeg(), &bad, &audio).unwrap_err();
        let left = audio.exists() || part_path(&audio).exists();
        fs::remove_dir_all(&dir).unwrap();
        assert!(err.to_string().starts_with("ffmpeg failed: "), "{err}");
        assert!(!err.to_string().contains('\n'));
        assert!(!left);
    }

    #[test]
    fn missing_ffmpeg_names_path_and_doctor() {
        let err = extract(
            Path::new("/nonexistent/ffmpeg"),
            Path::new(FIXTURE),
            Path::new("/tmp/never.audio.ogg"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("/nonexistent/ffmpeg") && err.contains("meeting doctor"));
    }
}
