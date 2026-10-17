use crate::store::{Event, Settings, Membership};

pub fn can_create(user: &Membership, settings: &Settings, roles: &[i64]) -> bool {
    if user.organizer {
        return true;
    }
    match settings.role_event {
        None => true,
        Some(role) => roles.contains(&role),
    }
}

pub fn can_edit(user: &Membership, event: &Event, settings: &Settings, roles: &[i64]) -> bool {
    if user.organizer || event.created_by == Some(user.id) {
        return true;
    }
    settings
        .role_edit
        .map(|role| roles.contains(&role))
        .unwrap_or(false)
}

pub fn can_delete(user: &Membership, event: &Event, settings: &Settings, roles: &[i64]) -> bool {
    if user.organizer || event.created_by == Some(user.id) {
        return true;
    }
    settings
        .role_delete
        .map(|role| roles.contains(&role))
        .unwrap_or(false)
}
