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
    /// The detail is for the log; people read only that something went wrong.
    #[error("Something went wrong: {0}")]
    Db(#[from] diesel::result::Error),
    /// A failure on Marian's side rather than in what was asked, such as a
    /// database it can't open. Like `Db`, the detail is only logged.
    #[error("{0}")]
    Internal(String),
}

impl OpError {
    pub fn message(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }

    pub fn internal(detail: impl Into<String>) -> Self {
        Self::Internal(detail.into())
    }

    /// Whether the fault is Marian's, not the request's.
    pub fn is_internal(&self) -> bool {
        matches!(self, Self::Db(_) | Self::Internal(_))
    }

    /// The error as `lang` reads it. An internal one says only that
    /// something went wrong, and its detail goes to the log.
    pub fn localized(&self, lang: Lang) -> String {
        match self {
            Self::Message(text) => text.clone(),
            Self::Text(msg) => msg.render(lang),
            Self::NotFound => crate::tr!(lang, "err-not-found"),
            Self::Db(_) | Self::Internal(_) => {
                tracing::error!(error = %self, "internal error");
                crate::tr!(lang, "err-db")
            }
        }
    }
}

/// Any error as `lang` reads it, for errors that may carry an [`OpError`] or
/// a database error inside: those show no detail, which goes to the log.
pub fn user_text(err: &(dyn std::error::Error + 'static), lang: Lang) -> String {
    if let Some(err) = err.downcast_ref::<OpError>() {
        return err.localized(lang);
    }
    if err.downcast_ref::<diesel::result::Error>().is_some()
        || err.downcast_ref::<diesel::r2d2::PoolError>().is_some()
    {
        tracing::error!(error = %err, "internal error");
        return crate::tr!(lang, "err-db");
    }
    err.to_string()
}

impl From<Msg> for OpError {
    fn from(msg: Msg) -> Self {
        Self::Text(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diesel::result::{DatabaseErrorKind, Error as DieselError};

    fn constraint_error() -> OpError {
        OpError::Db(DieselError::DatabaseError(
            DatabaseErrorKind::UniqueViolation,
            Box::new(
                r#"duplicate key value violates unique constraint "events_public_id_key""#
                    .to_string(),
            ),
        ))
    }

    #[test]
    fn a_database_error_reads_without_its_detail() {
        let err = constraint_error();
        for lang in [Lang::En, Lang::Pl] {
            let text = err.localized(lang);
            assert!(!text.contains("events_public_id_key"), "{text}");
            assert!(!text.contains("duplicate key"), "{text}");
            assert!(!text.is_empty());
        }
        assert_eq!(
            err.localized(Lang::En),
            "Something went wrong. Please try again."
        );
        // The log still gets it.
        assert!(err.to_string().contains("events_public_id_key"));
        assert!(err.is_internal());
    }

    #[test]
    fn an_error_passed_along_boxed_hides_database_detail_too() {
        let boxed: Box<dyn std::error::Error + Send + Sync> = Box::new(constraint_error());
        assert!(!user_text(boxed.as_ref(), Lang::En).contains("events_public_id_key"));
        let raw: Box<dyn std::error::Error + Send + Sync> = Box::new(DieselError::DatabaseError(
            DatabaseErrorKind::ForeignKeyViolation,
            Box::new(r#"violates foreign key constraint "signups_event_id_fkey""#.to_string()),
        ));
        assert!(!user_text(raw.as_ref(), Lang::En).contains("signups_event_id_fkey"));
        let internal =
            OpError::internal("Could not open the database: password authentication failed");
        assert!(!internal.localized(Lang::En).contains("password"));
        // What a person asked wrongly still reads as it is.
        let plain: Box<dyn std::error::Error + Send + Sync> =
            OpError::message("Pick a time.").into();
        assert_eq!(user_text(plain.as_ref(), Lang::En), "Pick a time.");
    }
}
