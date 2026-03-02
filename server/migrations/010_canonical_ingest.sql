CREATE TABLE canonical_territory_updates (
    id BIGSERIAL PRIMARY KEY,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    territory TEXT NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL,
    confidence DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    visibility TEXT NOT NULL DEFAULT 'public',
    source TEXT NOT NULL DEFAULT 'unknown',
    reporter_count INTEGER NOT NULL DEFAULT 0,
    idempotency_key TEXT,
    payload JSONB NOT NULL
);

CREATE INDEX idx_canonical_territory_updates_observed_desc
    ON canonical_territory_updates (observed_at DESC);

CREATE INDEX idx_canonical_territory_updates_territory_observed
    ON canonical_territory_updates (territory, observed_at DESC);

CREATE UNIQUE INDEX idx_canonical_territory_updates_idempotency
    ON canonical_territory_updates (idempotency_key)
    WHERE idempotency_key IS NOT NULL;

CREATE TABLE canonical_war_events (
    id BIGSERIAL PRIMARY KEY,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    territory TEXT NOT NULL,
    kind TEXT NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL,
    confidence DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    visibility TEXT NOT NULL DEFAULT 'public',
    source TEXT NOT NULL DEFAULT 'unknown',
    reporter_count INTEGER NOT NULL DEFAULT 0,
    idempotency_key TEXT,
    payload JSONB NOT NULL
);

CREATE INDEX idx_canonical_war_events_observed_desc
    ON canonical_war_events (observed_at DESC);

CREATE INDEX idx_canonical_war_events_territory_observed
    ON canonical_war_events (territory, observed_at DESC);

CREATE UNIQUE INDEX idx_canonical_war_events_idempotency
    ON canonical_war_events (idempotency_key)
    WHERE idempotency_key IS NOT NULL;
