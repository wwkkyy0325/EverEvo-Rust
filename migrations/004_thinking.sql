-- Add thinking column to messages for persisted chain-of-thought.
ALTER TABLE messages ADD COLUMN thinking TEXT NOT NULL DEFAULT '';
