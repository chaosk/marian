ALTER TABLE users DROP CONSTRAINT users_one_membership_per_account;
ALTER TABLE users ADD COLUMN discord_id TEXT;

UPDATE users AS membership
SET discord_id = accounts.discord_id
FROM accounts
WHERE accounts.id = membership.account_id;

ALTER TABLE users DROP COLUMN account_id;
DROP TABLE accounts;
