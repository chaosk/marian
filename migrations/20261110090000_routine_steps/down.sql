ALTER TABLE routine_runs DROP COLUMN decided_by;
ALTER TABLE routine_runs DROP COLUMN decide_by;
ALTER TABLE routine_runs DROP COLUMN approval_message_id;
ALTER TABLE routine_runs DROP COLUMN step;
DROP TABLE routine_steps;
