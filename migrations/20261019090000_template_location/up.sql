-- A series or routine keeps where its events happen, like the rest of their details.
ALTER TABLE event_templates ADD COLUMN location TEXT NOT NULL DEFAULT '';
