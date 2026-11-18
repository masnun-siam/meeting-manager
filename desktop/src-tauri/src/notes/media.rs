//! The `recording://localhost/<note id>` scheme (FR-5, D9): serves a note's recording to the
//! player with Range support. The URL carries only an id; the handler resolves it in the store
//! on every request and checks the own-folder rule before reading, so the webview can never
//! name a path. Every body is at most 1 MiB (D15): Tauri's responder takes a whole body.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use meeting::config::check_recording;
use meeting::store;
use tauri::http::{Request, Response};
use tauri::{AppHandle, Manager, Runtime};

use crate::App;
use crate::env::Env;

/// The largest body one response carries (D15).
pub const MAX_BODY: u64 = 1024 * 1024;

/// A response before it becomes an `http::Response`, so tests read it without a webview.
#[derive(Debug, PartialEq)]
pub struct Served {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

impl Served {
    fn empty(status: u16) -> Served {
        Served {
            status,
            headers: vec![("Accept-Ranges", "bytes".into())],
            body: Vec::new(),
        }
    }

    fn not_found() -> Served {
        Served::empty(404)
    }

    #[cfg(test)]
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Registers the scheme. The work runs off the main thread: it opens SQLite and reads a file.
pub fn register<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder.register_asynchronous_uri_scheme_protocol("recording", |ctx, request, responder| {
        let app = ctx.app_handle().clone();
        tauri::async_runtime::spawn_blocking(move || responder.respond(respond(&app, &request)));
    })
}

/// One request against the app's state; `404` while the state is not managed yet.
pub fn respond<R: Runtime>(app: &AppHandle<R>, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let range = request
        .headers()
        .get("range")
        .map(|v| v.to_str().unwrap_or(""));
    let served = match app.try_state::<App>() {
        Some(state) => serve(
            &state.env,
            request.method().as_str(),
            request.uri().path(),
            range,
        ),
        None => Served::not_found(),
    };
    let mut response = Response::builder().status(served.status);
    for (name, value) in &served.headers {
        response = response.header(*name, value);
    }
    response.body(served.body).unwrap_or_else(|_| {
        // Unreachable with the fixed header names above; still never panic in a handler.
        let mut fallback = Response::new(Vec::new());
        *fallback.status_mut() = tauri::http::StatusCode::INTERNAL_SERVER_ERROR;
        fallback
    })
}

/// Everything the scheme answers. Logs only the id and a fixed kind, never a path or a name.
pub fn serve(env: &Env, method: &str, path: &str, range: Option<&str>) -> Served {
    if method != "GET" {
        let mut served = Served::empty(405);
        served.headers.push(("Allow", "GET".into()));
        return served;
    }
    let Some(id) = parse_id(path) else {
        return Served::not_found();
    };
    let store = match env.store() {
        Ok(store) => store,
        Err(e) => {
            tracing::warn!(id, kind = store::error_kind(&e), "recording not served");
            return Served::not_found();
        }
    };
    let row = match store.get(id) {
        Ok(Some(row)) => row,
        Ok(None) => return Served::not_found(),
        Err(e) => {
            tracing::warn!(id, kind = store::error_kind(&e), "recording not served");
            return Served::not_found();
        }
    };
    if row.recording_deleted_at.is_some() {
        return Served::not_found();
    }
    let Ok(config) = env.config() else {
        tracing::warn!(id, kind = "config", "recording not served");
        return Served::not_found();
    };
    // The error names the path: it is dropped, not logged.
    let Ok(recording) = check_recording(&config.recordings_dir, &row.recording_path) else {
        return Served::not_found();
    };
    match read(&recording, range) {
        Ok(served) => served,
        Err(e) => {
            tracing::warn!(id, kind = ?e.kind(), "recording not served");
            Served::not_found()
        }
    }
}

/// The bytes `range` asks for, at most `MAX_BODY` of them.
fn read(recording: &Path, range: Option<&str>) -> io::Result<Served> {
    let mut file = File::open(recording)?;
    let size = file.metadata()?.len();
    let (status, start, end) = match range {
        None if size <= MAX_BODY => (200, 0, size),
        None => (206, 0, MAX_BODY),
        Some(header) => match parse_range(header, size) {
            Some((start, last)) => (206, start, last + 1),
            None => {
                let mut served = Served::empty(416);
                served
                    .headers
                    .push(("Content-Range", format!("bytes */{size}")));
                return Ok(served);
            }
        },
    };
    let mut body = vec![0; (end - start) as usize];
    file.seek(SeekFrom::Start(start))?;
    file.read_exact(&mut body)?;
    let ext = recording.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mut headers = vec![
        ("Accept-Ranges", "bytes".to_string()),
        ("Content-Type", content_type(ext).into()),
        ("Content-Length", body.len().to_string()),
    ];
    if status == 206 {
        headers.push(("Content-Range", format!("bytes {start}-{}/{size}", end - 1)));
    }
    Ok(Served {
        status,
        headers,
        body,
    })
}

/// The id of `/<id>`: decimal digits only, nothing before or after.
pub fn parse_id(path: &str) -> Option<i64> {
    let digits = path.strip_prefix('/')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The first and last byte of a single `bytes=a-b` or `bytes=a-` range: the end clamped to
/// the file and capped so at most `MAX_BODY` bytes are sent. `None` (answered `416`) for a
/// start at or past the end, a suffix or multi range, or anything else.
pub fn parse_range(header: &str, size: u64) -> Option<(u64, u64)> {
    let spec = header.trim().strip_prefix("bytes=")?;
    let (first, last) = spec.split_once('-')?;
    let number = |s: &str| -> Option<u64> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    };
    let start = number(first.trim())?;
    let last = match last.trim() {
        "" => u64::MAX,
        s => number(s)?,
    };
    if start >= size || last < start {
        return None;
    }
    Some((start, last.min(size - 1).min(start + MAX_BODY - 1)))
}

/// The media type of each recording extension (`config::RECORDING_EXTENSIONS`).
pub fn content_type(ext: &str) -> &'static str {
    match ext {
        "m4a" => "audio/mp4",
        "webm" => "video/webm",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use meeting::config::{Config, RECORDING_EXTENSIONS};
    use meeting::store::{RunResult, Source, Status, Store};

    use super::*;

    /// A temp home with a config and a recordings folder; removed on drop.
    struct Fixture {
        env: Env,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.env.home);
        }
    }

    impl Fixture {
        fn new(name: &str) -> Fixture {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let home = std::env::temp_dir().join(format!(
                "meeting-desktop-media-{name}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(home.join("Recordings")).unwrap();
            let config = Config {
                recordings_dir: home.join("Recordings"),
                vault_dir: home.join("Vault"),
                vault_folder: "Meetings".into(),
                ffmpeg_path: "/opt/homebrew/bin/ffmpeg".into(),
                claude_path: "/usr/local/bin/claude".into(),
                extension_id: "abcdefghijklmnopabcdefghijklmnop".into(),
                meeting_path: None,
            };
            config
                .save_to(&home.join(".config/meeting-manager/config.toml"))
                .unwrap();
            Fixture { env: Env::at(home) }
        }

        fn store(&self) -> Store {
            self.env.store().unwrap()
        }

        fn recordings(&self) -> PathBuf {
            self.env.home.join("Recordings")
        }

        /// `recording.<ext>` holding `bytes` in its own folder.
        fn recording(&self, folder: &str, ext: &str, bytes: &[u8]) -> PathBuf {
            let dir = self.recordings().join(folder);
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("recording.{ext}"));
            fs::write(&path, bytes).unwrap();
            path
        }

        /// A done row for `path`, which need not exist or pass the own-folder check.
        fn add_row(&self, path: &Path) -> i64 {
            self.store().merge_run(path, &done()).unwrap().id
        }

        fn add(&self, folder: &str, ext: &str, bytes: &[u8]) -> (i64, PathBuf) {
            let path = self.recording(folder, ext, bytes);
            (self.add_row(&path), path)
        }

        fn get(&self, id: i64, range: Option<&str>) -> Served {
            serve(&self.env, "GET", &format!("/{id}"), range)
        }
    }

    fn done() -> RunResult {
        RunResult {
            status: Status::Done,
            failed_stage: None,
            error: None,
            title: Some("Standup".into()),
            body_md: Some("## Summary\n".into()),
            speakers: vec![],
            language: None,
            duration_s: Some(60),
            transcript: Some("[00:00:01] Speaker 1: Hi.\n".into()),
            event: None,
            source: Source::Browser,
            recorded_at: "2026-10-19T14:30".into(),
            attendees: None,
        }
    }

    fn bytes(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i % 251) as u8).collect()
    }

    fn assert_not_found(served: &Served) {
        assert_eq!(served.status, 404);
        assert!(served.body.is_empty());
        assert_eq!(served.header("Content-Type"), None);
    }

    #[test]
    fn the_id_is_decimal_digits_only() {
        assert_eq!(parse_id("/7"), Some(7));
        assert_eq!(parse_id("/0042"), Some(42));
        for bad in [
            "",
            "/",
            "7",
            "/+7",
            "/-7",
            "/7/",
            "/7/x",
            "/x",
            "/7x",
            "/ 7",
            "/1e3",
            "/../7",
            "//7",
            "/99999999999999999999",
        ] {
            assert_eq!(parse_id(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_single_range_is_clamped_and_capped() {
        assert_eq!(parse_range("bytes=0-1", 10), Some((0, 1)));
        assert_eq!(parse_range("bytes=2-", 10), Some((2, 9)));
        assert_eq!(parse_range("bytes=9-", 10), Some((9, 9)));
        assert_eq!(parse_range("bytes=3-3", 10), Some((3, 3)));
        assert_eq!(parse_range("bytes=4-100", 10), Some((4, 9)));
        let big = 5 * MAX_BODY;
        assert_eq!(parse_range("bytes=0-", big), Some((0, MAX_BODY - 1)));
        assert_eq!(
            parse_range(&format!("bytes=10-{}", big - 1), big),
            Some((10, 10 + MAX_BODY - 1))
        );
    }

    #[test]
    fn any_other_range_is_unsatisfiable() {
        for bad in [
            "bytes=10-",
            "bytes=11-12",
            "bytes=-5",
            "bytes=0-1,3-4",
            "bytes=5-3",
            "bytes=",
            "bytes=-",
            "bytes=a-b",
            "bytes=+1-2",
            "items=0-1",
            "garbage",
            "",
        ] {
            assert_eq!(parse_range(bad, 10), None, "{bad:?}");
        }
        assert_eq!(parse_range("bytes=0-", 0), None);
    }

    #[test]
    fn every_extension_has_its_type() {
        let types: Vec<_> = RECORDING_EXTENSIONS
            .iter()
            .map(|e| content_type(e))
            .collect();
        assert_eq!(
            types,
            [
                "video/webm",
                "video/mp4",
                "audio/mp4",
                "video/quicktime",
                "video/x-matroska",
                "audio/mpeg",
                "audio/wav",
                "audio/ogg",
            ]
        );
    }

    #[test]
    fn a_small_file_is_served_whole() {
        let f = Fixture::new("whole");
        for ext in RECORDING_EXTENSIONS {
            let (id, _) = f.add(&format!("m-{ext}"), ext, b"audio bytes");
            let served = f.get(id, None);
            assert_eq!(served.status, 200, "{ext}");
            assert_eq!(served.body, b"audio bytes");
            assert_eq!(served.header("Accept-Ranges"), Some("bytes"));
            assert_eq!(served.header("Content-Type"), Some(content_type(ext)));
            assert_eq!(served.header("Content-Length"), Some("11"));
            assert_eq!(served.header("Content-Range"), None);
        }
    }

    #[test]
    fn a_file_of_exactly_the_cap_is_served_whole() {
        let f = Fixture::new("cap");
        let content = bytes(MAX_BODY as usize);
        let (id, _) = f.add("m", "m4a", &content);
        let served = f.get(id, None);
        assert_eq!(served.status, 200);
        assert_eq!(served.body, content);
    }

    #[test]
    fn a_large_file_without_range_gets_the_first_mebibyte() {
        let f = Fixture::new("large");
        let content = bytes(MAX_BODY as usize + 10);
        let (id, _) = f.add("m", "webm", &content);
        let served = f.get(id, None);
        assert_eq!(served.status, 206);
        assert_eq!(served.body, &content[..MAX_BODY as usize]);
        assert_eq!(
            served.header("Content-Range"),
            Some(format!("bytes 0-1048575/{}", content.len()).as_str())
        );
        assert_eq!(served.header("Content-Length"), Some("1048576"));
        assert_eq!(served.header("Accept-Ranges"), Some("bytes"));
    }

    #[test]
    fn a_range_gets_exactly_those_bytes() {
        let f = Fixture::new("range");
        let content = bytes(1000);
        let (id, _) = f.add("m", "mp3", &content);
        let cases = [
            ("bytes=0-1", 0, 1),
            ("bytes=100-199", 100, 199),
            ("bytes=990-", 990, 999),
            ("bytes=999-", 999, 999),
            ("bytes=500-5000", 500, 999),
        ];
        for (range, first, last) in cases {
            let served = f.get(id, Some(range));
            assert_eq!(served.status, 206, "{range}");
            assert_eq!(served.body, &content[first..=last], "{range}");
            assert_eq!(
                served.header("Content-Range"),
                Some(format!("bytes {first}-{last}/1000").as_str()),
                "{range}"
            );
            assert_eq!(
                served.header("Content-Length"),
                Some((last - first + 1).to_string().as_str())
            );
            assert_eq!(served.header("Content-Type"), Some("audio/mpeg"));
            assert_eq!(served.header("Accept-Ranges"), Some("bytes"));
        }
    }

    #[test]
    fn no_body_exceeds_the_cap_whatever_the_file_size() {
        let f = Fixture::new("sparse");
        let path = f.recording("m", "mkv", b"");
        let size = 6 * MAX_BODY;
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(size)
            .unwrap();
        let id = f.add_row(&path);
        for range in [None, Some("bytes=0-"), Some("bytes=1000-5000000")] {
            let served = f.get(id, range);
            assert_eq!(served.status, 206, "{range:?}");
            assert_eq!(served.body.len() as u64, MAX_BODY, "{range:?}");
        }
        let served = f.get(id, Some("bytes=1000-5000000"));
        assert_eq!(
            served.header("Content-Range"),
            Some(format!("bytes 1000-{}/{size}", 1000 + MAX_BODY - 1).as_str())
        );
        let tail = f.get(id, Some(&format!("bytes={}-", size - 1)));
        assert_eq!(tail.body.len(), 1);
    }

    #[test]
    fn an_unsatisfiable_range_is_416() {
        let f = Fixture::new("416");
        let (id, _) = f.add("m", "wav", &bytes(10));
        for range in [
            "bytes=10-",
            "bytes=20-30",
            "bytes=-5",
            "bytes=0-1,3-4",
            "nope",
        ] {
            let served = f.get(id, Some(range));
            assert_eq!(served.status, 416, "{range}");
            assert!(served.body.is_empty());
            assert_eq!(served.header("Content-Range"), Some("bytes */10"));
        }
    }

    #[test]
    fn only_get_is_allowed() {
        let f = Fixture::new("405");
        let (id, _) = f.add("m", "ogg", b"x");
        for method in ["POST", "HEAD", "PUT", "DELETE", "OPTIONS", "get"] {
            let served = serve(&f.env, method, &format!("/{id}"), None);
            assert_eq!(served.status, 405, "{method}");
            assert_eq!(served.header("Allow"), Some("GET"));
            assert!(served.body.is_empty());
        }
    }

    #[test]
    fn a_bad_path_or_unknown_id_is_404() {
        let f = Fixture::new("ids");
        let (id, _) = f.add("m", "webm", b"x");
        assert_not_found(&f.get(id + 1, None));
        for path in [
            "/".to_string(),
            "/x".into(),
            format!("/{id}/"),
            format!("/{id}/recording.webm"),
            format!("/{id}x"),
            "/etc/passwd".into(),
        ] {
            assert_not_found(&serve(&f.env, "GET", &path, None));
        }
    }

    #[test]
    fn a_missing_file_is_404() {
        let f = Fixture::new("missing");
        let (id, path) = f.add("m", "webm", b"x");
        assert_eq!(f.get(id, None).status, 200);
        fs::remove_file(&path).unwrap();
        assert_not_found(&f.get(id, None));
    }

    #[test]
    fn a_deleted_recording_is_404_even_if_the_file_is_back() {
        let f = Fixture::new("deleted");
        let (id, _) = f.add("m", "webm", b"x");
        f.store().set_recording_deleted(id, true).unwrap();
        assert_not_found(&f.get(id, None));
        f.store().set_recording_deleted(id, false).unwrap();
        assert_eq!(f.get(id, None).status, 200);
    }

    #[test]
    fn a_row_outside_its_own_folder_is_404() {
        let f = Fixture::new("outside");
        // Outside the recordings directory.
        let outside_dir = f.env.home.join("elsewhere/x");
        fs::create_dir_all(&outside_dir).unwrap();
        let outside = outside_dir.join("recording.webm");
        fs::write(&outside, b"secret").unwrap();
        assert_not_found(&f.get(f.add_row(&outside), None));
        // Not named `recording.<ext>`.
        let dir = f.recordings().join("n");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("notes.txt"), b"secret").unwrap();
        assert_not_found(&f.get(f.add_row(&dir.join("notes.txt")), None));
        // Directly in the recordings directory, without its own folder.
        fs::write(f.recordings().join("recording.webm"), b"secret").unwrap();
        assert_not_found(&f.get(f.add_row(&f.recordings().join("recording.webm")), None));
        // A symlink out of the recordings directory.
        let linked_dir = f.recordings().join("s");
        fs::create_dir_all(&linked_dir).unwrap();
        let link = linked_dir.join("recording.webm");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert_not_found(&f.get(f.add_row(&link), None));
        // A relative path.
        assert_not_found(&f.get(f.add_row(Path::new("m/recording.webm")), None));
    }

    #[test]
    fn an_archived_note_plays_from_its_new_place() {
        let f = Fixture::new("archived");
        let (id, path) = f.add("m", "m4a", b"archived audio");
        let archived_dir = f.recordings().join("Archive/m");
        fs::create_dir_all(archived_dir.parent().unwrap()).unwrap();
        fs::rename(path.parent().unwrap(), &archived_dir).unwrap();
        assert_not_found(&f.get(id, None));
        f.store()
            .moved(&path, &archived_dir.join("recording.m4a"))
            .unwrap();
        let served = f.get(id, None);
        assert_eq!(served.status, 200);
        assert_eq!(served.body, b"archived audio");
    }

    #[test]
    fn an_unreadable_file_is_404() {
        let f = Fixture::new("unreadable");
        let (id, path) = f.add("m", "webm", b"x");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        let served = f.get(id, None);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_not_found(&served);
    }

    #[test]
    fn a_broken_store_or_config_is_404() {
        let f = Fixture::new("broken");
        let (id, _) = f.add("m", "webm", b"x");
        fs::write(f.env.config_path(), "not toml =").unwrap();
        assert_not_found(&f.get(id, None));
        for suffix in ["-wal", "-shm"] {
            let mut side = f.env.store_path.clone().into_os_string();
            side.push(suffix);
            let _ = fs::remove_file(side);
        }
        fs::write(&f.env.store_path, "this is not a database, not at all").unwrap();
        assert_not_found(&f.get(id, None));
    }
}
