-- Text columns that hold one of a fixed set of words take only those words.
-- The server reads each as an enum and fails on any other, so a stray word
-- would break every page that loads the row. Adding a word means a new
-- migration that drops and re-adds the constraint.

ALTER TABLE events ADD CONSTRAINT events_status_check CHECK (status IN ('draft', 'scheduled', 'posted', 'archived', 'cancelled', 'skipped', 'failed'));
ALTER TABLE series ADD CONSTRAINT series_cadence_check CHECK (cadence IN ('once', 'daily', 'weekly', 'biweekly', 'monthly'));
ALTER TABLE routine_runs ADD CONSTRAINT routine_runs_status_check CHECK (status IN ('scheduled', 'open', 'locked', 'approval', 'skipped', 'expired'));
ALTER TABLE routine_steps ADD CONSTRAINT routine_steps_kind_check CHECK (kind IN ('poll', 'approval', 'post'));
ALTER TABLE signups ADD CONSTRAINT signups_state_check CHECK (state IN ('confirmed', 'waitlisted'));
ALTER TABLE settings ADD CONSTRAINT settings_digest_check CHECK (digest IN ('off', 'daily', 'weekly'));
ALTER TABLE settings ADD CONSTRAINT settings_language_check CHECK (language IN ('en', 'pl'));
ALTER TABLE thread_messages ADD CONSTRAINT thread_messages_place_check CHECK (place IN ('thread', 'channel'));
ALTER TABLE event_roles ADD CONSTRAINT event_roles_kind_check CHECK (kind IN ('mention', 'restrict'));
ALTER TABLE applied_access ADD CONSTRAINT applied_access_kind_check CHECK (kind IN ('thread', 'role'));
ALTER TABLE permission_roles ADD CONSTRAINT permission_roles_kind_check CHECK (kind IN ('event', 'edit', 'delete', 'channel', 'signup'));
ALTER TABLE channels ADD CONSTRAINT channels_kind_check CHECK (kind IN ('text', 'event'));
ALTER TABLE channel_overwrites ADD CONSTRAINT channel_overwrites_kind_check CHECK (kind IN ('member', 'role'));
ALTER TABLE event_prompts ADD CONSTRAINT event_prompts_step_check CHECK (step IN ('channel', 'channel_other', 'title', 'start', 'timezone', 'end', 'description', 'repeat', 'finish', 'signup', 'signup_preset', 'signup_custom', 'mention', 'image', 'color', 'restrict', 'role', 'cutoff', 'post', 'reminder', 'next', 'respond'));
