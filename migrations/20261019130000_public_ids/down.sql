ALTER TABLE signup_options DROP COLUMN public_id;
ALTER TABLE roles DROP COLUMN public_id;
ALTER TABLE channels DROP COLUMN public_id;
ALTER TABLE memberships DROP COLUMN public_id;
DROP FUNCTION marian_uuid_v7();
