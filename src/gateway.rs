//! Outbound Discord gateway. The cluster does not need a public interactions URL.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use diesel::prelude::*;
use opentelemetry::metrics::{Counter, Gauge, Histogram, UpDownCounter};
use opentelemetry::KeyValue;
use poise::serenity_prelude as serenity;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing::Instrument;

use crate::commands;
use crate::i18n::{self, Lang};
use crate::jobs::DiscordJob;
use crate::tr;
use crate::logic;
use crate::schema;
use crate::store::{self, Connection};

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

pub fn start(
    token: String,
    db: store::Db,
    jobs: UnboundedReceiver<DiscordJob>,
    retry: UnboundedSender<DiscordJob>,
    public_url: String,
    status: String,
) {
    let http = Arc::new(Mutex::new(None));
    let data_db = db.clone();
    let data_http = http.clone();
    let jobs_url = public_url.clone();
    let queue = retry.clone();
    tokio::spawn(async move {
        job_loop(data_db, jobs, retry, data_http, jobs_url).await;
    });
    tokio::spawn(async move {
        let mut members = true;
        loop {
            let stopped = connect(
                &token,
                db.clone(),
                http.clone(),
                public_url.clone(),
                status.clone(),
                members,
                queue.clone(),
            )
            .await;
            if let Ok(mut slot) = http.lock() {
                *slot = None;
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
            tokio::time::sleep(Duration::from_secs(8)).await;
        }
    });
}

async fn job_loop(
    db: store::Db,
    mut jobs: UnboundedReceiver<DiscordJob>,
    retry: UnboundedSender<DiscordJob>,
    http: Arc<Mutex<Option<Arc<serenity::Http>>>>,
    public_url: String,
) {
    JOB_METRICS.report_zeros();
    while let Some(job) = jobs.recv().await {
        JOB_METRICS.waiting.record(jobs.len() as u64, &[]);
        // While the gateway reconnects, the job waits for it without spending
        // a try: nothing could reach Discord anyway.
        while !http.lock().map(|slot| slot.is_some()).unwrap_or(false) {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        let (kind, id) = job_kind(&job);
        let span = tracing::info_span!("discord_job", job = kind, id, tries = job.tries());
        let started = Instant::now();
        let result = async {
            match &job {
                DiscordJob::Sync { event_id, .. } => {
                    push_sync(&db, &http, &public_url, *event_id).await
                }
                DiscordJob::Remind {
                    event_id,
                    text,
                    message,
                    ..
                } => push_remind(&db, &http, &public_url, *event_id, text, message).await,
                DiscordJob::Attend { event_id, .. } => push_attendance(&db, &http, *event_id).await,
                DiscordJob::Routine { run_id, .. } => {
                    push_routine(&db, &http, &public_url, *run_id).await
                }
                DiscordJob::Commands { guild_id, .. } => push_commands(&db, &http, *guild_id).await,
            }
        }
        .instrument(span.clone())
        .await;
        let outcome = match result {
            Ok(()) => "done",
            Err(err) => match job.bump() {
                Some(next) => {
                    span.in_scope(|| tracing::warn!("Discord job failed, trying again: {err}"));
                    retry_later(retry.clone(), next);
                    "failed"
                }
                None => {
                    span.in_scope(|| tracing::error!("Discord job dropped after its last try: {err}"));
                    "dropped"
                }
            },
        };
        let attributes = [KeyValue::new("kind", kind), KeyValue::new("outcome", outcome)];
        JOB_METRICS.runs.add(1, &attributes);
        JOB_METRICS.duration.record(started.elapsed().as_secs_f64(), &attributes);
    }
}

/// The Discord queue's metrics. Each run of a job counts once, as `done`,
/// `failed` (to be tried again) or `dropped` (out of tries).
struct JobMetrics {
    runs: Counter<u64>,
    duration: Histogram<f64>,
    waiting: Gauge<u64>,
    retrying: UpDownCounter<i64>,
}

static JOB_METRICS: LazyLock<JobMetrics> =
    LazyLock::new(|| JobMetrics::new(&opentelemetry::global::meter("marian")));

/// Every `kind` a job can have, as `job_kind` names them.
const JOB_KINDS: [&str; 5] = ["sync", "remind", "attend", "routine", "commands"];
const JOB_OUTCOMES: [&str; 3] = ["done", "failed", "dropped"];

impl JobMetrics {
    fn new(meter: &opentelemetry::metrics::Meter) -> Self {
        Self {
        runs: meter
            .u64_counter("marian.discord.jobs")
            .with_description("Discord jobs run, by job kind and outcome: done, failed (to be tried again) or dropped.")
            .build(),
        duration: meter
            .f64_histogram("marian.discord.job.duration")
            .with_unit("s")
            .with_description("How long a Discord job took, waits for Discord's rate limits included.")
            // The SDK's default buckets (0, 5, 10, 25 … 10000) suit milliseconds;
            // most runs take well under a second, and rate-limit waits a few.
            .with_boundaries(vec![0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0])
            .build(),
        waiting: meter
            .u64_gauge("marian.discord.queue.waiting")
            .with_description("Discord jobs left waiting in the queue when the last one was taken.")
            .build(),
        retrying: meter
            .i64_up_down_counter("marian.discord.queue.retrying")
            .with_description("Failed Discord jobs waiting out the pause before they go back in the queue.")
            .build(),
        }
    }

    /// Reports zero for every job count and for the queue at startup. Jobs
    /// are rare, and a series that first appears with the first job shows
    /// as "No data" until then, after every restart, and can't be alerted on.
    /// The duration histogram is left alone: a zero there would be a run.
    fn report_zeros(&self) {
        for kind in JOB_KINDS {
            for outcome in JOB_OUTCOMES {
                self.runs
                    .add(0, &[KeyValue::new("kind", kind), KeyValue::new("outcome", outcome)]);
            }
        }
        self.waiting.record(0, &[]);
        self.retrying.add(0, &[]);
    }
}

fn job_kind(job: &DiscordJob) -> (&'static str, i64) {
    match job {
        DiscordJob::Sync { event_id, .. } => ("sync", *event_id),
        DiscordJob::Remind { event_id, .. } => ("remind", *event_id),
        DiscordJob::Attend { event_id, .. } => ("attend", *event_id),
        DiscordJob::Routine { run_id, .. } => ("routine", *run_id),
        DiscordJob::Commands { guild_id, .. } => ("commands", *guild_id),
    }
}

/// Registers a server's slash commands again, after its language changed on
/// the web.
async fn push_commands(
    db: &store::Db,
    http: &Arc<Mutex<Option<Arc<serenity::Http>>>>,
    guild_id: i64,
) -> Result<(), Error> {
    let (discord, lang) = {
        let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
        let guild = store::guild(&mut conn, guild_id)?;
        (guild.and_then(|guild| guild.discord_id), i18n::lang_of(&mut conn, guild_id))
    };
    let Some(discord) = discord.and_then(|id| id.parse::<u64>().ok()) else {
        return Ok(());
    };
    let http = http.lock().ok().and_then(|slot| slot.clone()).ok_or("the bot is not connected")?;
    register_commands(&http, serenity::GuildId::new(discord), lang).await?;
    Ok(())
}

/// Puts a failed job back in the queue after a pause. Until then it's counted
/// in `retrying`, since the queue's own length can't see it.
fn retry_later(retry: UnboundedSender<DiscordJob>, job: DiscordJob) {
    JOB_METRICS.retrying.add(1, &[]);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        JOB_METRICS.retrying.add(-1, &[]);
        let _ = retry.send(job);
    });
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

async fn connect(
    token: &str,
    db: store::Db,
    http: Arc<Mutex<Option<Arc<serenity::Http>>>>,
    public_url: String,
    status: String,
    members: bool,
    queue: UnboundedSender<DiscordJob>,
) -> Result<(), Error> {
    let presence = presence_line(&status);
    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: all_commands(),
            event_handler: |ctx, event, framework, data| {
                Box::pin(on_event(ctx, event, framework, data))
            },
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
    client.start().await?;
    Ok(())
}

/// Each event is a trace of its own. Serenity runs the handler in a fresh task,
/// and `parent: None` keeps it from joining whatever span that task's thread
/// happens to have entered.
async fn on_event(
    ctx: &serenity::Context,
    event: &serenity::FullEvent,
    framework: poise::FrameworkContext<'_, Data, Error>,
    data: &Data,
) -> Result<(), Error> {
    let span = tracing::info_span!(parent: None, "discord_event", event = event.snake_case_name());
    handle_event(ctx, event, framework, data).instrument(span).await
}

async fn handle_event(
    ctx: &serenity::Context,
    event: &serenity::FullEvent,
    _framework: poise::FrameworkContext<'_, Data, Error>,
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
            if let Err(err) = remember_guild(data, guild) {
                tracing::warn!("could not store {}: {err}", guild.name);
            }
            // Without the presences intent, the guild comes with almost none of
            // its members; asking for them all brings everyone's name, nickname
            // included, in chunks.
            if data.members {
                ctx.shard.chunk_guild(guild.id, None, false, serenity::ChunkGuildFilter::None, None);
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
            if let Err(err) = follow_channel(data, channel, false) {
                tracing::warn!("could not follow #{}: {err}", channel.name);
            }
        }
        serenity::FullEvent::ChannelDelete { channel, .. } => {
            if let Err(err) = follow_channel(data, channel, true) {
                tracing::warn!("could not forget #{}: {err}", channel.name);
            }
        }
        serenity::FullEvent::GuildRoleCreate { new: role }
        | serenity::FullEvent::GuildRoleUpdate { new: role, .. } => {
            if let Err(err) = follow_role(data, role) {
                tracing::warn!("could not follow the {} role: {err}", role.name);
            }
        }
        serenity::FullEvent::GuildRoleDelete {
            guild_id,
            removed_role_id,
            ..
        } => {
            if let Err(err) = role_deleted(data, *guild_id, *removed_role_id) {
                tracing::warn!("could not forget a deleted role: {err}");
            }
        }
        serenity::FullEvent::GuildMemberUpdate { event, .. } => {
            if let Err(err) = follow_member(data, event.guild_id, &event.user, event.nick.as_deref(), &event.roles) {
                tracing::warn!("could not follow a member's name and roles: {err}");
            }
        }
        serenity::FullEvent::GuildMembersChunk { chunk } => {
            if let Err(err) = follow_names(data, chunk.guild_id, chunk.members.values()) {
                tracing::warn!("could not follow members' names: {err}");
            }
        }
        serenity::FullEvent::GuildMemberAddition { new_member } => {
            if let Err(err) = member_joined(data, new_member) {
                tracing::warn!("could not open the server to a new member: {err}");
            }
        }
        serenity::FullEvent::GuildMemberRemoval { guild_id, user, .. } => {
            if let Err(err) = member_left(data, *guild_id, user) {
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
                let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
                let replies = commands::answer_event_dm(
                    &mut conn,
                    &author,
                    &new_message.content,
                    &public_url,
                )?;
                let posts = commands::channel_posts_waiting(&mut conn, &author)?;
                (replies, posts)
            };
            for event_id in &posts {
                tell_pages(&data.db, *event_id, false);
                if let Err(err) = push_sync(&data.db, &data.http, &data.public_url, *event_id).await
                {
                    tracing::warn!("could not post event {event_id} in the channel: {err}");
                }
            }
            let replies = replies.map(|mut replies| {
                if let Some(event_id) = posts.first() {
                    if let Ok(mut conn) = data.db.get() {
                        if let Ok(Some(url)) = commands::posted_message_url(&mut conn, *event_id) {
                            commands::link_created_event(&mut replies, &url);
                        }
                    }
                }
                replies
            });
            if let Some(replies) = replies {
                for reply in replies {
                    new_message
                        .channel_id
                        .send_message(&ctx.http, dm_message(&reply))
                        .await?;
                }
            }
        }
        serenity::FullEvent::InteractionCreate { interaction } => {
            match interaction {
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
            }
        }
        _ => {}
    }
    Ok(())
}

