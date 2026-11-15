-- The attendee roles Marian made itself, which anyone making an event may
-- pick. Before this column, those were the roles not on Discord yet, and the
-- ones Marian created there: an event's attendee role in Marian's colour.
ALTER TABLE roles ADD COLUMN made_by_marian BOOLEAN NOT NULL DEFAULT false;
UPDATE roles SET made_by_marian = true
WHERE discord_id IS NULL
   OR (lower(color) = '#1e3a6e'
       AND (id IN (SELECT attendee_role_id FROM events WHERE attendee_role_id IS NOT NULL)
            OR id IN (SELECT attendee_role_id FROM event_templates WHERE attendee_role_id IS NOT NULL)));

-- Who picked an event's attendee role. Marian gives a role it didn't make
-- only while that person manages the server.
ALTER TABLE events ADD COLUMN attendee_role_by BIGINT REFERENCES memberships(id) ON DELETE SET NULL;
UPDATE events SET attendee_role_by = created_by WHERE attendee_role_id IS NOT NULL;

-- The same for a series' or routine's template, so the dates it makes carry it.
ALTER TABLE event_templates ADD COLUMN attendee_role_by BIGINT REFERENCES memberships(id) ON DELETE SET NULL;
UPDATE event_templates t SET attendee_role_by = r.created_by
FROM routines r
WHERE r.template_id = t.id AND t.attendee_role_id IS NOT NULL;
UPDATE event_templates t SET attendee_role_by = (
    SELECT e.created_by FROM series s JOIN events e ON e.series_id = s.id
    WHERE s.template_id = t.id AND e.created_by IS NOT NULL
    ORDER BY e.occurrence_index NULLS LAST, e.id
    LIMIT 1
)
WHERE t.attendee_role_id IS NOT NULL AND t.attendee_role_by IS NULL;

-- Clearing these when a member is deleted shouldn't scan the tables.
CREATE INDEX idx_events_attendee_role_by ON events (attendee_role_by);
CREATE INDEX idx_event_templates_attendee_role_by ON event_templates (attendee_role_by);
