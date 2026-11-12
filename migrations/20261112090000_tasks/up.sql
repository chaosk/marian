-- Work Marian does at a set time, one row per thing: post an event, send a
-- reminder, close signups, archive, move a routine on, send a digest, fill
-- series, or sync someone's Google Calendar. The worker sleeps until the
-- earliest row instead of scanning every event every few seconds.
CREATE TABLE tasks (
  id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  kind TEXT NOT NULL,
  subject_id BIGINT NOT NULL,
  run_at TIMESTAMPTZ NOT NULL,
  UNIQUE (kind, subject_id)
);
CREATE INDEX tasks_due ON tasks (run_at);

-- What changed since the worker last looked: 'event', 'routine', 'guild',
-- 'google' (a user), 'member', 'channel' or 'people' (everyone in a server,
-- for their calendars). Triggers fill this, so a change from any code path
-- moves the tasks that depend on it.
CREATE TABLE task_marks (
  kind TEXT NOT NULL,
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
     OLD.signups_closed, OLD.title, OLD.description, OLD.location, OLD.channel_id)
    IS DISTINCT FROM
    (NEW.status, NEW.post_at, NEW.post_attempts, NEW.starts_at, NEW.duration_minutes, NEW.cutoff_at,
     NEW.signups_closed, NEW.title, NEW.description, NEW.location, NEW.channel_id))
  EXECUTE FUNCTION mark_row('event', 'id');
CREATE TRIGGER reminders_tasks AFTER INSERT OR UPDATE OR DELETE ON reminders
  FOR EACH ROW EXECUTE FUNCTION mark_row('event', 'event_id');

-- A signup puts an event in, or takes it out of, that person's calendar.
CREATE TRIGGER signups_tasks AFTER INSERT OR UPDATE OR DELETE ON signups
  FOR EACH ROW EXECUTE FUNCTION mark_row('member', 'membership_id');

-- A routine's polls, approvals and next week's run.
CREATE TRIGGER routines_tasks AFTER INSERT OR UPDATE ON routines
  FOR EACH ROW EXECUTE FUNCTION mark_row('routine', 'id');
CREATE TRIGGER routine_runs_tasks AFTER INSERT OR DELETE ON routine_runs
  FOR EACH ROW EXECUTE FUNCTION mark_row('routine', 'routine_id');
CREATE TRIGGER routine_runs_tasks_update AFTER UPDATE ON routine_runs
  FOR EACH ROW WHEN (
    (OLD.status, OLD.opens_at, OLD.locks_at, OLD.decide_by)
    IS DISTINCT FROM (NEW.status, NEW.opens_at, NEW.locks_at, NEW.decide_by))
  EXECUTE FUNCTION mark_row('routine', 'routine_id');

-- A server's clock: archive timing, the digest, series fills, its time zone.
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
CREATE TRIGGER series_tasks AFTER INSERT OR DELETE ON series
  FOR EACH ROW EXECUTE FUNCTION mark_row('guild', 'guild_id');
CREATE TRIGGER series_tasks_update AFTER UPDATE ON series
  FOR EACH ROW WHEN (OLD.active IS DISTINCT FROM NEW.active)
  EXECUTE FUNCTION mark_row('guild', 'guild_id');

-- Google Calendar: a connection or its switch changed, or one event was sent by hand.
CREATE TRIGGER google_accounts_tasks AFTER INSERT ON google_accounts
  FOR EACH ROW EXECUTE FUNCTION mark_row('google', 'user_id');
CREATE TRIGGER google_accounts_tasks_update AFTER UPDATE ON google_accounts
  FOR EACH ROW WHEN (
    (OLD.auto_sync, OLD.revoking, OLD.last_error) IS DISTINCT FROM (NEW.auto_sync, NEW.revoking, NEW.last_error))
  EXECUTE FUNCTION mark_row('google', 'user_id');
CREATE TRIGGER google_events_tasks AFTER INSERT ON google_events
  FOR EACH ROW EXECUTE FUNCTION mark_row('google', 'user_id');
CREATE TRIGGER google_events_tasks_update AFTER UPDATE ON google_events
  FOR EACH ROW WHEN (OLD.forced IS DISTINCT FROM NEW.forced AND NEW.forced)
  EXECUTE FUNCTION mark_row('google', 'user_id');

-- Who sees which channel decides which events land in someone's calendar.
CREATE TRIGGER membership_roles_tasks AFTER INSERT OR DELETE ON membership_roles
  FOR EACH ROW EXECUTE FUNCTION mark_row('member', 'membership_id');
CREATE TRIGGER memberships_tasks_update AFTER UPDATE ON memberships
  FOR EACH ROW WHEN (OLD.server_manager IS DISTINCT FROM NEW.server_manager)
  EXECUTE FUNCTION mark_row('member', 'id');
CREATE TRIGGER channel_overwrites_tasks AFTER INSERT OR UPDATE OR DELETE ON channel_overwrites
  FOR EACH ROW EXECUTE FUNCTION mark_row('channel', 'channel_id');
CREATE TRIGGER roles_tasks_update AFTER UPDATE ON roles
  FOR EACH ROW WHEN (OLD.permissions IS DISTINCT FROM NEW.permissions)
  EXECUTE FUNCTION mark_row('people', 'guild_id');
CREATE TRIGGER guilds_people_update AFTER UPDATE ON guilds
  FOR EACH ROW WHEN (OLD.everyone_permissions IS DISTINCT FROM NEW.everyone_permissions)
  EXECUTE FUNCTION mark_row('people', 'id');

-- Everything that exists now gets its tasks on the worker's first pass.
INSERT INTO task_marks (kind, subject_id) SELECT 'guild', id FROM guilds ON CONFLICT DO NOTHING;
INSERT INTO task_marks (kind, subject_id) SELECT 'google', user_id FROM google_accounts ON CONFLICT DO NOTHING;
