-- Answer-state provenance for chat messages (KnowNote five-state borrow):
-- completed | truncated | blocked | aborted | failed. A truncated answer
-- keeps its partial text and the UI invites "继续"; a blocked one was
-- stopped by a safety filter; aborted is the user pressing stop; failed
-- means the model returned nothing usable. NULL for user/system rows and
-- pre-0077 messages (no retroactive guessing about old answers).
ALTER TABLE chat_messages ADD COLUMN answer_state TEXT;
