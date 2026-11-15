//! Outbound Discord gateway. The cluster does not need a public interactions URL.

mod buttons;
mod follow;
mod publish;
mod slash;
mod worker;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use poise::serenity_prelude as serenity;
use tokio::sync::mpsc::UnboundedSender;
use tracing::Instrument;

use crate::commands;
use crate::i18n::{self, Lang};
use crate::jobs::DiscordJob;
use crate::leader::Term;
use crate::store;

use buttons::{guests_from_form, handle_button};
use follow::{
    admit_signed_in, follow_channel, follow_member, follow_names, follow_role, member_joined,
    member_left, remember_guild, role_deleted,
};
use publish::{push_sync, tell_pages};
use slash::{all_commands, register_commands};
use worker::{DRAIN, job_loop};

pub use slash::channel::open_event_channel;
pub use worker::Saver;

type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

struct Data {
    db: store::Db,
    http: Arc<Mutex<Option<Arc<serenity::Http>>>>,
    public_url: String,
    registered: Mutex<HashSet<u64>>,
    /// The job queue, for work a button leaves behind.
    jobs: UnboundedSender<DiscordJob>,
    /// Whether Discord lets Marian see the server's members.
    members: bool,
}

/// The bot's view of each server's members, for the web to read roles from
/// instead of asking Discord. It is there only while the gateway is connected
/// with the Server Members intent, when Discord keeps every member current:
/// the full member list on connect, then each change as it happens.
#[derive(Clone, Default)]
pub struct MemberView(Arc<Mutex<Option<Arc<serenity::Cache>>>>);

impl MemberView {
    /// The member's Discord role ids as the bot sees them, or `None` when it
    /// can't vouch for them: not connected, no members intent, or the member
    /// hasn't arrived in the member list yet.
    pub fn roles(&self, guild: &str, user: &str) -> Option<Vec<String>> {
        let id = |text: &str| text.parse::<u64>().ok().filter(|id| *id != 0);
        let (guild, user) = (
            serenity::GuildId::new(id(guild)?),
            serenity::UserId::new(id(user)?),
        );
        let cache = self.0.lock().ok()?.clone()?;
        let guild = cache.guild(guild)?;
        let member = guild.members.get(&user)?;
        Some(
            member
                .roles
                .iter()
                .map(|role| role.get().to_string())
                .collect(),
        )
    }

    fn set(&self, cache: Option<Arc<serenity::Cache>>) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = cache;
        }
    }
}

/// The running gateway and job worker, on the replica that leads, to stop
/// when its term ends.
pub struct Gateway {
    worker: tokio::task::JoinHandle<()>,
    connection: tokio::task::JoinHandle<()>,
    shards: Arc<Mutex<Option<Arc<serenity::ShardManager>>>>,
}

impl Gateway {
    /// Call once the term has ended. At shutdown, lets the worker finish what
    /// it can in `DRAIN`; when the lock was lost, the job in hand at most.
    /// Then disconnects from Discord.
    pub async fn stop(self) {
        if tokio::time::timeout(DRAIN + Duration::from_secs(5), self.worker)
            .await
            .is_err()
        {
            tracing::warn!("the Discord job worker didn't stop in time");
        }
        let shards = self.shards.lock().ok().and_then(|slot| slot.clone());
        if let Some(shards) = shards {
            shards.shutdown_all().await;
        }
        if tokio::time::timeout(Duration::from_secs(5), self.connection)
            .await
            .is_err()
        {
            tracing::warn!("the Discord gateway didn't disconnect in time");
        }
    }
}

