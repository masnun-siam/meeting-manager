//! Copy a note to the macOS general pasteboard (FR-1, FR-2, D10): the note without its transcript
//! (D1), as markdown plain text, or as HTML with that markdown as the plain-text fallback.

use std::path::Path;

use meeting::Language;
use meeting::store::{self, Row, Source};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeHTML, NSPasteboardTypeString};
use objc2_foundation::NSString;
use serde::Deserialize;
use serde_json::json;

use super::markdown::render_html;
use super::{Notes, Reply, find, open_store};
use crate::env::Env;
use crate::reply;

#[derive(Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CopyFormat {
    Markdown,
    Rich,
}

/// Puts the note on the pasteboard. Only the note id and an error kind are logged; the error
/// message holds the writer's reason, never note text.
pub fn copy(env: &Env, notes: &Notes, id: i64, format: CopyFormat) -> Reply {
    let store = open_store(env)?;
    let row = find(&store, id)?;
    let md = markdown(&row);
    let html = match format {
        CopyFormat::Markdown => None,
        CopyFormat::Rich => Some(render_html(&md)),
    };
    (notes.pasteboard)(&md, html.as_deref()).map_err(|reason| {
        tracing::warn!(id, ?format, "clipboard write failed");
        reply::err(
            "clipboard_failed",
            format!("cannot write the clipboard: {reason}"),
        )
    })?;
    tracing::info!(id, ?format, "note copied");
    Ok(reply::ok(json!({})))
}

/// FR-1's markdown: heading, facts line (and Event line), summary, speakers, "My notes", one
/// blank line between blocks and a trailing newline. Never the transcript (D1).
pub fn markdown(row: &Row) -> String {
    let heading = format!(
        "# {}",
        row.title.clone().unwrap_or_else(|| folder_name(row))
    );
    let mut facts = facts_line(row);
    if let Some(event) = &row.event {
        facts.push_str(&format!("\nEvent: {} · {}", event.title, event.start));
    }
    let mut blocks = vec![heading, facts];
    if let Some(body) = row.body_md.as_deref().filter(|b| !b.trim().is_empty()) {
        blocks.push(body.trim_end_matches('\n').to_string());
    }
    if !row.speakers.is_empty() {
        let lines: Vec<String> = row
            .speakers
            .iter()
            .map(|s| format!("- {}", store::shown_name(s)))
            .collect();
        blocks.push(format!("## Speakers\n\n{}", lines.join("\n")));
    }
    if !row.my_notes.trim().is_empty() {
        blocks.push(format!(
            "## My notes\n\n{}",
            row.my_notes.trim_end_matches('\n')
        ));
    }
    let mut out = blocks.join("\n\n");
    out.push('\n');
    out
}

