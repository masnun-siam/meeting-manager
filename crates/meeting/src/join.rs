//! Continue recording: `recording-part-N.<ext>` files (N >= 2) joined onto `recording.<ext>`
//! by ffmpeg stream copy. `recording.<ext>` is replaced only by a complete file, and a journal
//! in `meta.json` (`joining_parts`) makes a crash at any point safe to run again.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, anyhow, bail};

use crate::meta::Meta;

const PREFIX: &str = "recording-part-";

/// N of `recording-part-<N>.<ext>`: N >= 2, digits only, no leading zero, so each part has
/// one name. The one parser of a part name; the host's path checks use it too.
pub fn part_number(file_name: &str, ext: &str) -> Option<u32> {
    split_part(file_name).and_then(|(n, e)| (e == ext).then_some(n))
}

/// N and the extension of `recording-part-<N>.<extension>`, whatever the extension.
fn split_part(file_name: &str) -> Option<(u32, &str)> {
    let (digits, ext) = file_name.strip_prefix(PREFIX)?.split_once('.')?;
    let canonical = digits.bytes().all(|b| b.is_ascii_digit()) && !digits.starts_with('0');
    if !canonical || ext.is_empty() || ext.contains('.') {
        return None;
    }
    digits.parse().ok().filter(|n| *n >= 2).map(|n| (n, ext))
}

/// The parts next to `recording`, in ascending N. A part in another format, or one that is
/// not a regular file (a symlink could lead out of the folder), is an error.
pub fn parts_of(recording: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let ext = ext_of(recording)?;
    let folder = recording.parent().context("the recording has no folder")?;
    let entries =
        fs::read_dir(folder).with_context(|| format!("cannot read {}", folder.display()))?;
    let mut parts = vec![];
    for entry in entries {
        let entry = entry.with_context(|| format!("cannot read {}", folder.display()))?;
        let name = entry.file_name();
        let Some((n, part_ext)) = name.to_str().and_then(split_part) else {
            continue;
        };
        let name = name.to_string_lossy();
        if part_ext != ext {
            bail!(
                "part {name} is .{part_ext}, the recording {} is .{ext}; remove or convert the part",
                file_name(recording)
            );
        }
        let kind = fs::symlink_metadata(entry.path())
            .with_context(|| format!("cannot read {name}"))?
            .file_type();
        if !kind.is_file() {
            bail!("part {name} is not a regular file; replace it with the recorded file");
        }
        parts.push((n, entry.path()));
    }
    parts.sort_by_key(|(n, _)| *n);
    Ok(parts.into_iter().map(|(_, path)| path).collect())
}

/// Joins every part onto `recording` and deletes them. Returns how many parts were joined,
/// counting those a crashed run had already joined; 0 when there was nothing to do. Saves
/// `meta.json` itself, as the journal.
pub fn join(ffmpeg: &Path, recording: &Path, meta: &mut Meta) -> anyhow::Result<usize> {
    let started = Instant::now();
    let temp = beside(recording, ".part");
    let list = beside(recording, ".join.txt");
    let recovered = recover(recording, meta, &temp, &list)?;

    let parts = parts_of(recording)?;
    if parts.is_empty() {
        return Ok(recovered);
    }
    let names: Vec<String> = parts.iter().map(|p| file_name(p)).collect();
    let written = write_joined(ffmpeg, recording, &names, &temp, &list).and_then(|()| {
        meta.joining_parts = names.clone();
        save(meta, recording)
    });
    if let Err(e) = written {
        meta.joining_parts.clear();
        let _ = fs::remove_file(&temp);
        let _ = fs::remove_file(&list);
        return Err(e);
    }
    // The journal is on disk: from here a crash is finished by `recover`.
    fs::rename(&temp, recording)
        .with_context(|| format!("cannot replace {}", recording.display()))?;
    remove(&list)?;
    for part in &parts {
        remove(part)?;
    }
    meta.joining_parts.clear();
    meta.parts_joined += parts.len() as u32;
    save(meta, recording)?;
    tracing::info!(
        parts = parts.len(),
        elapsed_s = started.elapsed().as_secs(),
        "parts joined"
    );
    Ok(recovered + parts.len())
}

