-- Work Marian does at a set time, one row per thing: post an event, send a
-- reminder, close signups, archive, move a routine on, send a digest, fill
-- series, or sync someone's Google Calendar. The leader's worker sleeps until
-- the earliest row instead of scanning every server every few seconds.
CREATE TABLE tasks (
  id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  kind TEXT NOT NULL CONSTRAINT tasks_kind_check CHECK (kind IN ('post', 'remind', 'close_signups', 'archive', 'routine', 'digest', 'fill_series', 'google_sync')),
  -- The event, reminder, routine, server or user the task is for.
  subject_id BIGINT NOT NULL,
  -- The server whose tick lock the task runs under; none for Google.
  guild_id BIGINT,
  run_at TIMESTAMPTZ NOT NULL,
  UNIQUE (kind, subject_id)
);
CREATE INDEX tasks_due ON tasks (run_at);

-- What changed since the worker last looked. Triggers fill this, so a change
-- from any code path moves the tasks that depend on it:
--   event    an event's post, reminders, cutoff, archive and calendars
--   routine  a routine's polls, approvals and next week's run
--   guild    a server's clock: digest, series fill, and everything in it
--   fill     a server's series: fill them now
--   google   a user's Google Calendar
--   member   a membership's calendar (its user's)
--   channel  who sees a channel changed: its server's calendars
--   people   who sees what in a server changed: its calendars
CREATE TABLE task_marks (
  kind TEXT NOT NULL CONSTRAINT task_marks_kind_check CHECK (kind IN ('event', 'routine', 'guild', 'fill', 'google', 'member', 'channel', 'people')),
  subject_id BIGINT NOT NULL,
  PRIMARY KEY (kind, subject_id)
);

CREATE FUNCTION mark_task(mark_kind TEXT, subject BIGINT) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
  IF subject IS NULL THEN
    RETURN;
  END IF;
  INSERT INTO task_marks (kind, subject_id) VALUES (mark_kind, subject) ON CONFLICT DO NOTHING;
  -- Postgres sends one per channel and payload at commit, however many marks.
  PERFORM pg_notify('marian_tasks', '');
END $$;

-- Marks TG_ARGV[0] for the row's TG_ARGV[1] column, the old value too when
-- an update moved it.
CREATE FUNCTION mark_row() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
  fresh jsonb;
  stale jsonb;
BEGIN
  IF TG_OP <> 'DELETE' THEN
    fresh := to_jsonb(NEW);
    PERFORM mark_task(TG_ARGV[0], (fresh ->> TG_ARGV[1])::bigint);
  END IF;
  IF TG_OP <> 'INSERT' THEN
    stale := to_jsonb(OLD);
    IF fresh IS NULL OR (stale ->> TG_ARGV[1]) IS DISTINCT FROM (fresh ->> TG_ARGV[1]) THEN
      PERFORM mark_task(TG_ARGV[0], (stale ->> TG_ARGV[1])::bigint);
    END IF;
  END IF;
  RETURN NULL;
END $$;

-- An event's post, reminders, cutoff and archive, and the calendars it is on.
CREATE TRIGGER events_tasks AFTER INSERT OR DELETE ON events
  FOR EACH ROW EXECUTE FUNCTION mark_row('event', 'id');
CREATE TRIGGER events_tasks_update AFTER UPDATE ON events
  FOR EACH ROW WHEN (
    (OLD.status, OLD.post_at, OLD.post_attempts, OLD.starts_at, OLD.duration_minutes, OLD.cutoff_at,
     OLD.signups_closed, OLD.title, OLD.description, OLD.location, OLD.channel_id, OLD.created_by)
    IS DISTINCT FROM
    (NEW.status, NEW.post_at, NEW.post_attempts, NEW.starts_at, NEW.duration_minutes, NEW.cutoff_at,
     NEW.signups_closed, NEW.title, NEW.description, NEW.location, NEW.channel_id, NEW.created_by))
  EXECUTE FUNCTION mark_row('event', 'id');
CREATE TRIGGER reminders_tasks AFTER INSERT OR DELETE ON reminders
  FOR EACH ROW EXECUTE FUNCTION mark_row('event', 'event_id');
