-- MCP clients (Claude and the like) that registered themselves with Marian's
-- OAuth server. A public client: it has no secret and proves itself with PKCE.
CREATE TABLE mcp_clients (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  redirect_uris TEXT[] NOT NULL,
  created_at TIMESTAMPTZ NOT NULL
);

-- One sign-in from an MCP client, from its authorize request until its code
-- is swapped for a token. `code`, `session_token` and the Discord grant are
-- set once the person allows it and Discord confirms who they are.
CREATE TABLE mcp_authorizations (
  id TEXT PRIMARY KEY,
  client_id TEXT NOT NULL REFERENCES mcp_clients(id) ON DELETE CASCADE,
  redirect_uri TEXT NOT NULL,
  code_challenge TEXT NOT NULL,
  state TEXT,
  created_at TIMESTAMPTZ NOT NULL,
  code TEXT UNIQUE,
  -- The browser sign-in that allowed it, which the token copies.
  session_token TEXT REFERENCES web_sessions(token) ON DELETE CASCADE,
  -- A Discord grant of the client's own, so its sign-in re-reads the
  -- person's servers like a browser's does.
  discord_access TEXT,
  discord_refresh TEXT,
  discord_expires_at TIMESTAMPTZ
);

-- A sign-in an MCP client holds, set: it answers only at /mcp, and the
-- browser session reads only those without one.
ALTER TABLE web_sessions
  ADD COLUMN mcp_client TEXT REFERENCES mcp_clients(id) ON DELETE CASCADE;
