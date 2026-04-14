-- Multi-user schema: each local actor is a row in `users`, with its own RSA
-- keypair in `user_keys`. Posts and followers scope by user_id.
--
-- Backfill: if a legacy `instance_keys` row exists, migrate it to a placeholder
-- user named `_bootstrap`; application code renames it to
-- `config.instance.username` on first boot.

CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS citext;

-- ── users ────────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS users (
    id            UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    -- CITEXT = case-insensitive TEXT; usernames are compared case-insensitively
    -- in WebFinger/AP, but stored in their original form.
    username      CITEXT NOT NULL UNIQUE,
    display_name  TEXT NOT NULL DEFAULT '',
    summary       TEXT NOT NULL DEFAULT '',
    -- Per-user bearer token for the admin API. NULL means "no per-user token"
    -- (OSS mode falls back to the global admin token; the SaaS layer ignores
    -- this column entirely and authenticates via Supabase JWT).
    api_token     TEXT,
    -- Subscription tier — kept in core so quota middleware in the SaaS layer
    -- can read it without a JOIN. OSS installs always stay at 'self_host'.
    plan_tier     TEXT NOT NULL DEFAULT 'self_host',
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- ── user_keys (replaces singleton instance_keys) ─────────────────────────────
CREATE TABLE IF NOT EXISTS user_keys (
    user_id          UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    private_key_pem  TEXT NOT NULL,
    public_key_pem   TEXT NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- ── Legacy migration from singleton instance_keys ────────────────────────────
-- If instance_keys exists and has a row, move that keypair to a `_bootstrap`
-- user. Application code reconciles `_bootstrap` → config.instance.username
-- on first boot.
DO $$
DECLARE
    v_user_id UUID;
    v_priv    TEXT;
    v_pub     TEXT;
BEGIN
    IF EXISTS (SELECT 1 FROM information_schema.tables
               WHERE table_schema = 'public' AND table_name = 'instance_keys')
       AND EXISTS (SELECT 1 FROM instance_keys WHERE id = 1) THEN

        SELECT private_key_pem, public_key_pem
          INTO v_priv, v_pub
          FROM instance_keys WHERE id = 1;

        INSERT INTO users (username, display_name, summary)
        VALUES ('_bootstrap', 'bootstrap',
                'Placeholder user migrated from legacy singleton; will be renamed on next boot.')
        ON CONFLICT (username) DO UPDATE SET updated_at = NOW()
        RETURNING id INTO v_user_id;

        INSERT INTO user_keys (user_id, private_key_pem, public_key_pem)
        VALUES (v_user_id, v_priv, v_pub)
        ON CONFLICT (user_id) DO NOTHING;
    END IF;
END $$;

-- ── posts: add user_id ───────────────────────────────────────────────────────
ALTER TABLE posts
    ADD COLUMN IF NOT EXISTS user_id UUID REFERENCES users(id) ON DELETE CASCADE;

UPDATE posts
   SET user_id = (SELECT id FROM users WHERE username = '_bootstrap' LIMIT 1)
 WHERE user_id IS NULL;

-- If user_id is still NULL after backfill, there are no legacy posts and no
-- legacy user; application code will assign user_id on the first sync.
-- SET NOT NULL only when we're sure everything's filled.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM posts WHERE user_id IS NULL) THEN
        ALTER TABLE posts ALTER COLUMN user_id SET NOT NULL;
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS idx_posts_user ON posts (user_id);

-- ── followers: add user_id and repin PK to (user_id, actor_id) ───────────────
ALTER TABLE followers
    ADD COLUMN IF NOT EXISTS user_id UUID REFERENCES users(id) ON DELETE CASCADE;

UPDATE followers
   SET user_id = (SELECT id FROM users WHERE username = '_bootstrap' LIMIT 1)
 WHERE user_id IS NULL;

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM followers WHERE user_id IS NULL) THEN
        ALTER TABLE followers ALTER COLUMN user_id SET NOT NULL;
    END IF;
END $$;

-- Repin PK so (user_id, actor_id) is unique — the same remote actor may
-- follow multiple local users.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'followers'::regclass AND contype = 'p'
          AND conname = 'followers_pkey'
    ) AND NOT EXISTS (
        -- only repin if the PK is currently the single-column actor_id
        SELECT 1 FROM pg_index i
        JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey)
        WHERE i.indrelid = 'followers'::regclass AND i.indisprimary
        GROUP BY i.indrelid
        HAVING COUNT(*) > 1
    ) THEN
        ALTER TABLE followers DROP CONSTRAINT followers_pkey;
        ALTER TABLE followers ADD PRIMARY KEY (user_id, actor_id);
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS idx_followers_user ON followers (user_id);

-- ── drop legacy singleton table ──────────────────────────────────────────────
DROP TABLE IF EXISTS instance_keys;
