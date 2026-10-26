//! Static YAML configuration: parse → validate → compile immutable snapshot.

mod compile;
mod error;
mod raw;

pub use compile::{
    load_snapshot_from_path, load_snapshot_from_str, CompiledRoute, CompiledUpstream,
    DockerDiscovery, HostMatch, ListenerConfig, Snapshot, TlsConfig,
};
pub(crate) use compile::{normalize_config_host, validate_path_prefix};
pub use error::ConfigError;
