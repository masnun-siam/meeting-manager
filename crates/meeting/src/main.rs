mod config;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by `meeting process` (#20)")
)]
mod extract;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by `meeting process` (#20)")
)]
mod jobs;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by `meeting process` (#20)")
)]
mod lock;
mod logging;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by `meeting process` (#20)")
)]
mod meta;
mod secret;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by `meeting process` (#20)")
)]
mod summarise;

use std::path::PathBuf;

use anyhow::bail;
use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

/// Turns meeting recordings into transcripts and Obsidian notes.
#[derive(Parser, Debug)]
#[command(name = "meeting", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Transcribe and summarise a recording in the foreground.
    Process {
        recording: PathBuf,
        /// Transcribe again even if a transcript exists.
        #[arg(long)]
        retranscribe: bool,
        #[arg(long, value_enum)]
        language: Option<Language>,
        #[arg(long, hide = true)]
        job_id: Option<String>,
    },
    /// Register the native host and write the config.
    Install,
    /// Manage settings.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Check that everything the tool needs is in place.
    Doctor,
    /// Entry point Chrome launches; not for direct use.
    NativeHost { origin: Option<String> },
}

#[derive(Subcommand, Debug)]
enum ConfigCommand {
    /// Store the Deepgram API key in the Keychain.
    SetKey,
}

#[derive(ValueEnum, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Bn,
    En,
}

fn main() -> anyhow::Result<()> {
    logging::init();
    let cli = Cli::try_parse().unwrap_or_else(|e| {
        if e.use_stderr() {
            tracing::error!("{}", e.kind());
        }
        e.exit()
    });
    let result = run(cli.command);
    if let Err(e) = &result {
        tracing::error!("{e:#}");
    }
    result
}

fn run(command: Command) -> anyhow::Result<()> {
    if let Command::Config {
        command: ConfigCommand::SetKey,
    } = command
    {
        tracing::info!("meeting config set-key");
        secret::set_key(&secret::read_hidden("Deepgram API key: ")?)?;
        println!("Deepgram key saved to the Keychain");
        return Ok(());
    }
    let name = match command {
        Command::Process { .. } => "process",
        Command::Install => "install",
        Command::Config {
            command: ConfigCommand::SetKey,
        } => "config set-key",
        Command::Doctor => "doctor",
        Command::NativeHost { .. } => "native-host",
    };
    tracing::info!("meeting {name}");
    bail!("`meeting {name}` is not implemented yet")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn process_accepts_language_and_hidden_job_id() {
        let cli = Cli::try_parse_from([
            "meeting",
            "process",
            "a.webm",
            "--language",
            "bn",
            "--job-id",
            "x",
        ])
        .unwrap();
        let Command::Process {
            recording,
            language,
            job_id,
            retranscribe,
        } = cli.command
        else {
            panic!("expected process");
        };
        assert_eq!(recording, PathBuf::from("a.webm"));
        assert_eq!(language, Some(Language::Bn));
        assert_eq!(job_id.as_deref(), Some("x"));
        assert!(!retranscribe);
    }

    #[test]
    fn process_rejects_unknown_language() {
        assert!(Cli::try_parse_from(["meeting", "process", "x.webm", "--language", "fr"]).is_err());
    }

    #[test]
    fn job_id_is_hidden_from_help() {
        let help = Cli::command()
            .find_subcommand_mut("process")
            .unwrap()
            .render_long_help()
            .to_string();
        assert!(help.contains("--retranscribe"));
        assert!(!help.contains("--job-id"));
    }

    #[test]
    fn native_host_takes_origin() {
        let cli =
            Cli::try_parse_from(["meeting", "native-host", "chrome-extension://abc/"]).unwrap();
        let Command::NativeHost { origin } = cli.command else {
            panic!("expected native-host");
        };
        assert_eq!(origin.as_deref(), Some("chrome-extension://abc/"));
    }
}
