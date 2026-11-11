//! A server's attendance types and presets. The library holds each type once:
//! its name, emoji, and whether people on it count as attending. Events,
//! presets, and series or routine templates point at library types and keep only
//! their own order, capacity, and role limit, so renaming a type renames it
//! everywhere. A preset is a named list of types; the server's default preset is
//! what a new event starts with.

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use diesel::prelude::*;

use super::access::{can_create, can_edit, holds_any};
use super::db::{log, must_event, must_membership, must_role};
use super::event::sync_options;
use super::model::{OptionInput, PresetInput, TypeInput, MAX_GUESTS};
use super::signup::{fill_all, sync_access};
use crate::error::OpError;
use crate::schema;
use crate::store::{self, Connection, LibraryOption, Preset};

const MAX_OPTIONS: usize = 8;

/// One type in a preset or template, as stored.
#[derive(Debug, Clone, Queryable)]
pub struct ListedType {
    pub type_id: i64,
    pub capacity: Option<i64>,
    pub role_id: Option<i64>,
    pub max_guests: i64,
}

#[derive(Insertable)]
#[diesel(table_name = schema::presets)]
struct NewPreset<'a> {
    public_id: uuid::Uuid,
    guild_id: i64,
    name: &'a str,
    multiple_signups: bool,
}

#[derive(Insertable)]
#[diesel(table_name = schema::preset_options)]
struct NewPresetOption {
    preset_id: i64,
    type_id: i64,
    position: i64,
    capacity: Option<i64>,
    role_id: Option<i64>,
    max_guests: i64,
}

/// The library type with this name, made if the server doesn't have it yet.
/// Names match without regard to case.
pub fn ensure_type(
    conn: &mut Connection,
    guild_id: i64,
    name: &str,
    emoji: &str,
    attending: bool,
) -> Result<i64, OpError> {
    let input = clean_type(&TypeInput {
        name: name.to_string(),
        emoji: emoji.to_string(),
        attending,
    })?;
    store::insert_type(conn, guild_id, &input.name, &input.emoji, input.attending)
}

/// Adds a type to the library.
pub fn add_type(
    conn: &mut Connection,
    guild_id: i64,
    actor_id: i64,
    input: &TypeInput,
) -> Result<LibraryOption, OpError> {
    require_editor(conn, guild_id, actor_id)?;
    let input = clean_type(input)?;
    if let Some(existing) = store::library_type_named(conn, guild_id, &input.name)? {
        return Err(OpError::from(crate::msg!("err-library-has", name = &existing.name)));
    }
    let id = ensure_type(conn, guild_id, &input.name, &input.emoji, input.attending)?;
    store::library_type(conn, id)?.ok_or(OpError::NotFound)
}

/// Renames a type, or changes its emoji or whether it counts as attending.
/// Every event using it shows the change on the website; the posted ones that
/// haven't ended are returned so their Discord posts can be refreshed.
pub fn update_type(
    conn: &mut Connection,
    type_id: i64,
    actor_id: i64,
    input: &TypeInput,
) -> Result<Vec<i64>, OpError> {
    let current = store::library_type(conn, type_id)?.ok_or(OpError::NotFound)?;
    require_editor(conn, current.guild_id, actor_id)?;
    let input = clean_type(input)?;
    if let Some(other) = store::library_type_named(conn, current.guild_id, &input.name)? {
        if other.id != type_id {
            return Err(OpError::from(crate::msg!("err-library-has", name = &other.name)));
        }
    }
    store::with_tx(conn, |tx| {
        diesel::update(schema::option_library::table.find(type_id))
            .set((
                schema::option_library::name.eq(&input.name),
                schema::option_library::emoji.eq(&input.emoji),
                schema::option_library::attending.eq(input.attending),
            ))
            .execute(tx)?;
        // Only posts people still act on: posted events that haven't ended. A
        // past one keeps its old post rather than costing a Discord call each.
        // An event lasts at most a day, so that bounds the query; the end is
        // checked exactly below.
        let now = Utc::now();
        let events: Vec<store::Event> = schema::events::table
            .filter(schema::events::guild_id.eq(current.guild_id))
            .filter(schema::events::status.eq("posted"))
            .filter(schema::events::starts_at.gt(now - chrono::Duration::days(1)))
            .filter(schema::events::id.eq_any(
                schema::signup_options::table
                    .filter(schema::signup_options::type_id.eq(type_id))
                    .select(schema::signup_options::event_id),
            ))
            .select(store::Event::as_select())
            .load::<store::Event>(tx)?
            .into_iter()
            .filter(|event| event.starts_at + chrono::Duration::minutes(event.duration_minutes) > now)
            .collect();
        // Attending decides who gets reminders and the attendee role.
        if input.attending != current.attending {
            for event in &events {
                sync_access(tx, event.id)?;
            }
        }
        Ok(events.into_iter().map(|event| event.id).collect())
    })
}

