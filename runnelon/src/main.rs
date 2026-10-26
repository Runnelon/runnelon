//! `runnelon` binary entry point.

use std::process::ExitCode;

fn main() -> ExitCode {
    // Initialize logging early so Pingora and Runnelon messages are visible.
    // Default to `info` when RUST_LOG is unset.
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .try_init();

    runnelon::run()
}
