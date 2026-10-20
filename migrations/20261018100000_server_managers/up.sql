-- Members with Discord's Manage Server permission are called server managers.
ALTER TABLE memberships RENAME COLUMN organizer TO server_manager;