/// Takes a type out of the library. A type still in use stays.
pub fn delete_type(conn: &mut Connection, type_id: i64, actor_id: i64) -> Result<(), OpError> {
    let current = store::library_type(conn, type_id)?.ok_or(OpError::NotFound)?;
    require_editor(conn, current.guild_id, actor_id)?;
    let usage = store::type_usage(conn, current.guild_id)?;
    if let Some(used) = usage.get(&type_id).filter(|used| used.total() > 0) {
        return Err(OpError::from(crate::msg!(
            "err-type-in-use",
            name = &current.name,
            usage = crate::i18n::each(|lang| used.describe_in(lang)),
        )));
    }
    diesel::delete(schema::option_library::table.find(type_id)).execute(conn)?;
    Ok(())
}

/// Adds a preset, or changes one when `preset_id` names it.
pub fn save_preset(
    conn: &mut Connection,
    guild_id: i64,
    actor_id: i64,
    preset_id: Option<i64>,
    input: &PresetInput,
) -> Result<i64, OpError> {
    require_editor(conn, guild_id, actor_id)?;
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err(OpError::from(crate::msg!("err-preset-name")));
    }
    let options = clean_types(conn, guild_id, &input.options)?;
    store::with_tx(conn, |tx| match preset_id {
        None => insert_preset(tx, guild_id, name, input.multiple_signups, &options),
        Some(id) => {
            let preset = store::preset(tx, id)?.filter(|preset| preset.guild_id == guild_id).ok_or(OpError::NotFound)?;
            diesel::update(schema::presets::table.find(preset.id))
                .set((
                    schema::presets::name.eq(name),
                    schema::presets::multiple_signups.eq(input.multiple_signups),
                ))
                .execute(tx)?;
            diesel::delete(schema::preset_options::table.filter(schema::preset_options::preset_id.eq(preset.id)))
                .execute(tx)?;
            insert_preset_options(tx, preset.id, &options)?;
            Ok(preset.id)
        }
    })
}

fn insert_preset(
    conn: &mut Connection,
    guild_id: i64,
    name: &str,
    multiple_signups: bool,
    options: &[OptionInput],
) -> Result<i64, OpError> {
    let id = diesel::insert_into(schema::presets::table)
        .values(&NewPreset {
            public_id: crate::ids::preset_id(),
            guild_id,
            name,
            multiple_signups,
        })
        .returning(schema::presets::id)
        .get_result(conn)?;
    insert_preset_options(conn, id, options)?;
    Ok(id)
}

fn insert_preset_options(conn: &mut Connection, preset_id: i64, options: &[OptionInput]) -> Result<(), OpError> {
    let rows: Vec<NewPresetOption> = options
        .iter()
        .enumerate()
        .map(|(index, opt)| NewPresetOption {
            preset_id,
            type_id: opt.type_id,
            position: index as i64,
            capacity: opt.capacity,
            role_id: opt.role_id,
            max_guests: opt.max_guests,
        })
        .collect();
    diesel::insert_into(schema::preset_options::table).values(&rows).execute(conn)?;
    Ok(())
}

