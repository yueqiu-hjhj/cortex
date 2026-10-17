-- Existing manually entered memories remain explicit declarations.
ALTER TABLE user_memory ADD COLUMN layer TEXT NOT NULL DEFAULT 'declarative' CHECK(layer IN ('declarative','inferred'));
ALTER TABLE user_memory ADD COLUMN confidence REAL NOT NULL DEFAULT 1.0 CHECK(confidence BETWEEN 0 AND 1);
ALTER TABLE user_memory ADD COLUMN confirmed INTEGER NOT NULL DEFAULT 1 CHECK(confirmed IN (0,1));
