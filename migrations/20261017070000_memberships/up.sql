ALTER TABLE users RENAME TO memberships;
ALTER TABLE accounts RENAME TO users;
ALTER TABLE memberships RENAME COLUMN account_id TO user_id;
ALTER TABLE memberships RENAME CONSTRAINT users_one_membership_per_account TO memberships_one_per_user;

ALTER TABLE user_roles RENAME COLUMN user_id TO membership_id;
ALTER TABLE signups RENAME COLUMN user_id TO membership_id;
ALTER TABLE thread_members RENAME COLUMN user_id TO membership_id;
ALTER TABLE role_grants RENAME COLUMN user_id TO membership_id;
ALTER TABLE command_log RENAME COLUMN user_id TO membership_id;
ALTER TABLE event_prompts RENAME COLUMN user_id TO membership_id;
ALTER TABLE web_sessions RENAME COLUMN user_id TO membership_id;
ALTER TABLE routine_votes RENAME COLUMN user_id TO membership_id;