CREATE TRIGGER reminders_tasks_update AFTER UPDATE ON reminders
  FOR EACH ROW WHEN (
    (OLD.event_id, OLD.offset_minutes, OLD.sent_at) IS DISTINCT FROM (NEW.event_id, NEW.offset_minutes, NEW.sent_at))
  EXECUTE FUNCTION mark_row('event', 'event_id');

-- A series date that comes, goes, posts or is skipped leaves room for the
-- next one: the server's series fill now rather than within the hour.
CREATE TRIGGER events_fill_insert AFTER INSERT ON events
  FOR EACH ROW WHEN (NEW.series_id IS NOT NULL)
  EXECUTE FUNCTION mark_row('fill', 'guild_id');
CREATE TRIGGER events_fill_delete AFTER DELETE ON events
  FOR EACH ROW WHEN (OLD.series_id IS NOT NULL)
  EXECUTE FUNCTION mark_row('fill', 'guild_id');
CREATE TRIGGER events_fill_update AFTER UPDATE ON events
  FOR EACH ROW WHEN (
    (OLD.series_id IS NOT NULL OR NEW.series_id IS NOT NULL)
    AND (OLD.status, OLD.series_id) IS DISTINCT FROM (NEW.status, NEW.series_id))
  EXECUTE FUNCTION mark_row('fill', 'guild_id');
CREATE TRIGGER series_tasks AFTER INSERT OR UPDATE OR DELETE ON series
  FOR EACH ROW EXECUTE FUNCTION mark_row('fill', 'guild_id');

-- A routine's polls, approvals and next week's run.
CREATE TRIGGER routines_tasks AFTER INSERT OR UPDATE OR DELETE ON routines
  FOR EACH ROW EXECUTE FUNCTION mark_row('routine', 'id');
CREATE TRIGGER routine_runs_tasks AFTER INSERT OR DELETE ON routine_runs
  FOR EACH ROW EXECUTE FUNCTION mark_row('routine', 'routine_id');
CREATE TRIGGER routine_runs_tasks_update AFTER UPDATE ON routine_runs
  FOR EACH ROW WHEN (
    (OLD.routine_id, OLD.status, OLD.opens_at, OLD.locks_at, OLD.decide_by)
    IS DISTINCT FROM (NEW.routine_id, NEW.status, NEW.opens_at, NEW.locks_at, NEW.decide_by))
  EXECUTE FUNCTION mark_row('routine', 'routine_id');

-- A server's clock: archive timing, the digest, its time zone.
CREATE TRIGGER settings_tasks AFTER INSERT ON settings
  FOR EACH ROW EXECUTE FUNCTION mark_row('guild', 'guild_id');
CREATE TRIGGER settings_tasks_update AFTER UPDATE ON settings
  FOR EACH ROW WHEN (
    (OLD.auto_archive, OLD.auto_archive_hours, OLD.digest)
    IS DISTINCT FROM (NEW.auto_archive, NEW.auto_archive_hours, NEW.digest))
  EXECUTE FUNCTION mark_row('guild', 'guild_id');
CREATE TRIGGER guilds_tasks_update AFTER UPDATE ON guilds
  FOR EACH ROW WHEN (OLD.timezone IS DISTINCT FROM NEW.timezone)
  EXECUTE FUNCTION mark_row('guild', 'id');

-- Google Calendar: a connection or its switch changed, or one event was
-- sent by hand.
CREATE TRIGGER google_accounts_tasks AFTER INSERT ON google_accounts
  FOR EACH ROW EXECUTE FUNCTION mark_row('google', 'user_id');
CREATE TRIGGER google_accounts_tasks_update AFTER UPDATE ON google_accounts
  FOR EACH ROW WHEN (
    (OLD.auto_sync, OLD.revoking, OLD.last_error, OLD.refresh_token)
    IS DISTINCT FROM (NEW.auto_sync, NEW.revoking, NEW.last_error, NEW.refresh_token))
  EXECUTE FUNCTION mark_row('google', 'user_id');
CREATE TRIGGER google_events_tasks AFTER INSERT ON google_events
  FOR EACH ROW WHEN (NEW.forced)
  EXECUTE FUNCTION mark_row('google', 'user_id');
