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

-- One pass over the table finds each guild's newest timestamp per season; the primary key
-- (observed_at, guild_name) then fetches those rows. Sorting the whole table by guild
-- instead took over a minute on production.
INSERT INTO season_guild_latest
    (season_id, guild_name, guild_uuid, guild_prefix, territory_count, season_rating, observed_at)
SELECT observation.season_id, observation.guild_name, observation.guild_uuid,
    observation.guild_prefix, observation.territory_count, observation.season_rating,
    observation.observed_at
FROM (
    SELECT season_id, guild_name, MAX(observed_at) AS observed_at
    FROM season_guild_observations
    GROUP BY season_id, guild_name
) newest
JOIN season_guild_observations observation
    ON observation.observed_at = newest.observed_at
   AND observation.guild_name = newest.guild_name;
