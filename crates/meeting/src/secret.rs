use std::io::{IsTerminal, Write, stdin};
use std::process::{Command, Stdio};

use anyhow::{Context, anyhow, bail};
use keyring::Entry;

use crate::calendar_fetch::FetchError;
use crate::ical;

const SERVICE: &str = "meeting-manager";

fn entry_for(account: &str) -> anyhow::Result<Entry> {
    Entry::new(SERVICE, account).map_err(|e| anyhow!("cannot read the Keychain: {e}"))
}

fn entry() -> anyhow::Result<Entry> {
    entry_for("deepgram")
}

/// The Keychain item holding the secret iCal address.
pub fn calendar_entry() -> anyhow::Result<Entry> {
    entry_for("calendar-ics")
}

/// The Deepgram key from the Keychain. Never log or print the returned value.
pub fn deepgram_key() -> anyhow::Result<String> {
    key_from(&entry()?)
}

fn key_from(entry: &Entry) -> anyhow::Result<String> {
    match entry.get_password() {
        Ok(key) => Ok(key.trim().to_string()),
        Err(keyring::Error::NoEntry) => Err(anyhow!(
            "no Deepgram key in the Keychain; run `meeting config set-key`"
        )),
        Err(e) => Err(anyhow!("cannot read the Keychain: {e}")),
    }
}

/// Stores the key, replacing any existing one.
pub fn set_key(key: &str) -> anyhow::Result<()> {
    set_key_in(&entry()?, key)
}

fn set_key_in(entry: &Entry, key: &str) -> anyhow::Result<()> {
    let key = key.trim();
    if key.is_empty() {
        bail!("no key entered");
    }
    entry
        .set_password(key)
        .map_err(|e| anyhow!("cannot write the Keychain: {e}"))
}

/// The stored secret iCal address, if any. Never log or print the returned value.
pub fn calendar_address() -> anyhow::Result<Option<String>> {
    address_from(&calendar_entry()?)
}