fn all_commands() -> Vec<poise::Command<Data, Error>> {
    vec![
        event(),
        edit(),
        reminders(),
        response(),
        settings(),
        signup_options(),
        signup_presets(),
        channel(),
        google(),
        sort(),
        help(),
    ]
}

/// Registers the commands in one server, described in its language.
async fn register_commands(
    http: impl AsRef<serenity::Http>,
    guild_id: serenity::GuildId,
    lang: Lang,
) -> Result<(), serenity::Error> {
    let mut commands = all_commands();
    for command in &mut commands {
        describe_command(command, lang, "");
    }
    poise::builtins::register_in_guild(http, &commands, guild_id).await
}

/// Puts a command's descriptions, its options' and its choices' in `lang`,
/// from the `cmd-` messages: `cmd-settings-hour_24` describes `/settings
/// hour_24`, `cmd-edit--title` its `title` option. English is what the
/// command itself says.
fn describe_command(command: &mut poise::Command<Data, Error>, lang: Lang, parent: &str) {
    let path = if parent.is_empty() {
        command.name.clone()
    } else {
        format!("{parent}-{}", command.name)
    };
    if lang != Lang::En {
        // Discord shows a description for the reader's own locale over the
        // plain one; the server's language wins here.
        command.description_localizations.clear();
        if let Some(text) = command_text(lang, &format!("cmd-{path}")) {
            command.description = Some(text);
        }
        for parameter in &mut command.parameters {
            parameter.description_localizations.clear();
            if let Some(text) = command_text(lang, &format!("cmd-{path}--{}", parameter.name)) {
                parameter.description = Some(text);
            }
            for choice in &mut parameter.choices {
                if let Some(text) =
                    command_text(lang, &format!("cmd-{path}--{}--{}", parameter.name, choice.name))
                {
                    choice.name = text;
                }
            }
        }
    }
    for subcommand in &mut command.subcommands {
        describe_command(subcommand, lang, &path);
    }
}

fn command_text(lang: Lang, key: &str) -> Option<String> {
    i18n::has(lang, key).then(|| i18n::text(lang, key, &[]))
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
    let lang = data
        .db
        .get()
        .ok()
        .and_then(|mut conn| {
            let guild = store::guild_by_discord(&mut conn, &guild_id.get().to_string()).ok()??;
            Some(i18n::lang_of(&mut conn, guild.id))
        })
        .unwrap_or_default();
    if let Err(err) = register_commands(ctx, guild_id, lang).await {
        if let Ok(mut seen) = data.registered.lock() {
            seen.remove(&guild_id.get());
        }
        tracing::warn!("could not register commands in {guild_id}: {err}");
    }
}

fn remember_guild(data: &Data, guild: &serenity::Guild) -> Result<(), Error> {
    let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
    let stored = logic::ensure_discord_guild(&mut conn, &guild.id.get().to_string(), &guild.name)?;
    let channels: Vec<(String, String)> = guild
        .channels
        .values()
        .filter(|channel| postable(channel.kind))
        .map(|channel| (channel.id.get().to_string(), channel.name.clone()))
        .collect();
    logic::sync_discord_channels(&mut conn, stored.id, &channels)?;
    let roles: Vec<(String, String, String)> = guild
        .roles
        .values()
        .filter(|role| role.id.get() != guild.id.get())
        .map(|role| (role.id.get().to_string(), role.name.clone(), role.colour.hex()))
        .collect();
    logic::sync_discord_roles(&mut conn, stored.id, &roles)?;
    let guild_discord = guild.id.get().to_string();
    for role in guild.roles.values() {
        logic::sync_discord_permissions(
            &mut conn,
            stored.id,
            &guild_discord,
            &role.id.get().to_string(),
            role.permissions.bits(),
        )?;
    }
    for channel in guild.channels.values().filter(|channel| postable(channel.kind)) {
        logic::sync_channel_overwrites(
            &mut conn,
            stored.id,
            &channel.id.get().to_string(),
            &overwrites(channel),
        )?;
    }
    Ok(())
}

/// A channel's permission overwrites, as Marian stores them.
fn overwrites(channel: &serenity::GuildChannel) -> Vec<logic::Overwrite> {
    channel
        .permission_overwrites
        .iter()
        .filter_map(|overwrite| {
            let (member, target) = match overwrite.kind {
                serenity::PermissionOverwriteType::Member(user) => (true, user.get()),
                serenity::PermissionOverwriteType::Role(role) => (false, role.get()),
                _ => return None,
            };
            Some(logic::Overwrite {
                member,
                target: target.to_string(),
                allow: overwrite.allow.bits(),
                deny: overwrite.deny.bits(),
            })
        })
        .collect()
}

fn stored_guild(conn: &mut Connection, guild: serenity::GuildId) -> Result<Option<store::Guild>, Error> {
    Ok(store::guild_by_discord(conn, &guild.get().to_string())?)
}

/// Follows a role Discord added or changed, and @everyone's permissions.
fn follow_role(data: &Data, role: &serenity::Role) -> Result<(), Error> {
    let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
    let Some(guild) = stored_guild(&mut conn, role.guild_id)? else {
        return Ok(());
    };
    if role.id.get() != role.guild_id.get() {
        logic::sync_discord_role(
            &mut conn,
            guild.id,
            &role.id.get().to_string(),
            &role.name,
            &role.colour.hex(),
        )?;
    }
    logic::sync_discord_permissions(
        &mut conn,
        guild.id,
        &role.guild_id.get().to_string(),
        &role.id.get().to_string(),
        role.permissions.bits(),
    )?;
    Ok(())
}

fn role_deleted(data: &Data, guild: serenity::GuildId, role: serenity::RoleId) -> Result<(), Error> {
    let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
    let Some(guild) = stored_guild(&mut conn, guild)? else {
        return Ok(());
    };
    logic::discord_role_gone(&mut conn, guild.id, &role.get().to_string())?;
    Ok(())
}

/// Brings the names of members Marian knows up to date, as a chunk of the
/// server's member list has them, and redraws each post that changes once.
fn follow_names<'a>(
    data: &Data,
    guild: serenity::GuildId,
    members: impl Iterator<Item = &'a serenity::Member>,
) -> Result<(), Error> {
    let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
    let Some(stored) = stored_guild(&mut conn, guild)? else {
        return Ok(());
    };
    let mut events = std::collections::BTreeSet::new();
    for member in members {
        let user = member.user.id.get().to_string();
        if let Some((_, renamed)) = logic::follow_discord_name(&mut conn, stored.id, &user, &server_name(member))? {
            events.extend(renamed);
        }
    }
    redraw(&data.jobs, events.into_iter().collect());
    Ok(())
}

/// Redraws the posts that still show someone's old name.
fn redraw(jobs: &UnboundedSender<DiscordJob>, events: Vec<i64>) {
    for event_id in events {
        let _ = jobs.send(DiscordJob::sync(event_id));
    }
}

/// The name a member goes by in their server: nickname first.
fn server_name(member: &serenity::Member) -> String {
    crate::auth::discord_display_name(member.nick.as_deref(), member.user.global_name.as_deref(), &member.user.name)
}

/// Keeps a member's name and roles as Discord has them. Only members Marian
/// already knows are kept; the rest are picked up when they first use Marian.
fn follow_member(
    data: &Data,
    guild: serenity::GuildId,
    user: &serenity::User,
    nick: Option<&str>,
    roles: &[serenity::RoleId],
) -> Result<(), Error> {
    let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
    let Some(guild) = stored_guild(&mut conn, guild)? else {
        return Ok(());
    };
    let name = crate::auth::discord_display_name(nick, user.global_name.as_deref(), &user.name);
    let Some((member, renamed)) = logic::follow_discord_name(&mut conn, guild.id, &user.id.get().to_string(), &name)? else {
        return Ok(());
    };
    redraw(&data.jobs, renamed);
    let roles: Vec<String> = roles.iter().map(|role| role.get().to_string()).collect();
    store::replace_discord_roles(&mut conn, guild.id, member.id, &roles)?;
    Ok(())
}

/// Opens the server to the new member's web sessions.
fn member_joined(data: &Data, member: &serenity::Member) -> Result<(), Error> {
    let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
    let Some(guild) = stored_guild(&mut conn, member.guild_id)? else {
        return Ok(());
    };
    let user = member.user.id.get().to_string();
    let sessions = store::sessions_without_guild(&mut conn, guild.id, Some(&user))?;
    if sessions.is_empty() {
        return Ok(());
    }
    let (_, renamed) = logic::ensure_discord_user(&mut conn, guild.id, &user, &server_name(member))?;
    redraw(&data.jobs, renamed);
    for (token, _, _) in &sessions {
        store::add_session_guild(&mut conn, token, guild.id)?;
    }
    Ok(())
}

/// Takes the server out of a departing member's web sessions and drops their roles in it.
fn member_left(data: &Data, guild: serenity::GuildId, user: &serenity::User) -> Result<(), Error> {
    let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
    let Some(guild) = stored_guild(&mut conn, guild)? else {
        return Ok(());
    };
    let user = user.id.get().to_string();
    if let Some(member) = store::membership_by_discord(&mut conn, guild.id, &user)? {
        store::replace_discord_roles(&mut conn, guild.id, member.id, &[])?;
    }
    store::drop_session_guild(&mut conn, &user, guild.id)?;
    Ok(())
}

/// Opens a server to the signed-in members who are in it, so a server Marian
/// joins shows up in their server picker without signing in again. Each comes
/// in with their roles, and as a server manager if they own the server or
/// their roles (`roles`: each role's permissions, by Discord id) make them one.
async fn admit_signed_in(
    db: store::Db,
    jobs: UnboundedSender<DiscordJob>,
    http: Arc<serenity::Http>,
    guild: serenity::GuildId,
    owner: serenity::UserId,
    roles: HashMap<u64, u64>,
) {
    let waiting = || -> Result<_, Error> {
        let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
        let Some(stored) = store::guild_by_discord(&mut conn, &guild.get().to_string())? else {
            return Ok(None);
        };
        Ok(Some((stored.id, store::sessions_without_guild(&mut conn, stored.id, None)?)))
    };
    let (guild_id, sessions) = match waiting() {
        Ok(Some(found)) => found,
        Ok(None) => return,
        Err(err) => {
            tracing::warn!("could not list sessions for {guild}: {err}");
            return;
        }
    };
    let mut members: Vec<(String, Vec<String>)> = Vec::new();
    for (token, user, _) in sessions {
        match members.last_mut() {
            Some((last, tokens)) if *last == user => tokens.push(token),
            _ => members.push((user, vec![token])),
        }
    }
    for (user, tokens) in members {
        let Ok(user_id) = user.parse::<u64>() else {
            continue;
        };
        let member = match http.get_member(guild, serenity::UserId::new(user_id)).await {
            Ok(member) => member,
            Err(serenity::Error::Http(serenity::HttpError::UnsuccessfulRequest(reply)))
                if reply.status_code == serenity::StatusCode::NOT_FOUND =>
            {
                continue;
            }
            Err(err) => {
                tracing::warn!("could not check membership in {guild}: {err}");
                continue;
            }
        };
        let held: Vec<u64> = member
            .roles
            .iter()
            .filter_map(|role| roles.get(&role.get()).copied())
            .collect();
        let everyone = roles.get(&guild.get()).copied().unwrap_or(0);
        let manages = crate::auth::manages_member(member.user.id == owner, everyone, &held);
        let role_ids: Vec<String> = member.roles.iter().map(|role| role.get().to_string()).collect();
        let name = server_name(&member);
        let admitted = || -> Result<(), Error> {
            let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
            let (membership, renamed) = logic::ensure_discord_user(&mut conn, guild_id, &user, &name)?;
            redraw(&jobs, renamed);
            logic::set_server_manager(&mut conn, membership.id, manages)?;
            store::replace_discord_roles(&mut conn, guild_id, membership.id, &role_ids)?;
            for token in &tokens {
                store::add_session_guild(&mut conn, token, guild_id)?;
            }
            Ok(())
        };
        if let Err(err) = admitted() {
            tracing::warn!("could not open {guild} to a signed-in member: {err}");
        }
    }
}

