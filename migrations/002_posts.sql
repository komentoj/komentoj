CREATE TABLE IF NOT EXISTS posts (
    id            TEXT PRIMARY KEY,   -- user-provided, e.g. "hello-world-2026"
    title         TEXT,
    url           TEXT NOT NULL,      -- canonical blog post URL
    content       TEXT NOT NULL DEFAULT '', -- Markdown body shown in the AP Note
    ap_note_id    TEXT UNIQUE,
    active        BOOLEAN NOT NULL DEFAULT TRUE,
    registered_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_posts_note_id
    ON posts (ap_note_id)
    WHERE ap_note_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_posts_url
    ON posts (url);

CREATE TABLE IF NOT EXISTS comments (
    id                TEXT PRIMARY KEY,
    post_id           TEXT NOT NULL REFERENCES posts(id),
    actor_id          TEXT NOT NULL REFERENCES actor_cache(id),
    content_html      TEXT NOT NULL,
    content_source    TEXT,
    published_at      TIMESTAMPTZ NOT NULL,
    in_reply_to       TEXT,
    in_reply_to_local BOOLEAN NOT NULL DEFAULT FALSE,
    visibility        TEXT NOT NULL DEFAULT 'public',
    raw_data          JSONB NOT NULL,
    deleted_at        TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_comments_post_id
    ON comments (post_id)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_comments_in_reply_to
    ON comments (in_reply_to)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_comments_actor
    ON comments (actor_id)
    WHERE deleted_at IS NULL;
