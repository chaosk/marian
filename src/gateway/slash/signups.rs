//! `/signup_options` and `/signup_presets`.

use crate::commands;
use crate::i18n::Lang;
use crate::tr;

use super::{deliver, remember_actor};
use crate::gateway::{Context, Error};

#[poise::command(
    slash_command,
    guild_only,
    subcommands("signup_options_list", "signup_options_add", "signup_options_remove"),
    description_localized("en-US", "List, add, or remove a library option")
)]
pub(super) async fn signup_options(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "list",
    description_localized("en-US", "List the option library")
)]
async fn signup_options_list(ctx: Context<'_>) -> Result<(), Error> {
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
            .await
            .map_err(|_| "Could not reach the database.")?;
        commands::library_text(&mut conn, actor.guild_id, actor.lang).await
    };
    deliver(ctx, &actor, "/signup_options list", outcome).await
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "add",
    description_localized("en-US", "Save an option in the library")
)]
async fn signup_options_add(
    ctx: Context<'_>,
    #[description = "Option name"] name: String,
    #[description = "Emoji shown with the name"] emoji: Option<String>,
    #[description = "People on it get reminders and the attendee role (default yes)"]
    attending: Option<bool>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(actor) = remember_actor(&ctx).await? else {
        ctx.say(tr!(Lang::En, "reply-outside")).await?;
        return Ok(());
    };
    let emoji = emoji.unwrap_or_else(|| "•".into());
    let outcome = {
        let mut conn = ctx
            .data()
            .db
            .get()
            .await
            .map_err(|_| "Could not reach the database.")?;
        commands::add_library_option(
            &mut conn,
            actor.guild_id,
            actor.membership_id,
            &name,
            &emoji,
            attending.unwrap_or(true),
            actor.lang,
        )
        .await
    };
    deliver(ctx, &actor, "/signup_options add", outcome).await
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "remove",
    description_localized("en-US", "Remove an option from the library")
)]
async fn signup_options_remove(
    ctx: Context<'_>,
    #[description = "Option name"] name: String,
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
            .await
            .map_err(|_| "Could not reach the database.")?;
        commands::remove_library_option(
            &mut conn,
            actor.guild_id,
            actor.membership_id,
            &name,
            actor.lang,
        )
        .await
    };
    deliver(ctx, &actor, "/signup_options remove", outcome).await
}

#[poise::command(
    slash_command,
    guild_only,
    subcommands("signup_presets_list", "signup_presets_add", "signup_presets_remove"),
    description_localized("en-US", "List, save, or remove a signup preset")
)]
pub(super) async fn signup_presets(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "list",
    description_localized("en-US", "List signup presets")
)]
async fn signup_presets_list(ctx: Context<'_>) -> Result<(), Error> {
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
            .await
            .map_err(|_| "Could not reach the database.")?;
        commands::preset_text(&mut conn, actor.guild_id, actor.lang).await
    };
    deliver(ctx, &actor, "/signup_presets list", outcome).await
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "add",
    description_localized("en-US", "Save a signup preset")
)]
async fn signup_presets_add(
    ctx: Context<'_>,
    #[description = "Preset name"] name: String,
    #[description = "Option names, separated by commas"] options: String,
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
            .await
            .map_err(|_| "Could not reach the database.")?;
        commands::add_preset(
            &mut conn,
            actor.guild_id,
            actor.membership_id,
            &name,
            &options,
            actor.lang,
        )
        .await
    };
    deliver(ctx, &actor, "/signup_presets add", outcome).await
}

#[poise::command(
    slash_command,
    guild_only,
    rename = "remove",
    description_localized("en-US", "Remove a signup preset")
)]
async fn signup_presets_remove(
    ctx: Context<'_>,
    #[description = "Preset name"] name: String,
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
            .await
            .map_err(|_| "Could not reach the database.")?;
        commands::remove_preset(
            &mut conn,
            actor.guild_id,
            actor.membership_id,
            &name,
            actor.lang,
        )
        .await
    };
    deliver(ctx, &actor, "/signup_presets remove", outcome).await
}
