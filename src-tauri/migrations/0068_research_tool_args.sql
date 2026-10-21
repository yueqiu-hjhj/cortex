-- Research chat: persist what each tool call was ASKED for.
--
-- The tool result was already stored; the arguments were not, which made
-- the transcript show "what came back" without "what was searched" — the
-- half you need to judge whether the right question was asked.
ALTER TABLE research_messages ADD COLUMN args TEXT;
