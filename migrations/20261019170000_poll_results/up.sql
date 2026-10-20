-- The reply a locked poll posts with its results, under the post the event took over.
ALTER TABLE routine_runs ADD COLUMN results_message_id TEXT;