fn address_from(entry: &Entry) -> anyhow::Result<Option<String>> {
    match entry.get_password() {
        Ok(address) => Ok(Some(address)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(anyhow!("cannot read the Keychain: {e}")),
    }
}

/// `meeting config set-calendar`: an empty answer removes the stored address; otherwise the
/// address is stored only once it was fetched and parsed. No message contains the address.
pub fn set_calendar(
    answer: &str,
    entry: &Entry,
    fetch: fn(&str) -> Result<String, FetchError>,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    let answer = answer.trim();
    if answer.is_empty() {
        let message = match entry.delete_credential() {
            Ok(()) => "Removed.",
            Err(keyring::Error::NoEntry) => "Nothing stored.",
            Err(e) => bail!("cannot write the Keychain: {e}"),
        };
        writeln!(out, "{message}")?;
        return Ok(());
    }
    let address = https_ics(answer)?;
    let text = fetch(&address).map_err(|kind| anyhow!("could not fetch: {kind}"))?;
    let calendar = ical::parse(&text).map_err(|_| anyhow!("not an iCalendar file"))?;
    entry
        .set_password(&address)
        .map_err(|e| anyhow!("cannot write the Keychain: {e}"))?;
    writeln!(out, "Saved. Found {} events.", calendar.events.len())?;
    Ok(())
}

/// The address with its scheme lower-cased, if it is `https://` and its path ends in `.ics`.
fn https_ics(address: &str) -> anyhow::Result<String> {
    const EXPECTED: &str = "expected an https:// address whose path ends in .ics";
    let Some(rest) = address
        .get(..8)
        .filter(|scheme| scheme.eq_ignore_ascii_case("https://"))
        .map(|_| &address[8..])
    else {
        bail!("not https; {EXPECTED}");
    };
    let end = rest.find(['?', '#']).unwrap_or(rest.len());
    let path = rest[..end].find('/').map_or("", |i| &rest[i..end]);
    if !path.to_ascii_lowercase().ends_with(".ics") {
        bail!("not an .ics address; {EXPECTED}");
    }
    Ok(format!("https://{rest}"))
}

/// Reads one line from stdin without echoing it when stdin is a terminal.
pub fn read_hidden(prompt: &str) -> anyhow::Result<String> {
    eprint!("{prompt}");
    let tty = stdin().is_terminal();
    if tty {
        stty("-echo")?;
    }
    let mut line = String::new();
    let read = stdin().read_line(&mut line);
    if tty {
        // Restore echo even when the read failed.
        let restored = stty("echo");
        eprintln!();
        restored?;
    }
    read.context("cannot read the key")?;
    Ok(line)
}

fn stty(arg: &str) -> anyhow::Result<()> {
    let status = Command::new("/bin/stty")
        .arg(arg)
        .stdin(Stdio::inherit())
        .status()
        .context("cannot run stty")?;
    if !status.success() {
        bail!("stty {arg} failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_entry() -> Entry {
        keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        entry().unwrap()
    }

    #[test]
    fn missing_key_names_the_fix() {
        let e = key_from(&mock_entry()).unwrap_err();
        assert_eq!(
            e.to_string(),
            "no Deepgram key in the Keychain; run `meeting config set-key`"
        );
    }

    #[test]
    fn deepgram_key_reports_a_missing_item() {
        mock_entry();
        let e = deepgram_key().unwrap_err();
        assert!(e.to_string().contains("meeting config set-key"));
    }

    #[test]
    fn blank_key_is_rejected_and_not_stored() {
        let entry = mock_entry();
        let e = set_key_in(&entry, "  \n").unwrap_err();
        assert_eq!(e.to_string(), "no key entered");
        assert!(key_from(&entry).is_err());
    }

    #[test]
    fn stored_key_is_returned_trimmed_and_replaced() {
        let entry = mock_entry();
        set_key_in(&entry, "  abc123\n").unwrap();
        assert_eq!(key_from(&entry).unwrap(), "abc123");
        set_key_in(&entry, "def456").unwrap();
        assert_eq!(key_from(&entry).unwrap(), "def456");
    }

    const SENTINEL: &str = "SENTINEL-SECRET-42";
    const ICS: &str = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261023T100000Z\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:b\r\nDTSTART:20261024T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    fn calendar_mock() -> Entry {
        keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        calendar_entry().unwrap()
    }

    fn feed(_: &str) -> Result<String, FetchError> {
        Ok(ICS.into())
    }

    fn empty_feed(_: &str) -> Result<String, FetchError> {
        Ok("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n".into())
    }

    fn garbage(_: &str) -> Result<String, FetchError> {
        Ok("BEGIN:VCALENDAR\r\n".into())
    }

    fn unreachable_fetch(_: &str) -> Result<String, FetchError> {
        panic!("fetched an address that should have been refused")
    }

    /// Runs the command and returns (stdout, error text), checking neither holds the sentinel.
    fn run(
        answer: &str,
        entry: &Entry,
        fetch: fn(&str) -> Result<String, FetchError>,
    ) -> (String, String) {
        let mut out = Vec::new();
        let result = set_calendar(answer, entry, fetch, &mut out);
        let out = String::from_utf8(out).unwrap();
        let err = result
            .err()
            .map(|e| format!("{e:#} {e:?}"))
            .unwrap_or_default();
        assert!(!out.contains(SENTINEL), "{out}");
        assert!(!err.contains(SENTINEL), "{err}");
        (out, err)
    }

    fn address(path: &str) -> String {
        format!("https://calendar.example.com/{SENTINEL}/{path}")
    }

    #[test]
    fn good_address_is_stored_trimmed_and_events_counted() {
        let entry = calendar_mock();
        let (out, err) = run(&format!("  {}\n", address("basic.ics")), &entry, feed);
        assert_eq!(
            (out.as_str(), err.as_str()),
            ("Saved. Found 2 events.\n", "")
        );
        assert_eq!(address_from(&entry).unwrap(), Some(address("basic.ics")));
    }

    #[test]
    fn uppercase_scheme_and_extension_with_a_query_are_accepted() {
        let entry = calendar_mock();
        let answer = format!("HTTPS://calendar.example.com/{SENTINEL}/BASIC.ICS?k=v#f");
        let (out, _) = run(&answer, &entry, empty_feed);
        assert_eq!(out, "Saved. Found 0 events.\n");
        assert_eq!(
            address_from(&entry).unwrap(),
            Some(format!(
                "https://calendar.example.com/{SENTINEL}/BASIC.ICS?k=v#f"
            ))
        );
    }

    #[test]
    fn bad_addresses_are_refused_with_the_expected_format() {
        let entry = calendar_mock();
        entry.set_password("old").unwrap();
        let expected = "expected an https:// address whose path ends in .ics";
        for (answer, reason) in [
            (format!("http://x/{SENTINEL}.ics"), "not https"),
            (format!("webcal://x/{SENTINEL}.ics"), "not https"),
            (SENTINEL.to_string(), "not https"),
            (format!("httpé://{SENTINEL}.ics"), "not https"),
            (format!("https://é/{SENTINEL}é"), "not an .ics address"),
            (address("basic"), "not an .ics address"),
            (address("basic?f=.ics"), "not an .ics address"),
            (address("basic#.ics"), "not an .ics address"),
            (format!("https://{SENTINEL}.ics"), "not an .ics address"),
            (
                format!("https://{SENTINEL}.ics?x=a.ics"),
                "not an .ics address",
            ),
        ] {
            let (out, err) = run(&answer, &entry, unreachable_fetch);
            assert_eq!(out, "");
            assert!(
                err.starts_with(&format!("{reason}; {expected}")),
                "{answer}: {err}"
            );
        }
        assert_eq!(address_from(&entry).unwrap().as_deref(), Some("old"));
    }

    #[test]
    fn failed_fetch_or_parse_stores_nothing_and_keeps_the_old_address() {
        fn offline(_: &str) -> Result<String, FetchError> {
            Err(FetchError::Offline)
        }
        fn rejected(_: &str) -> Result<String, FetchError> {
            Err(FetchError::Http(404))
        }
        fn html(_: &str) -> Result<String, FetchError> {
            Err(FetchError::NotIcalendar)
        }
        fn timeout(_: &str) -> Result<String, FetchError> {
            Err(FetchError::Timeout)
        }
        let entry = calendar_mock();
        let (_, err) = run(&address("a.ics"), &entry, offline);
        assert!(err.starts_with("could not fetch: offline"), "{err}");
        assert_eq!(address_from(&entry).unwrap(), None);
        entry.set_password("old").unwrap();
        for (fetch, text) in [
            (offline as fn(&str) -> _, "could not fetch: offline"),
            (rejected, "could not fetch: http 404"),
            (html, "could not fetch: not iCalendar"),
            (timeout, "could not fetch: timeout"),
            (garbage, "not an iCalendar file"),
        ] {
            let (out, err) = run(&address("a.ics"), &entry, fetch);
            assert_eq!(out, "");
            assert!(err.starts_with(text), "{err}");
        }
        assert_eq!(address_from(&entry).unwrap().as_deref(), Some("old"));
    }

    #[test]
    fn empty_answer_removes_or_reports_nothing_stored() {
        let entry = calendar_mock();
        assert_eq!(run(" \n", &entry, unreachable_fetch).0, "Nothing stored.\n");
        entry.set_password(&address("a.ics")).unwrap();
        assert_eq!(run("\n", &entry, unreachable_fetch).0, "Removed.\n");
        assert_eq!(address_from(&entry).unwrap(), None);
    }

    #[test]
    fn calendar_address_is_none_when_not_stored() {
        calendar_mock();
        assert_eq!(calendar_address().unwrap(), None);
    }
}
