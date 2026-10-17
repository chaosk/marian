//! Outbound Discord gateway. The cluster does not need a public interactions URL.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use diesel::prelude::*;
use poise::serenity_prelude as serenity;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing::Instrument;

use crate::commands;
use crate::jobs::DiscordJob;
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
    tokio::spawn(async move {
        job_loop(data_db, jobs, retry, data_http, jobs_url).await;
    });
    tokio::spawn(async move {
        loop {
            if let Err(err) = connect(
                &token,
                db.clone(),
                http.clone(),
                public_url.clone(),
                status.clone(),
            )
            .await
            {
                tracing::warn!("Discord gateway stopped: {err}");
            }
            if let Ok(mut slot) = http.lock() {
                *slot = None;
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
    while let Some(job) = jobs.recv().await {
        let ready = http.lock().map(|slot| slot.is_some()).unwrap_or(false);
        if !ready {
            requeue(retry.clone(), job);
            continue;
        }
        let (kind, id) = job_kind(&job);
        let span = tracing::info_span!("discord_job", job = kind, id);
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
            }
        }
        .instrument(span)
        .await;
        if let Err(err) = result {
            tracing::warn!("Discord job failed: {err}");
            requeue(retry.clone(), job);
        }
    }
}

fn job_kind(job: &DiscordJob) -> (&'static str, i64) {
    match job {
        DiscordJob::Sync { event_id, .. } => ("sync", *event_id),
        DiscordJob::Remind { event_id, .. } => ("remind", *event_id),
        DiscordJob::Attend { event_id, .. } => ("attend", *event_id),
        DiscordJob::Routine { run_id, .. } => ("routine", *run_id),
    }
}

fn requeue(retry: UnboundedSender<DiscordJob>, job: DiscordJob) {
    if let Some(next) = job.bump() {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let _ = retry.send(next);
        });
    }
}

async fn connect(
    token: &str,
    db: store::Db,
    http: Arc<Mutex<Option<Arc<serenity::Http>>>>,
    public_url: String,
    status: String,
) -> Result<(), Error> {
    let presence = presence_line(&status);
    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![
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
            ],
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
                })
            })
        })
        .build();

    let mut builder = serenity::ClientBuilder::new(
        token,
        serenity::GatewayIntents::GUILDS | serenity::GatewayIntents::DIRECT_MESSAGES,
    )
    .status(serenity::model::user::OnlineStatus::Online);
    if let Some(line) = presence {
        builder = builder.activity(serenity::gateway::ActivityData::custom(line));
    }
    let mut client = builder.framework(framework).await?;
    client.start().await?;
    Ok(())
}

async fn on_event(
    ctx: &serenity::Context,
    event: &serenity::FullEvent,
    framework: poise::FrameworkContext<'_, Data, Error>,
    data: &Data,
) -> Result<(), Error> {
    match event {
        serenity::FullEvent::Ready { data_about_bot } => {
            if let Ok(mut slot) = data.http.lock() {
                *slot = Some(ctx.http.clone());
            }
            for guild in &data_about_bot.guilds {
                register_guild(ctx, framework, data, guild.id).await;
            }
        }
        serenity::FullEvent::GuildCreate { guild, .. } => {
            if let Err(err) = remember_guild(data, guild) {
                tracing::warn!("could not store {}: {err}", guild.name);
            }
            register_guild(ctx, framework, data, guild.id).await;
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
            if let serenity::Interaction::Component(component) = interaction {
                if let Err(err) = handle_button(ctx, component, data).await {
                    tracing::warn!("button failed: {err}");
                }
            }
        }
        _ => {}
    }
    Ok(())
}

