use thiserror::Error;

#[derive(Debug, Error)]
pub enum OpError {
    #[error("{0}")]
    Message(String),
    #[error("That record isn't here.")]
    NotFound,
    #[error("Something went wrong: {0}")]
    Db(#[from] rusqlite::Error),
}

impl OpError {
    pub fn message(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }
}
