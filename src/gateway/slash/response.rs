//! `/response`: adding, removing or moving someone on an event.

use poise::serenity_prelude as serenity;

use crate::commands;
use crate::i18n::Lang;
use crate::logic;
use crate::tr;

use super::{complete_event, complete_option, remember_actor};
use crate::gateway::follow::{redraw, server_avatar};
use crate::gateway::publish::{push_attendance, push_sync, tell_pages};
use crate::gateway::{clip, Context, Error};

#[poise::command(
    slash_command,
    guild_only,
    subcommands("response_add", "response_remove", "response_move"),
    description_localized("en-US", "Add, remove, or move a response")
)]
pub(super) async fn response(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "add",
    description_localized("en-US", "Add someone to a signup option")
)]
async fn response_add(
    ctx: Context<'_>,
    #[description = "The event to add an attendee"]
    #[autocomplete = "complete_event"]
    event: String,
    #[description = "The person to add"] user: serenity::User,
    #[description = "The signup option"]
    #[autocomplete = "complete_option"]
    option: String,
    #[description = "Guests they bring, when the option takes guests"]
    #[min = 0]
    #[max = 10]
    guests: Option<i64>,
) -> Result<(), Error> {
    run_response(ctx, "add", &event, &user, &option, guests).await
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "remove",
    description_localized("en-US", "Remove someone from a signup option")
)]
async fn response_remove(
    ctx: Context<'_>,
    #[description = "The event to remove them from"]
    #[autocomplete = "complete_event"]
    event: String,
    #[description = "The person to remove"] user: serenity::User,
    #[description = "The signup option"]
    #[autocomplete = "complete_option"]
    option: String,
) -> Result<(), Error> {
    run_response(ctx, "remove", &event, &user, &option, None).await
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "move",
    description_localized("en-US", "Move someone to another signup option")
)]
async fn response_move(
    ctx: Context<'_>,
    #[description = "The event to move them on"]
    #[autocomplete = "complete_event"]
    event: String,
    #[description = "The person to move"] user: serenity::User,
    #[description = "The option to move them to"]
    #[autocomplete = "complete_option"]
    option: String,
    #[description = "Guests they bring, when the option takes guests"]
    #[min = 0]
    #[max = 10]
    guests: Option<i64>,
) -> Result<(), Error> {
    run_response(ctx, "move", &event, &user, &option, guests).await
}

async fn run_response(
    ctx: Context<'_>,
    action: &str,
    event: &str,
    user: &serenity::User,
    option: &str,
    guests: Option<i64>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(actor) = remember_actor(&ctx).await? else {
        ctx.say(tr!(Lang::En, "reply-outside")).await?;
        return Ok(());
    };
    let member = match ctx.guild_id() {
        Some(guild) => guild.member(ctx, user.id).await.ok(),
        None => None,
    };
    let name = crate::auth::discord_display_name(
        member.as_ref().and_then(|member| member.nick.as_deref()),
        user.global_name.as_deref(),
        &user.name,
    );
    let data = ctx.data();
    let outcome = {
        let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
        let (target, renamed) = logic::ensure_discord_user(
            &mut conn,
            actor.guild_id,
            &user.id.get().to_string(),
            &name,
        )?;
        redraw(&data.jobs, renamed);
        if let (Some(guild), Some(member)) = (ctx.guild_id(), &member) {
            let avatar = server_avatar(guild, user, member.avatar.as_ref());
            logic::follow_discord_avatar(&mut conn, &target, avatar.as_deref())?;
        }
        commands::roster_action(
            &mut conn,
            actor.membership_id,
            event,
            target.id,
            option,
            action,
            guests,
            actor.lang,
        )
    };
    match outcome {
        Ok((reply, event_id)) => {
            tell_pages(&data.db, event_id, true);
            if let Err(err) = push_sync(&data.db, &data.http, &data.public_url, event_id).await {
                tracing::warn!("could not refresh event {event_id}: {err}");
            }
            if let Err(err) = push_attendance(&data.db, &data.http, event_id).await {
                tracing::warn!("could not update attendance for event {event_id}: {err}");
            }
            ctx.say(clip(&reply, 1800)).await?;
        }
        Err(err) => {
            ctx.say(clip(&err.localized(actor.lang), 1800)).await?;
        }
    }
    Ok(())
}
