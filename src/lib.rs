//! Marian keeps a Discord community's events, signups, and reminders.

pub mod api;
pub mod assets;
mod auth;
pub mod clock;
pub mod commands;
pub mod error;
pub mod gateway;
pub mod google;
pub mod ics;
pub(crate) mod ids;
pub mod jobs;
mod live;
pub mod logic;
mod observe;
pub mod parse;
mod release_version;
mod schema;
pub mod serve;
pub mod store;

pub use error::OpError;