async fn register_guild(
    ctx: &serenity::Context,
    framework: poise::FrameworkContext<'_, Data, Error>,
    data: &Data,
    guild_id: serenity::GuildId,
) {
    let fresh = data
        .registered
        .lock()
        .map(|mut seen| seen.insert(guild_id.get()))
        .unwrap_or(true);
    if !fresh {
        return;
    }
    if let Err(err) =
        poise::builtins::register_in_guild(ctx, &framework.options().commands, guild_id).await
    {
        if let Ok(mut seen) = data.registered.lock() {
            seen.remove(&guild_id.get());
        }
        tracing::warn!("could not register commands in {guild_id}: {err}");
    }
}

fn remember_guild(data: &Data, guild: &serenity::Guild) -> Result<(), Error> {
    let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
    let stored = logic::ensure_discord_guild(&mut conn, &guild.id.get().to_string(), &guild.name)?;
    for channel in guild.channels.values() {
        if !matches!(
            channel.kind,
            serenity::ChannelType::Text | serenity::ChannelType::News
        ) {
            continue;
        }
        logic::ensure_discord_channel(
            &mut conn,
            stored.id,
            &channel.id.get().to_string(),
            &channel.name,
        )?;
    }
    for role in guild.roles.values() {
        if role.name == "@everyone" {
            continue;
        }
        ensure_role(
            &mut conn,
            stored.id,
            &role.name,
            &role.colour.hex(),
            &role.id.get().to_string(),
        )?;
    }
    Ok(())
}

#[derive(Insertable)]
#[diesel(table_name = schema::roles)]
struct NewRole<'a> {
    guild_id: i64,
    name: &'a str,
    color: &'a str,
    discord_id: &'a str,
}

fn ensure_role(
    conn: &mut Connection,
    guild_id: i64,
    name: &str,
    colour: &str,
    discord_id: &str,
) -> Result<(), Error> {
    let color = if colour.is_empty() {
        "#99aab5".to_string()
    } else if colour.starts_with('#') {
        colour.to_string()
    } else {
        format!("#{colour}")
    };
    if let Some(role) = store::role_by_name(conn, guild_id, name)? {
        diesel::update(schema::roles::table.filter(schema::roles::id.eq(role.id)))
            .set((
                schema::roles::discord_id.eq(discord_id),
                schema::roles::color.eq(&color),
            ))
            .execute(conn)?;
        return Ok(());
    }
    diesel::insert_into(schema::roles::table)
        .values(&NewRole {
            guild_id,
            name,
            color: &color,
            discord_id,
        })
        .execute(conn)?;
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
    component
        .create_response(&ctx.http, serenity::CreateInteractionResponse::Acknowledge)
        .await?;
    match action {
        ButtonAction::Edit { event_id } => edit_from_button(ctx, component, data, event_id).await,
        ButtonAction::Delete { event_id } => ask_delete(ctx, component, data, event_id).await,
        ButtonAction::ConfirmDelete { event_id } => {
            delete_from_button(ctx, component, data, event_id).await
        }
        ButtonAction::Signup {
            event_id,
            option_id,
        } => signup_from_button(ctx, component, data, event_id, option_id).await,
        ButtonAction::Routine { run_id, day } => {
            routine_from_button(ctx, component, data, run_id, day).await
        }
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
        followup(ctx, component, "Use that button inside a server.").await?;
        return Ok(());
    };
    let saved = {
        let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
        let membership_id = bind_clicker(&mut conn, &click)?;
        logic::cast_run(&mut conn, run_id, membership_id, day)
    };
    if let Err(err) = saved {
        return followup(ctx, component, &err.to_string()).await;
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
        followup(ctx, component, "Use that button inside a server.").await?;
        return Ok(());
    };
    let signed = {
        let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
        let membership_id = bind_clicker(&mut conn, &click)?;
        logic::signup(&mut conn, event_id, option_id, membership_id)
    };
    if let Err(err) = signed {
        return followup(ctx, component, &err.to_string()).await;
    }
    if let Err(err) = push_sync(&data.db, &data.http, &data.public_url, event_id).await {
        tracing::warn!("could not refresh the Discord post: {err}");
    }
    if let Err(err) = push_attendance(&data.db, &data.http, event_id).await {
        tracing::warn!("could not update attendance on Discord: {err}");
    }
    Ok(())
}