/// Channels Marian can post events in.
fn postable(kind: serenity::ChannelType) -> bool {
    matches!(
        kind,
        serenity::ChannelType::Text | serenity::ChannelType::News
    )
}

/// Follows a channel Discord added, renamed or deleted, so the channel lists
/// match Discord without waiting for a restart.
fn follow_channel(data: &Data, channel: &serenity::GuildChannel, deleted: bool) -> Result<(), Error> {
    let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
    let Some(guild) = store::guild_by_discord(&mut conn, &channel.guild_id.get().to_string())? else {
        return Ok(());
    };
    let discord_id = channel.id.get().to_string();
    if deleted || !postable(channel.kind) {
        logic::discord_channel_gone(&mut conn, guild.id, &discord_id)?;
    } else {
        logic::sync_discord_channel(&mut conn, guild.id, &discord_id, &channel.name)?;
        logic::sync_channel_overwrites(&mut conn, guild.id, &discord_id, &overwrites(channel))?;
    }
    Ok(())
}

async fn handle_button(
    ctx: &serenity::Context,
    component: &serenity::ComponentInteraction,
    data: &Data,
) -> Result<(), Error> {
    let Some(action) = parse_button(&component.data.custom_id) else {
        return Ok(());
    };
    // The form is the answer to the click, so this one isn't acknowledged first.
    if let ButtonAction::Guests { event, option } = action {
        return open_guests_form(ctx, component, data, event, option).await;
    }
    component
        .create_response(&ctx.http, serenity::CreateInteractionResponse::Acknowledge)
        .await?;
    // The button names rows by public id; the handlers below work on row keys.
    let found = {
        let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
        let event_key = |conn: &mut store::Connection, event: uuid::Uuid| -> Result<Option<i64>, crate::error::OpError> {
            Ok(store::event_by_public(conn, &event.to_string())?.map(|event| event.id))
        };
        match action {
            ButtonAction::Edit { event } => event_key(&mut conn, event)?.map(Found::Edit),
            ButtonAction::Delete { event } => event_key(&mut conn, event)?.map(Found::Delete),
            ButtonAction::ConfirmDelete { event } => event_key(&mut conn, event)?.map(Found::ConfirmDelete),
            // Opened as a form above, before the click was acknowledged.
            ButtonAction::Guests { .. } => None,
            ButtonAction::Signup { event, option } => match event_key(&mut conn, event)? {
                Some(event_id) => crate::ids::public::option_id(&mut conn, event_id, &option.to_string())
                    .ok()
                    .map(|option_id| Found::Signup(event_id, option_id)),
                None => None,
            },
            ButtonAction::Routine { run, day } => {
                logic::run_by_public(&mut conn, run)?.map(|run_id| Found::Routine(run_id, day))
            }
            ButtonAction::Decide { run, day } => {
                logic::run_by_public(&mut conn, run)?.map(|run_id| Found::Decide(run_id, day))
            }
        }
    };
    let Some(found) = found else {
        let lang = button_lang(data, component);
        return followup(ctx, component, &tr!(lang, "reply-gone")).await;
    };
    match found {
        Found::Edit(event_id) => edit_from_button(ctx, component, data, event_id).await,
        Found::Delete(event_id) => ask_delete(ctx, component, data, event_id).await,
        Found::ConfirmDelete(event_id) => delete_from_button(ctx, component, data, event_id).await,
        Found::Signup(event_id, option_id) => {
            signup_from_button(ctx, component, data, event_id, option_id).await
        }
        Found::Routine(run_id, day) => routine_from_button(ctx, component, data, run_id, day).await,
        Found::Decide(run_id, day) => decide_from_button(ctx, component, data, run_id, day).await,
    }
}

/// A button's action with its rows found.
enum Found {
    Edit(i64),
    Delete(i64),
    ConfirmDelete(i64),
    Signup(i64, i64),
    Routine(i64, u8),
    /// An approval's answer: a day, or none to skip the week.
    Decide(i64, Option<u8>),
}

async fn decide_from_button(
    ctx: &serenity::Context,
    component: &serenity::ComponentInteraction,
    data: &Data,
    run_id: i64,
    day: Option<u8>,
) -> Result<(), Error> {
    let Some(click) = click_context(ctx, component) else {
        followup(ctx, component, &tr!(Lang::En, "reply-button-outside")).await?;
        return Ok(());
    };
    let (decided, lang) = {
        let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
        let (membership_id, lang) = bind_clicker(&mut conn, &data.jobs, &click)?;
        (logic::decide_run(&mut conn, run_id, membership_id, day, chrono::Utc::now()), lang)
    };
    match decided {
        Ok(effect) => {
            for job in effect.jobs {
                let _ = data.jobs.send(job);
            }
            Ok(())
        }
        Err(err) => followup(ctx, component, &err.localized(lang)).await,
    }
}

async fn routine_from_button(
    ctx: &serenity::Context,
    component: &serenity::ComponentInteraction,
    data: &Data,
    run_id: i64,
    day: u8,
) -> Result<(), Error> {
    let Some(click) = click_context(ctx, component) else {
        followup(ctx, component, &tr!(Lang::En, "reply-button-outside")).await?;
        return Ok(());
    };
    let (saved, lang) = {
        let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
        let (membership_id, lang) = bind_clicker(&mut conn, &data.jobs, &click)?;
        (logic::cast_run(&mut conn, run_id, membership_id, day), lang)
    };
    if let Err(err) = saved {
        return followup(ctx, component, &err.localized(lang)).await;
    }
    if let Err(err) = push_routine(&data.db, &data.http, &data.public_url, run_id).await {
        tracing::warn!("could not refresh the routine poll: {err}");
    }
    Ok(())
}

async fn signup_from_button(
    ctx: &serenity::Context,
    component: &serenity::ComponentInteraction,
    data: &Data,
    event_id: i64,
    option_id: i64,
) -> Result<(), Error> {
    let Some(click) = click_context(ctx, component) else {
        followup(ctx, component, &tr!(Lang::En, "reply-button-outside")).await?;
        return Ok(());
    };
    let (signed, lang) = {
        let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
        let (membership_id, lang) = bind_clicker(&mut conn, &data.jobs, &click)?;
        (logic::signup(&mut conn, event_id, option_id, membership_id), lang)
    };
    if let Err(err) = signed {
        return followup(ctx, component, &err.localized(lang)).await;
    }
    tell_pages(&data.db, event_id, true);
    if let Err(err) = push_sync(&data.db, &data.http, &data.public_url, event_id).await {
        tracing::warn!("could not refresh the Discord post: {err}");
    }
    if let Err(err) = push_attendance(&data.db, &data.http, event_id).await {
        tracing::warn!("could not update attendance on Discord: {err}");
    }
    Ok(())
}

/// "+X": a form asking how many guests, filled with the count the person
/// already brings when they're on the option.
async fn open_guests_form(
    ctx: &serenity::Context,
    component: &serenity::ComponentInteraction,
    data: &Data,
    event: uuid::Uuid,
    option: uuid::Uuid,
) -> Result<(), Error> {
    let Some(click) = click_context(ctx, component) else {
        component
            .create_response(&ctx.http, serenity::CreateInteractionResponse::Acknowledge)
            .await?;
        return followup(ctx, component, &tr!(Lang::En, "reply-button-outside")).await;
    };
    let found = {
        let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
        let (membership_id, lang) = bind_clicker(&mut conn, &data.jobs, &click)?;
        let option_id = store::event_by_public(&mut conn, &event.to_string())?
            .and_then(|event| crate::ids::public::option_id(&mut conn, event.id, &option.to_string()).ok());
        let form = match option_id {
            Some(option_id) => Some(logic::guests_form(&mut conn, option_id, membership_id)?),
            None => None,
        };
        (form, lang)
    };
    let (form, lang) = found;
    let refusal = match &form {
        None => Some(tr!(lang, "reply-gone")),
        Some(form) if form.max_guests == 0 => {
            Some(crate::msg!("err-guests-off", option = &form.option).render(lang))
        }
        Some(_) => None,
    };
    let Some(form) = form.filter(|_| refusal.is_none()) else {
        component
            .create_response(&ctx.http, serenity::CreateInteractionResponse::Acknowledge)
            .await?;
        return followup(ctx, component, &refusal.unwrap_or_default()).await;
    };
    let mut input = serenity::CreateInputText::new(
        serenity::InputTextStyle::Short,
        clip(&tr!(lang, "guests-form-label", most = form.max_guests), 45),
        "guests",
    )
    .min_length(1)
    .max_length(2)
    .required(true);
    if let Some(current) = form.current {
        input = input.value(current.to_string());
    }
    let modal = serenity::CreateModal::new(
        format!("marian:{event}:{option}:g"),
        clip(&tr!(lang, "guests-form-title", option = form.option.as_str()), 45),
    )
    .components(vec![serenity::CreateActionRow::InputText(input)]);
    component
        .create_response(&ctx.http, serenity::CreateInteractionResponse::Modal(modal))
        .await?;
    Ok(())
}

/// The "+X" form sent back: signs the person up with that many guests, or
/// changes the count when they're already on the option.
async fn guests_from_form(
    ctx: &serenity::Context,
    form: &serenity::ModalInteraction,
    data: &Data,
) -> Result<(), Error> {
    let Some(ButtonAction::Guests { event, option }) = parse_button(&form.data.custom_id) else {
        return Ok(());
    };
    form.create_response(&ctx.http, serenity::CreateInteractionResponse::Acknowledge)
        .await?;
    let typed = form
        .data
        .components
        .iter()
        .flat_map(|row| row.components.iter())
        .find_map(|component| match component {
            serenity::ActionRowComponent::InputText(input) if input.custom_id == "guests" => input.value.clone(),
            _ => None,
        })
        .unwrap_or_default();
    let Some(click) = click_of(ctx, form.guild_id, &form.user, form.member.as_ref()) else {
        return form_followup(ctx, form, &tr!(Lang::En, "reply-button-outside")).await;
    };
    let (outcome, lang) = {
        let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
        let (membership_id, lang) = bind_clicker(&mut conn, &data.jobs, &click)?;
        let found = store::event_by_public(&mut conn, &event.to_string())?.and_then(|event| {
            crate::ids::public::option_id(&mut conn, event.id, &option.to_string())
                .ok()
                .map(|option_id| (event.id, option_id))
        });
        let outcome = match (found, typed.trim().parse::<i64>()) {
            (None, _) => Err(None),
            (Some(_), Err(_)) => Err(Some(crate::error::OpError::from(crate::msg!("err-guests-number")))),
            (Some((event_id, option_id)), Ok(guests)) => {
                logic::signup_guests(&mut conn, event_id, option_id, membership_id, guests)
                    .map(|_| event_id)
                    .map_err(Some)
            }
        };
        (outcome, lang)
    };
    let event_id = match outcome {
        Ok(event_id) => event_id,
        Err(None) => return form_followup(ctx, form, &tr!(lang, "reply-gone")).await,
        Err(Some(err)) => return form_followup(ctx, form, &err.localized(lang)).await,
    };
    tell_pages(&data.db, event_id, true);
    if let Err(err) = push_sync(&data.db, &data.http, &data.public_url, event_id).await {
        tracing::warn!("could not refresh the Discord post: {err}");
    }
    if let Err(err) = push_attendance(&data.db, &data.http, event_id).await {
        tracing::warn!("could not update attendance on Discord: {err}");
    }
    Ok(())
}

