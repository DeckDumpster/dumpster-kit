#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io at {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("fetch {source_name}: {message}")]
    Fetch {
        source_name: String,
        message: String,
    },
    #[error("parse {source_name}: {message}")]
    Parse {
        source_name: String,
        message: String,
    },
    #[error("apply: {0}")]
    Apply(String),
    #[error("{game} prices by printing id; a finish ({finish:?}) is not allowed")]
    FinishNotAllowed { game: &'static str, finish: String },
    #[error("{game} prices per finish; printing {printing_id} has none")]
    FinishRequired {
        game: &'static str,
        printing_id: String,
    },
    #[error("landed file {path} is not a landed payload name")]
    BadLanding { path: String },
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn io(path: &std::path::Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.display().to_string(),
        source,
    }
}
