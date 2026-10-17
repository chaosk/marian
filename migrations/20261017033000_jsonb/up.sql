ALTER TABLE event_templates
  ALTER COLUMN options_json DROP DEFAULT,
  ALTER COLUMN reminders_json DROP DEFAULT,
  ALTER COLUMN mention_role_ids DROP DEFAULT,
  ALTER COLUMN restrict_role_ids DROP DEFAULT;

ALTER TABLE event_templates
  ALTER COLUMN options_json TYPE jsonb USING options_json::jsonb,
  ALTER COLUMN reminders_json TYPE jsonb USING reminders_json::jsonb,
  ALTER COLUMN mention_role_ids TYPE jsonb USING mention_role_ids::jsonb,
  ALTER COLUMN restrict_role_ids TYPE jsonb USING restrict_role_ids::jsonb,
  ALTER COLUMN options_json SET DEFAULT '[]'::jsonb,
  ALTER COLUMN reminders_json SET DEFAULT '[]'::jsonb,
  ALTER COLUMN mention_role_ids SET DEFAULT '[]'::jsonb,
  ALTER COLUMN restrict_role_ids SET DEFAULT '[]'::jsonb;

ALTER TABLE routines
  ALTER COLUMN days_json TYPE jsonb USING days_json::jsonb;

ALTER TABLE routine_runs
  ALTER COLUMN days_json TYPE jsonb USING days_json::jsonb;

ALTER TABLE presets
  ALTER COLUMN body TYPE jsonb USING body::jsonb;

ALTER TABLE event_prompts
  ALTER COLUMN draft TYPE jsonb USING
    CASE
      WHEN draft IS NULL OR btrim(draft) = '' THEN NULL
      ELSE draft::jsonb
    END;
