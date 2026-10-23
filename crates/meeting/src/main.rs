mod calendar_fetch;
mod config;
mod context;
mod deepgram;
mod doctor;
mod extract;
// ponytail: unused until #99 reads it.
#[allow(dead_code)]
mod ical;
mod import;
mod install;
mod jobs;
mod lock;
mod logging;
mod meta;
mod native;
mod notify;
mod pipeline;
mod plan;
mod secret;
mod summarise;
mod summary_md;
#[cfg(test)]
mod test_support;
mod transcript;
mod vault;

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::bail;
use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::deepgram::Deepgram;
use crate::jobs::JobStore;
use crate::notify::Outcome;
use crate::pipeline::{Ending, Pipeline};

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
    /// Copy a recording made elsewhere into its own meeting folder and process it.
    Import {
        file: PathBuf,
        #[arg(long, value_enum)]
        language: Language,
        /// Meeting time, `YYYY-MM-DD HH:MM`; defaults to the file's modified time.
        #[arg(long)]
        date: Option<String>,
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

fn context_gate(value: Option<&str>) -> bool {
    value == Some("1")
}

/// Whether the tool was built with `FEATURE_MEETING_CONTEXT=1`. Read at build time
/// because Chrome starts the host with launchd's environment, not the shell's.
pub fn context_enabled() -> bool {
    context_gate(option_env!("FEATURE_MEETING_CONTEXT"))
}

fn main() -> anyhow::Result<()> {
    logging::init();
    let cli = Cli::try_parse_from(chrome_args(std::env::args_os())).unwrap_or_else(|e| {
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

/// Chrome launches a native host as `<path> chrome-extension://<id>/` and its
/// manifest can't add arguments, so an origin in first place means `native-host`.
fn chrome_args(args: impl Iterator<Item = OsString>) -> Vec<OsString> {
    let mut args: Vec<OsString> = args.collect();
    if args
        .get(1)
        .is_some_and(|a| a.to_string_lossy().starts_with("chrome-extension://"))
    {
        args.insert(1, "native-host".into());
    }
    args
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
    if let Command::NativeHost { origin } = &command {
        tracing::info!("meeting native-host");
        let host = native::Host {
            config: Config::load().map_err(|e| format!("{e:#}")),
            jobs: JobStore {
                dir: JobStore::default_dir()?,
            },
            open: native::open_with_system,
            trash: native::trash_with_system,
            key: secret::deepgram_key,
            manifest: doctor::manifest_path(&config::home()?),
            exe: std::env::current_exe()?,
            context: context_enabled(),
        };
        return native::serve(
            &host,
            origin.as_deref(),
            io::stdin().lock(),
            io::stdout().lock(),
        );
    }
    if let Command::Process {
        recording,
        retranscribe,
        language,
        job_id,
    } = &command
    {
        tracing::info!("meeting process");
        return process(
            Config::load()?,
            recording,
            *retranscribe,
            *language,
            job_id.clone(),
        );
    }
    if let Command::Import {
        file,
        language,
        date,
    } = &command
    {
        tracing::info!("meeting import");
        let config = Config::load()?;
        let path = import::import(
            &config.recordings_dir,
            file,
            date.as_deref(),
            import::local_minute,
        )?;
        println!("imported → {}", path.display());
        return process(config, &path, false, Some(*language), None);
    }
    if let Command::Install = command {
        tracing::info!("meeting install");
        install::install(
            &mut io::stdin().lock(),
            &mut io::stdout(),
            &config::home()?,
            &std::env::var_os("PATH").unwrap_or_default(),
            &std::env::current_exe()?,
        )?;
        if secret::deepgram_key().is_err() {
            println!("Next: run `meeting config set-key`, then `meeting doctor`.");
        } else {
            println!("Next: run `meeting doctor`.");
        }
        return Ok(());
    }
    if let Command::Doctor = command {
        tracing::info!("meeting doctor");
        let config = Config::load().map_err(|e| format!("{e:#}"));
        let checks = doctor::checks(&doctor::Env {
            config: &config,
            key: secret::deepgram_key,
            manifest: doctor::manifest_path(&config::home()?),
            exe: std::env::current_exe()?,
        });
        for c in &checks {
            if c.ok {
                println!("✓ {}", c.name);
            } else {
                println!("✗ {}: {}", c.name, c.fix);
            }
        }
        let failed = checks.iter().filter(|c| !c.ok).count();
        if failed > 0 {
            bail!("{failed} checks failed");
        }
        return Ok(());
    }
    let name = match command {
        Command::Process { .. } => "process",
        Command::Import { .. } => "import",
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

/// Runs the pipeline in the foreground and notifies the outcome.
fn process(
    config: Config,
    recording: &Path,
    retranscribe: bool,
    language: Option<Language>,
    job_id: Option<String>,
) -> anyhow::Result<()> {
    let pipeline = Pipeline {
        config,
        deepgram: Deepgram::default(),
        jobs: JobStore {
            dir: JobStore::default_dir()?,
        },
        key: secret::deepgram_key,
        claude_timeout: Duration::from_secs(15 * 60),
    };
    match pipeline.run(recording, retranscribe, language, job_id, &mut io::stdout()) {
        Ending::Done { title } => {
            notify::notify(&notify::message(&Outcome::Done { title: &title }));
            Ok(())
        }
        Ending::Failed { stage, error } => {
            let reason = format!("{error:#}");
            notify::notify(&notify::message(&Outcome::Failed {
                stage: stage.name(),
                reason: &reason,
            }));
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_gate_is_on_only_for_the_value_1() {
        assert!(context_gate(Some("1")));
        for off in [None, Some("0"), Some(""), Some("true")] {
            assert!(!context_gate(off), "{off:?}");
        }
    }

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

    #[test]
    fn chrome_launch_with_only_an_origin_runs_native_host() {
        let args = ["meeting", "chrome-extension://abc/"].map(OsString::from);
        let cli = Cli::try_parse_from(chrome_args(args.into_iter())).unwrap();
        let Command::NativeHost { origin } = cli.command else {
            panic!("expected native-host");
        };
        assert_eq!(origin.as_deref(), Some("chrome-extension://abc/"));
    }

    #[test]
    fn other_arguments_are_left_alone() {
        let args = ["meeting", "doctor"].map(OsString::from);
        assert_eq!(chrome_args(args.clone().into_iter()), args);
    }
}
