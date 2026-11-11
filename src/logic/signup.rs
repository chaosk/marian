use std::collections::HashSet;

use chrono::{DateTime, Utc};
use diesel::prelude::*;

use super::access::can_edit;
use super::db::{log, must_event, must_option, must_role, must_membership, role_names};
use crate::error::OpError;
use crate::schema;
use crate::store::{self, Connection, Event};

/// A signup button: signs the person up, or takes them off an option they're
/// already on, guests and all.
pub fn signup(
    conn: &mut Connection,
    event_id: i64,
    option_id: i64,
    membership_id: i64,
) -> Result<crate::i18n::Msg, OpError> {
    store::with_tx(conn, |tx| signup_tx(tx, event_id, option_id, membership_id, None))
}

/// A "+X" signup: signs the person up with `guests`, or changes how many guests
/// they bring when they're already on the option. Never takes them off it.
pub fn signup_guests(
    conn: &mut Connection,
    event_id: i64,
    option_id: i64,
    membership_id: i64,
    guests: i64,
) -> Result<crate::i18n::Msg, OpError> {
    store::with_tx(conn, |tx| signup_tx(tx, event_id, option_id, membership_id, Some(guests)))
}

fn signup_tx(
    conn: &mut Connection,
    event_id: i64,
    option_id: i64,
    membership_id: i64,
    guests: Option<i64>,
) -> Result<crate::i18n::Msg, OpError> {
    let event = must_event(conn, event_id)?;
    let user = must_membership(conn, membership_id)?;
    if user.guild_id != event.guild_id {
        return Err(OpError::from(crate::msg!("err-person-elsewhere")));
    }
    let option = must_option(conn, option_id)?;
    if option.event_id != event.id {
        return Err(OpError::NotFound);
    }
    if event.status != "posted" {
        return Err(OpError::from(crate::msg!("err-signups-not-open")));
    }
    let now = Utc::now();
    if let Some(existing) = find_signup(conn, option_id, membership_id)? {
        if let Some(guests) = guests {
            if event.signups_closed || cutoff_passed(&event, now) {
                return Err(OpError::from(crate::msg!("err-signups-closed", title = &event.title)));
            }
            return set_guests(conn, &event, &option, &user, existing, guests, now);
        }
        let state = existing.state.clone();
        diesel::delete(schema::signups::table.filter(schema::signups::id.eq(existing.id)))
            .execute(conn)?;
        fill_all(conn, &event)?;
        sync_access(conn, event.id)?;
        let notice = if state == "waitlisted" {
            crate::msg!("notice-left-waitlist", name = &user.name, option = &option.name)
        } else {
            crate::msg!("notice-left", name = &user.name, option = &option.name)
        };
        log(conn, event.guild_id, Some(event.id), &notice.to_string())?;
        return Ok(notice);
    }
    if event.signups_closed || cutoff_passed(&event, now) {
        return Err(OpError::from(crate::msg!("err-signups-closed", title = &event.title)));
    }
    let restricts = store::event_role_ids(conn, &event, "restrict")?;
    if !restricts.is_empty() && !user.server_manager {
        let roles = store::membership_role_ids(conn, &user)?;
        if !restricts.iter().any(|role| roles.contains(role)) {
            let names = role_names(conn, &restricts)?;
            return Err(OpError::from(crate::msg!(
                "err-event-limited",
                roles = crate::i18n::each(|lang| crate::i18n::list_or(lang, &names)),
            )));
        }
    }
    if let Some(role_id) = option.role_id {
        if !user.server_manager && !store::membership_has_role(conn, user.id, role_id)? {
            let role = must_role(conn, role_id)?;
            return Err(OpError::from(crate::msg!(
                "err-option-limited",
                name = &user.name,
                option = &option.name,
                role = &role.name,
            )));
        }
    }
    let guests = guests.unwrap_or(0);
    check_guests(&option, guests)?;
    let waitlist = store::settings(conn, event.guild_id)?.waitlist;
    // Checked before leaving other options, so a refusal moves no one.
    let state = place(conn, &option, 1 + guests, waitlist)?;
    let moved = if !event.multiple_signups {
        let others = signups_for_user(conn, event.id, user.id)?;
        let moved = !others.is_empty();
        for other in others {
            diesel::delete(schema::signups::table.filter(schema::signups::id.eq(other.id)))
                .execute(conn)?;
        }
        moved
    } else {
        false
    };
    diesel::insert_into(schema::signups::table)
        .values(&NewSignup {
            event_id: event.id,
            option_id: option.id,
            membership_id: user.id,
            state,
            created_at: now,
            guests,
        })
        .execute(conn)?;
    fill_all(conn, &event)?;
    sync_access(conn, event.id)?;
    let (name, option_name) = (&user.name, &option.name);
    let notice = match (state, moved, guests) {
        ("waitlisted", _, 0) => crate::msg!("notice-waiting", name = name, option = option_name),
        ("waitlisted", _, _) => crate::msg!("notice-waiting-guests", name = name, option = option_name, guests = guests),
        (_, true, 0) => crate::msg!("notice-moved", name = name, option = option_name),
        (_, true, _) => crate::msg!("notice-moved-guests", name = name, option = option_name, guests = guests),
        (_, false, 0) => crate::msg!("notice-signed-up", name = name, option = option_name),
        (_, false, _) => crate::msg!("notice-signed-up-guests", name = name, option = option_name, guests = guests),
    };
    log(conn, event.guild_id, Some(event.id), &notice.to_string())?;
    Ok(notice)
}