async fn form_followup(ctx: &serenity::Context, form: &serenity::ModalInteraction, text: &str) -> Result<(), Error> {
    form.create_followup(
        &ctx.http,
        serenity::CreateInteractionResponseFollowup::new()
            .content(clip(text, 1800))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

async fn edit_from_button(
    ctx: &serenity::Context,
    component: &serenity::ComponentInteraction,
    data: &Data,
    event_id: i64,
) -> Result<(), Error> {
    let Some(click) = click_context(ctx, component) else {
        followup(ctx, component, &tr!(Lang::En, "reply-button-outside")).await?;
        return Ok(());
    };
    let text = {
        let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
        let (membership_id, lang) = bind_clicker(&mut conn, &data.jobs, &click)?;
        match store::event(&mut conn, event_id)? {
            Some(event) if event.created_by == Some(membership_id) => {
                Some(event_link(&mut conn, &data.public_url, &event, lang)?)
            }
            Some(_) => None,
            None => Some(tr!(lang, "reply-event-missing")),
        }
    };
    let Some(text) = text else {
        return Ok(());
    };
    followup(ctx, component, &text).await
}

async fn ask_delete(
    ctx: &serenity::Context,
    component: &serenity::ComponentInteraction,
    data: &Data,
    event_id: i64,
) -> Result<(), Error> {
    let Some(click) = click_context(ctx, component) else {
        followup(ctx, component, &tr!(Lang::En, "reply-button-outside")).await?;
        return Ok(());
    };
    let (card, lang) = {
        let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
        let (membership_id, lang) = bind_clicker(&mut conn, &data.jobs, &click)?;
        let card = match store::event(&mut conn, event_id)? {
            None => Err(tr!(lang, "reply-event-missing")),
            Some(event) if event.created_by != Some(membership_id) => Ok(None),
            Some(event)
                if !matches!(
                    event.status.as_str(),
                    "draft" | "scheduled" | "failed" | "posted"
                ) =>
            {
                Err(tr!(lang, "err-event-closed"))
            }
            Some(event) => {
                let server = store::discord_server_id(&mut conn, event.guild_id).unwrap_or_default();
                let url = logic::web_event_url(
                    &data.public_url,
                    Some(server.as_str()).filter(|id| !id.is_empty()),
                    &event.public_id,
                );
                Ok(Some((
                    logic::delete_confirm_body(
                        lang,
                        &event.title,
                        event.starts_at,
                        url.as_deref(),
                        event.status == "posted",
                    ),
                    event.public_id,
                )))
            }
        };
        (card, lang)
    };
    let body = match card {
        Ok(Some(body)) => body,
        Ok(None) => return Ok(()),
        Err(text) => return followup(ctx, component, &text).await,
    };
    let (body, event) = body;
    let embed = serenity::CreateEmbed::new()
        .title(tr!(lang, "post-delete-title"))
        .description(clip(&body, 4000))
        .color(serenity::Colour::new(0xF1C40F));
    let delete = serenity::CreateButton::new(format!("marian:{event}:confirm-delete"))
        .style(serenity::ButtonStyle::Danger)
        .label(tr!(lang, "post-delete"));
    component
        .create_followup(
            &ctx.http,
            serenity::CreateInteractionResponseFollowup::new()
                .embed(embed)
                .components(vec![serenity::CreateActionRow::Buttons(vec![delete])])
                .ephemeral(true),
        )
        .await?;
    Ok(())
}

async fn delete_from_button(
    ctx: &serenity::Context,
    component: &serenity::ComponentInteraction,
    data: &Data,
    event_id: i64,
) -> Result<(), Error> {
    let Some(click) = click_context(ctx, component) else {
        followup(ctx, component, &tr!(Lang::En, "reply-button-outside")).await?;
        return Ok(());
    };
    let removed = {
        let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
        let (membership_id, lang) = bind_clicker(&mut conn, &data.jobs, &click)?;
        match store::event(&mut conn, event_id)? {
            None => Err(tr!(lang, "reply-event-missing")),
            Some(event) if event.created_by != Some(membership_id) => Ok(None),
            Some(event) => {
                let plan = logic::event_channel_purge(&mut conn, event.id)
                    .map_err(|err| err.localized(lang))?;
                let channel_discord = store::channel(&mut conn, event.channel_id)?
                    .and_then(|channel| channel.discord_id);
                let message_id = event.discord_message_id.clone();
                match logic::delete_event(&mut conn, event_id, membership_id) {
                    Ok(notice) => {
                        crate::live::event(event.guild_id, event.public_id);
                        Ok(Some((plan, channel_discord, message_id, notice.render(lang))))
                    }
                    Err(err) => Err(err.localized(lang)),
                }
            }
        }
    };
    let (plan, channel_discord, message_id, notice) = match removed {
        Ok(Some(removed)) => removed,
        Ok(None) => return Ok(()),
        Err(text) => return followup(ctx, component, &text).await,
    };
    if let (Some(channel_discord), Some(message_id)) = (channel_discord, message_id) {
        let http = data
            .http
            .lock()
            .map_err(|_| "http lock")?
            .clone()
            .ok_or("Discord is not connected")?;
        let deleted = serenity::ChannelId::new(channel_discord.parse()?)
            .delete_message(&http, serenity::MessageId::new(message_id.parse()?))
            .await;
        if let Err(err) = deleted {
            tracing::warn!("could not remove the Discord post: {err}");
        } else if let Ok(mut conn) = data.db.get() {
            diesel::update(schema::events::table.filter(schema::events::id.eq(event_id)))
                .set(schema::events::discord_message_id.eq(None::<String>))
                .execute(&mut *conn)?;
        }
        if let Some(plan) = plan {
            if let Err(err) = purge_event_channel(&data.db, &http, plan).await {
                tracing::warn!("could not purge the event channel: {err}");
            }
        }
    } else if let Some(plan) = plan {
        let http = data.http.lock().ok().and_then(|guard| guard.clone());
        if let Some(http) = http {
            if let Err(err) = purge_event_channel(&data.db, &http, plan).await {
                tracing::warn!("could not purge the event channel: {err}");
            }
        }
    }
    if let Err(err) = component
        .edit_response(
            &ctx.http,
            serenity::EditInteractionResponse::new()
                .content(clip(&notice, 1800))
                .embeds(Vec::new())
                .components(Vec::new()),
        )
        .await
    {
        tracing::warn!("could not update the delete confirmation: {err}");
        followup(ctx, component, &notice).await?;
    }
    Ok(())
}

struct Click {
    guild_discord: String,
    guild_name: String,
    user_discord: String,
    display: String,
    manage: Option<bool>,
    roles: Option<Vec<(String, String, String)>>,
}

fn click_context(
    ctx: &serenity::Context,
    component: &serenity::ComponentInteraction,
) -> Option<Click> {
    click_of(ctx, component.guild_id, &component.user, component.member.as_ref())
}

/// The member behind a click or a submitted form.
fn click_of(
    ctx: &serenity::Context,
    guild_id: Option<serenity::GuildId>,
    user: &serenity::User,
    member: Option<&serenity::Member>,
) -> Option<Click> {
    let guild_id = guild_id?;
    let guild_name = ctx
        .cache
        .guild(guild_id)
        .map(|guild| guild.name.clone())
        .unwrap_or_default();
    let display = crate::auth::discord_display_name(
        member.and_then(|member| member.nick.as_deref()),
        user.global_name.as_deref(),
        &user.name,
    );
    let manage = member.and_then(|member| {
        member
            .permissions
            .map(|permissions| permissions.manage_guild() || permissions.administrator())
    });
    let roles = member.map(|member| {
        member
            .roles
            .iter()
            .filter_map(|role_id| {
                ctx.cache.guild(guild_id).and_then(|guild| {
                    guild.roles.get(role_id).map(|role| {
                        (
                            role.name.clone(),
                            role.colour.hex(),
                            role.id.get().to_string(),
                        )
                    })
                })
            })
            .collect::<Vec<_>>()
    });
    Some(Click {
        guild_discord: guild_id.get().to_string(),
        guild_name,
        user_discord: user.id.get().to_string(),
        display,
        manage,
        roles,
    })
}

/// The member who pressed a button, and their server's language.
fn bind_clicker(conn: &mut Connection, jobs: &UnboundedSender<DiscordJob>, click: &Click) -> Result<(i64, Lang), Error> {
    let guild = logic::ensure_discord_guild(conn, &click.guild_discord, &click.guild_name)?;
    let (user, renamed) = logic::ensure_discord_user(conn, guild.id, &click.user_discord, &click.display)?;
    redraw(jobs, renamed);
    if let Some(manage) = click.manage {
        logic::set_server_manager(conn, user.id, manage)?;
    }
    if let Some(roles) = &click.roles {
        sync_roles(conn, guild.id, user.id, roles)?;
    }
    Ok((user.id, i18n::lang_of(conn, guild.id)))
}

/// The language of the server a button is in; English outside one.
fn button_lang(data: &Data, component: &serenity::ComponentInteraction) -> Lang {
    let Some(guild_id) = component.guild_id else {
        return Lang::En;
    };
    let Ok(mut conn) = data.db.get() else {
        return Lang::En;
    };
    match store::guild_by_discord(&mut conn, &guild_id.get().to_string()) {
        Ok(Some(guild)) => i18n::lang_of(&mut conn, guild.id),
        _ => Lang::En,
    }
}

fn event_link(
    conn: &mut Connection,
    public_url: &str,
    event: &store::Event,
    lang: Lang,
) -> Result<String, Error> {
    let server = store::discord_server_id(conn, event.guild_id).unwrap_or_default();
    let path = if server.is_empty() {
        format!("/events/{}", event.public_id)
    } else {
        format!("/servers/{server}/events/{}", event.public_id)
    };
    let base = public_url.trim().trim_end_matches('/');
    if base.is_empty() {
        Ok(tr!(lang, "reply-open-path", path = path))
    } else {
        Ok(format!("{base}{}", path))
    }
}

fn sync_roles(
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
    roles: &[(String, String, String)],
) -> Result<(), Error> {
    diesel::delete(schema::membership_roles::table.filter(schema::membership_roles::membership_id.eq(membership_id)))
        .execute(conn)?;
    let mut role_ids = Vec::new();
    for (name, colour, discord_id) in roles {
        role_ids.push(logic::sync_discord_role(conn, guild_id, discord_id, name, colour)?.id);
    }
    store::insert_membership_roles(conn, membership_id, &role_ids)?;
    Ok(())
}

async fn followup(
    ctx: &serenity::Context,
    component: &serenity::ComponentInteraction,
    text: &str,
) -> Result<(), Error> {
    component
        .create_followup(
            &ctx.http,
            serenity::CreateInteractionResponseFollowup::new()
                .content(clip(text, 1800))
                .ephemeral(true),
        )
        .await?;
    Ok(())
}

/// What a button asks for, with public ids as the button carried them.
#[derive(Debug, PartialEq)]
enum ButtonAction {
    Signup { event: uuid::Uuid, option: uuid::Uuid },
    /// "+X": a form asking how many guests, and the same id on the form.
    Guests { event: uuid::Uuid, option: uuid::Uuid },
    Edit { event: uuid::Uuid },
    Delete { event: uuid::Uuid },
    ConfirmDelete { event: uuid::Uuid },
    Routine { run: uuid::Uuid, day: u8 },
    /// An approval's answer; no day skips the week.
    Decide { run: uuid::Uuid, day: Option<u8> },
}

/// A button on an event's post. Its id names the event, and the option, by
/// public id: anyone can read a button's id in Discord.
fn action_button(event: uuid::Uuid, button: &logic::DiscordButton) -> serenity::CreateButton {
    let custom_id = match &button.role {
        logic::ButtonRole::Signup(option) => format!("marian:{event}:{option}"),
        logic::ButtonRole::Guests(option) => format!("marian:{event}:{option}:g"),
        logic::ButtonRole::Web(url) => return serenity::CreateButton::new_link(url).label(clip(&button.label, 80)),
    };
    let style = match button.tone {
        logic::ButtonTone::Secondary => serenity::ButtonStyle::Secondary,
        logic::ButtonTone::Primary => serenity::ButtonStyle::Primary,
        logic::ButtonTone::Danger => serenity::ButtonStyle::Danger,
    };
    let mut built = serenity::CreateButton::new(custom_id)
        .style(style)
        .disabled(button.disabled);
    if !button.emoji.trim().is_empty() {
        built = built.emoji(serenity::ReactionType::Unicode(button.emoji.clone()));
    }
    let label = clip(&button.label, 80);
    if !label.is_empty() {
        built = built.label(label);
    } else if button.emoji.trim().is_empty() {
        built = built.label("Option");
    }
    built
}

/// Reads a button's id. A button from before ids were public carries row
/// numbers, which no longer parse, so it does nothing.
fn parse_button(custom_id: &str) -> Option<ButtonAction> {
    let mut parts = custom_id.split(':');
    if parts.next() != Some("marian") {
        return None;
    }
    let head = parts.next()?;
    if head == "w" {
        let run = uuid::Uuid::parse_str(parts.next()?).ok()?;
        let day = parts.next()?.parse().ok()?;
        return Some(ButtonAction::Routine { run, day });
    }
    if head == "a" {
        let run = uuid::Uuid::parse_str(parts.next()?).ok()?;
        let day = match parts.next()? {
            "skip" => None,
            day => Some(day.parse().ok()?),
        };
        return Some(ButtonAction::Decide { run, day });
    }
    let event = uuid::Uuid::parse_str(head).ok()?;
    match parts.next()? {
        // Posts no longer carry Edit and Delete, but ones sent before still
        // do until they're next redrawn.
        "edit" => Some(ButtonAction::Edit { event }),
        "delete" => Some(ButtonAction::Delete { event }),
        "confirm-delete" => Some(ButtonAction::ConfirmDelete { event }),
        option => {
            let option = uuid::Uuid::parse_str(option).ok()?;
            match parts.next() {
                None => Some(ButtonAction::Signup { event, option }),
                Some("g") => Some(ButtonAction::Guests { event, option }),
                Some(_) => None,
            }
        }
    }
}

async fn push_sync(
    db: &store::Db,
    http: &Arc<Mutex<Option<Arc<serenity::Http>>>>,
    public_url: &str,
    event_id: i64,
) -> Result<(), Error> {
    post_event(db, http, public_url, event_id, true).await
}

/// Tells open pages about a change made from Discord to one event: to who is
/// signed up, or to the event itself.
fn tell_pages(db: &store::Db, event_id: i64, signups: bool) {
    let Ok(mut conn) = db.get() else { return };
    if let Ok(Some(event)) = store::event(&mut conn, event_id) {
        if signups {
            crate::live::signups(event.guild_id, event.public_id);
        } else {
            crate::live::event(event.guild_id, event.public_id);
        }
    }
}

async fn post_event(
    db: &store::Db,
    http: &Arc<Mutex<Option<Arc<serenity::Http>>>>,
    public_url: &str,
    event_id: i64,
    notify: bool,
) -> Result<(), Error> {
    let view = {
        let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
        logic::discord_view(&mut conn, event_id, public_url)?
    };
    let Some(view) = view else {
        return Ok(());
    };
    let http = http
        .lock()
        .map_err(|_| "http lock")?
        .clone()
        .ok_or("Discord is not connected")?;
    let channel_id = serenity::ChannelId::new(view.channel_id.parse()?);
    let mut embed = serenity::CreateEmbed::new()
        .title(clip(&view.title, 250))
        .description(clip(&view.body, 4000))
        .color(serenity::Colour::new(view.color))
        .footer(serenity::CreateEmbedFooter::new(clip(&view.footer, 2000)));
    for field in &view.fields {
        embed = embed.field(
            clip(&field.name, 256),
            clip(&field.value, 1024),
            field.inline,
        );
    }
    if let Some(image) = view.image_url.as_deref() {
        embed = embed.image(image);
    }
    let mut rows = Vec::new();
    let mut buttons = Vec::new();
    for button in &view.buttons {
        buttons.push(action_button(view.event, button));
        if buttons.len() == 5 {
            rows.push(serenity::CreateActionRow::Buttons(std::mem::take(
                &mut buttons,
            )));
        }
    }
    if !buttons.is_empty() {
        rows.push(serenity::CreateActionRow::Buttons(buttons));
    }
    // Discord notifies @everyone and roles only from the message text, not the embed.
    // An edit notifies a mention only when that mention is new, so signup refreshes
    // of an unchanged line do not ping again.
    let content = logic::mention_line(view.mention_everyone, &view.mention_roles);
    let mentions = if notify {
        event_mentions(view.mention_everyone, &view.mention_roles)
    } else {
        serenity::CreateAllowedMentions::new()
    };
    let sent = if let Some(message_id) = view
        .message_id
        .as_deref()
        .and_then(|value| value.parse().ok())
    {
        let edit = channel_id
            .edit_message(
                &http,
                serenity::MessageId::new(message_id),
                serenity::EditMessage::new()
                    .content(&content)
                    .embed(embed.clone())
                    .components(rows.clone())
                    .allowed_mentions(mentions.clone()),
            )
            .await;
        match edit {
            Ok(_) => None,
            Err(err) => {
                tracing::info!("replacing a missing Discord message: {err}");
                Some(
                    channel_id
                        .send_message(
                            &http,
                            serenity::CreateMessage::new()
                                .content(content)
                                .embed(embed)
                                .components(rows)
                                .allowed_mentions(mentions),
                        )
                        .await?,
                )
            }
        }
    } else {
        Some(
            channel_id
                .send_message(
                    &http,
                    serenity::CreateMessage::new()
                        .content(content)
                        .embed(embed)
                        .components(rows)
                        .allowed_mentions(mentions),
                )
                .await?,
        )
    };
    let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
    if let Some(message) = &sent {
        logic::remember_discord_message(&mut conn, event_id, &message.id.get().to_string())?;
    }
    let status = store::event(&mut conn, event_id)?
        .map(|event| event.status)
        .unwrap_or_default();
    let plan = if sent.is_some() || status == "cancelled" {
        logic::event_channel_purge(&mut conn, event_id)?
    } else {
        None
    };
    drop(conn);
    if let Some(plan) = plan {
        if let Err(err) = purge_event_channel(db, &http, plan).await {
            tracing::warn!("could not purge the event channel: {err}");
        }
    }
    Ok(())
}

async fn purge_event_channel(
    db: &store::Db,
    http: &serenity::Http,
    plan: logic::EventChannelPurge,
) -> Result<(), Error> {
    let channel_id = serenity::ChannelId::new(plan.discord_channel_id.parse()?);
    let mut before = None;
    for _ in 0..10 {
        let mut query = serenity::builder::GetMessages::new().limit(100);
        if let Some(id) = before {
            query = query.before(id);
        }
        let batch = channel_id.messages(http, query).await?;
        if batch.is_empty() {
            break;
        }
        let oldest = batch.iter().map(|message| message.id).min();
        for message in &batch {
            if message.pinned || plan.keep.contains(&message.id.get().to_string()) {
                continue;
            }
            if let Err(err) = channel_id.delete_message(http, message.id).await {
                tracing::warn!("could not purge a message in the event channel: {err}");
            }
        }
        if batch.len() < 100 {
            break;
        }
        before = oldest;
        if before.is_none() {
            break;
        }
    }
    let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
    logic::mark_channel_purged(&mut conn, plan.channel_id)?;
    Ok(())
}

/// Brings a week's poll up to date in Discord, and its approval request when
/// it has one.
async fn push_routine(
    db: &store::Db,
    http: &Arc<Mutex<Option<Arc<serenity::Http>>>>,
    public_url: &str,
    run_id: i64,
) -> Result<(), Error> {
    push_poll(db, http, public_url, run_id).await?;
    push_approval(db, http, public_url, run_id).await
}

/// The request an approval step posts in its channel, edited once answered.
async fn push_approval(
    db: &store::Db,
    http: &Arc<Mutex<Option<Arc<serenity::Http>>>>,
    public_url: &str,
    run_id: i64,
) -> Result<(), Error> {
    let post = {
        let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
        logic::approval_post(&mut conn, run_id, public_url, chrono::Utc::now())?
    };
    let Some(post) = post else {
        return Ok(());
    };
    let http = http
        .lock()
        .map_err(|_| "http lock")?
        .clone()
        .ok_or("Discord is not connected")?;
    let channel_id = serenity::ChannelId::new(post.channel_discord_id.parse()?);
    let mut embed = serenity::CreateEmbed::new()
        .title(clip(&post.title, 250))
        .description(clip(&post.body, 4000))
        .color(serenity::Colour::new(0x1e3a_6e));
    for field in &post.fields {
        embed = embed.field(clip(&field.name, 256), clip(&field.value, 1024), field.inline);
    }
    let sent = upsert_message(
        &http,
        channel_id,
        post.message_id.as_deref(),
        logic::mention_line(false, &post.mention_roles),
        event_mentions(false, &post.mention_roles),
        embed,
        button_rows(&post.buttons),
    )
    .await?;
    if let Some(message) = sent {
        let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
        logic::remember_approval_message(&mut conn, run_id, &message.id.get().to_string())?;
    }
    Ok(())
}

async fn push_poll(
    db: &store::Db,
    http: &Arc<Mutex<Option<Arc<serenity::Http>>>>,
    public_url: &str,
    run_id: i64,
) -> Result<(), Error> {
    let post = {
        let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
        logic::routine_post(&mut conn, run_id, public_url)?
    };
    let Some(post) = post else {
        return Ok(());
    };
    let http = http
        .lock()
        .map_err(|_| "http lock")?
        .clone()
        .ok_or("Discord is not connected")?;
    let channel_id = serenity::ChannelId::new(post.channel_discord_id.parse()?);
    let mut embed = serenity::CreateEmbed::new()
        .title(clip(&post.title, 250))
        .description(clip(&post.body, 4000))
        .color(serenity::Colour::new(post.color));
    for field in &post.fields {
        embed = embed.field(
            clip(&field.name, 256),
            clip(&field.value, 1024),
            field.inline,
        );
    }
    // A locked poll's post is now the event. Its results go in the event's
    // thread, or in a reply to the post when it has none; posted once and
    // edited after.
    if post.results {
        let thread = match (&post.thread, post.reply_to.as_deref()) {
            (Some(thread), Some(parent)) => match thread_on_message(
                db,
                &http,
                thread.event_id,
                &post.channel_discord_id,
                parent,
                &thread.name,
                thread.discord_id.as_deref(),
            )
            .await
            {
                Ok(id) => Some(serenity::ChannelId::new(id)),
                Err(err) => {
                    tracing::warn!("could not open the thread for the poll results: {err}");
                    None
                }
            },
            _ => None,
        };
        let target = thread.unwrap_or(channel_id);
        if let Some(sent) = post.message_id.as_deref().and_then(|id| id.parse().ok()) {
            let edited = target
                .edit_message(&http, serenity::MessageId::new(sent), serenity::EditMessage::new().embed(embed.clone()))
                .await;
            if edited.is_ok() {
                return Ok(());
            }
        }
        let mut message = serenity::CreateMessage::new().embed(embed);
        if thread.is_none() {
            if let Some(parent) = post.reply_to.as_deref().and_then(|id| id.parse().ok()) {
                message = message.reference_message((channel_id, serenity::MessageId::new(parent)));
            }
        }
        let message = target.send_message(&http, message).await?;
        let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
        logic::remember_results_message(&mut conn, run_id, &message.id.get().to_string())?;
        return Ok(());
    }
    // As with an event, a mention pings once: when the poll goes up, or when an
    // edit to the routine adds it.
    let sent = upsert_message(
        &http,
        channel_id,
        post.message_id.as_deref(),
        logic::mention_line(post.mention_everyone, &post.mention_roles),
        event_mentions(post.mention_everyone, &post.mention_roles),
        embed,
        button_rows(&post.buttons),
    )
    .await?;
    if let Some(message) = sent {
        let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
        logic::remember_routine_message(&mut conn, run_id, &message.id.get().to_string())?;
    }
    Ok(())
}

fn button_rows(buttons: &[logic::PollButton]) -> Vec<serenity::CreateActionRow> {
    let mut rows = Vec::new();
    let mut built = Vec::new();
    for button in buttons {
        let label = clip(&button.label, 80);
        let label = if label.is_empty() {
            "Choice".to_string()
        } else {
            label
        };
        let style = match button.tone {
            logic::ButtonTone::Primary => serenity::ButtonStyle::Primary,
            logic::ButtonTone::Danger => serenity::ButtonStyle::Danger,
            logic::ButtonTone::Secondary => serenity::ButtonStyle::Secondary,
        };
        built.push(
            serenity::CreateButton::new(button.custom_id.clone())
                .style(style)
                .label(label),
        );
        if built.len() == 5 {
            rows.push(serenity::CreateActionRow::Buttons(std::mem::take(
                &mut built,
            )));
        }
    }
    if !built.is_empty() {
        rows.push(serenity::CreateActionRow::Buttons(built));
    }
    rows
}

async fn upsert_message(
    http: &serenity::Http,
    channel_id: serenity::ChannelId,
    message_id: Option<&str>,
    content: String,
    mentions: serenity::CreateAllowedMentions,
    embed: serenity::CreateEmbed,
    rows: Vec<serenity::CreateActionRow>,
) -> Result<Option<serenity::Message>, Error> {
    if let Some(message_id) = message_id.and_then(|value| value.parse().ok()) {
        match channel_id
            .edit_message(
                http,
                serenity::MessageId::new(message_id),
                serenity::EditMessage::new()
                    .content(&content)
                    .embed(embed.clone())
                    .components(rows.clone())
                    .allowed_mentions(mentions.clone()),
            )
            .await
        {
            Ok(_) => return Ok(None),
            Err(err) => tracing::info!("replacing a missing vote message: {err}"),
        }
    }
    let message = channel_id
        .send_message(
            http,
            serenity::CreateMessage::new()
                .content(content)
                .embed(embed)
                .components(rows)
                .allowed_mentions(mentions),
        )
        .await?;
    Ok(Some(message))
}

async fn push_attendance(
    db: &store::Db,
    http: &Arc<Mutex<Option<Arc<serenity::Http>>>>,
    event_id: i64,
) -> Result<(), Error> {
    let work = {
        let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
        match logic::attendance(&mut conn, event_id)? {
            logic::Attendance::Idle => return Ok(()),
            logic::Attendance::Waiting => {
                return Err("the Discord post is not up yet".into());
            }
            logic::Attendance::Ready(work) => work,
        }
    };
    if !work.open_thread
        && work.thread_add.is_empty()
        && work.thread_remove.is_empty()
        && work.role_add.is_empty()
        && work.role_remove.is_empty()
    {
        return Ok(());
    }
    let http = http
        .lock()
        .map_err(|_| "http lock")?
        .clone()
        .ok_or("Discord is not connected")?;
    // A posted event with a thread gets one, whoever is in it. Reminders bring
    // people in by mentioning them; adding them here is for servers that ask.
    // A thread Discord won't open is no reason to hold back attendee roles.
    let opened = match work.open_thread {
        true => match open_thread(db, &http, event_id, &work).await {
            Ok(id) => Some(id),
            Err(err) => {
                tracing::warn!("could not open the thread for event {event_id}: {err}");
                None
            }
        },
        false => None,
    };
    if !work.thread_add.is_empty() || !work.thread_remove.is_empty() {
        if work.thread_discord_id.is_none() && opened.is_none() && work.thread_add.is_empty() {
            let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
            for membership_id in &work.thread_remove {
                store::clear_applied(&mut conn, event_id, "thread", membership_id)?;
            }
        } else {
            let thread_id = match opened {
                Some(id) => id,
                None => open_thread(db, &http, event_id, &work).await?,
            };
            for membership_id in &work.thread_add {
                join_thread(&http, thread_id, membership_id).await?;
                let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
                store::mark_applied(&mut conn, event_id, "thread", membership_id)?;
            }
            for membership_id in &work.thread_remove {
                leave_thread(&http, thread_id, membership_id).await?;
                let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
                store::clear_applied(&mut conn, event_id, "thread", membership_id)?;
            }
        }
    }
    if work.role_discord_id.is_none() && work.role_add.is_empty() {
        let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
        for membership_id in &work.role_remove {
            store::clear_applied(&mut conn, event_id, "role", membership_id)?;
        }
    } else if !work.role_add.is_empty() || !work.role_remove.is_empty() {
        let role_id = open_role(db, &http, &work).await?;
        let guild_id = serenity::GuildId::new(work.guild_discord_id.parse()?);
        for membership_id in &work.role_add {
            let user = serenity::UserId::new(membership_id.parse()?);
            match http.add_member_role(guild_id, user, role_id, None).await {
                Ok(()) => {}
                Err(err) if already_applied(&err) => {}
                Err(err) => return Err(err.into()),
            }
            let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
            store::mark_applied(&mut conn, event_id, "role", membership_id)?;
        }
        for membership_id in &work.role_remove {
            let user = serenity::UserId::new(membership_id.parse()?);
            match http.remove_member_role(guild_id, user, role_id, None).await {
                Ok(()) => {}
                Err(err) if already_applied(&err) => {}
                Err(err) => return Err(err.into()),
            }
            let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
            store::clear_applied(&mut conn, event_id, "role", membership_id)?;
        }
    }
    Ok(())
}

async fn open_thread(
    db: &store::Db,
    http: &serenity::Http,
    event_id: i64,
    work: &logic::AttendanceWork,
) -> Result<u64, Error> {
    thread_on_message(
        db,
        http,
        event_id,
        &work.channel_discord_id,
        &work.message_id,
        &work.thread_name,
        work.thread_discord_id.as_deref(),
    )
    .await
}

async fn thread_on_message(
    db: &store::Db,
    http: &serenity::Http,
    event_id: i64,
    channel_id: &str,
    message_id: &str,
    name: &str,
    existing: Option<&str>,
) -> Result<u64, Error> {
    if let Some(discord_id) = existing.filter(|id| !id.is_empty()) {
        return Ok(discord_id.parse()?);
    }
    let channel_id = serenity::ChannelId::new(channel_id.parse()?);
    let message_id = serenity::MessageId::new(message_id.parse()?);
    let name = if name.trim().is_empty() {
        "Event".to_string()
    } else {
        name.to_string()
    };
    let created = channel_id
        .create_thread_from_message(
            http,
            message_id,
            serenity::CreateThread::new(name.clone())
                .kind(serenity::ChannelType::PublicThread)
                .auto_archive_duration(serenity::AutoArchiveDuration::OneWeek),
        )
        .await;
    let thread_id = match created {
        Ok(thread) => thread.id.get(),
        Err(err) => match existing_thread(http, channel_id, message_id).await {
            Some(id) => id,
            None => return Err(err.into()),
        },
    };
    let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
    store::set_thread_discord_id(&mut conn, event_id, &name, &thread_id.to_string())?;
    Ok(thread_id)
}

async fn existing_thread(
    http: &serenity::Http,
    channel_id: serenity::ChannelId,
    message_id: serenity::MessageId,
) -> Option<u64> {
    let message = channel_id.message(http, message_id).await.ok()?;
    message.thread.map(|thread| thread.id.get())
}

async fn join_thread(http: &serenity::Http, thread_id: u64, membership_id: &str) -> Result<(), Error> {
    let thread = serenity::ChannelId::new(thread_id);
    let user = serenity::UserId::new(membership_id.parse()?);
    match thread.add_thread_member(http, user).await {
        Ok(()) => Ok(()),
        Err(err) if already_applied(&err) => Ok(()),
        Err(err) => Err(err.into()),
    }
}

async fn leave_thread(http: &serenity::Http, thread_id: u64, membership_id: &str) -> Result<(), Error> {
    let thread = serenity::ChannelId::new(thread_id);
    let user = serenity::UserId::new(membership_id.parse()?);
    match thread.remove_thread_member(http, user).await {
        Ok(()) => Ok(()),
        Err(err) if already_applied(&err) => Ok(()),
        Err(err) => Err(err.into()),
    }
}

async fn open_role(
    db: &store::Db,
    http: &serenity::Http,
    work: &logic::AttendanceWork,
) -> Result<serenity::RoleId, Error> {
    if let Some(discord_id) = &work.role_discord_id {
        return Ok(serenity::RoleId::new(discord_id.parse()?));
    }
    let stored_role_id = work.role_id.ok_or("this event has no attendee role")?;
    let guild_id = serenity::GuildId::new(work.guild_discord_id.parse()?);
    let name = if work.role_name.trim().is_empty() {
        format!("{} attendee", work.title)
    } else {
        work.role_name.clone()
    };
    let role = guild_id
        .create_role(
            http,
            serenity::EditRole::new()
                .name(clip(&name, 100))
                .colour(work.role_color),
        )
        .await?;
    let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
    store::set_role_discord_id(&mut conn, stored_role_id, &role.id.get().to_string())?;
    Ok(role.id)
}

fn already_applied(err: &serenity::Error) -> bool {
    let text = err.to_string().to_ascii_lowercase();
    text.contains("already") || text.contains("unknown member")
}

async fn push_remind(
    db: &store::Db,
    http: &Arc<Mutex<Option<Arc<serenity::Http>>>>,
    public_url: &str,
    event_id: i64,
    text: &str,
    message: &str,
) -> Result<(), Error> {
    let plan = {
        let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
        let event =
            store::event(&mut conn, event_id)?.ok_or_else(|| "event missing".to_string())?;
        let channel = store::channel(&mut conn, event.channel_id)?
            .ok_or_else(|| "channel missing".to_string())?;
        let Some(channel_id) = channel.discord_id.filter(|id| !id.is_empty()) else {
            return Ok(());
        };
        let guild = store::guild(&mut conn, event.guild_id)?;
        let settings = store::settings(&mut conn, event.guild_id)?;
        let start = event.starts_at;
        let end = start + chrono::Duration::minutes(event.duration_minutes);
        let message_id = event.discord_message_id.filter(|id| !id.is_empty());
        let timezone = guild
            .as_ref()
            .map(|guild| guild.timezone.clone())
            .unwrap_or_else(|| "UTC".into());
        let guild_id = guild.and_then(|guild| guild.discord_id);
        let link = message_id.as_ref().and_then(|message_id| {
            let guild_id = guild_id.as_deref()?;
            Some(logic::discord_message_link(
                guild_id,
                &channel_id,
                message_id,
            ))
        });
        let link = link.or_else(|| {
            logic::web_event_url(public_url, guild_id.as_deref(), &event.public_id)
        });
        RemindPlan {
            lang: i18n::lang_of(&mut conn, event.guild_id),
            channel_id,
            thread_id: store::thread_discord_id(&mut conn, event_id)?,
            in_thread: event.thread_enabled && settings.thread_reminders,
            message_id,
            thread_name: logic::event_thread_name(
                i18n::lang_of(&mut conn, event.guild_id),
                &event.title,
                event.starts_at,
                &timezone,
            ),
            title: event.title,
            start: start.timestamp(),
            end: end.timestamp(),
            link,
        }
    };
    let http = http
        .lock()
        .map_err(|_| "http lock")?
        .clone()
        .ok_or("Discord is not connected")?;
    let target = if plan.in_thread {
        if let Some(thread_id) = plan.thread_id.filter(|id| !id.is_empty()) {
            thread_id.parse()?
        } else if let Some(message_id) = &plan.message_id {
            thread_on_message(
                db,
                &http,
                event_id,
                &plan.channel_id,
                message_id,
                &plan.thread_name,
                None,
            )
            .await?
        } else {
            plan.channel_id.parse()?
        }
    } else {
        plan.channel_id.parse()?
    };
    let card = logic::reminder_card(
        plan.lang,
        &plan.title,
        plan.start,
        plan.end,
        plan.link.as_deref(),
        message,
    );
    let embed = serenity::CreateEmbed::new()
        .title(clip(&card.title, 256))
        .description(clip(&card.description, 4000))
        .color(serenity::Colour::new(0xF1C40F))
        .footer(serenity::CreateEmbedFooter::new(card.footer));
    serenity::ChannelId::new(target)
        .send_message(
            &http,
            serenity::CreateMessage::new()
                .content(clip(text, 2000))
                .embed(embed)
                .allowed_mentions(serenity::CreateAllowedMentions::new().all_users(true)),
        )
        .await?;
    Ok(())
}

struct RemindPlan {
    lang: Lang,
    channel_id: String,
    thread_id: Option<String>,
    in_thread: bool,
    message_id: Option<String>,
    thread_name: String,
    title: String,
    start: i64,
    end: i64,
    link: Option<String>,
}


struct Actor {
    guild_id: i64,
    membership_id: i64,
    discord_user: String,
    /// The server's language, for the reply.
    lang: Lang,
}

async fn remember_actor(ctx: &Context<'_>) -> Result<Option<Actor>, Error> {
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(None);
    };
    let author = ctx.author();
    let discord_user = author.id.get().to_string();
    let guild_name = ctx
        .guild_id()
        .and_then(|id| ctx.cache().guild(id).map(|guild| guild.name.clone()))
        .unwrap_or_default();
    let member = ctx.author_member().await;
    let display = crate::auth::discord_display_name(
        member.as_ref().and_then(|member| member.nick.as_deref()),
        author.global_name.as_deref(),
        &author.name,
    );
    let manage = member.as_ref().and_then(|member| {
        member
            .permissions
            .map(|permissions| permissions.manage_guild() || permissions.administrator())
    });
    let roles = member.as_ref().and_then(|member| {
        ctx.cache().guild(guild_id).map(|guild| {
            member
                .roles
                .iter()
                .filter_map(|role_id| {
                    guild.roles.get(role_id).map(|role| {
                        (
                            role.name.clone(),
                            role.colour.hex(),
                            role.id.get().to_string(),
                        )
                    })
                })
                .collect::<Vec<_>>()
        })
    });
    let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
    let guild = logic::ensure_discord_guild(&mut conn, &guild_id.get().to_string(), &guild_name)?;
    let (user, renamed) = logic::ensure_discord_user(&mut conn, guild.id, &discord_user, &display)?;
    redraw(&ctx.data().jobs, renamed);
    if let Some(manage) = manage {
        logic::set_server_manager(&mut conn, user.id, manage)?;
    }
    if let Some(roles) = &roles {
        sync_roles(&mut conn, guild.id, user.id, roles)?;
    }
    Ok(Some(Actor {
        guild_id: guild.id,
        membership_id: user.id,
        discord_user,
        lang: i18n::lang_of(&mut conn, guild.id),
    }))
}

fn presence_line(status: &str) -> Option<String> {
    let line: String = status.trim().chars().take(128).collect();
    if line.is_empty() {
        None
    } else {
        Some(line)
    }
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

fn event_mentions(everyone: bool, roles: &[String]) -> serenity::CreateAllowedMentions {
    let mut allowed = serenity::CreateAllowedMentions::new();
    if everyone {
        allowed = allowed.everyone(true);
    }
    let roles: Vec<u64> = roles.iter().filter_map(|id| id.parse().ok()).collect();
    if !roles.is_empty() {
        allowed = allowed.roles(roles);
    }
    allowed
}

fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

#[poise::command(
    slash_command,
    guild_only,
    description_localized("en-US", "Start an event. Marian asks for the details in a DM")
)]
async fn event(ctx: Context<'_>) -> Result<(), Error> {
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
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        let channel = logic::ensure_discord_channel(
            &mut conn,
            actor.guild_id,
            &invoked_id,
            if invoked_name.is_empty() {
                &invoked_id
            } else {
                &invoked_name
            },
        )?;
        commands::start_event_dm(
            &mut conn,
            actor.guild_id,
            actor.membership_id,
            &actor.discord_user,
            &channel.name,
            Some(&invoked_id),
            actor.lang,
        )
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
                    let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
                    commands::cancel_event_dm(&mut conn, &actor.discord_user)?;
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
                let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
                commands::cancel_event_dm(&mut conn, &actor.discord_user)?;
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

async fn complete_channel(ctx: Context<'_>, partial: &str) -> Vec<serenity::AutocompleteChoice> {
    let Some(guild_id) = ctx.guild_id() else {
        return Vec::new();
    };
    let Ok(mut conn) = ctx.data().db.get() else {
        return Vec::new();
    };
    let Ok(Some(guild)) = store::guild_by_discord(&mut conn, &guild_id.get().to_string()) else {
        return Vec::new();
    };
    let lang = i18n::lang_of(&mut conn, guild.id);
    commands::channel_choices(&mut conn, guild.id, &ctx.author().id.get().to_string(), partial, lang)
        .unwrap_or_default()
        .into_iter()
        .map(|choice| serenity::AutocompleteChoice::new(choice.label, choice.value))
        .collect()
}

async fn deliver(
    ctx: Context<'_>,
    actor: &Actor,
    label: &str,
    outcome: Result<String, crate::error::OpError>,
) -> Result<(), Error> {
    let data = ctx.data();
    let text = match &outcome {
        Ok(text) => text.clone(),
        Err(err) => err.localized(actor.lang),
    };
    let refresh = {
        let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
        let _ = logic::record_command(&mut conn, actor.guild_id, actor.membership_id, label, &text);
        crate::serve::events_to_refresh(&mut conn, label, &text)
    };
    crate::live::server(actor.guild_id);
    for event_id in refresh {
        if let Err(err) = push_sync(&data.db, &data.http, &data.public_url, event_id).await {
            tracing::warn!("could not refresh event {event_id}: {err}");
        }
        if let Err(err) = push_attendance(&data.db, &data.http, event_id).await {
            tracing::warn!("could not update attendance for event {event_id}: {err}");
        }
    }
    ctx.say(clip(&text, 1800)).await?;
    Ok(())
}

#[poise::command(
    slash_command,
    guild_only,
    description_localized("en-US", "Change an event")
)]
async fn edit(
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
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
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
    };
    deliver(ctx, &actor, &label, outcome).await
}

