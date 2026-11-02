-- Official-API territory state history: the fields that changed at a poll's stream position,
-- per territory, as a format byte and postcard (`server/src/history/state.rs`). Owner and
-- acquisition stay in history_fact; a checkpoint's `states` holds each territory's full state.
-- history_meta's earliest_at, latest_at and latest_fact_seq cover these rows as well, so the
-- history ends at the latest change of either kind; its counts remain ownership facts.
CREATE TABLE history_api_state (
    stream_seq BIGINT PRIMARY KEY,
    recorded_at TIMESTAMPTZ NOT NULL,
    -- When the poller received the payload these values came from.
    observed_at TIMESTAMPTZ NOT NULL,
    changes BYTEA NOT NULL
);

CREATE INDEX history_api_state_recorded ON history_api_state USING brin (recorded_at);

-- Kept apart from `ownership`, whose bytes servers without states still read. NULL where no
-- state was recorded, as before this migration or when an older server wrote the checkpoint.
ALTER TABLE history_checkpoint ADD COLUMN states BYTEA;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_trigger
        WHERE tgrelid = 'history_fact'::regclass AND NOT tgisinternal
          AND tgfoid = 'prevent_history_table_delete'::regproc
    ) THEN
        CREATE TRIGGER prevent_delete_history_api_state BEFORE DELETE ON history_api_state
            FOR EACH ROW EXECUTE FUNCTION prevent_history_table_delete();
    END IF;
EXCEPTION WHEN undefined_function THEN
    NULL;
END $$;
