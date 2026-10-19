//! Fakes shared by the tests: a scripted Deepgram server and a stub `claude`.

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
