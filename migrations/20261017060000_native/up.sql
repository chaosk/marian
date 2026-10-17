CREATE FUNCTION marian_jsonb_bigint_array(value jsonb) RETURNS bigint[]
LANGUAGE sql IMMUTABLE AS $$
  SELECT COALESCE(array_agg(elem::bigint), '{}')
  FROM jsonb_array_elements_text(value) AS elem
$$;

CREATE FUNCTION marian_jsonb_smallint_array(value jsonb) RETURNS smallint[]
LANGUAGE sql IMMUTABLE AS $$
  SELECT COALESCE(array_agg(elem::smallint), '{}')
  FROM jsonb_array_elements_text(value) AS elem
$$;

ALTER TABLE event_templates
  ALTER COLUMN mention_role_ids DROP DEFAULT,
  ALTER COLUMN restrict_role_ids DROP DEFAULT;

ALTER TABLE event_templates
  ALTER COLUMN mention_role_ids TYPE bigint[] USING marian_jsonb_bigint_array(mention_role_ids),
  ALTER COLUMN restrict_role_ids TYPE bigint[] USING marian_jsonb_bigint_array(restrict_role_ids),
  ALTER COLUMN mention_role_ids SET DEFAULT '{}'::bigint[],
  ALTER COLUMN restrict_role_ids SET DEFAULT '{}'::bigint[];

ALTER TABLE routines
  ALTER COLUMN days_json TYPE smallint[] USING marian_jsonb_smallint_array(days_json);

ALTER TABLE routine_runs
  ALTER COLUMN days_json TYPE smallint[] USING marian_jsonb_smallint_array(days_json);

DROP FUNCTION marian_jsonb_bigint_array(jsonb);
DROP FUNCTION marian_jsonb_smallint_array(jsonb);

ALTER TABLE events
  ALTER COLUMN public_id TYPE uuid USING public_id::uuid;

ALTER TABLE series
  ALTER COLUMN public_id TYPE uuid USING public_id::uuid;

ALTER TABLE routines
  ALTER COLUMN public_id TYPE uuid USING public_id::uuid;

ALTER TABLE presets
  ALTER COLUMN public_id TYPE uuid USING public_id::uuid;