#[poise::command(
    slash_command,
    guild_only,
    description_localized("en-US", "Turn your reminders on or off")
)]
async fn reminders(
    ctx: Context<'_>,
    #[description = "On or off. Leave empty to see the current choice"] enabled: Option<bool>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(actor) = remember_actor(&ctx).await? else {
        ctx.say(tr!(Lang::En, "reply-outside")).await?;
        return Ok(());
    };
    let outcome = {
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::set_reminders(&mut conn, actor.membership_id, enabled, actor.lang)
    };
    deliver(ctx, &actor, "/reminders", outcome).await
}

#[poise::command(
    slash_command,
    guild_only,
    subcommands("response_add", "response_remove", "response_move"),
    description_localized("en-US", "Add, remove, or move a response")
)]
async fn response(_ctx: Context<'_>) -> Result<(), Error> {
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

async fn complete_event(ctx: Context<'_>, partial: &str) -> Vec<serenity::AutocompleteChoice> {
    let Some(guild_id) = ctx.guild_id() else {
        return Vec::new();
    };
    let Ok(mut conn) = ctx.data().db.get() else {
        return Vec::new();
    };
    let Ok(Some(guild)) = store::guild_by_discord(&mut conn, &guild_id.get().to_string()) else {
        return Vec::new();
    };
    let lang = i18n::lang_of(&mut conn, guild.id);
    commands::response_events(&mut conn, guild.id, &ctx.author().id.get().to_string(), partial, chrono::Utc::now(), lang)
        .unwrap_or_default()
        .into_iter()
        .map(|choice| serenity::AutocompleteChoice::new(choice.label, choice.value))
        .collect()
}

async fn complete_option(ctx: Context<'_>, partial: &str) -> Vec<serenity::AutocompleteChoice> {
    let Some(guild_id) = ctx.guild_id() else {
        return Vec::new();
    };
    let Some(event) = slash_arg(&ctx, "event") else {
        return Vec::new();
    };
    let Ok(mut conn) = ctx.data().db.get() else {
        return Vec::new();
    };
    let Ok(Some(guild)) = store::guild_by_discord(&mut conn, &guild_id.get().to_string()) else {
        return Vec::new();
    };
    commands::response_options(&mut conn, guild.id, &ctx.author().id.get().to_string(), &event, partial)
        .unwrap_or_default()
        .into_iter()
        .map(|choice| serenity::AutocompleteChoice::new(choice.label, choice.value))
        .collect()
}

fn slash_arg(ctx: &Context<'_>, name: &str) -> Option<String> {
    let poise::Context::Application(app) = ctx else {
        return None;
    };
    app.args.iter().find_map(|opt| {
        if opt.name != name {
            return None;
        }
        match opt.value {
            serenity::ResolvedValue::String(value) => Some(value.to_string()),
            serenity::ResolvedValue::Autocomplete { value, .. } => Some(value.to_string()),
            _ => None,
        }
    })
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
        commands::roster_action(&mut conn, actor.membership_id, event, target.id, option, action, guests, actor.lang)
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

async fn setting(ctx: Context<'_>, label: &str, change: commands::Setting) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(actor) = remember_actor(&ctx).await? else {
        ctx.say(tr!(Lang::En, "reply-outside")).await?;
        return Ok(());
    };
    let language = matches!(change, commands::Setting::Language(_));
    let (outcome, lang) = {
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        let outcome = commands::apply_setting(&mut conn, actor.guild_id, actor.membership_id, change, actor.lang);
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
async fn settings(_ctx: Context<'_>) -> Result<(), Error> {
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

#[poise::command(
    slash_command,
    guild_only,
    subcommands("signup_options_list", "signup_options_add", "signup_options_remove"),
    description_localized("en-US", "List, add, or remove a library option")
)]
async fn signup_options(_ctx: Context<'_>) -> Result<(), Error> {
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
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::library_text(&mut conn, actor.guild_id, actor.lang)
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
    #[description = "People on it get reminders and the attendee role (default yes)"] attending: Option<bool>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(actor) = remember_actor(&ctx).await? else {
        ctx.say(tr!(Lang::En, "reply-outside")).await?;
        return Ok(());
    };
    let emoji = emoji.unwrap_or_else(|| "•".into());
    let outcome = {
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::add_library_option(
            &mut conn,
            actor.guild_id,
            actor.membership_id,
            &name,
            &emoji,
            attending.unwrap_or(true),
            actor.lang,
        )
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
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::remove_library_option(&mut conn, actor.guild_id, actor.membership_id, &name, actor.lang)
    };
    deliver(ctx, &actor, "/signup_options remove", outcome).await
}

#[poise::command(
    slash_command,
    guild_only,
    subcommands("signup_presets_list", "signup_presets_add", "signup_presets_remove"),
    description_localized("en-US", "List, save, or remove a signup preset")
)]
async fn signup_presets(_ctx: Context<'_>) -> Result<(), Error> {
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
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::preset_text(&mut conn, actor.guild_id, actor.lang)
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
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::add_preset(&mut conn, actor.guild_id, actor.membership_id, &name, &options, actor.lang)
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
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::remove_preset(&mut conn, actor.guild_id, actor.membership_id, &name, actor.lang)
    };
    deliver(ctx, &actor, "/signup_presets remove", outcome).await
}

