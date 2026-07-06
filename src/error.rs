use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("config file not found: {0}")]
    ConfigNotFound(String),

    #[error("invalid config: {0}")]
    ConfigParse(#[from] toml::de::Error),

    #[error("unknown harness '{name}' — add it to the [harnesses] section")]
    UnknownHarness { name: String },

    #[error("broken symlink at {path}: target no longer exists")]
    BrokenSymlink { path: String },

    #[error("conflict: {path} exists and is not a symlink")]
    PathConflict { path: String },

    #[error("symlink failed: {path} → {target}: {reason}")]
    SymlinkFailed {
        path: String,
        target: String,
        reason: String,
    },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, AppError>;
