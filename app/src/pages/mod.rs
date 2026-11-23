pub mod channels;
pub mod event;
pub mod home;
pub mod lists;
pub mod misc;
pub mod presets;
pub mod routines;
pub mod series;
pub mod settings;

use marian_types as dto;

/// The channel a new event or routine starts in: where the person last made
/// an event, else the server's default channel, else the first one listed.
pub(crate) fn starting_channel<'a>(
    channels: &'a [dto::Channel],
    membership: &dto::Membership,
) -> Option<&'a dto::Channel> {
    membership
        .new_event_channel
        .as_ref()
        .and_then(|id| channels.iter().find(|channel| &channel.id == id))
        .or_else(|| channels.first())
}
