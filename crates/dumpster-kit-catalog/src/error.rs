#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("{game} prices by printing id; a finish ({finish:?}) is not allowed")]
    FinishNotAllowed { game: &'static str, finish: String },
    #[error("{game} prices per finish; printing {printing_id} has none")]
    FinishRequired {
        game: &'static str,
        printing_id: i64,
    },
    #[error("parse: {0}")]
    Parse(String),
}

pub type Result<T> = std::result::Result<T, Error>;