#[poise::command(
    slash_command,
    guild_only,
    description_localized("en-US", "Create an event channel")
)]
async fn channel(
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
    let guild_discord = guild_discord.parse::<u64>().map_err(|_| {
        crate::error::OpError::from(crate::msg!("err-server-not-connected"))
    })?;
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
    match logic::bind_channel_discord(
        &mut conn,
        channel_id,
        &discord_channel.id.get().to_string(),
    ) {
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
    subcommands("google_authorize", "google_auto_sync", "google_sync", "google_revoke"),
    description_localized("en-US", "Connect, update, or disconnect a Google Calendar")
)]
async fn google(_ctx: Context<'_>) -> Result<(), Error> {
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
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::google_sign_in_url(&mut conn, actor.membership_id)
    };
    let url = match outcome {
        Ok(url) => url,
        Err(err) => return deliver(ctx, &actor, "/google authorize", Err(err)).await,
    };
    {
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        let _ = logic::record_command(
            &mut conn,
            actor.guild_id,
            actor.membership_id,
            "/google authorize",
            "Sent a Google sign-in button.",
        );
    }
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
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::google_auto_sync(&mut conn, actor.guild_id, actor.membership_id, enabled, actor.lang)
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
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::google_sync(&mut conn, actor.guild_id, actor.membership_id, &event, actor.lang)
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
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::google_revoke(&mut conn, actor.guild_id, actor.membership_id, actor.lang)
    };
    deliver(ctx, &actor, "/google revoke", outcome).await
}

