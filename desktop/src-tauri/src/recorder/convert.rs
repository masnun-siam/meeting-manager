//! `recording.partial.wav` to `recording.m4a` (AAC, mono, 64 kbps) with the configured ffmpeg.
//! The output appears only after ffmpeg exits 0, as `meeting::extract` does.

use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, anyhow, bail};

pub fn to_m4a(ffmpeg: &Path, wav: &Path, m4a: &Path) -> anyhow::Result<()> {
    let part = part_path(m4a);
    let output = Command::new(ffmpeg)
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(wav)
        .args(["-vn", "-ac", "1", "-c:a", "aac", "-b:a", "64k"])
        // The `.part` extension doesn't tell ffmpeg the container; audio-only MP4 is an m4a.
        .args(["-f", "mp4"])
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
    fs::rename(&part, m4a).with_context(|| format!("cannot write {}", m4a.display()))
}

fn part_path(path: &Path) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(".part");
    name.into()
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

    use super::*;
    use crate::recorder::wav::tests::temp;
    use crate::recorder::wav::{Chunk, WavWriter};

    pub(crate) fn ffmpeg() -> PathBuf {
        let out = Command::new("/usr/bin/which")
            .arg("ffmpeg")
            .output()
            .unwrap();
        assert!(out.status.success(), "install ffmpeg (brew install ffmpeg)");
        String::from_utf8(out.stdout).unwrap().trim().into()
    }

    /// What `ffmpeg -i <file>` says about it on stderr.
    pub(crate) fn probe(file: &Path) -> String {
        let out = Command::new(ffmpeg())
            .args(["-hide_banner", "-i"])
            .arg(file)
            .args(["-f", "null", "-"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    /// A WAV from the writer holding `secs` of a 440 Hz tone.
    pub(crate) fn tone_wav(path: &Path, secs: usize) {
        let writer = WavWriter::create(
            path,
            Duration::from_secs(5),
            1024,
            Arc::new(AtomicU64::new(0)),
            Box::new(|_| {}),
        )
        .unwrap();
        let tone: Vec<i16> = (0..48_000 * secs)
            .map(|i| ((i as f64 * 440.0 * std::f64::consts::TAU / 48_000.0).sin() * 8_000.0) as i16)
            .collect();
        for chunk in tone.chunks(4_800) {
            writer
                .sender()
                .send(Chunk::Samples(chunk.to_vec()))
                .unwrap();
        }
        assert_eq!(writer.finish(), Ok((48_000 * secs) as u64));
    }

    #[test]
    fn converts_to_mono_aac_at_about_64_kbps() {
        let dir = temp("convert-ok");
        let (wav, m4a) = (dir.join("recording.partial.wav"), dir.join("recording.m4a"));
        tone_wav(&wav, 3);
        to_m4a(&ffmpeg(), &wav, &m4a).unwrap();
        assert!(!part_path(&m4a).exists());
        let info = probe(&m4a);
        assert!(info.contains("Audio: aac"), "{info}");
        assert!(info.contains("mono"), "{info}");
        let kbps: u32 = info
            .split("Audio: aac")
            .nth(1)
            .and_then(|line| line.lines().next())
            .and_then(|line| {
                line.split(", ").find_map(|p| {
                    p.split_once(" kb/s")
                        .and_then(|(n, _)| n.trim().parse().ok())
                })
            })
            .unwrap_or_else(|| panic!("no bitrate in {info}"));
        assert!((56..=72).contains(&kbps), "{kbps} kb/s");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_corrupt_wav_fails_and_leaves_nothing() {
        let dir = temp("convert-corrupt");
        let (wav, m4a) = (dir.join("recording.partial.wav"), dir.join("recording.m4a"));
        fs::write(&wav, b"not audio at all").unwrap();
        let error = to_m4a(&ffmpeg(), &wav, &m4a).unwrap_err().to_string();
        assert!(error.starts_with("ffmpeg failed: "), "{error}");
        assert!(!m4a.exists());
        assert!(!part_path(&m4a).exists());
        assert!(wav.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_ffmpeg_names_the_path_and_the_fix() {
        let dir = temp("convert-missing");
        let missing = dir.join("no-ffmpeg");
        let error = to_m4a(&missing, &dir.join("a.wav"), &dir.join("a.m4a"))
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            format!(
                "ffmpeg not found at {}; run `meeting doctor`",
                missing.display()
            )
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
