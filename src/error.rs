use thiserror::Error;

use crate::i18n::{Lang, Msg};

#[derive(Debug, Error)]
pub enum OpError {
    #[error("{0}")]
    Message(String),
    /// A message in the reader's language: English on the web, the server's on Discord.
    #[error("{0}")]
    Text(Msg),
    #[error("That record isn't here.")]
    NotFound,
    #[error("Something went wrong: {0}")]
    Db(#[from] diesel::result::Error),
}

impl OpError {
    pub fn message(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }

    /// The error as `lang` reads it.
    pub fn localized(&self, lang: Lang) -> String {
        match self {
            Self::Message(text) => text.clone(),
            Self::Text(msg) => msg.render(lang),
            Self::NotFound => crate::tr!(lang, "err-not-found"),
            Self::Db(err) => crate::tr!(lang, "err-db", detail = err.to_string()),
        }
    }
}

impl From<Msg> for OpError {
    fn from(msg: Msg) -> Self {
        Self::Text(msg)
    }
}
