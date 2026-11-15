//! Slash commands: registering them in each server's language, and what they
//! share.

pub(super) mod channel;
mod event;
mod google;
mod response;
mod settings;
mod signups;

use poise::serenity_prelude as serenity;

use crate::commands;
use crate::i18n::{self, Lang};
use crate::logic;
use crate::store;

use super::follow::{redraw, server_avatar, sync_roles};
use super::publish::{push_attendance, push_sync};
use super::{clip, Context, Data, Error};

pub(super) fn all_commands() -> Vec<poise::Command<Data, Error>> {
    vec![
        event::event(),
        event::edit(),
        event::reminders(),
        response::response(),
        settings::settings(),
        signups::signup_options(),
        signups::signup_presets(),
        channel::channel(),
        google::google(),
        channel::sort(),
        event::help(),
    ]
}

/// Registers the commands in one server. They read the same everywhere:
/// Discord picks each reader's language from what [`describe_command`] lists.
#[allow(
    clippy::result_large_err,
    reason = "serenity::Error is serenity's own type"
)]
pub(super) async fn register_commands(
    http: impl AsRef<serenity::Http>,
    guild_id: serenity::GuildId,
) -> Result<(), serenity::Error> {
    let mut commands = all_commands();
    for command in &mut commands {
        describe_command(command, "");
    }
    poise::builtins::register_in_guild(http, &commands, guild_id).await
}

/// The Discord locales each language's text is listed under. English is also
/// the plain text, which every other locale falls back to.
fn discord_locales(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::En => &["en-US"],
        Lang::Pl => &["pl"],
    }
}

/// Describes a command, its options and its choices in English, and lists
/// every language Marian speaks as a translation, so Discord shows each reader
/// the one for their own client language and English to everyone else. The
/// other languages come from the `cmd-` messages: `cmd-settings-hour_24`
/// describes `/settings hour_24`, `cmd-edit--title` its `title` option. English
/// is what the command itself says.
fn describe_command(command: &mut poise::Command<Data, Error>, parent: &str) {
    let path = if parent.is_empty() {
        command.name.to_string()
    } else {
        format!("{parent}-{}", command.name)
    };
    // Commands give their English as the en-US text; without a plain one,
    // Discord showed "A slash command" to everyone not set to US English.
    if command.description.is_none() {
        command.description = command
            .description_localizations
            .iter()
            .find(|(locale, _)| locale == "en-US")
            .map(|(_, text)| text.clone());
    }
    for lang in Lang::ALL {
        for &locale in discord_locales(lang) {
            let translated = |key: &str, english: Option<&str>| {
                if lang == Lang::En {
                    english.map(str::to_string)
                } else {
                    command_text(lang, key)
                }
            };
            if let Some(text) = translated(&format!("cmd-{path}"), command.description.as_deref()) {
                localize(&mut command.description_localizations, locale, text);
            }
            for parameter in &mut command.parameters {
                let key = format!("cmd-{path}--{}", parameter.name);
                if let Some(text) = translated(&key, parameter.description.as_deref()) {
                    localize(&mut parameter.description_localizations, locale, text);
                }
                for choice in parameter.choices.to_mut() {
                    let key = format!("cmd-{path}--{}--{}", parameter.name, choice.name);
                    if let Some(text) = translated(&key, Some(&choice.name)) {
                        localize(&mut choice.localizations, locale, text);
                    }
                }
            }
        }
    }
    for subcommand in &mut command.subcommands {
        describe_command(subcommand, &path);
    }
}

/// Sets one locale's text in a list of localizations, replacing the text the
/// locale already has.
fn localize(
    list: &mut std::borrow::Cow<
        'static,
        [(
            std::borrow::Cow<'static, str>,
            std::borrow::Cow<'static, str>,
        )],
    >,
    locale: &str,
    text: String,
) {
    let list = list.to_mut();
    match list.iter_mut().find(|(existing, _)| existing == locale) {
        Some(entry) => entry.1 = text.into(),
        None => list.push((locale.to_string().into(), text.into())),
    }
}

