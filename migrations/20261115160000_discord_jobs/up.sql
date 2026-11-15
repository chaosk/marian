-- Discord work waiting to be done: written in the same transaction as the
-- change that needs it, deleted once Discord has it. A restart picks up what's
-- left. `job` is the serialized DiscordJob, tries included; `run_at` is when
-- it's next due, later after each failure.
CREATE TABLE discord_jobs (
  id BIGSERIAL PRIMARY KEY,
  job JSONB NOT NULL,
  run_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_discord_jobs_run_at ON discord_jobs (run_at, id);
