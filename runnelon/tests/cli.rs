//! End-to-end CLI smoke tests for help, version, and config validation.

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use std::io::Write;
use tempfile::NamedTempFile;

#[test]
fn help_flag_prints_usage_and_exits_zero() {
    cargo_bin_cmd!("runnelon")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("runnelon"))
        .stdout(predicate::str::contains("--config"));
}

#[test]
fn short_help_flag_exits_zero() {
    cargo_bin_cmd!("runnelon")
        .arg("-h")
        .assert()
        .success()
        .stdout(predicate::str::contains("runnelon"));
}

#[test]
fn version_flag_prints_package_version() {
    let version = env!("CARGO_PKG_VERSION");
    cargo_bin_cmd!("runnelon")
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(version));
}

#[test]
fn short_version_flag_prints_package_version() {
    let version = env!("CARGO_PKG_VERSION");
    cargo_bin_cmd!("runnelon")
        .arg("-V")
        .assert()
        .success()
        .stdout(predicate::str::contains(version));
}

#[test]
fn default_invocation_mentions_config_flag() {
    cargo_bin_cmd!("runnelon")
        .assert()
        .success()
        .stdout(predicate::str::contains("--config"));
}

#[test]
fn unknown_argument_exits_nonzero() {
    cargo_bin_cmd!("runnelon")
        .arg("--definitely-not-valid")
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("unexpected")
                .or(predicate::str::contains("unknown"))
                .or(predicate::str::contains("definitely-not-valid")),
        );
}

#[test]
fn invalid_config_exits_nonzero_with_actionable_error() {
    let mut file = NamedTempFile::new().expect("tempfile");
    writeln!(
        file,
        r#"
version: 1
listeners: []
routes: []
upstreams: {{}}
"#
    )
    .unwrap();

    cargo_bin_cmd!("runnelon")
        .arg("--config")
        .arg(file.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid configuration"));
}

#[test]
fn unreachable_docker_socket_fails_startup() {
    let mut file = NamedTempFile::new().expect("tempfile");
    writeln!(
        file,
        r#"
version: 1
listeners:
  - address: "127.0.0.1:18098"
routes: []
upstreams: {{}}
docker:
  socket: "/nonexistent/runnelon-docker.sock"
"#
    )
    .unwrap();

    cargo_bin_cmd!("runnelon")
        .arg("--config")
        .arg(file.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("docker discovery failed"));
}

#[test]
fn malformed_yaml_exits_nonzero() {
    let mut file = NamedTempFile::new().expect("tempfile");
    writeln!(file, "version: [not: valid").unwrap();

    cargo_bin_cmd!("runnelon")
        .arg("--config")
        .arg(file.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid YAML").or(predicate::str::contains("YAML")));
}
