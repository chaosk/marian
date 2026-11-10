-- A routine is a list of steps: a date poll first, the event's post last, and
-- approvals in between. Every routine so far polls and then posts.
CREATE TABLE routine_steps (
  id BIGSERIAL PRIMARY KEY,
  routine_id BIGINT NOT NULL REFERENCES routines (id) ON DELETE CASCADE,
  position BIGINT NOT NULL,
  -- 'poll', 'approval' or 'post'
  kind TEXT NOT NULL,
  -- Where an approval asks, and who it mentions and lets decide.
  channel_id BIGINT REFERENCES channels (id) ON DELETE SET NULL,
  role_ids BIGINT[] NOT NULL DEFAULT '{}',
  UNIQUE (routine_id, position)
);

INSERT INTO routine_steps (routine_id, position, kind)
SELECT id, 0, 'poll' FROM routines
UNION ALL
SELECT id, 1, 'post' FROM routines;

-- Which step a week's run is on, and an approval's request and outcome.
ALTER TABLE routine_runs ADD COLUMN step BIGINT NOT NULL DEFAULT 0;
ALTER TABLE routine_runs ADD COLUMN approval_message_id TEXT;
ALTER TABLE routine_runs ADD COLUMN decide_by TIMESTAMPTZ;
ALTER TABLE routine_runs ADD COLUMN decided_by BIGINT REFERENCES memberships (id) ON DELETE SET NULL;