/// Connects to Discord and starts the job worker, for the leader's `term`.
/// `queue` is where the bot's handlers send work; the `Saver` keeps it in the
/// `discord_jobs` table, which the worker runs from.
pub fn start(
    token: String,
    db: store::Db,
    queue: UnboundedSender<DiscordJob>,
    public_url: String,
    status: String,
    view: MemberView,
    term: Term,
) -> Gateway {
    let http = Arc::new(Mutex::new(None));
    let shards = Arc::new(Mutex::new(None));
    let worker = tokio::spawn(job_loop(
        db.clone(),
        http.clone(),
        public_url.clone(),
        term.clone(),
    ));
    let connection_shards = shards.clone();
    let connection = tokio::spawn(async move {
        let mut members = true;
        while !term.is_over() {
            let stopped = connect(
                &token,
                db.clone(),
                http.clone(),
                public_url.clone(),
                status.clone(),
                members,
                queue.clone(),
                view.clone(),
                connection_shards.clone(),
                &term,
            )
            .await;
            if let Ok(mut slot) = http.lock() {
                *slot = None;
            }
            // The client is gone; stopping its manager would only wait on it.
            if let Ok(mut slot) = connection_shards.lock() {
                *slot = None;
            }
            // Changes made while disconnected never arrive; the web asks
            // Discord until the next connection brings the member list again.
            view.set(None);
            if term.is_over() {
                break;
            }
            match stopped {
                Err(err) if members && members_refused(&err) => {
                    tracing::warn!(
                        "Discord refused the Server Members intent, so member roles update only when people use Marian. Turn it on under Bot in the Discord Developer Portal and restart Marian."
                    );
                    members = false;
                    continue;
                }
                Err(err) => tracing::warn!("Discord gateway stopped: {err}"),
                Ok(()) => {}
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(8)) => {}
                _ = term.ended() => {}
            }
        }
    });
    Gateway {
        worker,
        connection,
        shards,
    }
}

/// Whether Discord closed the gateway because the bot may not ask for members.
fn members_refused(err: &Error) -> bool {
    matches!(
        err.downcast_ref::<serenity::Error>(),
        Some(serenity::Error::Gateway(
            serenity::GatewayError::DisallowedGatewayIntents
        ))
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the gateway's wiring, called once per leadership term"
)]
async fn connect(
    token: &str,
    db: store::Db,
    http: Arc<Mutex<Option<Arc<serenity::Http>>>>,
    public_url: String,
    status: String,
    members: bool,
    queue: UnboundedSender<DiscordJob>,
    view: MemberView,
    shards: Arc<Mutex<Option<Arc<serenity::ShardManager>>>>,
    term: &Term,
) -> Result<(), Error> {
    let presence = presence_line(&status);
    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: all_commands(),
            event_handler: |framework, event| Box::pin(on_event(framework, event)),
            on_error: |error| Box::pin(on_error(error)),
            ..Default::default()
        })
        .setup(move |ctx, ready, _framework| {
            let db = db.clone();
            let http = http.clone();
            let public_url = public_url.clone();
            Box::pin(async move {
                if let Ok(mut slot) = http.lock() {
                    *slot = Some(ctx.http.clone());
                }
                // Without the intent no member changes arrive, so the cache's
                // roles could be any age.
                if members {
                    view.set(Some(ctx.cache.clone()));
                }
                tracing::info!("Discord gateway ready as {}", ready.user.name);
                Ok(Data {
                    db,
                    http,
                    public_url,
                    registered: Mutex::new(HashSet::new()),
                    jobs: queue,
                    members,
                })
            })
        })
        .build();

    let mut intents = serenity::GatewayIntents::GUILDS | serenity::GatewayIntents::DIRECT_MESSAGES;
    if members {
        // Privileged: it has to be turned on for the bot in the Developer Portal.
        intents |= serenity::GatewayIntents::GUILD_MEMBERS;
    }
    let mut builder = serenity::ClientBuilder::new(token, intents)
        .status(serenity::model::user::OnlineStatus::Online);
    if let Some(line) = presence {
        builder = builder.activity(serenity::gateway::ActivityData::custom(line));
    }
    let mut client = builder.framework(framework).await?;
    if let Ok(mut slot) = shards.lock() {
        *slot = Some(client.shard_manager.clone());
    }
    // The term can end while connecting, before the manager was there to stop.
    if term.is_over() {
        return Ok(());
    }
    client.start().await?;
    Ok(())
}

