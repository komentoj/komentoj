# Configuration

komentoj is configured via a TOML file. The path defaults to `config.toml` in the working directory and can be overridden with the `KOMENTOJ_CONFIG` environment variable.

## Full reference

```toml
# ── Server ────────────────────────────────────────────────────────────────────

[server]
host = "0.0.0.0"   # bind address
port = 8080        # bind port

# ── Instance ──────────────────────────────────────────────────────────────────

[instance]
# Public hostname. Must be reachable from the internet with valid HTTPS.
# No "https://" prefix.
domain = "comments.example.com"

# The local part of the fediverse handle: @<username>@<domain>
username = "comments"

# Human-readable name shown on the actor profile in Mastodon etc.
display_name = "Blog Comments"

# Short bio shown on the actor profile.
summary = "Fediverse comment bot for example.com"

# Only store comments that mention a URL from one of these domains.
# Prevents arbitrary third parties from using your instance as a comment host.
blog_domains = ["example.com", "www.example.com"]

# ── Database ──────────────────────────────────────────────────────────────────

[database]
url             = "postgresql://user:pass@localhost:5432/komentoj"
max_connections = 10

# ── Redis ─────────────────────────────────────────────────────────────────────

[redis]
url = "redis://localhost:6379"

# How long (seconds) to cache remote actor documents in Redis.
# Longer = fewer outbound fetches. Shorter = picks up key rotations faster.
actor_cache_ttl = 3600

# ── CORS ──────────────────────────────────────────────────────────────────────

[cors]
# Origins allowed to call GET /api/v1/comments from a browser.
# Must match the exact origin of your blog (scheme + host + optional port).
allowed_origins = [
  "https://example.com",
  "https://www.example.com",
]

# ── Admin ─────────────────────────────────────────────────────────────────────

[admin]
# Bearer token for write operations (POST /api/v1/posts/sync).
# Generate a strong random token:
#   openssl rand -hex 32
token = "change-me"
```

## Field notes

### `instance.blog_domains`

When komentoj receives an incoming `Create(Note)` that has no `inReplyTo` pointing to a known post, it falls back to scanning the note's content for links. Only links whose host is in `blog_domains` will match a registered post.

This acts as a guard: without it, any reply mentioning any URL would be associated with a post on a first-match basis.

### `redis.actor_cache_ttl`

Remote actor documents (public keys, inbox URLs, display names) are cached in Redis after the first fetch. The default of 3600 seconds (1 hour) is a reasonable trade-off between performance and key-rotation freshness.

When a signature verification fails with a cached key, komentoj automatically re-fetches the actor document and retries — so even a long TTL won't permanently break delivery after a key rotation.

### `cors.allowed_origins`

The `GET /api/v1/comments` endpoint is intended to be called directly from your blog's JavaScript. List every origin (protocol + host) your blog is served from. For local development you can add `http://localhost:3000` etc.

### `admin.token`

Used as a `Bearer` token in the `Authorization` header for `POST /api/v1/posts/sync`. Compared in constant time to prevent timing attacks. Keep it secret; it grants full write access to all post and comment data.

## Environment variable

```sh
KOMENTOJ_CONFIG=/etc/komentoj/config.toml ./komentoj
```

If the environment variable is not set, the binary looks for `config.toml` in the current working directory.
