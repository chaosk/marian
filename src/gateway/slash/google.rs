//! `/google`.

use poise::serenity_prelude as serenity;

use crate::commands;
use crate::i18n::Lang;
use crate::tr;

use super::{complete_event, deliver, remember_actor};
use crate::gateway::{Context, Error};

#[poise::command(
    slash_command,
    guild_only,
    subcommands("google_authorize", "google_auto_sync", "google_sync", "google_revoke"),
    description_localized("en-US", "Connect, update, or disconnect a Google Calendar")
)]
pub(super) async fn google(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "authorize",
    description_localized("en-US", "Sign in with Google")
)]
async fn google_authorize(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(actor) = remember_actor(&ctx).await? else {
        ctx.say(tr!(Lang::En, "reply-outside")).await?;
        return Ok(());
    };
    let outcome = {
        let mut conn = ctx
            .data()
            .db
            .get()
            .map_err(|_| "Could not reach the database.")?;
        commands::google_sign_in_url(&mut conn, actor.membership_id)
    };
    let url = match outcome {
        Ok(url) => url,
        Err(err) => return deliver(ctx, &actor, "/google authorize", Err(err)).await,
    };
    ctx.send(
        poise::CreateReply::default()
            .content(tr!(actor.lang, "reply-google-click"))
            .components(vec![serenity::CreateActionRow::Buttons(vec![
                serenity::CreateButton::new_link(url).label(tr!(actor.lang, "reply-google-button")),
            ])]),
    )
    .await?;
    Ok(())
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "auto_sync",
    description_localized("en-US", "Keep your Google Calendar updated")
)]
async fn google_auto_sync(
    ctx: Context<'_>,
    #[description = "Keep adding and updating events"] enabled: bool,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(actor) = remember_actor(&ctx).await? else {
        ctx.say(tr!(Lang::En, "reply-outside")).await?;
        return Ok(());
    };
    let outcome = {
        let mut conn = ctx
            .data()
            .db
            .get()
            .map_err(|_| "Could not reach the database.")?;
        commands::google_auto_sync(
            &mut conn,
            actor.guild_id,
            actor.membership_id,
            enabled,
            actor.lang,
        )
    };
    deliver(ctx, &actor, "/google auto_sync", outcome).await
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "sync",
    description_localized("en-US", "Add one event to your Google Calendar")
)]
async fn google_sync(
    ctx: Context<'_>,
    #[description = "The event to add"]
    #[autocomplete = "complete_event"]
    event: String,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(actor) = remember_actor(&ctx).await? else {
        ctx.say(tr!(Lang::En, "reply-outside")).await?;
        return Ok(());
    };
    let label = format!("/google sync {event}");
    let outcome = {
        let mut conn = ctx
            .data()
            .db
            .get()
            .map_err(|_| "Could not reach the database.")?;
        commands::google_sync(
            &mut conn,
            actor.guild_id,
            actor.membership_id,
            &event,
            actor.lang,
        )
    };
    deliver(ctx, &actor, &label, outcome).await
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "revoke",
    description_localized("en-US", "Disconnect Google Calendar")
)]
async fn google_revoke(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(actor) = remember_actor(&ctx).await? else {
        ctx.say(tr!(Lang::En, "reply-outside")).await?;
        return Ok(());
    };
    let outcome = {
        let mut conn = ctx
            .data()
            .db
            .get()
            .map_err(|_| "Could not reach the database.")?;
        commands::google_revoke(&mut conn, actor.guild_id, actor.membership_id, actor.lang)
    };
    deliver(ctx, &actor, "/google revoke", outcome).await
}