/// Each event is a trace of its own. Serenity runs the handler in a fresh task,
/// and `parent: None` keeps it from joining whatever span that task's thread
/// happens to have entered.
async fn on_event(
    framework: poise::FrameworkContext<'_, Data, Error>,
    event: &serenity::FullEvent,
) -> Result<(), Error> {
    let ctx = framework.serenity_context;
    let data = framework.user_data().await;
    let span = tracing::info_span!(parent: None, "discord_event", event = event.snake_case_name());
    handle_event(ctx, event, data).instrument(span).await
}

async fn handle_event(
    ctx: &serenity::Context,
    event: &serenity::FullEvent,
    data: &Data,
) -> Result<(), Error> {
    match event {
        serenity::FullEvent::Ready { data_about_bot } => {
            if let Ok(mut slot) = data.http.lock() {
                *slot = Some(ctx.http.clone());
            }
            for guild in &data_about_bot.guilds {
                register_guild(ctx, data, guild.id).await;
            }
        }
        serenity::FullEvent::GuildCreate { guild, .. } => {
            if let Err(err) = remember_guild(data, guild).await {
                tracing::warn!("could not store {}: {err}", guild.name);
            }
            // Without the presences intent, the guild comes with almost none of
            // its members; asking for them all brings everyone's name, nickname
            // included, in chunks.
            if data.members {
                ctx.shard.chunk_guild(
                    guild.id,
                    None,
                    false,
                    serenity::ChunkGuildFilter::None,
                    None,
                );
            }
            let roles = guild
                .roles
                .iter()
                .map(|(id, role)| (id.get(), role.permissions.bits()))
                .collect();
            tokio::spawn(admit_signed_in(
                data.db.clone(),
                data.jobs.clone(),
                ctx.http.clone(),
                guild.id,
                guild.owner_id,
                roles,
            ));
            register_guild(ctx, data, guild.id).await;
        }
        serenity::FullEvent::ChannelCreate { channel }
        | serenity::FullEvent::ChannelUpdate { new: channel, .. } => {
            if let Err(err) = follow_channel(data, channel, false).await {
                tracing::warn!("could not follow #{}: {err}", channel.name);
            }
        }
        serenity::FullEvent::ChannelDelete { channel, .. } => {
            if let Err(err) = follow_channel(data, channel, true).await {
                tracing::warn!("could not forget #{}: {err}", channel.name);
            }
        }
        serenity::FullEvent::GuildRoleCreate { new: role }
        | serenity::FullEvent::GuildRoleUpdate { new: role, .. } => {
            if let Err(err) = follow_role(data, role).await {
                tracing::warn!("could not follow the {} role: {err}", role.name);
            }
        }
        serenity::FullEvent::GuildRoleDelete {
            guild_id,
            removed_role_id,
            ..
        } => {
            if let Err(err) = role_deleted(data, *guild_id, *removed_role_id).await {
                tracing::warn!("could not forget a deleted role: {err}");
            }
        }
        serenity::FullEvent::GuildMemberUpdate { event, .. } => {
            if let Err(err) = follow_member(
                data,
                event.guild_id,
                &event.user,
                event.nick.as_deref(),
                event.avatar.as_ref(),
                &event.roles,
            )
            .await
            {
                tracing::warn!("could not follow a member's name and roles: {err}");
            }
        }
        serenity::FullEvent::GuildMembersChunk { chunk } => {
            if let Err(err) = follow_names(data, chunk.guild_id, chunk.members.values()).await {
                tracing::warn!("could not follow members' names: {err}");
            }
        }
        serenity::FullEvent::GuildMemberAddition { new_member } => {
            if let Err(err) = member_joined(data, new_member).await {
                tracing::warn!("could not open the server to a new member: {err}");
            }
        }
        serenity::FullEvent::GuildMemberRemoval { guild_id, user, .. } => {
            if let Err(err) = member_left(data, *guild_id, user).await {
                tracing::warn!("could not close the server to a member who left: {err}");
            }
        }
        serenity::FullEvent::Message { new_message } => {
            if new_message.guild_id.is_some() || new_message.author.bot {
                return Ok(());
            }
            let (replies, posts) = {
                let public_url = data.public_url.clone();
                let author = new_message.author.id.get().to_string();
                let mut conn = data
                    .db
                    .get()
                    .await
                    .map_err(|_| "Could not reach the database.")?;
                let replies = commands::answer_event_dm(
                    &mut conn,
                    &author,
                    &new_message.content,
                    &public_url,
                )
                .await?;
                let posts = commands::channel_posts_waiting(&mut conn, &author).await?;
                (replies, posts)
            };
            for event_id in &posts {
                tell_pages(&data.db, *event_id, false).await;
                if let Err(err) = push_sync(&data.db, &data.http, &data.public_url, *event_id).await
                {
                    tracing::warn!("could not post event {event_id} in the channel: {err}");
                }
            }
            let mut replies = replies;
            if let (Some(replies), Some(event_id)) = (replies.as_mut(), posts.first())
                && let Ok(mut conn) = data.db.get().await
                && let Ok(Some(url)) = commands::posted_message_url(&mut conn, *event_id).await
            {
                commands::link_created_event(replies, &url);
            }
            if let Some(replies) = replies {
                for reply in replies {
                    new_message
                        .channel_id
                        .send_message(&ctx.http, dm_message(&reply))
                        .await?;
                }
            }
        }
        serenity::FullEvent::InteractionCreate { interaction } => match interaction {
            serenity::Interaction::Component(component) => {
                if let Err(err) = handle_button(ctx, component, data).await {
                    tracing::warn!("button failed: {err}");
                }
            }
            serenity::Interaction::Modal(form) => {
                if let Err(err) = guests_from_form(ctx, form, data).await {
                    tracing::warn!("guests form failed: {err}");
                }
            }
            _ => {}
        },
        _ => {}
    }
    Ok(())
}

