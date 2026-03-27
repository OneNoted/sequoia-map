CREATE TABLE IF NOT EXISTS season_metadata (
    season_id INTEGER PRIMARY KEY,
    label TEXT,
    start_at TIMESTAMPTZ NOT NULL,
    end_at TIMESTAMPTZ NOT NULL,
    source TEXT NOT NULL DEFAULT 'configured',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (source IN ('configured', 'inferred')),
    CHECK (end_at > start_at)
);
