-- Roles used to be matched by name, so renaming a role in Discord stored it a
-- second time under the same Discord id. Fold each duplicate into the oldest
-- row, pointing everything that named a duplicate at that row.
CREATE TABLE role_merge AS
SELECT role.id AS dup, kept.keep
FROM roles role
JOIN (
  SELECT guild_id, discord_id, min(id) AS keep
  FROM roles
  WHERE discord_id IS NOT NULL AND discord_id <> ''
  GROUP BY guild_id, discord_id
  HAVING count(*) > 1
) kept ON kept.guild_id = role.guild_id AND kept.discord_id = role.discord_id AND role.id <> kept.keep;

INSERT INTO membership_roles (membership_id, role_id)
SELECT DISTINCT held.membership_id, merge.keep
FROM membership_roles held JOIN role_merge merge ON held.role_id = merge.dup
ON CONFLICT DO NOTHING;

INSERT INTO permission_roles (guild_id, kind, role_id)
SELECT DISTINCT granted.guild_id, granted.kind, merge.keep
FROM permission_roles granted JOIN role_merge merge ON granted.role_id = merge.dup
ON CONFLICT DO NOTHING;

UPDATE event_roles SET role_id = merge.keep FROM role_merge merge WHERE event_roles.role_id = merge.dup;
DELETE FROM event_roles later USING event_roles earlier
WHERE later.ctid > earlier.ctid
  AND later.event_id = earlier.event_id
  AND later.role_id = earlier.role_id
  AND later.kind = earlier.kind;

UPDATE role_grants SET role_id = merge.keep FROM role_merge merge WHERE role_grants.role_id = merge.dup;
UPDATE events SET attendee_role_id = merge.keep FROM role_merge merge WHERE events.attendee_role_id = merge.dup;
UPDATE event_templates SET attendee_role_id = merge.keep FROM role_merge merge WHERE event_templates.attendee_role_id = merge.dup;
UPDATE signup_options SET role_id = merge.keep FROM role_merge merge WHERE signup_options.role_id = merge.dup;
UPDATE preset_options SET role_id = merge.keep FROM role_merge merge WHERE preset_options.role_id = merge.dup;
UPDATE template_options SET role_id = merge.keep FROM role_merge merge WHERE template_options.role_id = merge.dup;

-- Role lists keep their order, with a duplicate and its kept row listed once.
UPDATE event_templates SET
  mention_role_ids = ARRAY(
    SELECT id FROM (
      SELECT coalesce(merge.keep, listed.id) AS id, min(listed.at) AS at
      FROM unnest(mention_role_ids) WITH ORDINALITY AS listed(id, at)
      LEFT JOIN role_merge merge ON merge.dup = listed.id
      GROUP BY 1
    ) folded ORDER BY at
  ),
  restrict_role_ids = ARRAY(
    SELECT id FROM (
      SELECT coalesce(merge.keep, listed.id) AS id, min(listed.at) AS at
      FROM unnest(restrict_role_ids) WITH ORDINALITY AS listed(id, at)
      LEFT JOIN role_merge merge ON merge.dup = listed.id
      GROUP BY 1
    ) folded ORDER BY at
  )
WHERE mention_role_ids && ARRAY(SELECT dup FROM role_merge)
   OR restrict_role_ids && ARRAY(SELECT dup FROM role_merge);

DELETE FROM roles WHERE id IN (SELECT dup FROM role_merge);
DROP TABLE role_merge;

CREATE UNIQUE INDEX idx_roles_discord_id ON roles (guild_id, discord_id)
WHERE discord_id IS NOT NULL AND discord_id <> '';

-- A role deleted in Discord keeps its row for the events and settings that
-- name it, but drops out of the role lists.
ALTER TABLE roles ADD COLUMN gone_at TIMESTAMPTZ;