async fn register_guild(ctx: &serenity::Context, data: &Data, guild_id: serenity::GuildId) {
    let fresh = data
        .registered
        .lock()
        .map(|mut seen| seen.insert(guild_id.get()))
        .unwrap_or(true);
    if !fresh {
        return;
    }
    if let Err(err) = register_commands(ctx, guild_id).await {
        if let Ok(mut seen) = data.registered.lock() {
            seen.remove(&guild_id.get());
        }
        tracing::warn!("could not register commands in {guild_id}: {err}");
    }
}

/// A command that failed says so without database detail, which goes to the
/// log. Poise's own handler would put the error's text in the reply.
async fn on_error(error: poise::FrameworkError<'_, Data, Error>) {
    match error {
        poise::FrameworkError::Command { ctx, error, .. } => {
            let lang = command_lang(&ctx).await;
            let text = crate::error::user_text(error.as_ref(), lang);
            tracing::warn!(command = %ctx.command().qualified_name, "command failed: {error}");
            if let Err(err) = ctx
                .send(
                    poise::CreateReply::default()
                        .content(clip(&text, 1800))
                        .allowed_mentions(serenity::CreateAllowedMentions::new())
                        .ephemeral(true),
                )
                .await
            {
                tracing::warn!("could not say a command failed: {err}");
            }
        }
        other => {
            if let Err(err) = poise::builtins::on_error(other).await {
                tracing::warn!("could not handle a Discord error: {err}");
            }
        }
    }
}

/// The language of the server a command was used in; English outside one.
async fn command_lang(ctx: &Context<'_>) -> Lang {
    let Some(guild_id) = ctx.guild_id() else {
        return Lang::En;
    };
    let Ok(mut conn) = ctx.data().db.get().await else {
        return Lang::En;
    };
    match store::guild_by_discord(&mut conn, &guild_id.get().to_string()).await {
        Ok(Some(guild)) => i18n::lang_of(&mut conn, guild.id).await,
        _ => Lang::En,
    }
}