/// Changes how many guests someone already on an option brings. Fewer keeps
/// their place. More keeps it while the party still fits; otherwise the party
/// goes to the back of the waitlist, and a waiting party that grows goes to the
/// back too.
fn set_guests(
    conn: &mut Connection,
    event: &Event,
    option: &store::SignupOption,
    user: &store::Membership,
    existing: store::Signup,
    guests: i64,
    now: DateTime<Utc>,
) -> Result<crate::i18n::Msg, OpError> {
    check_guests(option, guests)?;
    let size = 1 + guests;
    let mut state = existing.state.clone();
    let mut created_at = existing.created_at;
    if guests > existing.guests {
        if let Some(cap) = option.capacity {
            if size > cap {
                return Err(too_big(option, cap, size));
            }
            if state == "confirmed" {
                let others = places_used(conn, option.id)? - (1 + existing.guests);
                if others + size > cap {
                    if !store::settings(conn, event.guild_id)?.waitlist {
                        return Err(OpError::from(crate::msg!(
                            "err-party-too-big",
                            option = &option.name,
                            left = cap - others - 1,
                        )));
                    }
                    state = "waitlisted".to_string();
                    created_at = now;
                }
            } else {
                created_at = now;
            }
        }
    }
    diesel::update(schema::signups::table.filter(schema::signups::id.eq(existing.id)))
        .set((
            schema::signups::guests.eq(guests),
            schema::signups::state.eq(&state),
            schema::signups::created_at.eq(created_at),
        ))
        .execute(conn)?;
    fill_all(conn, event)?;
    sync_access(conn, event.id)?;
    let notice = if state == "waitlisted" && existing.state == "confirmed" {
        crate::msg!("notice-guests-waiting", name = &user.name, option = &option.name, size = size)
    } else {
        crate::msg!("notice-guests", name = &user.name, option = &option.name, guests = guests)
    };
    log(conn, event.guild_id, Some(event.id), &notice.to_string())?;
    Ok(notice)
}

/// What the "+X" form shows: the option, its limit, and the guests the person
/// already brings there, if they're on it.
pub struct GuestsForm {
    pub option: String,
    pub max_guests: i64,
    pub current: Option<i64>,
}

pub fn guests_form(conn: &mut Connection, option_id: i64, membership_id: i64) -> Result<GuestsForm, OpError> {
    let option = must_option(conn, option_id)?;
    let current = find_signup(conn, option_id, membership_id)?.map(|signup| signup.guests);
    Ok(GuestsForm {
        option: option.name,
        max_guests: option.max_guests,
        current,
    })
}

fn check_guests(option: &store::SignupOption, guests: i64) -> Result<(), OpError> {
    if guests == 0 {
        return Ok(());
    }
    if option.max_guests == 0 {
        return Err(OpError::from(crate::msg!("err-guests-off", option = &option.name)));
    }
    if !(0..=option.max_guests).contains(&guests) {
        return Err(OpError::from(crate::msg!(
            "err-guests-most",
            option = &option.name,
            most = option.max_guests,
        )));
    }
    Ok(())
}

