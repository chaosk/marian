-- +X: an option may let people bring guests, up to its limit (0 lets none),
-- and a signup carries how many it brings. A signup takes 1 + guests places.
ALTER TABLE signup_options ADD COLUMN max_guests BIGINT NOT NULL DEFAULT 0 CHECK (max_guests >= 0);
ALTER TABLE preset_options ADD COLUMN max_guests BIGINT NOT NULL DEFAULT 0 CHECK (max_guests >= 0);
ALTER TABLE template_options ADD COLUMN max_guests BIGINT NOT NULL DEFAULT 0 CHECK (max_guests >= 0);
ALTER TABLE signups ADD COLUMN guests BIGINT NOT NULL DEFAULT 0 CHECK (guests >= 0);