/// Finishes or undoes a join a crash interrupted, by the journal. With the temp file still
/// there the rename never happened: the original is intact, so the temp file goes and the
/// join is redone. Without it the rename happened: the listed parts are already in the
/// recording, so they are deleted, never joined again. Returns the parts finished this way.
fn recover(recording: &Path, meta: &mut Meta, temp: &Path, list: &Path) -> anyhow::Result<usize> {
    if meta.joining_parts.is_empty() {
        return Ok(0);
    }
    let names = std::mem::take(&mut meta.joining_parts);
    if temp.exists() {
        remove(temp)?;
        remove(list)?;
        save(meta, recording)?;
        return Ok(0);
    }
    let ext = ext_of(recording)?;
    // A name from the file is trusted only as a part of this recording.
    let listed: Vec<&String> = (names.iter())
        .filter(|name| part_number(name, ext).is_some())
        .collect();
    remove(list)?;
    for name in &listed {
        remove(&recording.with_file_name(name))?;
    }
    meta.parts_joined += listed.len() as u32;
    save(meta, recording)?;
    tracing::info!(parts = listed.len(), "interrupted join finished");
    Ok(listed.len())
}

/// Writes the joined recording to `temp`, complete and on disk, with the original's
/// permission bits.
fn write_joined(
    ffmpeg: &Path,
    recording: &Path,
    parts: &[String],
    temp: &Path,
    list: &Path,
) -> anyhow::Result<()> {
    let ext = ext_of(recording)?;
    let muxer = match ext {
        "webm" => "webm",
        "m4a" => "ipod",
        _ => bail!("cannot join .{ext} parts; only webm and m4a recordings are continued"),
    };
    // Names only, relative to the list file, so ffmpeg's safe mode accepts them.
    let lines: String = std::iter::once(file_name(recording))
        .chain(parts.iter().cloned())
        .map(|name| format!("file '{name}'\n"))
        .collect();
    // The concat demuxer skips a part it cannot open and still exits 0, so each part is
    // opened first. `-xerror` is no way out: it also fails m4a joins on AAC's harmless
    // non-monotonic DTS.
    let folder = recording.parent().context("the recording has no folder")?;
    for part in parts {
        ffmpeg_run(ffmpeg, |c| {
            c.arg("-i")
                .arg(folder.join(part))
                .args(["-t", "0", "-map", "0", "-c", "copy", "-f", "null", "-"])
        })?;
    }
    fs::write(list, lines).with_context(|| format!("cannot write {}", list.display()))?;
    ffmpeg_run(ffmpeg, |c| {
        c.args(["-y", "-f", "concat", "-i"])
            .arg(list)
            // The `.part` extension doesn't tell ffmpeg the container.
            .args(["-c", "copy", "-f", muxer])
            .arg(temp)
    })?;
    let mode = fs::metadata(recording)
        .with_context(|| format!("cannot read {}", recording.display()))?
        .permissions();
    fs::set_permissions(temp, mode).with_context(|| format!("cannot write {}", temp.display()))?;
    File::open(temp)
        .and_then(|f| f.sync_all())
        .with_context(|| format!("cannot write {}", temp.display()))
}

