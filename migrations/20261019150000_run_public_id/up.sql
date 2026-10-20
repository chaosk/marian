-- A routine poll's Discord buttons name the poll by a public UUIDv7, never its row key.
ALTER TABLE routine_runs ADD COLUMN public_id uuid NOT NULL DEFAULT marian_uuid_v7();
CREATE UNIQUE INDEX idx_routine_runs_public_id ON routine_runs (public_id);
