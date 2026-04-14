-- Instance keypair (singleton, generated on first run)
CREATE TABLE IF NOT EXISTS instance_keys (
    id INTEGER PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    private_key_pem TEXT NOT NULL,
    public_key_pem  TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Cache of remote AP actors
CREATE TABLE IF NOT EXISTS actor_cache (
    id                  TEXT PRIMARY KEY,
    preferred_username  TEXT NOT NULL,
    display_name        TEXT,
    avatar_url          TEXT,
    profile_url         TEXT,
    public_key_id       TEXT NOT NULL,
    public_key_pem      TEXT NOT NULL,
    inbox_url           TEXT NOT NULL,
    shared_inbox_url    TEXT,
    instance            TEXT NOT NULL,
    raw_data            JSONB NOT NULL,
    fetched_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Actors that follow us
CREATE TABLE IF NOT EXISTS followers (
    actor_id    TEXT PRIMARY KEY,
    inbox_url   TEXT NOT NULL,
    accepted    BOOLEAN NOT NULL DEFAULT TRUE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Deduplication log for processed AP activities (30-day retention sufficient)
CREATE TABLE IF NOT EXISTS processed_activities (
    activity_id  TEXT PRIMARY KEY,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_processed_activities_created
    ON processed_activities (created_at);
