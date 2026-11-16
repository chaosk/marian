-- An event cancelled with Cancel gets one reply in its thread, or under its
-- post when it has no thread, saying so. The row asks for that reply and
-- remembers where it went, so reinstating the event can take it back. An
-- event cancelled by deleting it has no row and gets no reply.
CREATE TABLE cancel_notices (
  event_id BIGINT PRIMARY KEY REFERENCES events (id) ON DELETE CASCADE,
  -- Where the reply is: the thread or channel, and the message. Both empty
  -- until it's posted.
  discord_channel_id TEXT,
  discord_message_id TEXT
);
