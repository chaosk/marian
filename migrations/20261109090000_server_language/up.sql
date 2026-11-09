-- The language Marian writes in on Discord for this server.
ALTER TABLE settings ADD COLUMN language TEXT NOT NULL DEFAULT 'en';
