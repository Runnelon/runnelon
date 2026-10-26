//! Off-request-path configuration reloads with last-known-good retention.

use crate::config::{load_snapshot_from_path, ConfigError};
use crate::publisher::Publisher;
use log::{error, info, warn};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// How often the configuration file is checked for changes.
pub const RELOAD_POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    modified: Option<SystemTime>,
    len: u64,
}

fn stamp(path: &Path) -> Option<FileStamp> {
    let metadata = fs::metadata(path).ok()?;
    Some(FileStamp {
        modified: metadata.modified().ok(),
        len: metadata.len(),
    })
}

/// Start the background reload loop for `path`.
///
/// The thread lives for the process lifetime; failures are logged and never
/// replace the published snapshot.
pub fn spawn_reloader(path: PathBuf, publisher: Arc<Publisher>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut last = stamp(&path);
        loop {
            std::thread::sleep(RELOAD_POLL_INTERVAL);
            let current = stamp(&path);
            if current == last {
                continue;
            }
            last = current;
            if current.is_none() {
                warn!(
                    "config {} disappeared; keeping last-known-good",
                    path.display()
                );
                continue;
            }
            match reload_once(&path, &publisher) {
                Ok(()) => info!("configuration reloaded from {}", path.display()),
                Err(err) => error!("reload rejected, keeping last-known-good: {err}"),
            }
        }
    })
}

/// Validate `path` and publish it if it is compatible with the running process.
///
/// Listener, management and Docker changes are rejected because their services
/// are already bound; everything else is merged with discovery and published.
/// On error, the published snapshot is untouched.
pub fn reload_once(path: &Path, publisher: &Publisher) -> Result<(), ConfigError> {
    let candidate = load_snapshot_from_path(path)?;
    publisher.replace_base(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::DockerState;
    use std::sync::Arc;
    use tempfile::NamedTempFile;

    fn config(listener: &str, prefix: &str, upstream: &str, endpoint: &str) -> String {
        format!(
            r#"
version: 1
listeners:
  - address: "{listener}"
routes:
  - host: "*"
    path_prefix: "{prefix}"
    upstream: "{upstream}"
upstreams:
  {upstream}:
    endpoints: ["{endpoint}"]
"#
        )
    }

    #[test]
    fn publishes_valid_reloads_and_keeps_last_known_good() {
        let file = NamedTempFile::new().expect("tempfile");
        let initial = config("127.0.0.1:8080", "/old", "one", "127.0.0.1:3001");
        fs::write(file.path(), &initial).unwrap();
        let base = load_snapshot_from_path(file.path()).expect("initial config");
        let publisher = Publisher::new(base, DockerState::default());
        let live = publisher.live();

        let updated = config("127.0.0.1:8080", "/new", "two", "127.0.0.1:3002");
        fs::write(file.path(), &updated).unwrap();
        reload_once(file.path(), &publisher).expect("valid reload");
        assert_eq!(live.load().routes[0].path_prefix, "/new");
        assert_eq!(live.load().upstreams.len(), 1);

        let before = live.load_full();
        fs::write(file.path(), "version: 1\n").unwrap();
        assert!(reload_once(file.path(), &publisher).is_err());
        assert!(
            Arc::ptr_eq(&before, &live.load_full()),
            "invalid config must not replace the snapshot"
        );

        let moved = config("127.0.0.1:9999", "/new", "two", "127.0.0.1:3002");
        fs::write(file.path(), &moved).unwrap();
        let err = reload_once(file.path(), &publisher)
            .unwrap_err()
            .to_string();
        assert!(err.contains("restart"), "{err}");
        assert_eq!(live.load().listeners[0].address.port(), 8080);
    }
}