fn command_text(lang: Lang, key: &str) -> Option<String> {
    i18n::has(lang, key).then(|| i18n::text(lang, key, &[]))
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
    let avatar = server_avatar(
        guild_id,
        author,
        member.as_ref().and_then(|member| member.avatar.as_ref()),
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
    let mut conn = ctx
        .data()
        .db
        .get()
        .map_err(|_| "Could not reach the database.")?;
    let guild = logic::ensure_discord_guild(&mut conn, &guild_id.get().to_string(), &guild_name)?;
    let (user, renamed) = logic::ensure_discord_user(&mut conn, guild.id, &discord_user, &display)?;
    redraw(&ctx.data().jobs, renamed);
    if member.is_some() {
        logic::follow_discord_avatar(&mut conn, &user, avatar.as_deref())?;
    }
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

async fn complete_channel(ctx: Context<'_>, partial: &str) -> serenity::CreateAutocompleteResponse {
    serenity::CreateAutocompleteResponse::new().set_choices(channel_choices(ctx, partial))
}

fn channel_choices(ctx: Context<'_>, partial: &str) -> Vec<serenity::AutocompleteChoice> {
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
    commands::channel_choices(
        &mut conn,
        guild.id,
        &ctx.author().id.get().to_string(),
        partial,
        lang,
    )
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

async fn complete_event(ctx: Context<'_>, partial: &str) -> serenity::CreateAutocompleteResponse {
    serenity::CreateAutocompleteResponse::new().set_choices(event_choices(ctx, partial))
}

fn event_choices(ctx: Context<'_>, partial: &str) -> Vec<serenity::AutocompleteChoice> {
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
    commands::response_events(
        &mut conn,
        guild.id,
        &ctx.author().id.get().to_string(),
        partial,
        chrono::Utc::now(),
        lang,
    )
    .unwrap_or_default()
    .into_iter()
    .map(|choice| serenity::AutocompleteChoice::new(choice.label, choice.value))
    .collect()
}

async fn complete_option(ctx: Context<'_>, partial: &str) -> serenity::CreateAutocompleteResponse {
    serenity::CreateAutocompleteResponse::new().set_choices(option_choices(ctx, partial))
}

fn option_choices(ctx: Context<'_>, partial: &str) -> Vec<serenity::AutocompleteChoice> {
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
    commands::response_options(
        &mut conn,
        guild.id,
        &ctx.author().id.get().to_string(),
        &event,
        partial,
    )
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

#[cfg(test)]
mod tests {
    use crate::i18n::Lang;

    /// Every command, option and choice, as the path its `cmd-` message uses.
    fn command_paths(
        command: &poise::Command<super::Data, super::Error>,
        parent: &str,
        out: &mut Vec<(String, Option<String>)>,
    ) {
        let path = if parent.is_empty() {
            command.name.to_string()
        } else {
            format!("{parent}-{}", command.name)
        };
        out.push((
            format!("cmd-{path}"),
            command.description.as_deref().map(str::to_string),
        ));
        for parameter in &command.parameters {
            out.push((
                format!("cmd-{path}--{}", parameter.name),
                parameter.description.as_deref().map(str::to_string),
            ));
            // A language is named in itself, whatever the server's language.
            if parameter.name == "language" {
                continue;
            }
            for choice in parameter.choices.iter() {
                out.push((
                    format!("cmd-{path}--{}--{}", parameter.name, choice.name),
                    None,
                ));
            }
        }
        for subcommand in &command.subcommands {
            command_paths(subcommand, &path, out);
        }
    }

    fn localized<'a>(
        list: &'a [(std::borrow::Cow<'a, str>, std::borrow::Cow<'a, str>)],
        locale: &str,
    ) -> Option<&'a str> {
        list.iter()
            .find(|(existing, _)| existing == locale)
            .map(|(_, text)| text.as_ref())
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
            assert!(
                polish.chars().count() <= 100,
                "{key} is longer than Discord allows"
            );
            if let Some(english) = english {
                assert!(
                    english.chars().count() <= 100,
                    "{key} in English is too long"
                );
            }
        }
        let mut described = super::all_commands();
        for command in &mut described {
            super::describe_command(command, "");
        }
        let settings = described
            .iter()
            .find(|command| command.name == "settings")
            .unwrap();
        let digest = settings
            .subcommands
            .iter()
            .find(|command| command.name == "digest")
            .unwrap();
        assert_eq!(
            digest.description.as_deref(),
            Some("How often Marian posts a digest"),
            "English is the plain text"
        );
        assert_eq!(
            localized(&digest.description_localizations, "en-US"),
            Some("How often Marian posts a digest")
        );
        assert_eq!(
            localized(&digest.description_localizations, "pl"),
            Some("Jak często Marian publikuje podsumowanie")
        );
        let daily = &digest.parameters[0].choices[1];
        assert_eq!(daily.name, "daily", "a choice keeps its English name");
        assert_eq!(localized(&daily.localizations, "pl"), Some("codziennie"));
        let language = settings
            .subcommands
            .iter()
            .find(|command| command.name == "language")
            .unwrap();
        assert_eq!(language.parameters[0].choices[1].name, "Polski");
    }

    /// The commands as Discord receives them, against `commands.json` next to
    /// this file. `UPDATE_SNAPSHOTS=1` rewrites it after a deliberate change
    /// to a command.
    #[test]
    fn the_registered_commands_match_the_snapshot() {
        let mut commands = super::all_commands();
        for command in &mut commands {
            super::describe_command(command, "");
        }
        let created = poise::builtins::create_application_commands(&commands);
        // Translations are kept in hash maps, so sort keys for a stable file.
        fn sorted(value: serde_json::Value) -> serde_json::Value {
            match value {
                serde_json::Value::Object(map) => {
                    let map: std::collections::BTreeMap<_, _> = map
                        .into_iter()
                        .map(|(key, value)| (key, sorted(value)))
                        .collect();
                    serde_json::Value::Object(map.into_iter().collect())
                }
                serde_json::Value::Array(items) => {
                    serde_json::Value::Array(items.into_iter().map(sorted).collect())
                }
                other => other,
            }
        }
        let created = sorted(serde_json::to_value(&created).unwrap());
        let actual = serde_json::to_string_pretty(&created).unwrap() + "\n";
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/gateway/slash/commands.json"
        );
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
            std::fs::write(path, &actual).unwrap();
        }
        let expected = std::fs::read_to_string(path).unwrap_or_default();
        assert!(
            actual == expected,
            "the registered commands changed; UPDATE_SNAPSHOTS=1 rewrites {path}"
        );
    }
}