pub fn delete_preset(conn: &mut Connection, preset_id: i64, actor_id: i64) -> Result<(), OpError> {
    let preset = store::preset(conn, preset_id)?.ok_or(OpError::NotFound)?;
    require_editor(conn, preset.guild_id, actor_id)?;
    if store::settings(conn, preset.guild_id)?.default_preset_id == Some(preset.id) {
        return Err(OpError::from(crate::msg!("err-preset-default")));
    }
    diesel::delete(schema::presets::table.find(preset.id)).execute(conn)?;
    Ok(())
}

/// Makes a preset the one new events start with.
pub fn set_default_preset(conn: &mut Connection, preset_id: i64, actor_id: i64) -> Result<(), OpError> {
    let preset = store::preset(conn, preset_id)?.ok_or(OpError::NotFound)?;
    require_editor(conn, preset.guild_id, actor_id)?;
    store::set_default_preset(conn, preset.guild_id, Some(preset.id))
}

/// A preset's types, in order.
pub fn preset_types(conn: &mut Connection, preset_id: i64) -> Result<Vec<OptionInput>, OpError> {
    let rows: Vec<ListedType> = schema::preset_options::table
        .filter(schema::preset_options::preset_id.eq(preset_id))
        .order(schema::preset_options::position.asc())
        .select((
            schema::preset_options::type_id,
            schema::preset_options::capacity,
            schema::preset_options::role_id,
            schema::preset_options::max_guests,
        ))
        .load(conn)?;
    Ok(rows.into_iter().map(listed).collect())
}

pub(super) fn listed(row: ListedType) -> OptionInput {
    OptionInput {
        id: None,
        type_id: row.type_id,
        capacity: row.capacity,
        max_guests: row.max_guests,
        role_id: row.role_id,
    }
}

/// What a new event starts with: the default preset's types and its
/// multiple-responses setting. A server without one gets its starting types.
pub fn starting_options(conn: &mut Connection, guild_id: i64) -> Result<(Vec<OptionInput>, bool), OpError> {
    let settings = store::settings(conn, guild_id)?;
    if let Some(preset) = settings.default_preset_id.map(|id| store::preset(conn, id)).transpose()?.flatten() {
        let types = preset_types(conn, preset.id)?;
        if !types.is_empty() {
            return Ok((types, preset.multiple_signups));
        }
    }
    let mut types = Vec::new();
    for (name, emoji, attending) in store::STARTING_TYPES {
        types.push(OptionInput::of_type(ensure_type(conn, guild_id, name, emoji, attending)?));
    }
    Ok((types, false))
}

/// Gives an event a preset's types, capacities, role limits, and multiple
/// responses. A type the event already has keeps its responses; responses on a
/// type the preset doesn't have go with it.
pub fn apply_preset(
    conn: &mut Connection,
    event_id: i64,
    actor_id: i64,
    preset_id: i64,
) -> Result<String, OpError> {
    store::with_tx(conn, |tx| {
        let event = must_event(tx, event_id)?;
        let actor = must_membership(tx, actor_id)?;
        let settings = store::settings(tx, event.guild_id)?;
        let roles = store::membership_role_ids(tx, &actor)?;
        if actor.guild_id != event.guild_id || !can_edit(&actor, &event, &settings, &roles) {
            return Err(OpError::from(crate::msg!("err-event-not-yours")));
        }
        if matches!(event.status.as_str(), "archived" | "skipped" | "cancelled") {
            return Err(OpError::from(crate::msg!("err-date-closed")));
        }
        let preset = store::preset(tx, preset_id)?
            .filter(|preset| preset.guild_id == event.guild_id)
            .ok_or(OpError::NotFound)?;
        let current: HashMap<i64, i64> = store::options_for(tx, &event)?
            .into_iter()
            .map(|opt| (opt.type_id, opt.id))
            .collect();
        let options: Vec<OptionInput> = preset_types(tx, preset.id)?
            .into_iter()
            .map(|opt| OptionInput {
                id: current.get(&opt.type_id).copied(),
                ..opt
            })
            .collect();
        if options.is_empty() {
            return Err(OpError::from(crate::msg!("err-preset-empty")));
        }
        sync_options(tx, event.id, &options)?;
        diesel::update(schema::events::table.find(event.id))
            .set((
                schema::events::multiple_signups.eq(preset.multiple_signups),
                schema::events::updated_at.eq(Utc::now()),
            ))
            .execute(tx)?;
        if event.status == "posted" {
            let event = must_event(tx, event.id)?;
            fill_all(tx, &event)?;
            sync_access(tx, event.id)?;
        }
        let notice = format!("Gave {} the {} attendance.", event.title, preset.name);
        log(tx, event.guild_id, Some(event.id), &notice)?;
        Ok(notice)
    })
}

