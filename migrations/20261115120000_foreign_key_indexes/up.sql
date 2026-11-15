-- Indexes on foreign-key columns that had none. Without them, the tick's
-- reminder and thread lookups scan the whole table, and deleting an event,
-- channel, role, member or server scans each referencing table to cascade.
CREATE INDEX IF NOT EXISTS idx_reminders_event ON reminders (event_id);
CREATE INDEX IF NOT EXISTS idx_signup_options_event ON signup_options (event_id);
CREATE INDEX IF NOT EXISTS idx_thread_messages_event ON thread_messages (event_id);
-- The command log lists a server's newest commands first.
CREATE INDEX IF NOT EXISTS idx_command_log_guild ON command_log (guild_id, id);
CREATE INDEX IF NOT EXISTS idx_events_channel ON events (channel_id);
CREATE INDEX IF NOT EXISTS idx_membership_roles_role ON membership_roles (role_id);
CREATE INDEX IF NOT EXISTS idx_signups_membership ON signups (membership_id);
