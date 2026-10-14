//! Marian keeps a Discord community's events, signups, and reminders.

mod assets;
mod auth;
pub mod clock;
pub mod commands;
pub mod error;
pub mod gateway;
pub mod ics;
mod ids;
pub mod jobs;
pub mod logic;
mod markdown;
mod observe;
pub mod parse;
pub mod present;
mod release_version;
pub mod serve;
pub mod store;

pub use error::OpError;
