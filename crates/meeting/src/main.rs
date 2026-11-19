use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::bail;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

use meeting::Language;
use meeting::calendar_fetch::{self, FetchError, Policy};
use meeting::config::{self, Config};
use meeting::deepgram::Deepgram;
use meeting::jobs::JobStore;
use meeting::notify::{self, Outcome};
use meeting::pipeline::{Ending, Pipeline};
use meeting::{doctor, import, install, logging, native, secret, store};

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
        /// Stop an earlier run of this meeting, join its new parts and process again.
        #[arg(long = "continue")]
        continue_run: bool,
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
    /// Store the secret iCal address of your calendar in the Keychain.
    SetCalendar,
}

fn flag_gate(value: Option<&str>) -> bool {
    value == Some("1")
}

/// Whether the tool was built with `FEATURE_MEETING_CONTEXT=1`. Read at build time
/// because Chrome starts the host with launchd's environment, not the shell's.
pub fn context_enabled() -> bool {
    flag_gate(option_env!("FEATURE_MEETING_CONTEXT"))
}

/// Whether the tool was built with `FEATURE_DESKTOP_APP=1`. Read at build time
/// because Chrome starts the host with launchd's environment, not the shell's.
pub fn desktop_enabled() -> bool {
    flag_gate(option_env!("FEATURE_DESKTOP_APP"))
}

/// Whether the tool was built with `FEATURE_CONTINUE_RECORDING=1`; read at build time, as
/// the other flags.
pub fn continue_enabled() -> bool {
    flag_gate(option_env!("FEATURE_CONTINUE_RECORDING"))
}

/// The CLI for this build. Clap cannot remove a derived subcommand or argument, so with
/// its gate off `set-calendar` (context) or `process --continue` (continue) is hidden here
/// and refused in `run`.
fn cli(context: bool, continuing: bool) -> clap::Command {
    let mut cli = Cli::command();
    if !context {
        cli = cli.mut_subcommand("config", |c| {
            c.mut_subcommand("set-calendar", |s| s.hide(true))
        });
    }
    if !continuing {
        cli = cli.mut_subcommand("process", |s| s.mut_arg("continue_run", |a| a.hide(true)));
    }
    cli
}

fn parse(context: bool, continuing: bool, args: Vec<OsString>) -> Result<Cli, clap::Error> {
    cli(context, continuing)
        .try_get_matches_from(args)
        .and_then(|m| Cli::from_arg_matches(&m))
}

/// The calendar fetch under the real policy, as the `fn` pointer `secret` and `doctor` take.
fn fetch_calendar(address: &str) -> Result<String, FetchError> {
    calendar_fetch::fetch(address, &Policy::REAL)
}

