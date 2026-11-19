//! Fakes shared by the tests: a scripted Deepgram server, a stub `claude` and a stub `meeting`.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

pub const VALID: &str = r#"{"title":"Standup","summary":"We met.","key_points":["a"],"decisions":["b"],"action_items":[{"task":"Ship","owner":"Rahim (inferred)","due":null,"timestamp":"00:00:03"}],"insights":[{"kind":"open_question","text":"When?"}],"speakers":[{"label":"Speaker 1","name":"Rahim","evidence":"Thanks, Rahim."}]}"#;

pub fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("meeting-test-{}-{name}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A stand-in `meeting` that records its arguments, pid, process group and the devices
/// behind its stdio, then `done` last. The stdio descriptors are copied first because a
/// redirect would replace them.
pub fn stub_meeting(dir: &Path) -> PathBuf {
    let path = dir.join("meeting");
    let script = r#"#!/bin/sh
d=$(/usr/bin/dirname "$0")
exec 3<&0 4>&1 5>&2
printf '%s\n' "$@" > "$d/args"
echo $$ > "$d/pid"
/bin/ps -o pgid= -p $$ > "$d/pgid"
/usr/bin/stat -f %r /dev/fd/3 /dev/fd/4 /dev/fd/5 > "$d/stdio"
/usr/bin/touch "$d/done"
"#;
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A stub `claude` that records its arguments and stdin, then runs `tail`.
pub fn stub(dir: &Path, tail: &str) -> PathBuf {
    let path = dir.join("claude");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{d}/args.txt\"\ncat > \"{d}/stdin.txt\"\n{tail}\n",
        d = dir.display()
    );
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

pub fn envelope(structured: &str) -> String {
    format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"result":"","structured_output":{structured}}}"#
    )
}

/// Answers one scripted `(status, body)` per connection and sends each request's
/// head (request line plus headers) down the returned channel.
pub fn mock(responses: Vec<(u16, &'static str)>) -> (String, mpsc::Receiver<String>) {
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

/// Writes each raw HTTP response, one per connection, with `{base}` replaced by this
/// server's base URL (for `Location:` redirect chains). Sends each request head down the
/// channel.
pub fn mock_raw(responses: Vec<String>) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    let own = base.clone();
    std::thread::spawn(move || {
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            while reader.read_line(&mut head).unwrap() > 2 {}
            tx.send(head).unwrap();
            stream
                .write_all(response.replace("{base}", &own).as_bytes())
                .unwrap();
        }
    });
    (base, rx)
}

/// Accepts connections and never answers them.
pub fn stall() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let _held: Vec<_> = listener.incoming().collect();
    });
    base
}