/// Where a new party of `size` lands: confirmed when it fits and no one is
/// waiting ahead of it, waitlisted otherwise. A party bigger than the whole
/// option is refused, since it would hold the waitlist up for good.
fn place(
    conn: &mut Connection,
    option: &store::SignupOption,
    size: i64,
    waitlist: bool,
) -> Result<&'static str, OpError> {
    let Some(cap) = option.capacity else {
        return Ok("confirmed");
    };
    if size > cap {
        return Err(too_big(option, cap, size));
    }
    let used = places_used(conn, option.id)?;
    let queue = count_state(conn, option.id, "waitlisted")? > 0;
    if !queue && used + size <= cap {
        return Ok("confirmed");
    }
    if waitlist {
        return Ok("waitlisted");
    }
    if size > 1 && used < cap {
        return Err(OpError::from(crate::msg!(
            "err-party-too-big",
            option = &option.name,
            left = cap - used,
        )));
    }
    Err(OpError::from(crate::msg!("err-option-full", option = &option.name)))
}

fn too_big(option: &store::SignupOption, cap: i64, size: i64) -> OpError {
    OpError::from(crate::msg!(
        "err-party-over-capacity",
        option = &option.name,
        cap = cap,
        size = size,
    ))
}

/// Places the confirmed signups on an option take: each person and their guests.
pub(crate) fn places_used(conn: &mut Connection, option_id: i64) -> Result<i64, OpError> {
    let guests: Vec<i64> = schema::signups::table
        .filter(schema::signups::option_id.eq(option_id))
        .filter(schema::signups::state.eq("confirmed"))
        .select(schema::signups::guests)
        .load(conn)?;
    Ok(guests.iter().map(|guests| 1 + guests).sum())
}

pub fn editor_response(
    conn: &mut Connection,
    event_id: i64,
    actor_id: i64,
    target_id: i64,
    option_id: i64,
    action: &str,
    guests: Option<i64>,
) -> Result<crate::i18n::Msg, OpError> {
    store::with_tx(conn, |tx| {
        let event = must_event(tx, event_id)?;
        let actor = must_membership(tx, actor_id)?;
        let settings = store::settings(tx, event.guild_id)?;
        let roles = store::membership_role_ids(tx, &actor)?;
        if !can_edit(&actor, &event, &settings, &roles) {
            return Err(OpError::from(crate::msg!("err-cant-change-responses")));
        }
        if event.status != "posted" {
            return Err(OpError::from(crate::msg!("err-responses-unposted")));
        }
        let target = must_membership(tx, target_id)?;
        let option = must_option(tx, option_id)?;
        if option.event_id != event.id || target.guild_id != event.guild_id {
            return Err(OpError::NotFound);
        }
        match action {
            "remove" => {
                let Some(existing) = find_signup(tx, option.id, target.id)? else {
                    return Ok(crate::msg!("notice-was-not-on", name = &target.name, option = &option.name));
                };
                diesel::delete(schema::signups::table.filter(schema::signups::id.eq(existing.id)))
                    .execute(tx)?;
                fill_all(tx, &event)?;
                sync_access(tx, event.id)?;
                let notice = crate::msg!("notice-removed", name = &target.name, option = &option.name);
                log(tx, event.guild_id, Some(event.id), &notice.to_string())?;
                Ok(notice)
            }
            "add" | "move" => {
                if !event.multiple_signups || action == "move" {
                    for other in signups_for_user(tx, event.id, target.id)? {
                        if other.option_id != option.id {
                            diesel::delete(
                                schema::signups::table.filter(schema::signups::id.eq(other.id)),
                            )
                            .execute(tx)?;
                        }
                    }
                }
                if let Some(guests) = guests {
                    check_guests(&option, guests)?;
                }
                if let Some(existing) = find_signup(tx, option.id, target.id)? {
                    // An editor's change keeps the party where it is, full or not.
                    if let Some(guests) = guests.filter(|guests| *guests != existing.guests) {
                        diesel::update(schema::signups::table.filter(schema::signups::id.eq(existing.id)))
                            .set(schema::signups::guests.eq(guests))
                            .execute(tx)?;
                        fill_all(tx, &event)?;
                        sync_access(tx, event.id)?;
                        let notice = crate::msg!("notice-guests", name = &target.name, option = &option.name, guests = guests);
                        log(tx, event.guild_id, Some(event.id), &notice.to_string())?;
                        return Ok(notice);
                    }
                    fill_all(tx, &event)?;
                    sync_access(tx, event.id)?;
                    return Ok(crate::msg!("notice-already-on", name = &target.name, option = &option.name));
                }
                diesel::insert_into(schema::signups::table)
                    .values(&NewSignup {
                        event_id: event.id,
                        option_id: option.id,
                        membership_id: target_id,
                        state: "confirmed",
                        created_at: Utc::now(),
                        guests: guests.unwrap_or(0),
                    })
                    .execute(tx)?;
                fill_all(tx, &event)?;
                sync_access(tx, event.id)?;
                let notice = crate::msg!("notice-added", name = &target.name, option = &option.name);
                log(tx, event.guild_id, Some(event.id), &notice.to_string())?;
                Ok(notice)
            }
            _ => Err(OpError::from(crate::msg!("err-response-action"))),
        }
    })
}

