use crate::store::{Event, Settings, User};

pub fn can_create(user: &User, settings: &Settings, roles: &[i64]) -> bool {
    if user.organizer {
        return true;
    }
    match settings.role_event {
        None => true,
        Some(role) => roles.contains(&role),
    }
}

pub fn can_edit(user: &User, event: &Event, settings: &Settings, roles: &[i64]) -> bool {
    if user.organizer || user.id == event.created_by {
        return true;
    }
    settings
        .role_edit
        .map(|role| roles.contains(&role))
        .unwrap_or(false)
}

pub fn can_delete(user: &User, event: &Event, settings: &Settings, roles: &[i64]) -> bool {
    if user.organizer || user.id == event.created_by {
        return true;
    }
    settings
        .role_delete
        .map(|role| roles.contains(&role))
        .unwrap_or(false)
}