/// The meeting folder's name, as the notes list shows it for a note without a title.
fn folder_name(row: &Row) -> String {
    row.recording_path
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `<date and time> · <duration> · <language> · <source>`, unknown parts left out. The formats
/// are the notes list's (D15, `desktop/src/notes/list/format.ts`).
fn facts_line(row: &Row) -> String {
    let parts = [
        Some(recorded(&row.recorded_at)),
        row.duration_s.map(duration),
        row.language.map(|l| language(l).to_string()),
        Some(source(row.source).to_string()),
    ];
    parts.into_iter().flatten().collect::<Vec<_>>().join(" · ")
}

/// `YYYY-MM-DDTHH:MM` as `YYYY-MM-DD HH:MM`; anything else as stored.
fn recorded(at: &str) -> String {
    let b = at.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    let shaped = b.len() == 16
        && digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && b[10] == b'T'
        && digits(11..13)
        && b[13] == b':'
        && digits(14..16);
    if shaped {
        format!("{} {}", &at[..10], &at[11..])
    } else {
        at.to_string()
    }
}

/// `h:mm:ss` from an hour up, `m:ss` below it.
fn duration(seconds: i64) -> String {
    let total = seconds.max(0);
    let (h, m, s) = (total / 3600, total % 3600 / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn language(language: Language) -> &'static str {
    match language {
        Language::Bn => "Bengali",
        Language::En => "English",
    }
}

fn source(source: Source) -> &'static str {
    match source {
        Source::Browser => "Browser",
        Source::Import => "Import",
        Source::Desktop => "Desktop",
    }
}

/// Replaces the general pasteboard's content with `plain` and, when given, `html`.
pub fn write_pasteboard(plain: &str, html: Option<&str>) -> Result<(), String> {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    // SAFETY: AppKit's constant pasteboard type, a valid `NSString` for the process's life.
    let string_type = unsafe { NSPasteboardTypeString };
    let mut written = pasteboard.setString_forType(&NSString::from_str(plain), string_type);
    if let Some(html) = html {
        // SAFETY: as above, AppKit's constant HTML pasteboard type.
        let html_type = unsafe { NSPasteboardTypeHTML };
        written &= pasteboard.setString_forType(&NSString::from_str(html), html_type);
    }
    if written {
        Ok(())
    } else {
        Err("the pasteboard refused the content".into())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use meeting::store::{Edit, EventLink, RunResult, RunStart, SpeakerRow, Status};

    use super::*;

    /// A temp home with a store and a recordings folder; removed on drop.
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
                "meeting-desktop-copy-{name}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&home).unwrap();
            Fixture { env: Env::at(home) }
        }

        fn recording(&self, folder: &str) -> PathBuf {
            let dir = self.env.home.join("Recordings").join(folder);
            fs::create_dir_all(&dir).unwrap();
            dir.join("recording.webm")
        }

        fn add(&self, result: RunResult) -> i64 {
            let recording = self.recording("2026-10-19 1430");
            let store = self.env.store().unwrap();
            store.merge_run(&recording, &result).unwrap().id
        }
    }

    fn speaker(label: &str, name: Option<&str>, name_edited: bool) -> SpeakerRow {
        SpeakerRow {
            label: label.into(),
            name: name.map(Into::into),
            evidence: String::new(),
            name_edited,
        }
    }

    /// A done row: edited title, one renamed and one inferred speaker, "My notes".
    fn done_row() -> Row {
        Row {
            id: 1,
            recording_path: "/r/2026-10-19 1430/recording.webm".into(),
            source: Source::Browser,
            recorded_at: "2026-10-19T14:30".into(),
            duration_s: Some(754),
            language: Some(Language::En),
            status: Status::Done,
            failed_stage: None,
            error: None,
            title: Some("Weekly sync".into()),
            title_edited: true,
            body_md: Some("## Decisions\n\n- Ship it.\n".into()),
            body_edited: false,
            speakers: vec![
                speaker("Speaker 0", Some("Rahim Uddin"), true),
                speaker("Speaker 1", Some("Karim"), false),
            ],
            my_notes: "Call back Friday.".into(),
            transcript: Some("[00:00:01] Speaker 0: SECRET TRANSCRIPT\n".into()),
            event: None,
            vault_note_path: None,
            created_at: String::new(),
            updated_at: String::new(),
            attendees: None,
            recording_deleted_at: None,
        }
    }

    /// A failed row with only "My notes".
    fn failed_row() -> Row {
        Row {
            status: Status::Failed,
            failed_stage: Some("summarising".into()),
            error: Some("claude failed".into()),
            title: None,
            title_edited: false,
            body_md: None,
            speakers: vec![],
            duration_s: None,
            language: None,
            source: Source::Desktop,
            ..done_row()
        }
    }

    #[test]
    fn a_done_note_copies_in_fr1_order_without_the_transcript() {
        assert_eq!(
            markdown(&done_row()),
            "# Weekly sync\n\n\
             2026-10-19 14:30 · 12:34 · English · Browser\n\n\
             ## Decisions\n\n- Ship it.\n\n\
             ## Speakers\n\n- Rahim Uddin\n- Karim (inferred)\n\n\
             ## My notes\n\nCall back Friday.\n"
        );
    }

    #[test]
    fn a_failed_note_with_only_my_notes_has_heading_facts_and_my_notes() {
        assert_eq!(
            markdown(&failed_row()),
            "# 2026-10-19 1430\n\n2026-10-19 14:30 · Desktop\n\n## My notes\n\nCall back Friday.\n"
        );
    }

    #[test]
    fn empty_blocks_are_left_out() {
        let row = Row {
            my_notes: "  \n".into(),
            body_md: Some("\n".into()),
            ..failed_row()
        };
        assert_eq!(
            markdown(&row),
            "# 2026-10-19 1430\n\n2026-10-19 14:30 · Desktop\n"
        );
    }

    #[test]
    fn the_event_line_follows_the_facts_line_when_linked() {
        let row = Row {
            event: Some(EventLink {
                occurrence_id: "o".into(),
                series_id: "s".into(),
                title: "Team standup".into(),
                start: "2026-10-19T14:30:00+06:00".into(),
                end: "2026-10-19T15:00:00+06:00".into(),
                calendar: None,
            }),
            duration_s: Some(3_725),
            language: Some(Language::Bn),
            source: Source::Import,
            ..done_row()
        };
        let md = markdown(&row);
        assert!(
            md.starts_with(
                "# Weekly sync\n\n2026-10-19 14:30 · 1:02:05 · Bengali · Import\n\
                 Event: Team standup · 2026-10-19T14:30:00+06:00\n\n## Decisions"
            ),
            "{md}"
        );
    }

    #[test]
    fn the_facts_line_formats_match_the_notes_list() {
        assert_eq!(recorded("2026-10-19T09:05"), "2026-10-19 09:05");
        assert_eq!(recorded("odd value"), "odd value");
        assert_eq!(duration(0), "0:00");
        assert_eq!(duration(59), "0:59");
        assert_eq!(duration(3_600), "1:00:00");
        assert_eq!(duration(-5), "0:00");
    }

    thread_local! {
        static WRITTEN: RefCell<Vec<(String, Option<String>)>> = const { RefCell::new(Vec::new()) };
    }

    fn record_write(plain: &str, html: Option<&str>) -> Result<(), String> {
        WRITTEN.with(|w| w.borrow_mut().push((plain.into(), html.map(Into::into))));
        Ok(())
    }

    fn refuse_write(_: &str, _: Option<&str>) -> Result<(), String> {
        Err("the pasteboard refused the content".into())
    }

    fn written() -> Vec<(String, Option<String>)> {
        WRITTEN.with(|w| w.take())
    }

    fn fakes(pasteboard: fn(&str, Option<&str>) -> Result<(), String>) -> Notes {
        Notes {
            open: |_| Ok(()),
            spawn: |_, _, _, _| Ok(()),
            pasteboard,
        }
    }

    fn done_result(body: &str) -> RunResult {
        let speaker = |label: &str, name: &str| (label.into(), Some(name.into()), "e".into());
        RunResult {
            status: Status::Done,
            failed_stage: None,
            error: None,
            title: Some("Standup".into()),
            body_md: Some(body.into()),
            speakers: vec![speaker("Speaker 0", "Rahim"), speaker("Speaker 1", "Karim")],
            language: Some(Language::En),
            duration_s: Some(60),
            transcript: Some("[00:00:01] Speaker 0: SECRET TRANSCRIPT\n".into()),
            event: None,
            source: Source::Browser,
            recorded_at: "2026-10-19T14:30".into(),
            attendees: None,
        }
    }

    #[test]
    fn markdown_format_writes_the_stored_edits_as_plain_text_only() {
        let f = Fixture::new("plain");
        let id = f.add(done_result("Body.\n"));
        let store = f.env.store().unwrap();
        store.edit(id, Edit::Title("Weekly sync".into())).unwrap();
        let rename = Edit::Speaker {
            label: "Speaker 0".into(),
            name: "Rahim Uddin".into(),
        };
        store.edit(id, rename).unwrap();
        store.edit(id, Edit::MyNotes("mine".into())).unwrap();

        let reply = copy(&f.env, &fakes(record_write), id, CopyFormat::Markdown).unwrap();
        assert_eq!(reply, json!({"ok": true}));
        let [(plain, html)] = written().try_into().unwrap();
        assert_eq!(html, None);
        assert_eq!(
            plain,
            "# Weekly sync\n\n2026-10-19 14:30 · 1:00 · English · Browser\n\nBody.\n\n\
             ## Speakers\n\n- Rahim Uddin\n- Karim (inferred)\n\n## My notes\n\nmine\n"
        );
        assert!(!plain.contains("SECRET TRANSCRIPT"));
    }

    #[test]
    fn rich_format_writes_safe_html_and_the_markdown_fallback() {
        let f = Fixture::new("rich");
        let id = f.add(done_result("Hi <b>x</b> **team**\n"));
        copy(&f.env, &fakes(record_write), id, CopyFormat::Rich).unwrap();
        let [(plain, html)] = written().try_into().unwrap();
        let html = html.unwrap();
        assert_eq!(render_html(&plain), html);
        assert!(plain.contains("Hi <b>x</b> **team**"), "{plain}");
        assert!(html.contains("<h1>Standup</h1>"), "{html}");
        assert!(html.contains("<ul>"), "{html}");
        assert!(html.contains("&lt;b&gt;x&lt;/b&gt;"), "{html}");
        assert!(!html.contains("<b>"), "{html}");
        assert!(html.contains("<strong>team</strong>"), "{html}");
    }

    #[test]
    fn every_status_copies() {
        let f = Fixture::new("statuses");
        let store = f.env.store().unwrap();
        let recording = f.recording("2026-10-19 1500");
        let run = RunStart {
            recording: &recording,
            source: Source::Desktop,
            recorded_at: "2026-10-19T15:00",
            language: None,
            attendees: None,
        };
        let processing = store.start_run(&run).unwrap();
        let no_speech = f.add(RunResult {
            status: Status::NoSpeech,
            title: None,
            body_md: None,
            speakers: vec![],
            transcript: None,
            ..done_result("")
        });
        for id in [processing, no_speech] {
            copy(&f.env, &fakes(record_write), id, CopyFormat::Rich).unwrap();
        }
        let plains: Vec<String> = written().into_iter().map(|(plain, _)| plain).collect();
        assert_eq!(
            plains,
            [
                "# 2026-10-19 1500\n\n2026-10-19 15:00 · Desktop\n",
                "# 2026-10-19 1430\n\n2026-10-19 14:30 · 1:00 · English · Browser\n",
            ]
        );
    }

    #[test]
    fn an_unknown_note_writes_nothing() {
        let f = Fixture::new("unknown");
        let id = f.add(done_result("Body.\n"));
        let error = copy(&f.env, &fakes(record_write), id + 1, CopyFormat::Rich).unwrap_err();
        assert_eq!(error["code"], "unknown_note");
        assert!(written().is_empty());
    }

    #[test]
    fn an_unopenable_store_is_store_unavailable() {
        let f = Fixture::new("corrupt");
        fs::create_dir_all(f.env.store_path.parent().unwrap()).unwrap();
        fs::write(&f.env.store_path, "this is not a database, not at all").unwrap();
        let error = copy(&f.env, &fakes(record_write), 1, CopyFormat::Markdown).unwrap_err();
        assert_eq!(error["code"], "store_unavailable");
        assert!(written().is_empty());
    }

    #[test]
    fn a_refused_write_is_clipboard_failed_without_note_text() {
        let f = Fixture::new("refused");
        let id = f.add(done_result("Secret plans\n"));
        let error = copy(&f.env, &fakes(refuse_write), id, CopyFormat::Rich).unwrap_err();
        assert_eq!(
            error,
            json!({
                "ok": false,
                "code": "clipboard_failed",
                "message": "cannot write the clipboard: the pasteboard refused the content",
            })
        );
    }

    #[test]
    fn the_format_deserialises_from_its_wire_values() {
        let rich: CopyFormat = serde_json::from_value(json!("rich")).unwrap();
        assert_eq!(rich, CopyFormat::Rich);
        let md: CopyFormat = serde_json::from_value(json!("markdown")).unwrap();
        assert_eq!(md, CopyFormat::Markdown);
        assert!(serde_json::from_value::<CopyFormat>(json!("html")).is_err());
    }

    /// Writes the real general pasteboard (replacing what is on it):
    /// `cargo test -p meeting-desktop notes::copy -- --ignored`.
    #[test]
    #[ignore]
    fn the_real_pasteboard_holds_both_flavours() {
        let html = "<h1>Copy test</h1>\n";
        write_pasteboard("# Copy test\n", Some(html)).unwrap();
        let pasteboard = NSPasteboard::generalPasteboard();
        // SAFETY: AppKit's constant pasteboard types.
        let (string_type, html_type) = unsafe { (NSPasteboardTypeString, NSPasteboardTypeHTML) };
        let read = |t| pasteboard.stringForType(t).map(|s| s.to_string());
        assert_eq!(read(string_type).as_deref(), Some("# Copy test\n"));
        assert_eq!(read(html_type).as_deref(), Some(html));

        write_pasteboard("plain only\n", None).unwrap();
        assert_eq!(read(string_type).as_deref(), Some("plain only\n"));
        assert_eq!(read(html_type), None);
    }
}
