use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("config file not found: {0}")]
    ConfigNotFound(String),

    #[error("invalid config: {0}")]
    ConfigParse(#[from] toml::de::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, AppError>;