/// Checks a list of types for an event, preset, or template: each from this
/// server's library, each once, with a sensible capacity and a role from here.
pub(super) fn clean_types(
    conn: &mut Connection,
    guild_id: i64,
    input: &[OptionInput],
) -> Result<Vec<OptionInput>, OpError> {
    let library: HashSet<i64> = store::list_library(conn, guild_id)?.into_iter().map(|row| row.id).collect();
    let mut seen = HashSet::new();
    let mut options = Vec::new();
    for opt in input {
        if !library.contains(&opt.type_id) {
            return Err(OpError::from(crate::msg!("err-type-elsewhere")));
        }
        if !seen.insert(opt.type_id) {
            return Err(OpError::from(crate::msg!("err-type-twice")));
        }
        if let Some(cap) = opt.capacity {
            if !(1..=500).contains(&cap) {
                return Err(OpError::from(crate::msg!("err-capacity")));
            }
        }
        if !(0..=MAX_GUESTS).contains(&opt.max_guests) {
            return Err(OpError::from(crate::msg!("err-max-guests", most = MAX_GUESTS)));
        }
        if let Some(role_id) = opt.role_id {
            if must_role(conn, role_id)?.guild_id != guild_id {
                return Err(OpError::from(crate::msg!("err-type-role-elsewhere")));
            }
        }
        options.push(opt.clone());
    }
    if options.is_empty() {
        return Err(OpError::from(crate::msg!("err-types-none")));
    }
    if options.len() > MAX_OPTIONS {
        return Err(OpError::from(crate::msg!("err-types-many")));
    }
    Ok(options)
}

fn clean_type(input: &TypeInput) -> Result<TypeInput, OpError> {
    let name = input.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 40 {
        return Err(OpError::from(crate::msg!("err-type-name")));
    }
    let emoji = match input.emoji.trim() {
        "" => "•".to_string(),
        emoji if emoji.chars().count() > 8 => return Err(OpError::from(crate::msg!("err-emoji-long"))),
        emoji => emoji.to_string(),
    };
    Ok(TypeInput {
        name,
        emoji,
        attending: input.attending,
    })
}

/// Server managers, and people who may create events or hold a signup-option role.
pub(super) fn require_editor(conn: &mut Connection, guild_id: i64, actor_id: i64) -> Result<(), OpError> {
    let actor = must_membership(conn, actor_id)?;
    if actor.guild_id != guild_id {
        return Err(OpError::NotFound);
    }
    if actor.server_manager {
        return Ok(());
    }
    let settings = store::settings(conn, guild_id)?;
    let roles = store::membership_role_ids(conn, &actor)?;
    let allowed = if settings.role_signup.is_empty() {
        can_create(&actor, &settings, &roles)
    } else {
        holds_any(&settings.role_signup, &roles)
    };
    if allowed {
        Ok(())
    } else {
        Err(OpError::from(crate::msg!("err-cant-change-types")))
    }
}