fn main() -> anyhow::Result<()> {
    logging::init();
    let context = context_enabled();
    let continuing = continue_enabled();
    let cli = parse(context, continuing, chrome_args(std::env::args_os())).unwrap_or_else(|e| {
        if e.use_stderr() {
            tracing::error!("{}", e.kind());
        }
        e.exit()
    });
    let result = run(cli.command, context, continuing);
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

/// What `meeting install` prints last.
fn install_next(has_key: bool, context: bool) -> String {
    let mut next = if has_key {
        "Next: run `meeting doctor`.\n".to_string()
    } else {
        "Next: run `meeting config set-key`, then `meeting doctor`.\n".to_string()
    };
    if context {
        next.push_str("Optional: run `meeting config set-calendar` to give summaries your meeting agendas and attendee names.\n");
    }
    next
}

fn run(command: Command, context: bool, continuing: bool) -> anyhow::Result<()> {
    if let Command::Config {
        command: ConfigCommand::SetCalendar,
    } = command
    {
        if !context {
            bail!("unrecognized subcommand 'set-calendar'");
        }
        tracing::info!("meeting config set-calendar");
        return secret::set_calendar(
            &secret::read_hidden("Calendar secret iCal address: ")?,
            &secret::calendar_entry()?,
            fetch_calendar,
            &mut io::stdout(),
        );
    }
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
            store_path: store::default_path(&config::home()?),
            exe: std::env::current_exe()?,
            context,
            desktop: desktop_enabled(),
            continue_recording: continue_enabled(),
            calendar: secret::calendar_address,
            fetch: fetch_calendar,
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
        continue_run,
    } = &command
    {
        if *continue_run && !continuing {
            bail!("unexpected argument '--continue'");
        }
        tracing::info!("meeting process");
        return process(
            Config::load()?,
            recording,
            *retranscribe,
            *language,
            job_id.clone(),
            *continue_run,
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
        return process(config, &path, false, Some(*language), None, false);
    }
    if let Command::Install = command {
        tracing::info!("meeting install");
        install::install(
            &mut io::stdin().lock(),
            &mut io::stdout(),
            &config::home()?,
            &std::env::var_os("PATH").unwrap_or_default(),
            &std::env::current_exe()?,
            desktop_enabled(),
        )?;
        print!("{}", install_next(secret::deepgram_key().is_ok(), context));
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
            context,
            desktop: desktop_enabled(),
            calendar: secret::calendar_address,
            fetch: fetch_calendar,
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
        Command::Config {
            command: ConfigCommand::SetCalendar,
        } => "config set-calendar",
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
    continuing: bool,
) -> anyhow::Result<()> {
    let pipeline = Pipeline {
        config,
        deepgram: Deepgram::default(),
        jobs: JobStore {
            dir: JobStore::default_dir()?,
        },
        key: secret::deepgram_key,
        claude_timeout: Duration::from_secs(15 * 60),
        context: context_enabled(),
        desktop: desktop_enabled(),
        store_path: store::default_path(&config::home()?),
        continue_recording: continue_enabled(),
        stop_grace: Duration::from_secs(10),
        calendar: secret::calendar_address,
        calendar_policy: Policy::REAL,
    };
    match pipeline.run(
        recording,
        retranscribe,
        language,
        job_id,
        continuing,
        &mut io::stdout(),
    ) {
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

    #[test]
    fn the_gate_is_on_only_for_the_value_1() {
        assert!(flag_gate(Some("1")));
        for off in [None, Some("0"), Some(""), Some("true"), Some("yes")] {
            assert!(!flag_gate(off), "{off:?}");
        }
    }

    #[test]
    fn every_flag_goes_through_the_gate() {
        assert_eq!(
            context_enabled(),
            flag_gate(option_env!("FEATURE_MEETING_CONTEXT"))
        );
        assert_eq!(
            desktop_enabled(),
            flag_gate(option_env!("FEATURE_DESKTOP_APP"))
        );
        assert_eq!(
            continue_enabled(),
            flag_gate(option_env!("FEATURE_CONTINUE_RECORDING"))
        );
    }

    #[test]
    fn cli_is_valid() {
        Cli::command().debug_assert();
    }

    fn config_help(context: bool) -> String {
        cli(context, false)
            .find_subcommand_mut("config")
            .unwrap()
            .render_long_help()
            .to_string()
    }

    #[test]
    fn set_calendar_is_listed_only_with_the_gate_on() {
        assert!(config_help(true).contains("set-calendar"));
        let off = config_help(false);
        assert!(off.contains("set-key"));
        assert!(!off.contains("set-calendar"), "{off}");
        cli(false, false).debug_assert();
    }

    #[test]
    fn set_calendar_is_refused_with_the_gate_off() {
        let args = ["meeting", "config", "set-calendar"].map(OsString::from);
        let on = parse(true, false, args.to_vec()).unwrap();
        assert!(matches!(
            on.command,
            Command::Config {
                command: ConfigCommand::SetCalendar
            }
        ));
        let off = parse(false, false, args.to_vec()).unwrap();
        let e = run(off.command, false, false).unwrap_err();
        assert_eq!(e.to_string(), "unrecognized subcommand 'set-calendar'");
    }

    #[test]
    fn install_mentions_set_calendar_only_with_the_gate_on() {
        let today = [
            "Next: run `meeting doctor`.\n",
            "Next: run `meeting config set-key`, then `meeting doctor`.\n",
        ];
        assert_eq!(install_next(true, false), today[0]);
        assert_eq!(install_next(false, false), today[1]);
        for (has_key, base) in [(true, today[0]), (false, today[1])] {
            let on = install_next(has_key, true);
            let extra = on.strip_prefix(base).unwrap();
            assert!(extra.starts_with("Optional: run `meeting config set-calendar`"));
            assert_eq!(extra.lines().count(), 1);
        }
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
            continue_run,
        } = cli.command
        else {
            panic!("expected process");
        };
        assert_eq!(recording, PathBuf::from("a.webm"));
        assert_eq!(language, Some(Language::Bn));
        assert_eq!(job_id.as_deref(), Some("x"));
        assert!(!retranscribe);
        assert!(!continue_run);
    }

    #[test]
    fn continue_is_accepted_with_language_and_job_id() {
        let args = [
            "meeting",
            "process",
            "/r/m/recording.m4a",
            "--continue",
            "--language",
            "en",
            "--retranscribe",
            "--job-id",
            "j-1",
        ]
        .map(OsString::from);
        let cli = parse(false, true, args.to_vec()).unwrap();
        let Command::Process {
            language,
            job_id,
            retranscribe,
            continue_run,
            ..
        } = cli.command
        else {
            panic!("expected process");
        };
        assert_eq!(language, Some(Language::En));
        assert_eq!(job_id.as_deref(), Some("j-1"));
        assert!(retranscribe && continue_run);
    }

    fn process_help(continuing: bool) -> String {
        cli(false, continuing)
            .find_subcommand_mut("process")
            .unwrap()
            .render_long_help()
            .to_string()
    }

    #[test]
    fn continue_is_hidden_from_help_and_refused_with_the_gate_off() {
        assert!(process_help(true).contains("--continue"));
        let off = process_help(false);
        assert!(off.contains("--retranscribe"));
        assert!(!off.contains("--continue"), "{off}");
        assert!(!off.contains("--job-id"), "{off}");
        cli(false, false).debug_assert();
        let args = ["meeting", "process", "/r/m/recording.m4a", "--continue"].map(OsString::from);
        let off = parse(false, false, args.to_vec()).unwrap();
        let e = run(off.command, false, false).unwrap_err();
        assert_eq!(e.to_string(), "unexpected argument '--continue'");
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
