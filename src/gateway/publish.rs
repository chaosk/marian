//! What Marian puts on Discord for an event or a routine: the post, its
//! thread, attendance, reminders and the routine poll.

use std::sync::{Arc, Mutex};

use poise::serenity_prelude as serenity;

use crate::i18n::{self, Lang};
use crate::logic;
use crate::store;

use super::buttons::action_button;
use super::{clip, Error};
use crate::values::{AccessKind, EventStatus};

pub(super) async fn push_sync(
    db: &store::Db,
    http: &Arc<Mutex<Option<Arc<serenity::Http>>>>,
    public_url: &str,
    event_id: i64,
) -> Result<(), Error> {
    post_event(db, http, public_url, event_id, true).await
}

/// Tells open pages about a change made from Discord to one event: to who is
/// signed up, or to the event itself.
pub(super) fn tell_pages(db: &store::Db, event_id: i64, signups: bool) {
    let found = store::blocking(|| {
        db.get()
            .ok()
            .and_then(|mut conn| store::event(&mut conn, event_id).ok())
    });
    if let Some(Some(event)) = found {
        if signups {
            crate::live::signups(event.guild_id, event.public_id);
        } else {
            crate::live::event(event.guild_id, event.public_id);
        }
    }
}

pub(super) async fn post_event(
    db: &store::Db,
    http: &Arc<Mutex<Option<Arc<serenity::Http>>>>,
    public_url: &str,
    event_id: i64,
    notify: bool,
) -> Result<(), Error> {
    let view = store::blocking(|| -> Result<_, Error> {
        let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
        Ok(logic::discord_view(&mut conn, event_id, public_url)?)
    })?;
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
    let cancelled = store::event(&mut conn, event_id)?
        .is_some_and(|event| event.status == EventStatus::Cancelled);
    let plan = if sent.is_some() || cancelled {
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

pub(super) async fn purge_event_channel(
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
pub(super) async fn push_routine(
    db: &store::Db,
    http: &Arc<Mutex<Option<Arc<serenity::Http>>>>,
    public_url: &str,
    run_id: i64,
) -> Result<(), Error> {
    push_poll(db, http, run_id).await?;
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
    run_id: i64,
) -> Result<(), Error> {
    let post = {
        let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
        logic::routine_post(&mut conn, run_id)?
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
                .edit_message(
                    &http,
                    serenity::MessageId::new(sent),
                    serenity::EditMessage::new().embed(embed.clone()),
                )
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

pub(super) async fn push_attendance(
    db: &store::Db,
    http: &Arc<Mutex<Option<Arc<serenity::Http>>>>,
    event_id: i64,
) -> Result<(), Error> {
    let attendance = store::blocking(|| -> Result<_, Error> {
        let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
        Ok(logic::attendance(&mut conn, event_id)?)
    })?;
    let work = match attendance {
        logic::Attendance::Idle => return Ok(()),
        logic::Attendance::Waiting => {
            return Err("the Discord post is not up yet".into());
        }
        logic::Attendance::Ready(work) => work,
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
                store::clear_applied(&mut conn, event_id, AccessKind::Thread, membership_id)?;
            }
        } else {
            let thread_id = match opened {
                Some(id) => id,
                None => open_thread(db, &http, event_id, &work).await?,
            };
            for membership_id in &work.thread_add {
                join_thread(&http, thread_id, membership_id).await?;
                let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
                store::mark_applied(&mut conn, event_id, AccessKind::Thread, membership_id)?;
            }
            for membership_id in &work.thread_remove {
                leave_thread(&http, thread_id, membership_id).await?;
                let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
                store::clear_applied(&mut conn, event_id, AccessKind::Thread, membership_id)?;
            }
        }
    }
    if work.role_discord_id.is_none() && work.role_add.is_empty() {
        let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
        for membership_id in &work.role_remove {
            store::clear_applied(&mut conn, event_id, AccessKind::Role, membership_id)?;
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
            store::mark_applied(&mut conn, event_id, AccessKind::Role, membership_id)?;
        }
        for membership_id in &work.role_remove {
            let user = serenity::UserId::new(membership_id.parse()?);
            match http.remove_member_role(guild_id, user, role_id, None).await {
                Ok(()) => {}
                Err(err) if already_applied(&err) => {}
                Err(err) => return Err(err.into()),
            }
            let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
            store::clear_applied(&mut conn, event_id, AccessKind::Role, membership_id)?;
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

async fn join_thread(
    http: &serenity::Http,
    thread_id: u64,
    membership_id: &str,
) -> Result<(), Error> {
    let thread = serenity::ChannelId::new(thread_id);
    let user = serenity::UserId::new(membership_id.parse()?);
    match thread.add_thread_member(http, user).await {
        Ok(()) => Ok(()),
        Err(err) if already_applied(&err) => Ok(()),
        Err(err) => Err(err.into()),
    }
}

async fn leave_thread(
    http: &serenity::Http,
    thread_id: u64,
    membership_id: &str,
) -> Result<(), Error> {
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

pub(super) async fn push_remind(
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
        let link = link
            .or_else(|| logic::web_event_url(public_url, guild_id.as_deref(), &event.public_id));
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