#[poise::command(
    slash_command,
    guild_only,
    description_localized("en-US", "Reverse the posts in this event channel")
)]
async fn sort(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(actor) = remember_actor(&ctx).await? else {
        ctx.say(tr!(Lang::En, "reply-outside")).await?;
        return Ok(());
    };
    let discord_channel = ctx.channel_id().get().to_string();
    let plan = {
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
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
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
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

#[poise::command(
    slash_command,
    guild_only,
    description_localized(
        "en-US",
        "How /event works, and where the other commands are explained"
    )
)]
async fn help(ctx: Context<'_>) -> Result<(), Error> {
    let lang = match ctx.guild_id() {
        Some(guild_id) => {
            let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
            match store::guild_by_discord(&mut conn, &guild_id.get().to_string())? {
                Some(guild) => i18n::lang_of(&mut conn, guild.id),
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
    ctx.say(format!("{}\n\n{more}", tr!(lang, "reply-help"))).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{dm_blocked, embed_parts, parse_button, presence_line, ButtonAction, Lang};

    #[test]
    fn a_button_names_its_rows_by_public_id() {
        let event = uuid::Uuid::now_v7();
        let option = uuid::Uuid::now_v7();
        assert_eq!(
            parse_button(&format!("marian:{event}:{option}")),
            Some(ButtonAction::Signup { event, option })
        );
        assert_eq!(
            parse_button(&format!("marian:{event}:{option}:g")),
            Some(ButtonAction::Guests { event, option })
        );
        assert_eq!(parse_button(&format!("marian:{event}:{option}:x")), None);
        assert!(format!("marian:{event}:{option}:g").len() <= 100, "Discord's limit on a button id");
        assert_eq!(parse_button(&format!("marian:{event}:edit")), Some(ButtonAction::Edit { event }));
        assert_eq!(
            parse_button(&format!("marian:{event}:confirm-delete")),
            Some(ButtonAction::ConfirmDelete { event })
        );
        assert_eq!(
            parse_button(&format!("marian:w:{option}:3")),
            Some(ButtonAction::Routine { run: option, day: 3 })
        );
        // Buttons posted before carry row numbers. They do nothing now.
        assert_eq!(parse_button("marian:12:34"), None);
        assert_eq!(parse_button("marian:12:edit"), None);
        assert_eq!(parse_button("marian:w:5:3"), None);
        assert_eq!(
            parse_button(&format!("marian:a:{option}:4")),
            Some(ButtonAction::Decide { run: option, day: Some(4) })
        );
        assert_eq!(
            parse_button(&format!("marian:a:{option}:skip")),
            Some(ButtonAction::Decide { run: option, day: None })
        );
        assert!(format!("marian:{event}:{option}").len() <= 100, "Discord's limit on a button id");
    }
    use crate::commands::DmReply;

    /// Every command, option and choice, as the path its `cmd-` message uses.
    fn command_paths(
        command: &poise::Command<super::Data, super::Error>,
        parent: &str,
        out: &mut Vec<(String, Option<String>)>,
    ) {
        let path = if parent.is_empty() {
            command.name.clone()
        } else {
            format!("{parent}-{}", command.name)
        };
        out.push((format!("cmd-{path}"), command.description.clone()));
        for parameter in &command.parameters {
            out.push((format!("cmd-{path}--{}", parameter.name), parameter.description.clone()));
            // A language is named in itself, whatever the server's language.
            if parameter.name == "language" {
                continue;
            }
            for choice in &parameter.choices {
                out.push((format!("cmd-{path}--{}--{}", parameter.name, choice.name), None));
            }
        }
        for subcommand in &command.subcommands {
            command_paths(subcommand, &path, out);
        }
    }

    #[test]
    fn every_command_is_described_in_polish_within_discords_limit() {
        let mut paths = Vec::new();
        for command in super::all_commands() {
            command_paths(&command, "", &mut paths);
        }
        assert!(paths.len() > 80, "{}", paths.len());
        for (key, english) in &paths {
            assert!(crate::i18n::has(Lang::Pl, key), "Polish has no {key}");
            let polish = crate::i18n::text(Lang::Pl, key, &[]);
            assert!(polish.chars().count() <= 100, "{key} is longer than Discord allows");
            if let Some(english) = english {
                assert!(english.chars().count() <= 100, "{key} in English is too long");
            }
        }
        let mut described = super::all_commands();
        for command in &mut described {
            super::describe_command(command, Lang::Pl, "");
        }
        let settings = described.iter().find(|command| command.name == "settings").unwrap();
        let digest = settings.subcommands.iter().find(|command| command.name == "digest").unwrap();
        assert_eq!(digest.description.as_deref(), Some("Jak często Marian publikuje podsumowanie"));
        assert!(digest.description_localizations.is_empty(), "the server's language wins");
        assert_eq!(digest.parameters[0].choices[1].name, "codziennie");
        let language = settings.subcommands.iter().find(|command| command.name == "language").unwrap();
        assert_eq!(language.parameters[0].choices[1].name, "Polski");
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
        assert!(polish.starts_with("Nie udało się wysłać wiadomości prywatnej."), "{polish}");
        assert!(polish.ends_with("https://marian.example.com/servers/4242/events/new."), "{polish}");
    }

    #[test]
    fn every_job_kind_gets_a_zero_count_at_startup() {
        use super::{job_kind, DiscordJob, JobMetrics, JOB_KINDS, JOB_OUTCOMES};
        use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
        use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

        for job in [
            DiscordJob::sync(1),
            DiscordJob::remind(1, String::new(), String::new()),
            DiscordJob::attend(1),
            DiscordJob::routine(1),
            DiscordJob::commands(1),
        ] {
            assert!(JOB_KINDS.contains(&job_kind(&job).0), "{job:?} has a kind outside JOB_KINDS");
        }

        let exporter = InMemoryMetricExporter::default();
        let provider = SdkMeterProvider::builder()
            .with_reader(PeriodicReader::builder(exporter.clone()).build())
            .build();
        JobMetrics::new(&opentelemetry::metrics::MeterProvider::meter(&provider, "marian")).report_zeros();
        provider.force_flush().unwrap();

        let exported = exporter.get_finished_metrics().unwrap();
        let metrics: Vec<_> = exported
            .iter()
            .flat_map(|resource| resource.scope_metrics())
            .flat_map(|scope| scope.metrics())
            .collect();
        let names: Vec<_> = metrics.iter().map(|metric| metric.name()).collect();
        assert!(names.contains(&"marian.discord.queue.waiting"), "{names:?}");
        assert!(names.contains(&"marian.discord.queue.retrying"), "{names:?}");
        assert!(!names.contains(&"marian.discord.job.duration"), "a zero duration would count as a run");

        let runs = metrics.iter().find(|metric| metric.name() == "marian.discord.jobs").unwrap();
        let AggregatedMetrics::U64(MetricData::Sum(sum)) = runs.data() else {
            panic!("marian.discord.jobs is not a u64 sum");
        };
        let points: Vec<_> = sum.data_points().collect();
        assert_eq!(points.len(), JOB_KINDS.len() * JOB_OUTCOMES.len());
        assert!(points.iter().all(|point| point.value() == 0));
    }
}
