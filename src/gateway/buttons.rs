//! Buttons on Marian's posts, and the guests form one of them opens.

use diesel::prelude::*;

use diesel_async::RunQueryDsl;
use poise::serenity_prelude as serenity;
use tokio::sync::mpsc::UnboundedSender;

use crate::i18n::{self, Lang};
use crate::jobs::DiscordJob;
use crate::logic;
use crate::schema;
use crate::store::{self, Connection};
use crate::tr;

use super::follow::{redraw, server_avatar, sync_roles};
use super::publish::{purge_event_channel, push_attendance, push_routine, push_sync, tell_pages};
use super::{Data, Error, clip};
use crate::values::EventStatus;

pub(super) async fn handle_button(
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
        let mut conn = data
            .db
            .get()
            .await
            .map_err(|_| "Could not reach the database.")?;
        async fn event_key(
            conn: &mut store::Connection,
            event: uuid::Uuid,
        ) -> Result<Option<i64>, crate::error::OpError> {
            Ok(store::event_by_public(conn, &event.to_string())
                .await?
                .map(|event| event.id))
        }
        match action {
            ButtonAction::Edit { event } => event_key(&mut conn, event).await?.map(Found::Edit),
            ButtonAction::Delete { event } => event_key(&mut conn, event).await?.map(Found::Delete),
            ButtonAction::ConfirmDelete { event } => {
                event_key(&mut conn, event).await?.map(Found::ConfirmDelete)
            }
            // Opened as a form above, before the click was acknowledged.
            ButtonAction::Guests { .. } => None,
            ButtonAction::Signup { event, option } => match event_key(&mut conn, event).await? {
                Some(event_id) => {
                    crate::ids::public::option_id(&mut conn, event_id, &option.to_string())
                        .await
                        .ok()
                        .map(|option_id| Found::Signup(event_id, option_id))
                }
                None => None,
            },
            ButtonAction::Routine { run, day } => logic::run_by_public(&mut conn, run)
                .await?
                .map(|run_id| Found::Routine(run_id, day)),
            ButtonAction::Decide { run, day } => logic::run_by_public(&mut conn, run)
                .await?
                .map(|run_id| Found::Decide(run_id, day)),
        }
    };
    let Some(found) = found else {
        let lang = button_lang(data, component).await;
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
        let mut conn = data
            .db
            .get()
            .await
            .map_err(|_| "Could not reach the database.")?;
        let (membership_id, lang) = bind_clicker(&mut conn, &data.jobs, &click).await?;
        (
            logic::decide_run(&mut conn, run_id, membership_id, day, chrono::Utc::now()).await,
            lang,
        )
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
        let mut conn = data
            .db
            .get()
            .await
            .map_err(|_| "Could not reach the database.")?;
        let (membership_id, lang) = bind_clicker(&mut conn, &data.jobs, &click).await?;
        (
            logic::cast_run(&mut conn, run_id, membership_id, day).await,
            lang,
        )
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
        let mut conn = data
            .db
            .get()
            .await
            .map_err(|_| "Could not reach the database.")?;
        let (membership_id, lang) = bind_clicker(&mut conn, &data.jobs, &click).await?;
        (
            logic::signup(&mut conn, event_id, option_id, membership_id).await,
            lang,
        )
    };
    if let Err(err) = signed {
        return followup(ctx, component, &err.localized(lang)).await;
    }
    tell_pages(&data.db, event_id, true).await;
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
    let (form, lang) = {
        let mut conn = data
            .db
            .get()
            .await
            .map_err(|_| "Could not reach the database.")?;
        let (membership_id, lang) = bind_clicker(&mut conn, &data.jobs, &click).await?;
        let option_id = match store::event_by_public(&mut conn, &event.to_string()).await? {
            Some(event) => crate::ids::public::option_id(&mut conn, event.id, &option.to_string())
                .await
                .ok(),
            None => None,
        };
        let form = match option_id {
            Some(option_id) => Some(logic::guests_form(&mut conn, option_id, membership_id).await?),
            None => None,
        };
        (form, lang)
    };
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
        clip(
            &tr!(lang, "guests-form-title", option = form.option.as_str()),
            45,
        ),
    )
    .components(vec![serenity::CreateActionRow::InputText(input)]);
    component
        .create_response(&ctx.http, serenity::CreateInteractionResponse::Modal(modal))
        .await?;
    Ok(())
}