/// Runs ffmpeg quietly with `args`; errors as `extract::extract` reports them.
fn ffmpeg_run(
    ffmpeg: &Path,
    args: impl FnOnce(&mut Command) -> &mut Command,
) -> anyhow::Result<()> {
    let mut command = Command::new(ffmpeg);
    command.args(["-nostdin", "-hide_banner", "-loglevel", "error"]);
    let output = args(&mut command).output().map_err(|e| match e.kind() {
        ErrorKind::NotFound => anyhow!(
            "ffmpeg not found at {}; run `meeting doctor`",
            ffmpeg.display()
        ),
        _ => anyhow!(e).context(format!("cannot run {}", ffmpeg.display())),
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let last = stderr.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("");
        bail!("ffmpeg failed: {}", last.trim());
    }
    Ok(())
}

fn save(meta: &Meta, recording: &Path) -> anyhow::Result<()> {
    meta.save(&Meta::path_for(recording))
}

fn ext_of(recording: &Path) -> anyhow::Result<&str> {
    recording
        .extension()
        .and_then(|e| e.to_str())
        .with_context(|| format!("{} has no extension", recording.display()))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn beside(recording: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(recording.as_os_str());
    name.push(suffix);
    name.into()
}

fn remove(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != ErrorKind::NotFound => {
            Err(e).with_context(|| format!("cannot remove {}", path.display()))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;
    use crate::Language;

    const WEBM: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/short.webm");
    const M4A: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/short.m4a");

    /// `<tmp>/<name>/m/recording.<ext>` from `fixture`; the extra level gives `../x` a target
    /// inside the test's own directory.
    struct Folder {
        root: PathBuf,
        dir: PathBuf,
        recording: PathBuf,
    }

    impl Folder {
        fn new(name: &str, fixture: &str, ext: &str) -> Folder {
            let root = std::env::temp_dir()
                .join(format!("meeting-test-{}-join-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            let dir = root.join("m");
            fs::create_dir_all(&dir).unwrap();
            let recording = dir.join(format!("recording.{ext}"));
            fs::copy(fixture, &recording).unwrap();
            Folder {
                root,
                dir,
                recording,
            }
        }

        fn add(&self, name: &str, fixture: &str) {
            fs::copy(fixture, self.dir.join(name)).unwrap();
        }

        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(&self.dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }

        fn saved(&self) -> Option<Meta> {
            Meta::load(&self.dir.join("meta.json")).unwrap()
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn meta(recording: &Path) -> Meta {
        Meta {
            recording: recording.into(),
            language: Language::En,
            recorded_at: "2026-10-19T14:30".into(),
            duration_s: None,
            title: None,
            speakers: vec![],
            runs: vec![],
            parts_joined: 0,
            joining_parts: vec![],
        }
    }

    fn ffmpeg() -> PathBuf {
        let out = Command::new("/usr/bin/which")
            .arg("ffmpeg")
            .output()
            .unwrap();
        assert!(out.status.success(), "install ffmpeg (brew install ffmpeg)");
        String::from_utf8(out.stdout).unwrap().trim().into()
    }

    /// Seconds from the `Duration:` line ffmpeg prints for `path`.
    fn duration(path: &Path) -> f64 {
        let out = Command::new(ffmpeg())
            .args(["-hide_banner", "-i"])
            .arg(path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stderr);
        let at = text.find("Duration: ").expect("ffmpeg prints a duration");
        let hms = &text[at + 10..at + 21];
        let parts: Vec<f64> = hms.split(':').map(|p| p.parse().unwrap()).collect();
        parts[0] * 3600.0 + parts[1] * 60.0 + parts[2]
    }

    fn about(secs: f64, expected: f64) -> bool {
        (secs - expected).abs() < 0.5
    }

    #[test]
    fn part_numbers_are_digits_from_2_without_leading_zeros() {
        assert_eq!(part_number("recording-part-2.webm", "webm"), Some(2));
        assert_eq!(part_number("recording-part-10.m4a", "m4a"), Some(10));
        for name in [
            "recording-part-1.webm",
            "recording-part-0.webm",
            "recording-part-02.webm",
            "recording-part-x.webm",
            "recording-part-.webm",
            "recording-part-2.m4a",
            "recording-part-2.partial.wav",
            "recording-part-2.webm.part",
            "recording-parts.webm",
            "recording-part-99999999999.webm",
            "../recording-part-2.webm",
        ] {
            assert_eq!(part_number(name, "webm"), None, "{name}");
        }
    }

    #[test]
    fn parts_sort_by_number_not_by_name() {
        let f = Folder::new("order", WEBM, "webm");
        for n in [10, 3, 9, 2] {
            fs::write(f.dir.join(format!("recording-part-{n}.webm")), "").unwrap();
        }
        let names: Vec<String> = (parts_of(&f.recording).unwrap().iter())
            .map(|p| file_name(p))
            .collect();
        assert_eq!(
            names,
            [
                "recording-part-2.webm",
                "recording-part-3.webm",
                "recording-part-9.webm",
                "recording-part-10.webm",
            ]
        );
    }

    #[test]
    fn joins_two_webm_files_and_deletes_the_part() {
        let f = Folder::new("two", WEBM, "webm");
        f.add("recording-part-2.webm", WEBM);
        let mut m = meta(&f.recording);
        assert_eq!(join(&ffmpeg(), &f.recording, &mut m).unwrap(), 1);
        assert!(about(duration(&f.recording), 6.0));
        assert_eq!(f.names(), ["meta.json", "recording.webm"]);
        assert_eq!((m.parts_joined, m.joining_parts.len()), (1, 0));
        assert_eq!(f.saved(), Some(m));
        let saved = fs::read_to_string(f.dir.join("meta.json")).unwrap();
        assert!(!saved.contains("joining_parts"));
    }

    #[test]
    fn joins_three_webm_and_three_m4a_files() {
        for (ext, fixture) in [("webm", WEBM), ("m4a", M4A)] {
            let f = Folder::new(&format!("three-{ext}"), fixture, ext);
            f.add(&format!("recording-part-3.{ext}"), fixture);
            f.add(&format!("recording-part-2.{ext}"), fixture);
            let mut m = meta(&f.recording);
            assert_eq!(join(&ffmpeg(), &f.recording, &mut m).unwrap(), 2, "{ext}");
            assert!(about(duration(&f.recording), 9.0), "{ext}");
            assert_eq!(f.names(), ["meta.json".into(), format!("recording.{ext}")]);
            assert_eq!(m.parts_joined, 2);
        }
    }

    #[test]
    fn no_parts_touches_nothing() {
        let f = Folder::new("none", WEBM, "webm");
        let look_alikes = [
            "recording-part-1.webm",
            "recording-part-x.webm",
            "recording-part-2.partial.wav",
            "recording-part-2.webm.part",
            "recording-parts.webm",
        ];
        for name in look_alikes {
            fs::write(f.dir.join(name), "x").unwrap();
        }
        let before = f.names();
        let mut m = meta(&f.recording);
        assert_eq!(join(&ffmpeg(), &f.recording, &mut m).unwrap(), 0);
        assert_eq!(f.names(), before);
        assert_eq!(fs::read(&f.recording).unwrap(), fs::read(WEBM).unwrap());
        assert_eq!(m, meta(&f.recording));
    }

    /// A failed join leaves the recording's bytes and the part, and no temp, list or meta.
    fn assert_untouched(f: &Folder, part: &str) {
        assert_eq!(fs::read(&f.recording).unwrap(), fs::read(WEBM).unwrap());
        assert_eq!(f.names(), [part, "recording.webm"]);
        assert_eq!(f.saved(), None);
    }

    #[test]
    fn a_corrupt_part_fails_and_changes_nothing() {
        let f = Folder::new("corrupt", WEBM, "webm");
        fs::write(f.dir.join("recording-part-2.webm"), "not a video").unwrap();
        let mut m = meta(&f.recording);
        let err = join(&ffmpeg(), &f.recording, &mut m)
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("ffmpeg failed: "), "{err}");
        assert!(!err.contains('\n'));
        assert_untouched(&f, "recording-part-2.webm");
        assert_eq!(m, meta(&f.recording));
    }

    #[test]
    fn a_part_in_another_format_is_refused() {
        let f = Folder::new("format", WEBM, "webm");
        f.add("recording-part-2.m4a", M4A);
        let err = join(&ffmpeg(), &f.recording, &mut meta(&f.recording))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("recording-part-2.m4a") && err.contains("recording.webm"),
            "{err}"
        );
        assert_untouched(&f, "recording-part-2.m4a");
    }

    #[test]
    fn a_symlinked_part_is_refused() {
        let f = Folder::new("symlink", WEBM, "webm");
        let outside = f.root.join("outside.webm");
        fs::copy(WEBM, &outside).unwrap();
        symlink(&outside, f.dir.join("recording-part-2.webm")).unwrap();
        let err = join(&ffmpeg(), &f.recording, &mut meta(&f.recording))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a regular file"), "{err}");
        assert_untouched(&f, "recording-part-2.webm");
        assert!(outside.exists());
    }

    #[test]
    fn missing_ffmpeg_names_path_and_doctor() {
        let f = Folder::new("no-ffmpeg", WEBM, "webm");
        f.add("recording-part-2.webm", WEBM);
        let err = join(
            Path::new("/nonexistent/ffmpeg"),
            &f.recording,
            &mut meta(&f.recording),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            err,
            "ffmpeg not found at /nonexistent/ffmpeg; run `meeting doctor`"
        );
        assert_untouched(&f, "recording-part-2.webm");
    }

    #[test]
    fn other_formats_are_not_joined() {
        let f = Folder::new("wav", WEBM, "wav");
        fs::write(f.dir.join("recording-part-2.wav"), "x").unwrap();
        let err = join(&ffmpeg(), &f.recording, &mut meta(&f.recording))
            .unwrap_err()
            .to_string();
        assert!(err.contains("only webm and m4a"), "{err}");
        assert_eq!(f.names(), ["recording-part-2.wav", "recording.wav"]);
    }

    #[test]
    fn the_joined_file_keeps_the_permission_bits() {
        for mode in [0o600, 0o644] {
            let f = Folder::new(&format!("mode-{mode:o}"), WEBM, "webm");
            f.add("recording-part-2.webm", WEBM);
            fs::set_permissions(&f.recording, fs::Permissions::from_mode(mode)).unwrap();
            join(&ffmpeg(), &f.recording, &mut meta(&f.recording)).unwrap();
            let got = fs::metadata(&f.recording).unwrap().permissions().mode() & 0o777;
            assert_eq!(got, mode);
        }
    }

    #[test]
    fn a_crash_before_the_rename_is_joined_afresh() {
        for marked in [false, true] {
            let f = Folder::new(&format!("crash-1-{marked}"), WEBM, "webm");
            f.add("recording-part-2.webm", WEBM);
            // A complete-looking temp file: joined again, the audio would be doubled.
            fs::copy(WEBM, f.dir.join("recording.webm.part")).unwrap();
            fs::write(f.dir.join("recording.webm.join.txt"), "file 'x'\n").unwrap();
            let mut m = meta(&f.recording);
            if marked {
                m.joining_parts = vec!["recording-part-2.webm".into()];
                m.save(&f.dir.join("meta.json")).unwrap();
            }
            assert_eq!(join(&ffmpeg(), &f.recording, &mut m).unwrap(), 1);
            assert!(about(duration(&f.recording), 6.0), "marked {marked}");
            assert_eq!(f.names(), ["meta.json", "recording.webm"]);
            assert_eq!((m.parts_joined, m.joining_parts.len()), (1, 0));
            assert_eq!(f.saved(), Some(m));
        }
    }

    #[test]
    fn a_crash_after_the_rename_deletes_the_parts_without_joining_them_again() {
        let f = Folder::new("crash-2", WEBM, "webm");
        f.add("recording-part-2.webm", WEBM);
        join(&ffmpeg(), &f.recording, &mut meta(&f.recording)).unwrap();
        let joined = duration(&f.recording);
        // The state a crash between the rename and the deletion leaves.
        f.add("recording-part-2.webm", WEBM);
        let mut m = meta(&f.recording);
        m.joining_parts = vec!["recording-part-2.webm".into()];
        m.save(&f.dir.join("meta.json")).unwrap();

        assert_eq!(join(&ffmpeg(), &f.recording, &mut m).unwrap(), 1);
        assert_eq!(duration(&f.recording), joined);
        assert!(about(joined, 6.0));
        assert_eq!(f.names(), ["meta.json", "recording.webm"]);
        assert_eq!((m.parts_joined, m.joining_parts.len()), (1, 0));
        assert_eq!(f.saved(), Some(m));
    }

    #[test]
    fn a_journal_name_that_is_not_a_part_is_never_deleted() {
        let f = Folder::new("journal", WEBM, "webm");
        fs::write(f.root.join("x"), "x").unwrap();
        let mut m = meta(&f.recording);
        m.joining_parts = [
            "../x",
            "meta.json",
            "recording.webm",
            "recording-part-2.m4a",
        ]
        .map(String::from)
        .to_vec();
        m.save(&f.dir.join("meta.json")).unwrap();
        assert_eq!(join(&ffmpeg(), &f.recording, &mut m).unwrap(), 0);
        assert!(f.root.join("x").exists());
        assert_eq!(f.names(), ["meta.json", "recording.webm"]);
        assert_eq!((m.parts_joined, m.joining_parts.len()), (0, 0));
        assert_eq!(f.saved(), Some(m));
    }
}