fn find_signup(
    conn: &mut Connection,
    option_id: i64,
    membership_id: i64,
) -> Result<Option<store::Signup>, OpError> {
    Ok(store::Signup::for_option_member(option_id, membership_id)
        .first(conn)
        .optional()?)
}

fn signups_for_user(
    conn: &mut Connection,
    event_id: i64,
    membership_id: i64,
) -> Result<Vec<store::Signup>, OpError> {
    Ok(store::Signup::for_event_member(event_id, membership_id).load(conn)?)
}

fn count_state(conn: &mut Connection, option_id: i64, state: &str) -> Result<i64, OpError> {
    Ok(schema::signups::table
        .filter(schema::signups::option_id.eq(option_id))
        .filter(schema::signups::state.eq(state))
        .count()
        .get_result(conn)?)
}

pub(super) fn fill_all(conn: &mut Connection, event: &Event) -> Result<(), OpError> {
    for opt in store::options_for(conn, event)? {
        fill_option(conn, opt.id)?;
    }
    Ok(())
}

/// Moves waiting parties onto the option in the order they came, while the one
/// at the front fits. Nobody passes it: a smaller party behind waits too.
fn fill_option(conn: &mut Connection, option_id: i64) -> Result<(), OpError> {
    let option = must_option(conn, option_id)?;
    let Some(cap) = option.capacity else {
        return Ok(());
    };
    for _ in 0..50 {
        let next: Option<(i64, i64, i64)> = schema::signups::table
            .filter(schema::signups::option_id.eq(option_id))
            .filter(schema::signups::state.eq("waitlisted"))
            .order((schema::signups::created_at.asc(), schema::signups::id.asc()))
            .select((schema::signups::id, schema::signups::membership_id, schema::signups::guests))
            .first(conn)
            .optional()?;
        let Some((signup_id, membership_id, guests)) = next else {
            break;
        };
        if places_used(conn, option_id)? + 1 + guests > cap {
            break;
        }
        diesel::update(schema::signups::table.filter(schema::signups::id.eq(signup_id)))
            .set(schema::signups::state.eq("confirmed"))
            .execute(conn)?;
        let user = must_membership(conn, membership_id)?;
        let event = must_event(conn, option.event_id)?;
        log(
            conn,
            event.guild_id,
            Some(event.id),
            &format!(
                "Marian pulled {} off the {} waitlist.",
                user.name, option.name
            ),
        )?;
    }
    Ok(())
}

