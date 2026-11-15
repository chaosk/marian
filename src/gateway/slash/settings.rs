//! `/settings`.

use crate::commands;
use crate::i18n::{self, Lang};
use crate::tr;

use super::{deliver, register_commands, remember_actor, Actor};
use crate::gateway::{Context, Error};

async fn setting(ctx: Context<'_>, label: &str, change: commands::Setting) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(actor) = remember_actor(&ctx).await? else {
        ctx.say(tr!(Lang::En, "reply-outside")).await?;
        return Ok(());
    };
    let language = matches!(change, commands::Setting::Language(_));
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
    if language && outcome.is_ok() {
        // The commands' descriptions follow the server's language.
        if let Some(guild_id) = ctx.guild_id() {
            if let Err(err) = register_commands(ctx.http(), guild_id, lang).await {
                tracing::warn!("could not register commands in {guild_id}: {err}");
            }
        }
    }
    let actor = Actor { lang, ..actor };
    deliver(ctx, &actor, label, outcome).await
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

#[poise::command(
    slash_command,
    guild_only,
    rename = "reminder_interval",
    description_localized("en-US", "Default reminder, in minutes")
)]
async fn settings_reminder_interval(
    ctx: Context<'_>,
    #[description = "Minutes before the event"] minutes: i64,
) -> Result<(), Error> {
    setting(
        ctx,
        "/settings reminder_interval",
        commands::Setting::ReminderInterval(minutes),
    )
    .await
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "hour_24",
    description_localized("en-US", "Use a 24-hour clock")
)]
async fn settings_hour_24(
    ctx: Context<'_>,
    #[description = "On uses a 24-hour clock"] enabled: bool,
) -> Result<(), Error> {
    setting(ctx, "/settings hour_24", commands::Setting::Hour24(enabled)).await
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "auto_archive",
    description_localized("en-US", "Archive posted events on a timer")
)]
async fn settings_auto_archive(
    ctx: Context<'_>,
    #[description = "Archive posted events after they end"] enabled: bool,
) -> Result<(), Error> {
    setting(
        ctx,
        "/settings auto_archive",
        commands::Setting::AutoArchive(enabled),
    )
    .await
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "auto_archive_interval",
    description_localized("en-US", "Hours after which a posted event archives")
)]
async fn settings_auto_archive_interval(
    ctx: Context<'_>,
    #[description = "Hours after the event ends"] hours: i64,
) -> Result<(), Error> {
    setting(
        ctx,
        "/settings auto_archive_interval",
        commands::Setting::AutoArchiveHours(hours),
    )
    .await
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "auto_join_threads",
    description_localized("en-US", "Join event threads for attendees")
)]
async fn settings_auto_join_threads(
    ctx: Context<'_>,
    #[description = "Add attendees to the event thread"] enabled: bool,
) -> Result<(), Error> {
    setting(
        ctx,
        "/settings auto_join_threads",
        commands::Setting::AutoJoinThreads(enabled),
    )
    .await
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "event_threads",
    description_localized("en-US", "Open a thread for each posted event")
)]
async fn settings_event_threads(
    ctx: Context<'_>,
    #[description = "Open a thread when an event is posted"] enabled: bool,
) -> Result<(), Error> {
    setting(
        ctx,
        "/settings event_threads",
        commands::Setting::EventThreads(enabled),
    )
    .await
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "localize_time",
    description_localized("en-US", "Show the start in each member's own zone")
)]
async fn settings_localize_time(
    ctx: Context<'_>,
    #[description = "Members see the start in their own zone"] enabled: bool,
) -> Result<(), Error> {
    setting(
        ctx,
        "/settings localize_time",
        commands::Setting::LocalizeTime(enabled),
    )
    .await
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "purge",
    description_localized("en-US", "Purge other messages from event channels")
)]
async fn settings_purge(
    ctx: Context<'_>,
    #[description = "On removes other messages when an event is posted or deleted"] enabled: bool,
) -> Result<(), Error> {
    setting(
        ctx,
        "/settings purge",
        commands::Setting::PurgeChannels(enabled),
    )
    .await
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "thread_reminders",
    description_localized("en-US", "Post reminders in the event thread")
)]
async fn settings_thread_reminders(
    ctx: Context<'_>,
    #[description = "Reminders post in the event thread"] enabled: bool,
) -> Result<(), Error> {
    setting(
        ctx,
        "/settings thread_reminders",
        commands::Setting::ThreadReminders(enabled),
    )
    .await
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "show_attendees",
    description_localized("en-US", "List names on the Discord post")
)]
async fn settings_show_attendees(
    ctx: Context<'_>,
    #[description = "The post lists who accepted each option"] enabled: bool,
) -> Result<(), Error> {
    setting(
        ctx,
        "/settings show_attendees",
        commands::Setting::ShowAttendees(enabled),
    )
    .await
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "temp_roles",
    description_localized("en-US", "Give attendees a temporary role")
)]
async fn settings_temp_roles(
    ctx: Context<'_>,
    #[description = "Create an attendee role for the event"] enabled: bool,
) -> Result<(), Error> {
    setting(
        ctx,
        "/settings temp_roles",
        commands::Setting::TempRoles(enabled),
    )
    .await
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "waitlist",
    description_localized("en-US", "Let full options take a waitlist")
)]
async fn settings_waitlist(
    ctx: Context<'_>,
    #[description = "Sign people up to a waitlist when an option is full"] enabled: bool,
) -> Result<(), Error> {
    setting(
        ctx,
        "/settings waitlist",
        commands::Setting::Waitlist(enabled),
    )
    .await
}

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

#[poise::command(
    slash_command,
    guild_only,
    rename = "post_lead",
    description_localized("en-US", "How early a series date is posted")
)]
async fn settings_post_lead(
    ctx: Context<'_>,
    #[description = "Hours before the start"] hours: i64,
) -> Result<(), Error> {
    setting(
        ctx,
        "/settings post_lead",
        commands::Setting::PostLeadHours(hours),
    )
    .await
}

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