/// The "+X" form sent back: signs the person up with that many guests, or
/// changes the count when they're already on the option.
pub(super) async fn guests_from_form(
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
            serenity::ActionRowComponent::InputText(input) if input.custom_id == "guests" => {
                input.value.clone()
            }
            _ => None,
        })
        .unwrap_or_default();
    let Some(click) = click_of(ctx, form.guild_id, &form.user, form.member.as_ref()) else {
        return form_followup(ctx, form, &tr!(Lang::En, "reply-button-outside")).await;
    };
    let (outcome, lang) = {
        let mut conn = data
            .db
            .get()
            .await
            .map_err(|_| "Could not reach the database.")?;
        let (membership_id, lang) = bind_clicker(&mut conn, &data.jobs, &click).await?;
        let found = match store::event_by_public(&mut conn, &event.to_string()).await? {
            Some(event) => crate::ids::public::option_id(&mut conn, event.id, &option.to_string())
                .await
                .ok()
                .map(|option_id| (event.id, option_id)),
            None => None,
        };
        let outcome = match (found, typed.trim().parse::<i64>()) {
            (None, _) => Err(None),
            (Some(_), Err(_)) => Err(Some(crate::error::OpError::from(crate::msg!(
                "err-guests-number"
            )))),
            (Some((event_id, option_id)), Ok(guests)) => {
                logic::signup_guests(&mut conn, event_id, option_id, membership_id, guests)
                    .await
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
    tell_pages(&data.db, event_id, true).await;
    if let Err(err) = push_sync(&data.db, &data.http, &data.public_url, event_id).await {
        tracing::warn!("could not refresh the Discord post: {err}");
    }
    if let Err(err) = push_attendance(&data.db, &data.http, event_id).await {
        tracing::warn!("could not update attendance on Discord: {err}");
    }
    Ok(())
}

async fn form_followup(
    ctx: &serenity::Context,
    form: &serenity::ModalInteraction,
    text: &str,
) -> Result<(), Error> {
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
        let mut conn = data
            .db
            .get()
            .await
            .map_err(|_| "Could not reach the database.")?;
        let (membership_id, lang) = bind_clicker(&mut conn, &data.jobs, &click).await?;
        match store::event(&mut conn, event_id).await? {
            Some(event) if event.created_by == Some(membership_id) => {
                Some(event_link(&mut conn, &data.public_url, &event, lang).await?)
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
        let mut conn = data
            .db
            .get()
            .await
            .map_err(|_| "Could not reach the database.")?;
        let (membership_id, lang) = bind_clicker(&mut conn, &data.jobs, &click).await?;
        let card = match store::event(&mut conn, event_id).await? {
            None => Err(tr!(lang, "reply-event-missing")),
            Some(event) if event.created_by != Some(membership_id) => Ok(None),
            Some(event)
                if !matches!(
                    event.status,
                    EventStatus::Draft
                        | EventStatus::Scheduled
                        | EventStatus::Failed
                        | EventStatus::Posted
                ) =>
            {
                Err(tr!(lang, "err-event-closed"))
            }
            Some(event) => {
                let server = store::discord_server_id(&mut conn, event.guild_id)
                    .await
                    .unwrap_or_default();
                let url = logic::web_event_url(
                    &data.public_url,
                    Some(server.as_str()).filter(|id| !id.is_empty()),
                    event.public_id,
                );
                Ok(Some((
                    logic::delete_confirm_body(
                        lang,
                        &event.title,
                        event.starts_at,
                        url.as_deref(),
                        event.status == EventStatus::Posted,
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
        let mut conn = data
            .db
            .get()
            .await
            .map_err(|_| "Could not reach the database.")?;
        let (membership_id, lang) = bind_clicker(&mut conn, &data.jobs, &click).await?;
        match store::event(&mut conn, event_id).await? {
            None => Err(tr!(lang, "reply-event-missing")),
            Some(event) if event.created_by != Some(membership_id) => Ok(None),
            Some(event) => {
                let plan = logic::event_channel_purge(&mut conn, event.id)
                    .await
                    .map_err(|err| err.localized(lang))?;
                let channel_discord = store::channel(&mut conn, event.channel_id)
                    .await?
                    .and_then(|channel| channel.discord_id);
                let message_id = event.discord_message_id.clone();
                match logic::delete_event(&mut conn, event_id, membership_id).await {
                    Ok(notice) => {
                        crate::live::event(event.guild_id, event.public_id);
                        Ok(Some((
                            plan,
                            channel_discord,
                            message_id,
                            notice.render(lang),
                        )))
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
        } else if let Ok(mut conn) = data.db.get().await {
            diesel::update(schema::events::table.filter(schema::events::id.eq(event_id)))
                .set(schema::events::discord_message_id.eq(None::<String>))
                .execute(&mut *conn)
                .await?;
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
    avatar: Option<String>,
    manage: Option<bool>,
    roles: Option<Vec<(String, String, String)>>,
}

fn click_context(
    ctx: &serenity::Context,
    component: &serenity::ComponentInteraction,
) -> Option<Click> {
    click_of(
        ctx,
        component.guild_id,
        &component.user,
        component.member.as_ref(),
    )
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
        avatar: server_avatar(
            guild_id,
            user,
            member.and_then(|member| member.avatar.as_ref()),
        ),
        manage,
        roles,
    })
}

/// The member who pressed a button, and their server's language.
async fn bind_clicker(
    conn: &mut Connection,
    jobs: &UnboundedSender<DiscordJob>,
    click: &Click,
) -> Result<(i64, Lang), Error> {
    let guild = logic::ensure_discord_guild(conn, &click.guild_discord, &click.guild_name).await?;
    let (user, renamed) =
        logic::ensure_discord_user(conn, guild.id, &click.user_discord, &click.display).await?;
    redraw(jobs, renamed);
    logic::follow_discord_avatar(conn, &user, click.avatar.as_deref()).await?;
    if let Some(manage) = click.manage {
        logic::set_server_manager(conn, user.id, manage).await?;
    }
    if let Some(roles) = &click.roles {
        sync_roles(conn, guild.id, user.id, roles).await?;
    }
    Ok((user.id, i18n::lang_of(conn, guild.id).await))
}

/// The language of the server a button is in; English outside one.
async fn button_lang(data: &Data, component: &serenity::ComponentInteraction) -> Lang {
    let Some(guild_id) = component.guild_id else {
        return Lang::En;
    };
    let Ok(mut conn) = data.db.get().await else {
        return Lang::En;
    };
    match store::guild_by_discord(&mut conn, &guild_id.get().to_string()).await {
        Ok(Some(guild)) => i18n::lang_of(&mut conn, guild.id).await,
        _ => Lang::En,
    }
}

async fn event_link(
    conn: &mut Connection,
    public_url: &str,
    event: &store::Event,
    lang: Lang,
) -> Result<String, Error> {
    let server = store::discord_server_id(conn, event.guild_id)
        .await
        .unwrap_or_default();
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
    Signup {
        event: uuid::Uuid,
        option: uuid::Uuid,
    },
    /// "+X": a form asking how many guests, and the same id on the form.
    Guests {
        event: uuid::Uuid,
        option: uuid::Uuid,
    },
    Edit {
        event: uuid::Uuid,
    },
    Delete {
        event: uuid::Uuid,
    },
    ConfirmDelete {
        event: uuid::Uuid,
    },
    Routine {
        run: uuid::Uuid,
        day: u8,
    },
    /// An approval's answer; no day skips the week.
    Decide {
        run: uuid::Uuid,
        day: Option<u8>,
    },
}

/// A button on an event's post. Its id names the event, and the option, by
/// public id: anyone can read a button's id in Discord.
pub(super) fn action_button(
    event: uuid::Uuid,
    button: &logic::DiscordButton,
) -> serenity::CreateButton {
    let custom_id = match &button.role {
        logic::ButtonRole::Signup(option) => format!("marian:{event}:{option}"),
        logic::ButtonRole::Guests(option) => format!("marian:{event}:{option}:g"),
        logic::ButtonRole::Web(url) => {
            return serenity::CreateButton::new_link(url).label(clip(&button.label, 80));
        }
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

#[cfg(test)]
mod tests {
    use super::{ButtonAction, parse_button};

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
        assert!(
            format!("marian:{event}:{option}:g").len() <= 100,
            "Discord's limit on a button id"
        );
        assert_eq!(
            parse_button(&format!("marian:{event}:edit")),
            Some(ButtonAction::Edit { event })
        );
        assert_eq!(
            parse_button(&format!("marian:{event}:confirm-delete")),
            Some(ButtonAction::ConfirmDelete { event })
        );
        assert_eq!(
            parse_button(&format!("marian:w:{option}:3")),
            Some(ButtonAction::Routine {
                run: option,
                day: 3
            })
        );
        // Buttons posted before carry row numbers. They do nothing now.
        assert_eq!(parse_button("marian:12:34"), None);
        assert_eq!(parse_button("marian:12:edit"), None);
        assert_eq!(parse_button("marian:w:5:3"), None);
        assert_eq!(
            parse_button(&format!("marian:a:{option}:4")),
            Some(ButtonAction::Decide {
                run: option,
                day: Some(4)
            })
        );
        assert_eq!(
            parse_button(&format!("marian:a:{option}:skip")),
            Some(ButtonAction::Decide {
                run: option,
                day: None
            })
        );
        assert!(
            format!("marian:{event}:{option}").len() <= 100,
            "Discord's limit on a button id"
        );
    }
}
