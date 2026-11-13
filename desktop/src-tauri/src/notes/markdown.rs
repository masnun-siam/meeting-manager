//! What the note view shows: the summary as HTML that can run nothing, and the transcript with
//! each speaker's current name.

use std::borrow::Cow;

use meeting::store::{self, SpeakerRow};
use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd, html};

/// Renders `md` to HTML. Raw HTML is shown as text (D25), and a link or image whose destination
/// is not `http`, `https` or `mailto` loses its tag, keeping only its text.
pub fn render_html(md: &str) -> String {
    // One entry per open link or image: whether its tag was dropped.
    let mut dropped: Vec<bool> = Vec::new();
    let events = Parser::new_ext(md, Options::empty()).filter_map(|event| match event {
        Event::Html(raw) | Event::InlineHtml(raw) => Some(Event::Text(raw)),
        Event::Start(Tag::Link { ref dest_url, .. } | Tag::Image { ref dest_url, .. }) => {
            let safe = is_safe_url(dest_url);
            dropped.push(!safe);
            safe.then_some(event)
        }
        Event::End(TagEnd::Link | TagEnd::Image) => {
            let was_dropped = dropped.pop().unwrap_or(false);
            (!was_dropped).then_some(event)
        }
        other => Some(other),
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

fn is_safe_url(url: &CowStr) -> bool {
    let lower = url.to_ascii_lowercase();
    ["http://", "https://", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
}

/// `transcript` with the label of each `[hh:mm:ss] <label>: <text>` line that names one of
/// `speakers` replaced by that speaker's shown name. Every other byte is kept.
pub fn shown_transcript(transcript: &str, speakers: &[SpeakerRow]) -> String {
    transcript
        .split_inclusive('\n')
        .map(|line| relabel(line, speakers))
        .collect()
}

fn relabel<'a>(line: &'a str, speakers: &[SpeakerRow]) -> Cow<'a, str> {
    let Some((clock, rest)) = line
        .strip_prefix('[')
        .and_then(|after| after.split_once("] "))
    else {
        return Cow::Borrowed(line);
    };
    if clock.is_empty() || !clock.chars().all(|c| c.is_ascii_digit() || c == ':') {
        return Cow::Borrowed(line);
    }
    for speaker in speakers {
        // The label must be followed by `: `, so `Speaker 1` never matches `Speaker 10`.
        if let Some(text) = rest
            .strip_prefix(speaker.label.as_str())
            .and_then(|after| after.strip_prefix(": "))
        {
            let name = store::shown_name(speaker);
            return Cow::Owned(format!("[{clock}] {name}: {text}"));
        }
    }
    Cow::Borrowed(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speaker(label: &str, name: Option<&str>, name_edited: bool) -> SpeakerRow {
        SpeakerRow {
            label: label.into(),
            name: name.map(Into::into),
            evidence: String::new(),
            name_edited,
        }
    }

    #[test]
    fn markdown_renders_headings_lists_and_bold() {
        let html = render_html("## Actions\n\n- **Ship** it\n");
        assert_eq!(
            html,
            "<h2>Actions</h2>\n<ul>\n<li><strong>Ship</strong> it</li>\n</ul>\n"
        );
    }

    #[test]
    fn inline_html_is_shown_as_text() {
        let html = render_html("Look <img src=x onerror=alert(1)> here");
        assert!(
            html.contains("&lt;img src=x onerror=alert(1)&gt;"),
            "{html}"
        );
        assert!(!html.contains("<img"), "{html}");
    }

    #[test]
    fn block_html_is_shown_as_text() {
        for (md, escaped, raw) in [
            ("<script>alert(1)</script>\n", "&lt;script&gt;", "<script"),
            ("<div onclick=x>hi</div>\n", "&lt;div onclick=x&gt;", "<div"),
            ("<img src=x onerror=alert(1)>\n", "&lt;img src=x", "<img"),
        ] {
            let html = render_html(md);
            assert!(html.contains(escaped), "{md}: {html}");
            assert!(!html.contains(raw), "{md}: {html}");
        }
    }

    #[test]
    fn html_inside_a_list_item_is_shown_as_text() {
        let html = render_html("- one <b onmouseover=x>two</b>\n");
        assert!(html.contains("&lt;b onmouseover=x&gt;"), "{html}");
        assert!(!html.contains("<b "), "{html}");
    }

    #[test]
    fn unsafe_links_and_images_lose_their_tag() {
        for md in [
            "[click](javascript:alert(1))",
            "[click](JavaScript:alert(1))",
            "<javascript:alert(1)>",
            "[click](data:text/html,x)",
            "[click](relative/path)",
        ] {
            let html = render_html(md);
            assert!(!html.contains("<a"), "{md}: {html}");
            assert!(!html.contains("href"), "{md}: {html}");
        }
        let html = render_html("![alt text](javascript:alert(1))");
        assert_eq!(html, "<p>alt text</p>\n");
    }

    #[test]
    fn safe_links_and_images_are_kept() {
        assert_eq!(
            render_html("[a](https://example.com) [b](mailto:a@example.com)"),
            "<p><a href=\"https://example.com\">a</a> <a href=\"mailto:a@example.com\">b</a></p>\n"
        );
        assert_eq!(
            render_html("![pic](http://example.com/a.png)"),
            "<p><img src=\"http://example.com/a.png\" alt=\"pic\" /></p>\n"
        );
    }

    #[test]
    fn an_unsafe_image_inside_a_safe_link_keeps_the_link() {
        let html = render_html("[![x](javascript:1)](https://example.com)");
        assert_eq!(html, "<p><a href=\"https://example.com\">x</a></p>\n");
    }

    #[test]
    fn transcript_labels_become_shown_names() {
        let speakers = [
            speaker("Speaker 1", Some("Rahim"), false),
            speaker("Speaker 10", None, false),
            speaker("Speaker 2", Some("Karim"), true),
        ];
        let transcript = "[00:00:01] Speaker 1: Hi, I am Speaker 1.\n\
                          [00:00:02] Speaker 10: Speaker 1: are you there?\n\
                          [00:00:03] Speaker 2: Yes.\n\
                          Speaker 1: not a line\n\
                          [note] Speaker 1: not a clock\n\
                          [00:00:04] Speaker 3: Unknown.";
        let want = "[00:00:01] Rahim (inferred): Hi, I am Speaker 1.\n\
                    [00:00:02] Speaker 10: Speaker 1: are you there?\n\
                    [00:00:03] Karim: Yes.\n\
                    Speaker 1: not a line\n\
                    [note] Speaker 1: not a clock\n\
                    [00:00:04] Speaker 3: Unknown.";
        assert_eq!(shown_transcript(transcript, &speakers), want);
    }

    #[test]
    fn a_transcript_without_speakers_is_unchanged() {
        let transcript = "[00:00:01] Speaker 1: Hi.\r\n\n";
        assert_eq!(shown_transcript(transcript, &[]), transcript);
    }
}