/// Presets of a server, with their types, default first.
pub fn presets(conn: &mut Connection, guild_id: i64) -> Result<Vec<(Preset, Vec<OptionInput>, bool)>, OpError> {
    let default = store::settings(conn, guild_id)?.default_preset_id;
    let mut list = Vec::new();
    for preset in store::list_presets(conn, guild_id)? {
        let types = preset_types(conn, preset.id)?;
        let is_default = default == Some(preset.id);
        list.push((preset, types, is_default));
    }
    list.sort_by_key(|(preset, _, is_default)| (!is_default, preset.name.to_lowercase()));
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logic::{save_event, signup, EventInput, SaveAction};
    use chrono::Duration;
    use diesel::connection::SimpleConnection;

    fn world() -> Connection {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone) VALUES (1, 'Test', 'UTC'), (2, 'Other', 'UTC');
            INSERT INTO channels (id, guild_id, name, kind, position) VALUES (1, 1, 'events', 'text', 0);
            INSERT INTO users (discord_id) VALUES ('u1'), ('u2');
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color, reminders_enabled, server_manager)
            SELECT 1, id, 1, 'Nell', 'nell', '#112233', true, true FROM users WHERE discord_id = 'u1'
            UNION ALL
            SELECT 2, id, 1, 'June', 'june', '#223344', true, false FROM users WHERE discord_id = 'u2';
            ",
        )
        .unwrap();
        store::insert_default_settings(&mut conn, 1).unwrap();
        store::insert_default_settings(&mut conn, 2).unwrap();
        conn
    }

    fn named(conn: &mut Connection, name: &str) -> i64 {
        store::library_type_named(conn, 1, name).unwrap().unwrap().id
    }

    fn event(conn: &mut Connection, options: Vec<OptionInput>) -> i64 {
        save_event(
            conn,
            EventInput {
                id: None,
                guild_id: 1,
                actor_id: 1,
                channel_id: 1,
                title: "Raid".into(),
                description: String::new(),
                location: String::new(),
                starts_at: Utc::now() + Duration::days(2),
                duration_minutes: 60,
                color: "#1e3a6e".into(),
                image_url: None,
                cutoff_at: None,
                attendee_role_id: None,
                thread_enabled: false,
                multiple_signups: false,
                mention_everyone: false,
                mention_role_ids: vec![],
                restrict_role_ids: vec![],
                options,
                reminders: vec![],
                repeat: None,
                action: SaveAction::Publish,
                post_at: None,
                apply_series: false,
            },
        )
        .unwrap()
    }

    #[test]
    fn a_new_server_starts_with_three_types_and_a_default_preset() {
        let mut conn = world();
        let names: Vec<String> = store::list_library(&mut conn, 1).unwrap().into_iter().map(|kind| kind.name).collect();
        assert_eq!(names, ["Accepted", "Declined", "Tentative"]);
        assert!(store::library_type_named(&mut conn, 1, "accepted").unwrap().unwrap().attending);
        let (types, multiple) = starting_options(&mut conn, 1).unwrap();
        assert_eq!(types.len(), 3);
        assert!(!multiple);
        let listed = presets(&mut conn, 1).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0.name, "Standard");
        assert!(listed[0].2, "it is the default");
        // Seeding again leaves things as they are.
        store::seed_attendance(&mut conn, 1).unwrap();
        assert_eq!(store::list_library(&mut conn, 1).unwrap().len(), 3);
        assert_eq!(presets(&mut conn, 1).unwrap().len(), 1);
    }

    #[test]
    fn a_renamed_type_is_renamed_on_every_event() {
        let mut conn = world();
        let accepted = named(&mut conn, "Accepted");
        let id = event(&mut conn, vec![OptionInput::of_type(accepted)]);
        let posted = update_type(
            &mut conn,
            accepted,
            1,
            &TypeInput { name: "Going".into(), emoji: "🟢".into(), attending: true },
        )
        .unwrap();
        assert_eq!(posted, [id], "the posted event gets its Discord post refreshed");
        // One that has ended keeps its post: no Discord call for it.
        diesel::update(schema::events::table.find(id))
            .set(schema::events::starts_at.eq(Utc::now() - Duration::days(3)))
            .execute(&mut conn)
            .unwrap();
        let again = update_type(&mut conn, accepted, 1, &TypeInput { name: "Going".into(), emoji: "🟢".into(), attending: true }).unwrap();
        assert!(again.is_empty(), "{again:?}");
        let loaded = must_event(&mut conn, id).unwrap();
        let shown = store::options_for(&mut conn, &loaded).unwrap();
        assert_eq!((shown[0].name.as_str(), shown[0].emoji.as_str()), ("Going", "🟢"));

        let clash = update_type(
            &mut conn,
            accepted,
            1,
            &TypeInput { name: "declined".into(), emoji: String::new(), attending: true },
        )
        .unwrap_err()
        .to_string();
        assert!(clash.contains("already has Declined"), "{clash}");
        let again = add_type(&mut conn, 1, 1, &TypeInput { name: "TENTATIVE".into(), emoji: String::new(), attending: false })
            .unwrap_err()
            .to_string();
        assert!(again.contains("already has Tentative"), "{again}");
    }

    #[test]
    fn a_type_in_use_and_the_default_preset_stay() {
        let mut conn = world();
        let declined = named(&mut conn, "Declined");
        let err = delete_type(&mut conn, declined, 1).unwrap_err().to_string();
        assert_eq!(err, "Declined is still used by 1 preset.");
        let spare = add_type(&mut conn, 1, 1, &TypeInput { name: "Bench".into(), emoji: String::new(), attending: false }).unwrap();
        assert_eq!(spare.emoji, "•");
        delete_type(&mut conn, spare.id, 1).unwrap();
        let standard = store::settings(&mut conn, 1).unwrap().default_preset_id.unwrap();
        let err = delete_preset(&mut conn, standard, 1).unwrap_err().to_string();
        assert!(err.contains("default preset"), "{err}");
    }

    #[test]
    fn a_preset_keeps_responses_on_the_types_it_shares() {
        let mut conn = world();
        let (accepted, declined) = (named(&mut conn, "Accepted"), named(&mut conn, "Declined"));
        let id = event(&mut conn, vec![OptionInput::of_type(accepted), OptionInput::of_type(declined)]);
        let loaded = must_event(&mut conn, id).unwrap();
        let shown = store::options_for(&mut conn, &loaded).unwrap();
        signup(&mut conn, id, shown[0].id, 1).unwrap();
        signup(&mut conn, id, shown[1].id, 2).unwrap();

        let tank = ensure_type(&mut conn, 1, "Tank", "🛡", true).unwrap();
        let raid = save_preset(
            &mut conn,
            1,
            1,
            None,
            &PresetInput {
                name: "Raid".into(),
                multiple_signups: true,
                options: vec![
                    OptionInput { capacity: Some(2), ..OptionInput::of_type(tank) },
                    OptionInput::of_type(accepted),
                ],
            },
        )
        .unwrap();
        apply_preset(&mut conn, id, 1, raid).unwrap();

        let event = must_event(&mut conn, id).unwrap();
        assert!(event.multiple_signups, "the preset brings its multiple responses");
        let shown = store::options_for(&mut conn, &event).unwrap();
        let names: Vec<&str> = shown.iter().map(|opt| opt.name.as_str()).collect();
        assert_eq!(names, ["Tank", "Accepted"]);
        assert_eq!(shown[0].capacity, Some(2));
        let left = store::signups_for(&mut conn, &event).unwrap();
        assert_eq!(left.len(), 1, "Declined went, with June's response");
        assert_eq!((left[0].option_id, left[0].membership_id), (shown[1].id, 1), "Nell is still on Accepted");

        let elsewhere = store::settings(&mut conn, 2).unwrap().default_preset_id.unwrap();
        assert!(matches!(apply_preset(&mut conn, id, 1, elsewhere), Err(OpError::NotFound)));
    }
}
