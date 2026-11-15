-- The newest observation of each guild in each season. Every refresh asked for it, and
-- answering from season_guild_observations meant reading a guild's whole history.
CREATE TABLE season_guild_latest (
    season_id INTEGER NOT NULL,
    guild_name TEXT NOT NULL,
    guild_uuid TEXT,
    guild_prefix TEXT,
    territory_count SMALLINT NOT NULL,
    season_rating INTEGER NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (season_id, guild_name)
);

CREATE INDEX idx_season_guild_latest_guild_observed_desc
    ON season_guild_latest (guild_name, observed_at DESC);

-- Every writer of season_guild_observations keeps the table current through this trigger.
CREATE FUNCTION track_season_guild_latest() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO season_guild_latest
        (season_id, guild_name, guild_uuid, guild_prefix, territory_count, season_rating,
         observed_at)
    SELECT DISTINCT ON (season_id, guild_name)
        season_id, guild_name, guild_uuid, guild_prefix, territory_count, season_rating,
        observed_at
    FROM inserted
    ORDER BY season_id, guild_name, observed_at DESC
    ON CONFLICT (season_id, guild_name) DO UPDATE
    SET guild_uuid = EXCLUDED.guild_uuid,
        guild_prefix = EXCLUDED.guild_prefix,
        territory_count = EXCLUDED.territory_count,
        season_rating = EXCLUDED.season_rating,
        observed_at = EXCLUDED.observed_at
    WHERE season_guild_latest.observed_at <= EXCLUDED.observed_at;
    RETURN NULL;
END;
$$;

CREATE TRIGGER track_season_guild_latest
AFTER INSERT ON season_guild_observations
REFERENCING NEW TABLE AS inserted
FOR EACH STATEMENT
EXECUTE FUNCTION track_season_guild_latest();

-- One pass over the existing history; the sort spills less with more memory.
SET LOCAL work_mem = '64MB';

INSERT INTO season_guild_latest
    (season_id, guild_name, guild_uuid, guild_prefix, territory_count, season_rating, observed_at)
SELECT DISTINCT ON (season_id, guild_name)
    season_id, guild_name, guild_uuid, guild_prefix, territory_count, season_rating, observed_at
FROM season_guild_observations
ORDER BY season_id, guild_name, observed_at DESC;
