-- PDF reader annotations: normalized highlight rectangles on the excerpt.
-- rects is a JSON array of {x,y,w,h} in 0..1 page-fraction coordinates, so
-- the same rects render at any zoom/scale; page (existing column) anchors it.
ALTER TABLE excerpts ADD COLUMN rects TEXT;