async fn edit_from_button(
    ctx: &serenity::Context,
    component: &serenity::ComponentInteraction,
    data: &Data,
    event_id: i64,
) -> Result<(), Error> {
    let Some(click) = click_context(ctx, component) else {
        followup(ctx, component, "Use that button inside a server.").await?;
        return Ok(());
    };
    let text = {
        let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
        let membership_id = bind_clicker(&mut conn, &click)?;
        match store::event(&mut conn, event_id)? {
            Some(event) if event.created_by == Some(membership_id) => {
                Some(event_link(&mut conn, &data.public_url, &event)?)
            }
            Some(_) => None,
            None => Some("That event isn't here.".to_string()),
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
        followup(ctx, component, "Use that button inside a server.").await?;
        return Ok(());
    };
    let card = {
        let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
        let membership_id = bind_clicker(&mut conn, &click)?;
        match store::event(&mut conn, event_id)? {
            None => Err("That event isn't here.".to_string()),
            Some(event) if event.created_by != Some(membership_id) => Ok(None),
            Some(event)
                if !matches!(
                    event.status.as_str(),
                    "draft" | "scheduled" | "failed" | "posted"
                ) =>
            {
                Err("That event is already closed.".to_string())
            }
            Some(event) => {
                let server = store::discord_server_id(&mut conn, event.guild_id).unwrap_or_default();
                let url = logic::web_event_url(
                    &data.public_url,
                    Some(server.as_str()).filter(|id| !id.is_empty()),
                    &event.public_id,
                );
                Ok(Some(logic::delete_confirm_body(
                    &event.title,
                    event.starts_at,
                    url.as_deref(),
                    event.status == "posted",
                )))
            }
        }
    };
    let body = match card {
        Ok(Some(body)) => body,
        Ok(None) => return Ok(()),
        Err(text) => return followup(ctx, component, &text).await,
    };
    let embed = serenity::CreateEmbed::new()
        .title("Delete event")
        .description(clip(&body, 4000))
        .color(serenity::Colour::new(0xF1C40F));
    let delete = serenity::CreateButton::new(format!("marian:{event_id}:confirm-delete"))
        .style(serenity::ButtonStyle::Danger)
        .label("Delete");
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
        followup(ctx, component, "Use that button inside a server.").await?;
        return Ok(());
    };
    let removed = {
        let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
        let membership_id = bind_clicker(&mut conn, &click)?;
        match store::event(&mut conn, event_id)? {
            None => Err("That event isn't here.".to_string()),
            Some(event) if event.created_by != Some(membership_id) => Ok(None),
            Some(event) => {
                let channel_discord = store::channel(&mut conn, event.channel_id)?
                    .and_then(|channel| channel.discord_id);
                let message_id = event.discord_message_id.clone();
                match logic::delete_event(&mut conn, event_id, membership_id) {
                    Ok(notice) => Ok(Some((channel_discord, message_id, notice))),
                    Err(err) => Err(err.to_string()),
                }
            }
        }
    };
    let (channel_discord, message_id, notice) = match removed {
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
    let guild_id = component.guild_id?;
    let guild_name = ctx
        .cache
        .guild(guild_id)
        .map(|guild| guild.name.clone())
        .unwrap_or_default();
    let display = component
        .user
        .global_name
        .clone()
        .unwrap_or_else(|| component.user.name.clone());
    let manage = component.member.as_ref().and_then(|member| {
        member
            .permissions
            .map(|permissions| permissions.manage_guild() || permissions.administrator())
    });
    let roles = component.member.as_ref().map(|member| {
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
        user_discord: component.user.id.get().to_string(),
        display,
        manage,
        roles,
    })
}

fn bind_clicker(conn: &mut Connection, click: &Click) -> Result<i64, Error> {
    let guild = logic::ensure_discord_guild(conn, &click.guild_discord, &click.guild_name)?;
    let user = logic::ensure_discord_user(conn, guild.id, &click.user_discord, &click.display)?;
    if let Some(manage) = click.manage {
        logic::set_organizer(conn, user.id, manage)?;
    }
    if let Some(roles) = &click.roles {
        sync_roles(conn, guild.id, user.id, roles)?;
    }
    Ok(user.id)
}

fn event_link(
    conn: &mut Connection,
    public_url: &str,
    event: &store::Event,
) -> Result<String, Error> {
    let server = store::discord_server_id(conn, event.guild_id).unwrap_or_default();
    let path = if server.is_empty() {
        format!("/events/{}", event.public_id)
    } else {
        format!("/servers/{server}/events/{}", event.public_id)
    };
    let base = public_url.trim().trim_end_matches('/');
    if base.is_empty() {
        Ok(format!("Open {path}."))
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
        ensure_role(conn, guild_id, name, colour, discord_id)?;
        let Some(role) = store::role_by_name(conn, guild_id, name)? else {
            continue;
        };
        role_ids.push(role.id);
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

enum ButtonAction {
    Signup { event_id: i64, option_id: i64 },
    Edit { event_id: i64 },
    Delete { event_id: i64 },
    ConfirmDelete { event_id: i64 },
    Routine { run_id: i64, day: u8 },
}

fn action_button(event_id: i64, button: &logic::DiscordButton) -> serenity::CreateButton {
    let custom_id = match button.role {
        logic::ButtonRole::Signup(option_id) => format!("marian:{event_id}:{option_id}"),
        logic::ButtonRole::Edit => format!("marian:{event_id}:edit"),
        logic::ButtonRole::Delete => format!("marian:{event_id}:delete"),
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

fn parse_button(custom_id: &str) -> Option<ButtonAction> {
    let mut parts = custom_id.split(':');
    if parts.next() != Some("marian") {
        return None;
    }
    let head = parts.next()?;
    if head == "w" {
        let run_id = parts.next()?.parse().ok()?;
        let day = parts.next()?.parse().ok()?;
        return Some(ButtonAction::Routine { run_id, day });
    }
    let event_id = head.parse().ok()?;
    match parts.next()? {
        "edit" => Some(ButtonAction::Edit { event_id }),
        "delete" => Some(ButtonAction::Delete { event_id }),
        "confirm-delete" => Some(ButtonAction::ConfirmDelete { event_id }),
        option => Some(ButtonAction::Signup {
            event_id,
            option_id: option.parse().ok()?,
        }),
    }
}

async fn push_sync(
    db: &store::Db,
    http: &Arc<Mutex<Option<Arc<serenity::Http>>>>,
    public_url: &str,
    event_id: i64,
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
        buttons.push(action_button(event_id, button));
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
    let mentions = event_mentions(view.mention_everyone, &view.mention_roles);
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
    if let Some(message) = sent {
        let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
        logic::remember_discord_message(&mut conn, event_id, &message.id.get().to_string())?;
    }
    Ok(())
}

async fn push_routine(
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
    let sent = upsert_message(
        &http,
        channel_id,
        post.message_id.as_deref(),
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
        built.push(
            serenity::CreateButton::new(button.custom_id.clone())
                .style(serenity::ButtonStyle::Secondary)
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
    embed: serenity::CreateEmbed,
    rows: Vec<serenity::CreateActionRow>,
) -> Result<Option<serenity::Message>, Error> {
    if let Some(message_id) = message_id.and_then(|value| value.parse().ok()) {
        match channel_id
            .edit_message(
                http,
                serenity::MessageId::new(message_id),
                serenity::EditMessage::new()
                    .embed(embed.clone())
                    .components(rows.clone()),
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
            serenity::CreateMessage::new().embed(embed).components(rows),
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
    if work.thread_add.is_empty()
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
    if !work.thread_add.is_empty() || !work.thread_remove.is_empty() {
        if work.thread_discord_id.is_none() && work.thread_add.is_empty() {
            let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
            for membership_id in &work.thread_remove {
                store::clear_applied(&mut conn, event_id, "thread", membership_id)?;
            }
        } else {
            let thread_id = open_thread(db, &http, event_id, &work).await?;
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
            serenity::CreateThread::new(name)
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
    store::set_thread_discord_id(&mut conn, event_id, &thread_id.to_string())?;
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
            channel_id,
            thread_id: store::thread_discord_id(&mut conn, event_id)?,
            in_thread: event.thread_enabled && settings.thread_reminders,
            message_id,
            thread_name: logic::event_thread_name(&event.title, event.starts_at, &timezone),
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
}

async fn remember_actor(ctx: &Context<'_>) -> Result<Option<Actor>, Error> {
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(None);
    };
    let author = ctx.author();
    let display = author
        .global_name
        .clone()
        .unwrap_or_else(|| author.name.clone());
    let discord_user = author.id.get().to_string();
    let guild_name = ctx
        .guild_id()
        .and_then(|id| ctx.cache().guild(id).map(|guild| guild.name.clone()))
        .unwrap_or_default();
    let member = ctx.author_member().await;
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
    let user = logic::ensure_discord_user(&mut conn, guild.id, &discord_user, &display)?;
    if let Some(manage) = manage {
        logic::set_organizer(&mut conn, user.id, manage)?;
    }
    if let Some(roles) = &roles {
        sync_roles(&mut conn, guild.id, user.id, roles)?;
    }
    Ok(Some(Actor {
        guild_id: guild.id,
        membership_id: user.id,
        discord_user,
    }))
}

async fn desk(ctx: Context<'_>, line: &str) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(actor) = remember_actor(&ctx).await? else {
        ctx.say("Use Marian inside a server.").await?;
        return Ok(());
    };
    let data = ctx.data();
    let (reply, refresh) = {
        let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
        let reply = crate::commands::execute(&mut conn, actor.guild_id, actor.membership_id, line);
        let refresh = crate::serve::events_to_refresh(&mut conn, line, &reply);
        (reply, refresh)
    };
    for event_id in refresh {
        if let Err(err) = push_sync(&data.db, &data.http, &data.public_url, event_id).await {
            tracing::warn!("could not refresh event {event_id}: {err}");
        }
        if let Err(err) = push_attendance(&data.db, &data.http, event_id).await {
            tracing::warn!("could not update attendance for event {event_id}: {err}");
        }
    }
    ctx.say(clip(&reply, 1800)).await?;
    Ok(())
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

fn dm_blocked(lead: &str, public_url: &str, server: Option<u64>) -> String {
    let base = public_url.trim().trim_end_matches('/');
    let web = match (base.is_empty(), server) {
        (false, Some(server)) => {
            format!(" You can always create the event at {base}/servers/{server}/events/new.")
        }
        (false, None) => format!(" You can always create the event at {base}."),
        _ => " You can always create the event on the web.".to_string(),
    };
    format!("{lead} Allow direct messages from server members, then run /event again.{web}")
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
        ctx.say("Use /event inside a server.").await?;
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
        )
    };
    let question = match started {
        Ok(question) => question,
        Err(err) => {
            ctx.say(clip(&err.to_string(), 1800)).await?;
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
                    "I couldn't send the DM.",
                    &ctx.data().public_url,
                    ctx.guild_id().map(|id| id.get()),
                ))
                .await?;
                return Ok(());
            }
            ctx.say("I sent you a DM. Answer there and I'll post the event.")
                .await?;
        }
        Err(err) => {
            {
                let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
                commands::cancel_event_dm(&mut conn, &actor.discord_user)?;
            }
            tracing::warn!("could not open a DM: {err}");
            ctx.say(dm_blocked(
                "I couldn't open a DM.",
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
    commands::channel_choices(&mut conn, guild.id, partial)
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
        Err(err) => err.to_string(),
    };
    let refresh = {
        let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
        let _ = logic::record_command(&mut conn, actor.guild_id, actor.membership_id, label, &text);
        crate::serve::events_to_refresh(&mut conn, label, &text)
    };
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

#[derive(poise::ChoiceParameter, Copy, Clone)]
enum ChannelKind {
    #[name = "Text"]
    Text,
    #[name = "Event channel"]
    Event,
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
        ctx.say("Use Marian inside a server.").await?;
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
        ctx.say("Use Marian inside a server.").await?;
        return Ok(());
    };
    let outcome = {
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::set_reminders(&mut conn, actor.membership_id, enabled)
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
) -> Result<(), Error> {
    run_response(ctx, "add", &event, &user, &option).await
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
    run_response(ctx, "remove", &event, &user, &option).await
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
) -> Result<(), Error> {
    run_response(ctx, "move", &event, &user, &option).await
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
    commands::response_events(&mut conn, guild.id, partial, chrono::Utc::now())
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
    commands::response_options(&mut conn, guild.id, &event, partial)
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
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(actor) = remember_actor(&ctx).await? else {
        ctx.say("Use Marian inside a server.").await?;
        return Ok(());
    };
    let name = crate::auth::discord_display_name(user.global_name.as_deref(), &user.name);
    let data = ctx.data();
    let outcome = {
        let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
        let target = logic::ensure_discord_user(
            &mut conn,
            actor.guild_id,
            &user.id.get().to_string(),
            &name,
        )?;
        commands::roster_action(&mut conn, actor.membership_id, event, target.id, option, action)
    };
    match outcome {
        Ok((reply, event_id)) => {
            if let Err(err) = push_sync(&data.db, &data.http, &data.public_url, event_id).await {
                tracing::warn!("could not refresh event {event_id}: {err}");
            }
            if let Err(err) = push_attendance(&data.db, &data.http, event_id).await {
                tracing::warn!("could not update attendance for event {event_id}: {err}");
            }
            ctx.say(clip(&reply, 1800)).await?;
        }
        Err(err) => {
            ctx.say(clip(&err.to_string(), 1800)).await?;
        }
    }
    Ok(())
}

async fn setting(ctx: Context<'_>, label: &str, change: commands::Setting) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(actor) = remember_actor(&ctx).await? else {
        ctx.say("Use Marian inside a server.").await?;
        return Ok(());
    };
    let outcome = {
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::apply_setting(&mut conn, actor.guild_id, actor.membership_id, change)
    };
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
        "settings_digest",
        "settings_post_lead",
        "settings_role"
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
    description_localized("en-US", "Clear an event channel when the event archives")
)]
async fn settings_purge(
    ctx: Context<'_>,
    #[description = "Clear the event channel on archive"] enabled: bool,
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
        ctx.say("Use Marian inside a server.").await?;
        return Ok(());
    };
    let outcome = {
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::library_text(&mut conn, actor.guild_id)
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
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(actor) = remember_actor(&ctx).await? else {
        ctx.say("Use Marian inside a server.").await?;
        return Ok(());
    };
    let emoji = emoji.unwrap_or_else(|| "•".into());
    let outcome = {
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::add_library_option(&mut conn, actor.guild_id, actor.membership_id, &name, &emoji)
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
        ctx.say("Use Marian inside a server.").await?;
        return Ok(());
    };
    let outcome = {
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::remove_library_option(&mut conn, actor.guild_id, actor.membership_id, &name)
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
        ctx.say("Use Marian inside a server.").await?;
        return Ok(());
    };
    let outcome = {
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::preset_text(&mut conn, actor.guild_id)
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
        ctx.say("Use Marian inside a server.").await?;
        return Ok(());
    };
    let outcome = {
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::add_preset(&mut conn, actor.guild_id, actor.membership_id, &name, &options)
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
        ctx.say("Use Marian inside a server.").await?;
        return Ok(());
    };
    let outcome = {
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::remove_preset(&mut conn, actor.guild_id, actor.membership_id, &name)
    };
    deliver(ctx, &actor, "/signup_presets remove", outcome).await
}

#[poise::command(
    slash_command,
    guild_only,
    description_localized("en-US", "Create a channel")
)]
async fn channel(
    ctx: Context<'_>,
    #[description = "Channel name"] name: String,
    #[description = "Text, or an event channel"] kind: Option<ChannelKind>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let Some(actor) = remember_actor(&ctx).await? else {
        ctx.say("Use Marian inside a server.").await?;
        return Ok(());
    };
    let kind = match kind {
        Some(ChannelKind::Event) => "event",
        _ => "text",
    };
    let outcome = {
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::create_named_channel(&mut conn, actor.guild_id, actor.membership_id, &name, kind)
    };
    deliver(ctx, &actor, "/channel", outcome).await
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
        ctx.say("Use Marian inside a server.").await?;
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
            .content("Click the button below to connect Google Calendar.")
            .components(vec![serenity::CreateActionRow::Buttons(vec![
                serenity::CreateButton::new_link(url).label("Sign in with Google"),
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
        ctx.say("Use Marian inside a server.").await?;
        return Ok(());
    };
    let outcome = {
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::google_auto_sync(&mut conn, actor.guild_id, actor.membership_id, enabled)
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
        ctx.say("Use Marian inside a server.").await?;
        return Ok(());
    };
    let label = format!("/google sync {event}");
    let outcome = {
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::google_sync(&mut conn, actor.guild_id, actor.membership_id, &event)
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
        ctx.say("Use Marian inside a server.").await?;
        return Ok(());
    };
    let outcome = {
        let mut conn = ctx.data().db.get().map_err(|_| "Could not reach the database.")?;
        commands::google_revoke(&mut conn, actor.guild_id, actor.membership_id)
    };
    deliver(ctx, &actor, "/google revoke", outcome).await
}

#[poise::command(
    slash_command,
    guild_only,
    description_localized("en-US", "Events stay in chronological order")
)]
async fn sort(ctx: Context<'_>) -> Result<(), Error> {
    desk(ctx, "/sort").await
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
    let base = ctx.data().public_url.trim().trim_end_matches('/');
    let more = if base.is_empty() {
        "The other commands are explained on the Help page.".to_string()
    } else {
        format!("The other commands are explained at {base}/help.")
    };
    ctx.say(format!(
        "/event starts an event. Marian asks in a DM where to post it, the title, the description, when it starts, when it ends, and how often it repeats.\n\n{more}"
    ))
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{dm_blocked, embed_parts, presence_line};
    use crate::commands::DmReply;

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
        });
        assert_eq!(question.0.as_deref(), Some("What should it be called?"));
        assert!(question.1.is_empty());

        let when = embed_parts(&DmReply {
            title: None,
            body: "When does **Shore** start?\n\nFor example, \"tomorrow at 7pm\".".into(),
        });
        assert!(when.0.is_none());
        assert!(when.1.contains("**Shore**"));
    }

    #[test]
    fn a_blocked_dm_offers_the_web_page() {
        let noted = dm_blocked("I couldn't send the DM.", "https://marian.example.com/", Some(4242));
        assert_eq!(
            noted,
            "I couldn't send the DM. Allow direct messages from server members, then run /event again. You can always create the event at https://marian.example.com/servers/4242/events/new."
        );
        let plain = dm_blocked("I couldn't open a DM.", "  ", None);
        assert_eq!(
            plain,
            "I couldn't open a DM. Allow direct messages from server members, then run /event again. You can always create the event on the web."
        );
    }
}
