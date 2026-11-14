-- The member's Discord picture: their server avatar, else their account's,
-- as a CDN URL. Null shows their initial instead.
ALTER TABLE memberships ADD COLUMN avatar TEXT;
