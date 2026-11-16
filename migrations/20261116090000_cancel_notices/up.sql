-- Deleting a posted event cancels it, and its thread (or its post, when it
-- has no thread) gets one reply saying so. The row asks for that reply and
-- remembers where it went, so reinstating the event can take it back.
CREATE TABLE cancel_notices (
  event_id BIGINT PRIMARY KEY REFERENCES events (id) ON DELETE CASCADE,
  -- Where the reply is: the thread or channel, and the message. Both empty
  -- until it's posted.
  discord_channel_id TEXT,
  discord_message_id TEXT
);
