//! Configuration loading and validation errors.

use std::path::PathBuf;

/// Errors produced while reading, parsing, or validating configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Filesystem read failure.
    #[error("failed to read config file {path}: {source}")]
    Io {
        /// Path that could not be read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// YAML syntax or structural serde failure.
    #[error("invalid YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),

    /// Schema or semantic validation failure.
    #[error("invalid configuration: {0}")]
    Validation(String),
}

impl ConfigError {
    /// Build a validation error from a displayable message.
    pub fn validation(msg: impl Into<String>) -> Self {
        Self::Validation(msg.into())
    }
}
