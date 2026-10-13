#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("tcgcsv payload is not json: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("tcgcsv: {0}")]
    Source(String),
    #[error("fetch {url}: {message}")]
    Fetch { url: String, message: String },
    #[error("landing: {0}")]
    Landing(String),
}

pub type Result<T> = std::result::Result<T, Error>;
