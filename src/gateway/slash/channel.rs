//! `/channel` and `/sort`.

use poise::serenity_prelude as serenity;

use crate::i18n::{self, Lang};
use crate::logic;
use crate::store;
use crate::tr;

use super::{deliver, remember_actor};
use crate::gateway::publish::{post_event, push_attendance};
use crate::gateway::{Context, Error};

#[poise::command(
    slash_command,
    guild_only,
    description_localized("en-US", "Create an event channel")
)]
pub(super) async fn channel(
    ctx: Context<'_>,
    #[description = "Channel name"] name: String,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(actor) = remember_actor(&ctx).await? else {
        ctx.say(tr!(Lang::En, "reply-outside")).await?;
        return Ok(());
    };
    let outcome = open_event_channel(
        ctx.http(),
        &ctx.data().db,
        actor.guild_id,
        actor.membership_id,
        &name,
    )
    .await;
    deliver(ctx, &actor, "/channel", outcome).await
}

pub async fn open_event_channel(
    http: &serenity::Http,
    db: &store::Db,
    guild_id: i64,
    membership_id: i64,
    name: &str,
) -> Result<String, crate::error::OpError> {
    let guild_discord = {
        let mut conn = db
            .get()
            .map_err(|_| crate::error::OpError::from(crate::msg!("err-database")))?;
        store::guild(&mut conn, guild_id)?
            .and_then(|guild| guild.discord_id)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| crate::error::OpError::from(crate::msg!("err-server-not-connected")))?
    };
    let guild_discord = guild_discord
        .parse::<u64>()
        .map_err(|_| crate::error::OpError::from(crate::msg!("err-server-not-connected")))?;
    let (channel_id, slug) = {
        let mut conn = db
            .get()
            .map_err(|_| crate::error::OpError::from(crate::msg!("err-database")))?;
        let id = logic::create_channel(&mut conn, guild_id, membership_id, name)?;
        let channel = store::channel(&mut conn, id)?.ok_or(crate::error::OpError::NotFound)?;
        (id, channel.name)
    };
    let created = serenity::GuildId::new(guild_discord)
        .create_channel(
            http,
            serenity::CreateChannel::new(&slug).kind(serenity::ChannelType::Text),
        )
        .await;
    let discord_channel = match created {
        Ok(channel) => channel,
        Err(err) => {
            tracing::warn!("could not create the Discord channel: {err}");
            if let Ok(mut conn) = db.get() {
                let _ = logic::discard_channel(&mut conn, channel_id);
            }
            return Err(crate::error::OpError::from(channel_create_failure(&err)));
        }
    };
    let mut conn = db
        .get()
        .map_err(|_| crate::error::OpError::from(crate::msg!("err-database")))?;
    match logic::bind_channel_discord(&mut conn, channel_id, &discord_channel.id.get().to_string())
    {
        Ok(notice) => Ok(notice.render(i18n::lang_of(&mut conn, guild_id))),
        Err(err) => {
            if let Err(delete_err) = discord_channel.delete(http).await {
                tracing::warn!("could not remove the Discord channel: {delete_err}");
            }
            let _ = logic::discard_channel(&mut conn, channel_id);
            Err(err)
        }
    }
}

fn channel_create_failure(err: &serenity::Error) -> crate::i18n::Msg {
    let text = err.to_string().to_ascii_lowercase();
    if text.contains("missing permission") || text.contains("50013") || text.contains("50001") {
        crate::msg!("err-manage-channels")
    } else {
        crate::msg!("err-channel-not-created")
    }
}

#[poise::command(
    slash_command,
    guild_only,
    description_localized("en-US", "Reverse the posts in this event channel")
)]
pub(super) async fn sort(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(actor) = remember_actor(&ctx).await? else {
        ctx.say(tr!(Lang::En, "reply-outside")).await?;
        return Ok(());
    };
    let discord_channel = ctx.channel_id().get().to_string();
    let plan = {
        let mut conn = ctx
            .data()
            .db
            .get()
            .map_err(|_| "Could not reach the database.")?;
        logic::channel_reverse_plan(&mut conn, actor.guild_id, &discord_channel)
    };
    let plan = match plan {
        Ok(plan) => plan,
        Err(err) => return deliver(ctx, &actor, "/sort", Err(err)).await,
    };
    let http = ctx
        .data()
        .http
        .lock()
        .map_err(|_| "http lock")?
        .clone()
        .ok_or("Discord is not connected")?;
    for message in &plan.messages {
        let message_id = serenity::MessageId::new(message.parse()?);
        if let Err(err) = ctx.channel_id().delete_message(&http, message_id).await {
            let text = err.to_string().to_ascii_lowercase();
            if !text.contains("unknown") {
                return Err(err.into());
            }
        }
    }
    {
        let mut conn = ctx
            .data()
            .db
            .get()
            .map_err(|_| "Could not reach the database.")?;
        logic::detach_discord_posts(&mut conn, &plan.repost)?;
    }
    for event_id in &plan.repost {
        post_event(
            &ctx.data().db,
            &ctx.data().http,
            &ctx.data().public_url,
            *event_id,
            false,
        )
        .await?;
        if let Err(err) = push_attendance(&ctx.data().db, &ctx.data().http, *event_id).await {
            tracing::warn!("could not update attendance for event {event_id}: {err}");
        }
    }
    deliver(
        ctx,
        &actor,
        "/sort",
        Ok(tr!(actor.lang, "reply-sorted", channel = &plan.name)),
    )
    .await
}
