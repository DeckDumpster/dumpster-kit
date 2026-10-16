/// What went wrong talking to the lake.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0} is not set — it must be {1}")]
    MissingEnv(&'static str, &'static str),
    #[error("iceberg: {0}")]
    Iceberg(#[from] iceberg::Error),
    #[error("arrow: {0}")]
    Arrow(#[from] arrow_schema::ArrowError),
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;