pub(super) fn sync_access(conn: &mut Connection, event_id: i64) -> Result<(), OpError> {
    let event = must_event(conn, event_id)?;
    let settings = store::settings(conn, event.guild_id)?;
    let options = store::options_for(conn, &event)?;
    let signups = store::signups_for(conn, &event)?;
    let mut reminding = HashSet::new();
    for signup in &signups {
        if signup.state != "confirmed" {
            continue;
        }
        if options
            .iter()
            .any(|opt| opt.id == signup.option_id && opt.reminders_enabled)
        {
            reminding.insert(signup.membership_id);
        }
    }
    if event.thread_enabled && settings.auto_join_threads {
        let name = super::discord::stored_thread_name(conn, &event)?;
        diesel::insert_into(schema::threads::table)
            .values(&NewThread {
                event_id,
                name: &name,
            })
            .on_conflict_do_nothing()
            .execute(conn)?;
        let members = store::thread_member_ids(conn, &event)?;
        let joining: Vec<i64> = reminding
            .iter()
            .copied()
            .filter(|membership_id| !members.contains(membership_id))
            .collect();
        insert_thread_members(conn, event_id, &joining)?;
        for membership_id in joining {
            let user = must_membership(conn, membership_id)?;
            log(
                conn,
                event.guild_id,
                Some(event_id),
                &format!("Marian added {} to the thread.", user.name),
            )?;
        }
        for membership_id in members {
            if !reminding.contains(&membership_id) {
                diesel::delete(
                    schema::thread_members::table
                        .filter(schema::thread_members::event_id.eq(event_id))
                        .filter(schema::thread_members::membership_id.eq(membership_id)),
                )
                .execute(conn)?;
                let user = must_membership(conn, membership_id)?;
                log(
                    conn,
                    event.guild_id,
                    Some(event_id),
                    &format!("Marian removed {} from the thread.", user.name),
                )?;
            }
        }
    }

    let mut role_id = event.attendee_role_id;
    if role_id.is_none() && settings.temp_roles && !reminding.is_empty() {
        let lang = crate::i18n::lang_of(conn, event.guild_id);
        let name: String = crate::tr!(lang, "role-attendee", title = &event.title)
            .chars()
            .take(80)
            .collect();
        let new_id = diesel::insert_into(schema::roles::table)
            .values(&NewRole {
                guild_id: event.guild_id,
                name: &name,
                color: "#1e3a6e",
            })
            .returning(schema::roles::id)
            .get_result(conn)?;
        role_id = Some(new_id);
        diesel::update(schema::events::table.filter(schema::events::id.eq(event_id)))
            .set(schema::events::attendee_role_id.eq(role_id))
            .execute(conn)?;
    }
    if let Some(role_id) = role_id {
        let role_name = must_role(conn, role_id)?.name;
        let grants = store::grant_membership_ids(conn, &event)?;
        let granting: Vec<i64> = reminding
            .iter()
            .copied()
            .filter(|membership_id| !grants.contains(membership_id))
            .collect();
        insert_role_grants(conn, event_id, role_id, &granting)?;
        for membership_id in granting {
            let user = must_membership(conn, membership_id)?;
            log(
                conn,
                event.guild_id,
                Some(event_id),
                &format!("Marian gave {} the {} role.", user.name, role_name),
            )?;
        }
        for membership_id in grants {
            if !reminding.contains(&membership_id) {
                diesel::delete(
                    schema::role_grants::table
                        .filter(schema::role_grants::event_id.eq(event_id))
                        .filter(schema::role_grants::membership_id.eq(membership_id)),
                )
                .execute(conn)?;
                let user = must_membership(conn, membership_id)?;
                log(
                    conn,
                    event.guild_id,
                    Some(event_id),
                    &format!(
                        "Marian took the {} role back from {}.",
                        role_name, user.name
                    ),
                )?;
            }
        }
    }
    Ok(())
}

pub(super) fn cutoff_passed(event: &Event, now: DateTime<Utc>) -> bool {
    event.cutoff_at.is_some_and(|cutoff| now >= cutoff)
}

#[derive(Insertable)]
#[diesel(table_name = schema::signups)]
struct NewSignup<'a> {
    event_id: i64,
    option_id: i64,
    membership_id: i64,
    state: &'a str,
    created_at: DateTime<Utc>,
    guests: i64,
}

#[derive(Insertable)]
#[diesel(table_name = schema::threads)]
struct NewThread<'a> {
    event_id: i64,
    name: &'a str,
}

#[derive(Insertable)]
#[diesel(table_name = schema::roles)]
struct NewRole<'a> {
    guild_id: i64,
    name: &'a str,
    color: &'a str,
}

#[derive(Insertable)]
#[diesel(table_name = schema::thread_members)]
struct NewThreadMember {
    event_id: i64,
    membership_id: i64,
}

fn insert_thread_members(
    conn: &mut Connection,
    event_id: i64,
    membership_ids: &[i64],
) -> Result<(), OpError> {
    let rows: Vec<_> = membership_ids
        .iter()
        .map(|membership_id| NewThreadMember {
            event_id,
            membership_id: *membership_id,
        })
        .collect();
    if rows.is_empty() {
        return Ok(());
    }
    diesel::insert_into(schema::thread_members::table)
        .values(&rows)
        .on_conflict_do_nothing()
        .execute(conn)?;
    Ok(())
}

#[derive(Insertable)]
#[diesel(table_name = schema::role_grants)]
struct NewRoleGrant {
    event_id: i64,
    membership_id: i64,
    role_id: i64,
}

fn insert_role_grants(
    conn: &mut Connection,
    event_id: i64,
    role_id: i64,
    membership_ids: &[i64],
) -> Result<(), OpError> {
    let rows: Vec<_> = membership_ids
        .iter()
        .map(|membership_id| NewRoleGrant {
            event_id,
            membership_id: *membership_id,
            role_id,
        })
        .collect();
    if rows.is_empty() {
        return Ok(());
    }
    diesel::insert_into(schema::role_grants::table)
        .values(&rows)
        .on_conflict_do_nothing()
        .execute(conn)?;
    Ok(())
}
