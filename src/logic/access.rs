use crate::store::{Event, Settings, Membership};

/// Whether someone with `roles` holds any of the roles a permission names.
pub fn holds_any(allowed: &[i64], roles: &[i64]) -> bool {
    allowed.iter().any(|role| roles.contains(role))
}

pub fn can_create(user: &Membership, settings: &Settings, roles: &[i64]) -> bool {
    user.server_manager || settings.role_event.is_empty() || holds_any(&settings.role_event, roles)
}

pub fn can_edit(user: &Membership, event: &Event, settings: &Settings, roles: &[i64]) -> bool {
    user.server_manager || event.created_by == Some(user.id) || holds_any(&settings.role_edit, roles)
}

pub fn can_delete(user: &Membership, event: &Event, settings: &Settings, roles: &[i64]) -> bool {
    user.server_manager || event.created_by == Some(user.id) || holds_any(&settings.role_delete, roles)
}
