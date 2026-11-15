//! The closed sets of words Marian stores in text columns and sends as
//! strings: an event's status, a series' cadence, a routine run's status, a
//! channel's kind.
//! Each is an enum that reads and writes exactly the words it always has, in
//! JSON and, with the `diesel` feature, in Postgres; a word outside the set
//! is an error, never a quiet default.

use std::fmt;

/// A stored or sent word that isn't one of its set's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownValue {
    /// What the word should have been, such as "event status".
    pub what: &'static str,
    pub value: String,
}

impl fmt::Display for UnknownValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown {} {:?}", self.what, self.value)
    }
}

impl std::error::Error for UnknownValue {}

/// An enum of unit variants, each written as one fixed word: `as_str`,
/// `Display` and serde give the word, `FromStr` and serde take it back and
/// refuse any other. `ALL` lists every variant in order.
#[macro_export]
macro_rules! text_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident ($what:literal) {
            $( $(#[$variant_meta:meta])* $variant:ident = $text:literal, )+
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        $vis enum $name {
            $( $(#[$variant_meta])* $variant, )+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// The word as it's stored and sent.
            pub fn as_str(self) -> &'static str {
                match self {
                    $( $name::$variant => $text, )+
                }
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = $crate::values::UnknownValue;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                match text {
                    $( $text => Ok($name::$variant), )+
                    _ => Err($crate::values::UnknownValue {
                        what: $what,
                        value: text.to_string(),
                    }),
                }
            }
        }

        impl $crate::values::__serde::Serialize for $name {
            fn serialize<S: $crate::values::__serde::Serializer>(&self, out: S) -> Result<S::Ok, S::Error> {
                out.serialize_str(self.as_str())
            }
        }

        impl<'de> $crate::values::__serde::Deserialize<'de> for $name {
            fn deserialize<D: $crate::values::__serde::Deserializer<'de>>(reader: D) -> Result<Self, D::Error> {
                let text = <::std::borrow::Cow<'de, str> as $crate::values::__serde::Deserialize>::deserialize(reader)?;
                text.parse().map_err(<D::Error as $crate::values::__serde::de::Error>::custom)
            }
        }
    };
}

/// Reads and writes a `text_enum!` as a Postgres `TEXT`. Put
/// `#[derive(diesel::expression::AsExpression, diesel::deserialize::FromSqlRow)]` and
/// `#[diesel(sql_type = diesel::sql_types::Text)]` on the enum too. A stored
/// word outside the set fails the query.
#[macro_export]
macro_rules! text_sql {
    ($name:ident) => {
        impl ::diesel::serialize::ToSql<::diesel::sql_types::Text, ::diesel::pg::Pg> for $name {
            fn to_sql<'b>(
                &'b self,
                out: &mut ::diesel::serialize::Output<'b, '_, ::diesel::pg::Pg>,
            ) -> ::diesel::serialize::Result {
                ::std::io::Write::write_all(out, self.as_str().as_bytes())?;
                Ok(::diesel::serialize::IsNull::No)
            }
        }

        impl ::diesel::deserialize::FromSql<::diesel::sql_types::Text, ::diesel::pg::Pg> for $name {
            fn from_sql(value: ::diesel::pg::PgValue<'_>) -> ::diesel::deserialize::Result<Self> {
                let text = ::std::str::from_utf8(value.as_bytes())?;
                Ok(text.parse::<$name>()?)
            }
        }
    };
}

#[doc(hidden)]
pub use serde as __serde;

text_enum! {
    /// Where an event is in its life.
    #[cfg_attr(feature = "diesel", derive(diesel::expression::AsExpression, diesel::deserialize::FromSqlRow))]
    #[cfg_attr(feature = "diesel", diesel(sql_type = diesel::sql_types::Text))]
    pub enum EventStatus("event status") {
        /// Saved but not to be posted yet.
        Draft = "draft",
        /// To be posted at its post time.
        Scheduled = "scheduled",
        Posted = "posted",
        /// Over and put away.
        Archived = "archived",
        Cancelled = "cancelled",
        /// A series date left out.
        Skipped = "skipped",
        /// Its post time came and posting didn't work.
        Failed = "failed",
    }
}

#[cfg(feature = "diesel")]
text_sql!(EventStatus);

text_enum! {
    /// How often a series repeats.
    #[cfg_attr(feature = "diesel", derive(diesel::expression::AsExpression, diesel::deserialize::FromSqlRow))]
    #[cfg_attr(feature = "diesel", diesel(sql_type = diesel::sql_types::Text))]
    pub enum Cadence("cadence") {
        Once = "once",
        Daily = "daily",
        Weekly = "weekly",
        Biweekly = "biweekly",
        Monthly = "monthly",
    }
}

#[cfg(feature = "diesel")]
text_sql!(Cadence);

text_enum! {
    /// Where one week's run of a routine is.
    #[cfg_attr(feature = "diesel", derive(diesel::expression::AsExpression, diesel::deserialize::FromSqlRow))]
    #[cfg_attr(feature = "diesel", diesel(sql_type = diesel::sql_types::Text))]
    pub enum RunStatus("routine run status") {
        /// Its poll opens later.
        Scheduled = "scheduled",
        /// Its poll takes votes.
        Open = "open",
        /// Its poll closed and the week's event is made.
        Locked = "locked",
        /// Its poll closed and waits for someone to confirm the day.
        Approval = "approval",
        /// Nobody voted, or the approval said to skip the week.
        Skipped = "skipped",
        /// The approval lapsed unanswered.
        Expired = "expired",
    }
}

#[cfg(feature = "diesel")]
text_sql!(RunStatus);

text_enum! {
    /// What a channel is for.
    #[cfg_attr(feature = "diesel", derive(diesel::expression::AsExpression, diesel::deserialize::FromSqlRow))]
    #[cfg_attr(feature = "diesel", diesel(sql_type = diesel::sql_types::Text))]
    pub enum ChannelKind("channel kind") {
        /// One of the server's channels.
        Text = "text",
        /// A channel made for events, from Marian.
        Event = "event",
    }
}

#[cfg(feature = "diesel")]
text_sql!(ChannelKind);

#[cfg(test)]
mod tests {
    use super::{Cadence, EventStatus, RunStatus};

    #[test]
    fn every_word_reads_and_writes_as_itself() {
        for status in EventStatus::ALL {
            let json = format!("\"{}\"", status.as_str());
            assert_eq!(serde_json::to_string(status).unwrap(), json);
            assert_eq!(serde_json::from_str::<EventStatus>(&json).unwrap(), *status);
            assert_eq!(status.to_string().parse::<EventStatus>(), Ok(*status));
        }
        for cadence in Cadence::ALL {
            assert_eq!(cadence.as_str().parse::<Cadence>(), Ok(*cadence));
        }
        for status in RunStatus::ALL {
            assert_eq!(status.as_str().parse::<RunStatus>(), Ok(*status));
        }
        assert_eq!(
            serde_json::to_string(&EventStatus::Failed).unwrap(),
            "\"failed\""
        );
        assert_eq!(
            serde_json::to_string(&Cadence::Biweekly).unwrap(),
            "\"biweekly\""
        );
    }

    #[test]
    fn a_word_outside_the_set_is_an_error() {
        let unknown = "postd".parse::<EventStatus>().unwrap_err();
        assert_eq!(unknown.to_string(), "unknown event status \"postd\"");
        assert!(serde_json::from_str::<Cadence>("\"yearly\"").is_err());
        assert!(
            "Open".parse::<RunStatus>().is_err(),
            "words are matched exactly"
        );
    }
}
