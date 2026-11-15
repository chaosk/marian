//! `/event`, `/edit`, `/reminders` and `/help`.

use crate::commands;
use crate::i18n::{self, Lang};
use crate::logic;
use crate::store;
use crate::tr;

use super::{complete_channel, complete_event, deliver, remember_actor};
use crate::gateway::{clip, dm_message, Context, Error};

/// Why the event questions didn't arrive, and where else to make the event.
/// `sent` is whether the DM opened and the message was refused.
fn dm_blocked(lang: Lang, sent: bool, public_url: &str, server: Option<u64>) -> String {
    let lead = if sent {
        tr!(lang, "reply-dm-not-sent")
    } else {
        tr!(lang, "reply-dm-not-opened")
    };
    let base = public_url.trim().trim_end_matches('/');
    let web = match (base.is_empty(), server) {
        (false, Some(server)) => tr!(
            lang,
            "reply-dm-web-at",
            url = format!("{base}/servers/{server}/events/new")
        ),
        (false, None) => tr!(lang, "reply-dm-web-at", url = base),
        _ => tr!(lang, "reply-dm-web"),
    };
    format!("{lead} {} {web}", tr!(lang, "reply-dm-allow"))
}

#[poise::command(
    slash_command,
    guild_only,
    description_localized("en-US", "Start an event. Marian asks for the details in a DM")
)]
pub(super) async fn event(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(actor) = remember_actor(&ctx).await? else {
        ctx.say(tr!(Lang::En, "reply-event-outside")).await?;
        return Ok(());
    };
    let invoked_id = ctx.channel_id().get().to_string();
    let invoked_name = ctx
        .guild_channel()
        .await
        .map(|channel| channel.name)
        .unwrap_or_default();
    let started = {
        let mut conn = ctx
            .data()
            .db
            .get()
            .await
            .map_err(|_| "Could not reach the database.")?;
        let channel = logic::ensure_discord_channel(
            &mut conn,
            actor.guild_id,
            &invoked_id,
            if invoked_name.is_empty() {
                &invoked_id
            } else {
                &invoked_name
            },
        )
        .await?;
        commands::start_event_dm(
            &mut conn,
            actor.guild_id,
            actor.membership_id,
            &actor.discord_user,
            &channel.name,
            Some(&invoked_id),
            actor.lang,
        )
        .await
    };
    let question = match started {
        Ok(question) => question,
        Err(err) => {
            ctx.say(clip(&err.localized(actor.lang), 1800)).await?;
            return Ok(());
        }
    };
    match ctx.author().create_dm_channel(ctx.http()).await {
        Ok(channel) => {
            if let Err(err) = channel
                .send_message(ctx.http(), dm_message(&question))
                .await
            {
                {
                    let mut conn = ctx
                        .data()
                        .db
                        .get()
                        .await
                        .map_err(|_| "Could not reach the database.")?;
                    commands::cancel_event_dm(&mut conn, &actor.discord_user).await?;
                }
                tracing::warn!("could not send the event questions: {err}");
                ctx.say(dm_blocked(
                    actor.lang,
                    true,
                    &ctx.data().public_url,
                    ctx.guild_id().map(|id| id.get()),
                ))
                .await?;
                return Ok(());
            }
            ctx.say(tr!(actor.lang, "reply-dm-sent")).await?;
        }
        Err(err) => {
            {
                let mut conn = ctx
                    .data()
                    .db
                    .get()
                    .await
                    .map_err(|_| "Could not reach the database.")?;
                commands::cancel_event_dm(&mut conn, &actor.discord_user).await?;
            }
            tracing::warn!("could not open a DM: {err}");
            ctx.say(dm_blocked(
                actor.lang,
                false,
                &ctx.data().public_url,
                ctx.guild_id().map(|id| id.get()),
            ))
            .await?;
        }
    }
    Ok(())
}

#[poise::command(
    slash_command,
    guild_only,
    description_localized("en-US", "Change an event")
)]
pub(super) async fn edit(
    ctx: Context<'_>,
    #[description = "The event to change"]
    #[autocomplete = "complete_event"]
    event: String,
    #[description = "New title"] title: Option<String>,
    #[description = "New description"] description: Option<String>,
    #[description = "New start, such as friday 7pm"] start: Option<String>,
    #[description = "Length in minutes"] duration: Option<i64>,
    #[description = "Channel to post in"]
    #[autocomplete = "complete_channel"]
    channel: Option<String>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(actor) = remember_actor(&ctx).await? else {
        ctx.say(tr!(Lang::En, "reply-outside")).await?;
        return Ok(());
    };
    let label = format!("/edit {event}");
    let outcome = {
        let mut conn = ctx
            .data()
            .db
            .get()
            .await
            .map_err(|_| "Could not reach the database.")?;
        commands::edit_event(
            &mut conn,
            actor.guild_id,
            actor.membership_id,
            commands::EditChange {
                event,
                title,
                description,
                start,
                duration,
                channel,
            },
            actor.lang,
        )
        .await
    };
    deliver(ctx, &actor, &label, outcome).await
}

#[poise::command(
    slash_command,
    guild_only,
    description_localized("en-US", "Turn your reminders on or off")
)]
pub(super) async fn reminders(
    ctx: Context<'_>,
    #[description = "On or off. Leave empty to see the current choice"] enabled: Option<bool>,
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
        commands::set_reminders(&mut conn, actor.membership_id, enabled, actor.lang).await
    };
    deliver(ctx, &actor, "/reminders", outcome).await
}

#[poise::command(
    slash_command,
    guild_only,
    description_localized(
        "en-US",
        "How /event works, and where the other commands are explained"
    )
)]
pub(super) async fn help(ctx: Context<'_>) -> Result<(), Error> {
    let lang = match ctx.guild_id() {
        Some(guild_id) => {
            let mut conn = ctx
                .data()
                .db
                .get()
                .await
                .map_err(|_| "Could not reach the database.")?;
            match store::guild_by_discord(&mut conn, &guild_id.get().to_string()).await? {
                Some(guild) => i18n::lang_of(&mut conn, guild.id).await,
                None => Lang::En,
            }
        }
        None => Lang::En,
    };
    let base = ctx.data().public_url.trim().trim_end_matches('/');
    let more = if base.is_empty() {
        tr!(lang, "reply-help-more-page")
    } else {
        tr!(lang, "reply-help-more-at", url = format!("{base}/help"))
    };
    ctx.say(format!("{}\n\n{more}", tr!(lang, "reply-help")))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::dm_blocked;
    use crate::i18n::Lang;

    #[test]
    fn a_blocked_dm_offers_the_web_page() {
        let noted = dm_blocked(Lang::En, true, "https://marian.example.com/", Some(4242));
        assert_eq!(
            noted,
            "I couldn't send the DM. Allow direct messages from server members, then run /event again. You can always create the event at https://marian.example.com/servers/4242/events/new."
        );
        let plain = dm_blocked(Lang::En, false, "  ", None);
        assert_eq!(
            plain,
            "I couldn't open a DM. Allow direct messages from server members, then run /event again. You can always create the event on the web."
        );
        let polish = dm_blocked(Lang::Pl, true, "https://marian.example.com", Some(4242));
        assert!(
            polish.starts_with("Nie udało się wysłać wiadomości prywatnej."),
            "{polish}"
        );
        assert!(
            polish.ends_with("https://marian.example.com/servers/4242/events/new."),
            "{polish}"
        );
    }
}