CREATE TRIGGER google_events_tasks_update AFTER UPDATE ON google_events
  FOR EACH ROW WHEN (NEW.forced AND NOT OLD.forced)
  EXECUTE FUNCTION mark_row('google', 'user_id');

-- What puts an event in someone's calendar: a confirmed signup on an
-- attending option.
CREATE TRIGGER signups_tasks AFTER INSERT OR DELETE ON signups
  FOR EACH ROW EXECUTE FUNCTION mark_row('member', 'membership_id');
CREATE TRIGGER signups_tasks_update AFTER UPDATE ON signups
  FOR EACH ROW WHEN (
    (OLD.event_id, OLD.option_id, OLD.membership_id, OLD.state)
    IS DISTINCT FROM (NEW.event_id, NEW.option_id, NEW.membership_id, NEW.state))
  EXECUTE FUNCTION mark_row('member', 'membership_id');
CREATE TRIGGER signup_options_tasks AFTER INSERT OR DELETE ON signup_options
  FOR EACH ROW EXECUTE FUNCTION mark_row('event', 'event_id');
CREATE TRIGGER signup_options_tasks_update AFTER UPDATE ON signup_options
  FOR EACH ROW WHEN (OLD.type_id IS DISTINCT FROM NEW.type_id)
  EXECUTE FUNCTION mark_row('event', 'event_id');
CREATE TRIGGER option_library_tasks_update AFTER UPDATE ON option_library
  FOR EACH ROW WHEN (OLD.attending IS DISTINCT FROM NEW.attending)
  EXECUTE FUNCTION mark_row('people', 'guild_id');

-- Who sees which channel, and so which events stay in someone's calendar.
CREATE TRIGGER memberships_tasks AFTER INSERT OR DELETE ON memberships
  FOR EACH ROW EXECUTE FUNCTION mark_row('google', 'user_id');
CREATE TRIGGER memberships_tasks_update AFTER UPDATE ON memberships
  FOR EACH ROW WHEN (OLD.server_manager IS DISTINCT FROM NEW.server_manager)
  EXECUTE FUNCTION mark_row('member', 'id');
CREATE TRIGGER membership_roles_tasks AFTER INSERT OR DELETE ON membership_roles
  FOR EACH ROW EXECUTE FUNCTION mark_row('member', 'membership_id');
CREATE TRIGGER roles_tasks AFTER DELETE ON roles
  FOR EACH ROW EXECUTE FUNCTION mark_row('people', 'guild_id');
CREATE TRIGGER roles_tasks_update AFTER UPDATE ON roles
  FOR EACH ROW WHEN ((OLD.permissions, OLD.gone_at) IS DISTINCT FROM (NEW.permissions, NEW.gone_at))
  EXECUTE FUNCTION mark_row('people', 'guild_id');
CREATE TRIGGER guilds_people_update AFTER UPDATE ON guilds
  FOR EACH ROW WHEN (OLD.everyone_permissions IS DISTINCT FROM NEW.everyone_permissions)
  EXECUTE FUNCTION mark_row('people', 'id');
CREATE TRIGGER channels_tasks AFTER DELETE ON channels
  FOR EACH ROW EXECUTE FUNCTION mark_row('people', 'guild_id');
CREATE TRIGGER channels_tasks_update AFTER UPDATE ON channels
  FOR EACH ROW WHEN ((OLD.name, OLD.discord_id, OLD.gone_at) IS DISTINCT FROM (NEW.name, NEW.discord_id, NEW.gone_at))
  EXECUTE FUNCTION mark_row('channel', 'id');
CREATE TRIGGER channel_overwrites_tasks AFTER INSERT OR UPDATE OR DELETE ON channel_overwrites
  FOR EACH ROW EXECUTE FUNCTION mark_row('channel', 'channel_id');

-- Everything that exists now gets its tasks on the worker's first pass.
INSERT INTO task_marks (kind, subject_id) SELECT 'guild', id FROM guilds ON CONFLICT DO NOTHING;
INSERT INTO task_marks (kind, subject_id) SELECT 'google', user_id FROM google_accounts ON CONFLICT DO NOTHING;
