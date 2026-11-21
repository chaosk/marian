DELETE FROM web_sessions WHERE mcp_client IS NOT NULL;
ALTER TABLE web_sessions DROP COLUMN mcp_client;
DROP TABLE mcp_authorizations;
DROP TABLE mcp_clients;
