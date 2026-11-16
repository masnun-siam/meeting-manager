# Recording import

Covers: crates/meeting/src/import.rs

PRD: [recorder/recording-import](../../prds/recorder/recording-import.md). Processing after the import: [meeting-recorder.md](meeting-recorder.md).

`meeting import <file> --language bn|en [--date "YYYY-MM-DD HH:MM"]` handles a recording made outside Chrome, such as a phone voice memo or a desktop call. It checks the file, copies it to `<recordings_dir>/meeting-<date>_<time>/recording.<ext>`, prints `imported → <path>`, then runs the pipeline above with the same stage lines, job, notification and exit code as `meeting process`.

- Accepted extensions: `webm`, `mp4`, `m4a`, `mov`, `mkv`, `mp3`, `wav`, `ogg`.
- The meeting time is `--date` (local time) or, without it, the file's modified time.
- The original is copied, never moved or changed.
- `--language` is required. A refusal or copy failure exits non-zero and writes no job; a failure after that is resumed with `meeting process <new path>`.
