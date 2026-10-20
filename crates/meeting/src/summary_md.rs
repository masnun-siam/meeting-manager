use std::fmt::Write;

use crate::summarise::{ActionItem, InsightKind, SpeakerGuess, Summary};

const INFERRED: &str = "(inferred)";

/// Renders the body of `summary.md`, which is also the body of the vault note.
pub fn render(s: &Summary) -> String {
    let mut out = format!("# {}\n\n{}\n", s.title, s.summary);
    section(&mut out, "Key points", &s.key_points, |p| format!("- {p}"));
    section(&mut out, "Decisions", &s.decisions, |d| format!("- {d}"));
    section(&mut out, "Action items", &s.action_items, |a| {
        action_item(a, &s.speakers)
    });
    section(&mut out, "Insights", &s.insights, |i| {
        format!("- **{}:** {}", insight_label(i.kind), i.text)
    });
    section(&mut out, "Speakers", &s.speakers, speaker);
    out
}

/// The `speakers` frontmatter list of the vault note: inferred names marked, unnamed ones by label.
pub fn speaker_names(s: &Summary) -> Vec<String> {
    s.speakers
        .iter()
        .map(|g| g.name.as_deref().map_or_else(|| g.label.clone(), mark))
        .collect()
}

fn section<T>(out: &mut String, heading: &str, items: &[T], line: impl Fn(&T) -> String) {
    let _ = write!(out, "\n## {heading}\n\n");
    if items.is_empty() {
        out.push_str("None.\n");
    }
    for item in items {
        let _ = writeln!(out, "{}", line(item));
    }
}

/// Claude may already have written the mark, so it is never added twice.
fn mark(name: &str) -> String {
    if name.ends_with(INFERRED) {
        name.to_owned()
    } else {
        format!("{name} {INFERRED}")
    }
}

fn action_item(a: &ActionItem, speakers: &[SpeakerGuess]) -> String {
    let inferred = speakers.iter().any(|g| g.name.as_deref() == Some(&a.owner));
    let owner = if inferred {
        mark(&a.owner)
    } else {
        a.owner.clone()
    };
    let due = a
        .due
        .as_ref()
        .map_or_else(String::new, |d| format!(", due {d}"));
    format!("- [ ] {} — {owner}{due} ({})", a.task, a.timestamp)
}

fn insight_label(kind: InsightKind) -> &'static str {
    match kind {
        InsightKind::Risk => "Risk",
        InsightKind::Disagreement => "Disagreement",
        InsightKind::OpenQuestion => "Open question",
        InsightKind::Signal => "Signal",
    }
}

fn speaker(g: &SpeakerGuess) -> String {
    let Some(name) = &g.name else {
        return format!("- {}: unknown", g.label);
    };
    let evidence = g.evidence.replace(['\r', '\n'], " ");
    let evidence = if evidence.starts_with('"') {
        evidence
    } else {
        format!("\"{evidence}\"")
    };
    format!("- {}: {}, {evidence}", g.label, mark(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::summarise::Insight;

    fn item(owner: &str, due: Option<&str>) -> ActionItem {
        ActionItem {
            task: "Send the deck".into(),
            owner: owner.into(),
            due: due.map(Into::into),
            timestamp: "00:12:31".into(),
        }
    }

    fn guess(label: &str, name: Option<&str>, evidence: &str) -> SpeakerGuess {
        SpeakerGuess {
            label: label.into(),
            name: name.map(Into::into),
            evidence: evidence.into(),
        }
    }

    fn fixture() -> Summary {
        Summary {
            title: "Weekly sync".into(),
            summary: "We met.".into(),
            key_points: vec!["Launch is on track".into()],
            decisions: vec!["Ship Friday".into()],
            action_items: vec![
                item("Rahim (inferred)", Some("Friday")),
                ActionItem {
                    task: "Book the room".into(),
                    owner: "unassigned".into(),
                    due: None,
                    timestamp: "00:20:02".into(),
                },
            ],
            insights: [
                (InsightKind::Risk, "Late"),
                (InsightKind::Disagreement, "Scope"),
                (InsightKind::OpenQuestion, "Budget?"),
                (InsightKind::Signal, "Keen"),
            ]
            .map(|(kind, text)| Insight {
                kind,
                text: text.into(),
            })
            .into(),
            speakers: vec![
                guess("Speaker 0", Some("Rahim"), "\"Thanks, Rahim.\" at 00:00:04"),
                guess("Speaker 1", None, ""),
            ],
        }
    }

    #[test]
    fn renders_the_full_layout() {
        let want = "# Weekly sync

We met.

## Key points

- Launch is on track

## Decisions

- Ship Friday

## Action items

- [ ] Send the deck — Rahim (inferred), due Friday (00:12:31)
- [ ] Book the room — unassigned (00:20:02)

## Insights

- **Risk:** Late
- **Disagreement:** Scope
- **Open question:** Budget?
- **Signal:** Keen

## Speakers

- Speaker 0: Rahim (inferred), \"Thanks, Rahim.\" at 00:00:04
- Speaker 1: unknown
";
        assert_eq!(render(&fixture()), want);
    }

    #[test]
    fn empty_lists_render_none() {
        let s = Summary {
            key_points: vec![],
            decisions: vec![],
            action_items: vec![],
            insights: vec![],
            speakers: vec![],
            ..fixture()
        };
        assert_eq!(render(&s).matches("\nNone.\n").count(), 5);
    }

    #[test]
    fn owner_is_marked_once() {
        let speakers = [guess("Speaker 0", Some("Rahim"), "x")];
        for (owner, want) in [
            ("Rahim", "Rahim (inferred)"),
            ("Rahim (inferred)", "Rahim (inferred)"),
            ("unassigned", "unassigned"),
            ("Speaker 1", "Speaker 1"),
        ] {
            let line = action_item(&item(owner, None), &speakers);
            assert_eq!(line, format!("- [ ] Send the deck — {want} (00:12:31)"));
        }
    }

    #[test]
    fn evidence_is_quoted_and_kept_on_one_line() {
        let line = speaker(&guess("Speaker 0", Some("Rahim"), "said\nhi"));
        assert_eq!(line, "- Speaker 0: Rahim (inferred), \"said hi\"");
    }

    #[test]
    fn speaker_names_match_the_body() {
        assert_eq!(speaker_names(&fixture()), ["Rahim (inferred)", "Speaker 1"]);
    }
}
