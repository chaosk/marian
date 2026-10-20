-- Rows the browser can point at get a public UUIDv7. The sequential id stays
-- inside the server; the API only ever shows and accepts public_id.

-- A UUIDv7: the first 48 bits are the Unix time in milliseconds, the rest are
-- random, with the version and variant bits set. Postgres 18 has uuidv7();
-- this does the same on 17.
CREATE FUNCTION marian_uuid_v7() RETURNS uuid
LANGUAGE sql VOLATILE
AS $$
  SELECT encode(
    set_bit(
      set_bit(
        overlay(uuid_send(gen_random_uuid())
          PLACING substring(int8send(floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint) FROM 3)
          FROM 1 FOR 6),
        52, 1),
      53, 1),
    'hex')::uuid
$$;

ALTER TABLE memberships ADD COLUMN public_id uuid NOT NULL DEFAULT marian_uuid_v7();
ALTER TABLE channels ADD COLUMN public_id uuid NOT NULL DEFAULT marian_uuid_v7();
ALTER TABLE roles ADD COLUMN public_id uuid NOT NULL DEFAULT marian_uuid_v7();
ALTER TABLE signup_options ADD COLUMN public_id uuid NOT NULL DEFAULT marian_uuid_v7();

CREATE UNIQUE INDEX idx_memberships_public_id ON memberships (public_id);
CREATE UNIQUE INDEX idx_channels_public_id ON channels (public_id);
CREATE UNIQUE INDEX idx_roles_public_id ON roles (public_id);
CREATE UNIQUE INDEX idx_signup_options_public_id ON signup_options (public_id);
