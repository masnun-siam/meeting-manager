use std::io::{IsTerminal, stdin};
use std::process::{Command, Stdio};

use anyhow::{Context, anyhow, bail};
use keyring::Entry;

const SERVICE: &str = "meeting-manager";
const ACCOUNT: &str = "deepgram";

fn entry() -> anyhow::Result<Entry> {
    Entry::new(SERVICE, ACCOUNT).map_err(|e| anyhow!("cannot read the Keychain: {e}"))
}

/// The Deepgram key from the Keychain. Never log or print the returned value.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "used by `meeting process` (#20) and `doctor` (#24)"
    )
)]
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
}
