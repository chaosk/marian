ALTER TABLE routine_votes RENAME COLUMN membership_id TO user_id;
ALTER TABLE web_sessions RENAME COLUMN membership_id TO user_id;
ALTER TABLE event_prompts RENAME COLUMN membership_id TO user_id;
ALTER TABLE command_log RENAME COLUMN membership_id TO user_id;
ALTER TABLE role_grants RENAME COLUMN membership_id TO user_id;
ALTER TABLE thread_members RENAME COLUMN membership_id TO user_id;
ALTER TABLE signups RENAME COLUMN membership_id TO user_id;
ALTER TABLE user_roles RENAME COLUMN membership_id TO user_id;

ALTER TABLE memberships RENAME CONSTRAINT memberships_one_per_user TO users_one_membership_per_account;
ALTER TABLE memberships RENAME COLUMN user_id TO account_id;
ALTER TABLE users RENAME TO accounts;
ALTER TABLE memberships RENAME TO users;
