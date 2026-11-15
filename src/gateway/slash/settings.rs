//! `/settings`.

use crate::commands;
use crate::i18n::{self, Lang};
use crate::tr;

use super::{deliver, remember_actor, Actor};
use crate::gateway::{Context, Error};

async fn setting(ctx: Context<'_>, label: &str, change: commands::Setting) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(actor) = remember_actor(&ctx).await? else {
        ctx.say(tr!(Lang::En, "reply-outside")).await?;
        return Ok(());
    };
    let (outcome, lang) = {
        let mut conn = ctx
            .data()
            .db
            .get()
            .map_err(|_| "Could not reach the database.")?;
        let outcome = commands::apply_setting(
            &mut conn,
            actor.guild_id,
            actor.membership_id,
            change,
            actor.lang,
        );
        (outcome, i18n::lang_of(&mut conn, actor.guild_id))
    };
    let actor = Actor { lang, ..actor };
    deliver(ctx, &actor, label, outcome).await
}

/// A `/settings` subcommand that takes one value and saves it as
/// `commands::Setting::$variant`: the command `rename`d `$name` with the
/// description `$about`, and one option `$param` described as `$param_about`.
// rustfmt re-indents the attribute inside on every run, so it never settles.
#[rustfmt::skip]
macro_rules! setting_command {
    ($fn:ident, $name:tt, $about:tt, $param:ident: $ty:ident = $param_about:tt, $variant:ident) => {
        #[poise::command(
            slash_command,
            guild_only,
            rename = $name,
            description_localized("en-US", $about)
        )]
        async fn $fn(
            ctx: Context<'_>,
            #[description = $param_about] $param: $ty,
        ) -> Result<(), Error> {
            setting(
                ctx,
                concat!("/settings ", $name),
                commands::Setting::$variant($param),
            )
            .await
        }
    };
}

#[poise::command(
    slash_command,
    guild_only,
    subcommands(
        "settings_reminder_interval",
        "settings_hour_24",
        "settings_auto_archive",
        "settings_auto_archive_interval",
        "settings_auto_join_threads",
        "settings_event_threads",
        "settings_localize_time",
        "settings_purge",
        "settings_thread_reminders",
        "settings_show_attendees",
        "settings_temp_roles",
        "settings_waitlist",
        "settings_digest",
        "settings_post_lead",
        "settings_role",
        "settings_language"
    ),
    description_localized("en-US", "Change a server setting")
)]
pub(super) async fn settings(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

setting_command!(
    settings_reminder_interval,
    "reminder_interval",
    "Default reminder, in minutes",
    minutes: i64 = "Minutes before the event",
    ReminderInterval
);

setting_command!(
    settings_hour_24,
    "hour_24",
    "Use a 24-hour clock",
    enabled: bool = "On uses a 24-hour clock",
    Hour24
);

setting_command!(
    settings_auto_archive,
    "auto_archive",
    "Archive posted events on a timer",
    enabled: bool = "Archive posted events after they end",
    AutoArchive
);

setting_command!(
    settings_auto_archive_interval,
    "auto_archive_interval",
    "Hours after which a posted event archives",
    hours: i64 = "Hours after the event ends",
    AutoArchiveHours
);

setting_command!(
    settings_auto_join_threads,
    "auto_join_threads",
    "Join event threads for attendees",
    enabled: bool = "Add attendees to the event thread",
    AutoJoinThreads
);

setting_command!(
    settings_event_threads,
    "event_threads",
    "Open a thread for each posted event",
    enabled: bool = "Open a thread when an event is posted",
    EventThreads
);

setting_command!(
    settings_localize_time,
    "localize_time",
    "Show the start in each member's own zone",
    enabled: bool = "Members see the start in their own zone",
    LocalizeTime
);

setting_command!(
    settings_purge,
    "purge",
    "Purge other messages from event channels",
    enabled: bool = "On removes other messages when an event is posted or deleted",
    PurgeChannels
);

setting_command!(
    settings_thread_reminders,
    "thread_reminders",
    "Post reminders in the event thread",
    enabled: bool = "Reminders post in the event thread",
    ThreadReminders
);

setting_command!(
    settings_show_attendees,
    "show_attendees",
    "List names on the Discord post",
    enabled: bool = "The post lists who accepted each option",
    ShowAttendees
);

setting_command!(
    settings_temp_roles,
    "temp_roles",
    "Give attendees a temporary role",
    enabled: bool = "Create an attendee role for the event",
    TempRoles
);

setting_command!(
    settings_waitlist,
    "waitlist",
    "Let full options take a waitlist",
    enabled: bool = "Sign people up to a waitlist when an option is full",
    Waitlist
);

#[poise::command(
    slash_command,
    guild_only,
    rename = "digest",
    description_localized("en-US", "How often Marian posts a digest")
)]
async fn settings_digest(
    ctx: Context<'_>,
    #[description = "Off, daily, or weekly"]
    #[choices("off", "daily", "weekly")]
    schedule: &'static str,
) -> Result<(), Error> {
    setting(
        ctx,
        "/settings digest",
        commands::Setting::Digest(schedule.to_string()),
    )
    .await
}

setting_command!(
    settings_post_lead,
    "post_lead",
    "How early a series date is posted",
    hours: i64 = "Hours before the start",
    PostLeadHours
);

#[poise::command(
    slash_command,
    guild_only,
    rename = "role",
    description_localized("en-US", "Choose which role may do something")
)]
async fn settings_role(
    ctx: Context<'_>,
    #[description = "What this role is allowed to do"]
    #[choices("event", "edit", "delete", "channel", "signup")]
    which: &'static str,
    #[description = "Role name. Leave empty to clear it"] name: Option<String>,
) -> Result<(), Error> {
    setting(
        ctx,
        "/settings role",
        commands::Setting::Role {
            which: which.to_string(),
            name: name.unwrap_or_default(),
        },
    )
    .await
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "language",
    description_localized("en-US", "The language Marian writes in on this server")
)]
async fn settings_language(
    ctx: Context<'_>,
    #[description = "English or Polski"]
    #[choices("English", "Polski")]
    language: &'static str,
) -> Result<(), Error> {
    let code = Lang::ALL
        .into_iter()
        .find(|lang| lang.name() == language)
        .unwrap_or_default()
        .code();
    setting(
        ctx,
        "/settings language",
        commands::Setting::Language(code.to_string()),
    )
    .await
}
