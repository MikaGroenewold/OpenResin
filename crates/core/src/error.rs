use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported file type: {0}")]
    UnsupportedFormat(String),
    #[error("failed to parse {path}: {msg}")]
    Parse { path: PathBuf, msg: String },
    #[error("invalid data: {0}")]
    Invalid(String),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("operation cancelled")]
    Cancelled,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn parse(path: impl Into<PathBuf>, msg: impl Into<String>) -> Self {
        Error::Parse {
            path: path.into(),
            msg: msg.into(),
        }
    }
}