fn presence_line(status: &str) -> Option<String> {
    let line: String = status.trim().chars().take(128).collect();
    if line.is_empty() { None } else { Some(line) }
}

fn dm_message(reply: &commands::DmReply) -> serenity::CreateMessage {
    let (title, body) = embed_parts(reply);
    let mut embed = serenity::CreateEmbed::new().color(serenity::Colour::new(0x1e3a6e));
    if let Some(title) = title {
        embed = embed.title(clip(&title, 256));
    }
    if !body.is_empty() {
        embed = embed.description(clip(&body, 4000));
    }
    serenity::CreateMessage::new().embed(embed)
}

fn embed_parts(reply: &commands::DmReply) -> (Option<String>, String) {
    if let Some(title) = &reply.title {
        return (Some(title.clone()), reply.body.clone());
    }
    let (head, rest) = reply
        .body
        .split_once("\n\n")
        .unwrap_or((reply.body.as_str(), ""));
    let head = head.trim();
    let rest = rest.trim();
    let heading = !head.is_empty()
        && head.chars().count() <= 256
        && !head.contains('\n')
        && !head.contains(['*', '_', '`', '<']);
    if heading {
        (Some(head.to_string()), rest.to_string())
    } else {
        (None, reply.body.clone())
    }
}

fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{MemberView, embed_parts, presence_line, serenity};
    use crate::commands::DmReply;

    #[test]
    fn the_member_view_vouches_only_for_members_it_holds() {
        let view = MemberView::default();
        assert_eq!(view.roles("1", "2"), None, "not connected");

        let cache = Arc::new(serenity::Cache::new());
        let mut event: serenity::GuildCreateEvent = serde_json::from_value(serde_json::json!({
            "id": "1", "name": "Guild", "owner_id": "9", "roles": [], "emojis": [], "features": [],
            "afk_timeout": 300, "verification_level": 0, "default_message_notifications": 0,
            "explicit_content_filter": 0, "mfa_level": 0, "nsfw_level": 0, "premium_tier": 0,
            "preferred_locale": "en-US", "system_channel_flags": 0, "stickers": [],
            "premium_progress_bar_enabled": false, "joined_at": "2026-01-01T00:00:00Z",
            "large": false, "member_count": 1, "unavailable": false, "voice_states": [],
            "channels": [], "threads": [], "presences": [], "stage_instances": [],
            "guild_scheduled_events": [],
            "members": [{
                "user": { "id": "2", "username": "nell", "discriminator": "0" },
                "roles": ["3", "4"], "joined_at": "2026-01-01T00:00:00Z", "deaf": false, "mute": false, "flags": 0,
            }],
        }))
        .unwrap();
        cache.update(&mut event);
        view.set(Some(cache));
        assert_eq!(
            view.roles("1", "2"),
            Some(vec!["3".to_string(), "4".to_string()])
        );
        assert_eq!(view.roles("1", "5"), None, "a member not loaded yet");
        assert_eq!(view.roles("0", "2"), None, "an id Discord never gives");
        view.set(None);
        assert_eq!(view.roles("1", "2"), None, "disconnected again");
    }

    #[test]
    fn presence_is_the_status_text() {
        assert_eq!(
            presence_line("  marian.example.com | /event  ").as_deref(),
            Some("marian.example.com | /event")
        );
        assert_eq!(presence_line("   "), None);
    }

    #[test]
    fn every_dm_reply_is_an_embed_heading() {
        let question = embed_parts(&DmReply {
            title: None,
            body: "What should it be called?".into(),
            created: None,
        });
        assert_eq!(question.0.as_deref(), Some("What should it be called?"));
        assert!(question.1.is_empty());

        let when = embed_parts(&DmReply {
            title: None,
            body: "When does **Shore** start?\n\nFor example, \"tomorrow at 7pm\".".into(),
            created: None,
        });
        assert!(when.0.is_none());
        assert!(when.1.contains("**Shore**"));
    }
}
