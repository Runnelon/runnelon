//! Command-line interface for the `runnelon` binary.

use crate::config::{load_snapshot_from_path, ConfigError};
use crate::server::run_proxy;
use clap::Parser;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

/// Open-source reverse proxy and API gateway (early development).
#[derive(Debug, Parser)]
#[command(
    name = "runnelon",
    version,
    about = "Traffic flows better here.",
    long_about = "Runnelon is an early-stage reverse proxy and API gateway built on Pingora.\n\n\
         Start the file-based HTTP proxy with `--config <file>` (schema version 1).\n\
         Config changes are reloaded automatically; listener changes need a restart.\n\
         See ROADMAP.md and docs/ARCHITECTURE.md for scope and limitations."
)]
pub struct Cli {
    /// Path to a `runnelon.yaml` configuration file (schema version 1).
    #[arg(short = 'c', long = "config", value_name = "FILE")]
    pub config: Option<PathBuf>,
}

/// Parse CLI arguments from the process environment and execute the command.
pub fn run() -> ExitCode {
    let cli = Cli::parse();
    match execute(cli) {
        Ok(code) => code,
        Err(err) => {
            let _ = writeln!(io::stderr(), "error: {err}");
            ExitCode::from(1)
        }
    }
}

/// Execute a parsed CLI. Returns process exit code.
pub fn execute(cli: Cli) -> Result<ExitCode, String> {
    match cli.config {
        None => {
            write_startup_message(io::stdout()).map_err(|e| e.to_string())?;
            Ok(ExitCode::SUCCESS)
        }
        Some(path) => {
            let snapshot = load_snapshot_from_path(&path).map_err(format_config_error)?;
            // run_proxy only returns on setup failure; success blocks forever.
            run_proxy(snapshot, path)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Write the default no-config message to `out`.
pub fn write_startup_message(mut out: impl Write) -> io::Result<()> {
    writeln!(
        out,
        "runnelon {} — static HTTP reverse proxy (M1).",
        env!("CARGO_PKG_VERSION")
    )?;
    writeln!(
        out,
        "Pass `--config <file>` to start. Try `runnelon --help` or `runnelon --version`."
    )?;
    Ok(())
}

fn format_config_error(err: ConfigError) -> String {
    err.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_metadata_includes_name_and_version() {
        let cmd = Cli::command();
        assert_eq!(cmd.get_name(), "runnelon");
        assert_eq!(cmd.get_version(), Some(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn help_mentions_config() {
        let mut cmd = Cli::command();
        let mut help = Vec::new();
        cmd.write_long_help(&mut help).expect("write help");
        let help = String::from_utf8(help).expect("utf8 help");
        assert!(help.contains("--config") || help.contains("-c"), "{help}");
        assert!(help.contains("Pingora") || help.contains("proxy"), "{help}");
    }

    #[test]
    fn startup_message_mentions_config_flag() {
        let mut buf = Vec::new();
        write_startup_message(&mut buf).expect("write");
        let msg = String::from_utf8(buf).expect("utf8");
        assert!(msg.contains(env!("CARGO_PKG_VERSION")));
        assert!(msg.contains("--config"));
    }

    #[test]
    fn parse_empty_args_succeeds() {
        let cli = Cli::try_parse_from(["runnelon"]).expect("empty args are valid");
        assert!(cli.config.is_none());
    }

    #[test]
    fn parse_config_flag() {
        let cli = Cli::try_parse_from(["runnelon", "--config", "runnelon.yaml"]).unwrap();
        assert_eq!(
            cli.config.as_deref(),
            Some(std::path::Path::new("runnelon.yaml"))
        );
    }

    #[test]
    fn unknown_flag_is_rejected() {
        let err = Cli::try_parse_from(["runnelon", "--not-a-real-flag"])
            .expect_err("unknown flags must fail");
        let rendered = err.to_string();
        assert!(
            rendered.contains("unexpected")
                || rendered.contains("unknown")
                || rendered.contains("not-a-real-flag"),
            "{rendered}"
        );
    }

    #[test]
    fn execute_without_config_prints_message() {
        let code = execute(Cli { config: None }).expect("ok");
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn execute_with_missing_config_file_fails() {
        let err = execute(Cli {
            config: Some(PathBuf::from("/nonexistent/runnelon-m1-missing.yaml")),
        })
        .expect_err("missing file");
        assert!(
            err.contains("failed to read") || err.contains("No such file"),
            "{err}"
        );
    }
}
