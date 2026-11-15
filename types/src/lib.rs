//! What Marian's server sends and the browser app reads, and how both word it.

pub mod dto;
pub mod labels;
pub mod time;
pub mod values;

pub use dto::*;
pub use values::{Cadence, ChannelKind, EventStatus, RunStatus};
