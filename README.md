# komentoj

A lightweight ActivityPub comment server for static blogs.

Write posts on your static site, let the Fediverse reply, and embed the comments back — no JavaScript framework, no third-party service.

## How it works

1. Your static site generator calls `POST /api/v1/posts/sync` with a list of posts.
2. komentoj publishes a `Create(Note)` to the Fediverse for each new post.
3. Fediverse users reply to that Note from Mastodon, Misskey, etc.
4. Your blog frontend fetches `GET /api/v1/comments?id=<post-id>` and renders the thread.

Follows are accepted automatically so any fediverse user can subscribe to your blog's actor (`@comments@your-domain.tld`).

## Requirements

- Rust 1.75+
- PostgreSQL 14+
- Redis 6+
- A domain with HTTPS (required by ActivityPub)

## Setup

### 1. Database

```sql
CREATE DATABASE komentoj;
```

Run migrations in order:

```sh
psql komentoj < migrations/001_init.sql
psql komentoj < migrations/002_posts.sql
```

### 2. Configuration

Copy and edit the example config:

```sh
cp config.toml my-config.toml
```

Key fields:

```toml
[instance]
domain   = "comments.example.com"   # must be reachable from the internet
username = "comments"               # fediverse handle: @comments@comments.example.com

[database]
url = "postgresql://user:pass@localhost/komentoj"

[admin]
token = "..."   # openssl rand -hex 32
```

### 3. Run

```sh
KOMENTOJ_CONFIG=my-config.toml cargo run --release
```

The binary listens on `0.0.0.0:8080` by default (change in `[server]`).

## API

### Sync posts — `POST /api/v1/posts/sync`

Requires `Authorization: Bearer <admin_token>`.

```json
{
  "posts": [
    {
      "id":      "hello-world-2024",
      "title":   "Hello World",
      "url":     "https://example.com/hello-world-2024/",
      "content": "First post. Written in **Markdown**."
    }
  ]
}
```

- New `id` → publishes `Create(Note)` to followers.
- Changed `title`/`url`/`content` → publishes `Update(Note)`.
- IDs absent from the list → marked inactive (no AP activity sent).

Response:

```json
{
  "upserted": 1,
  "published": 1,
  "updated": 0,
  "deactivated": 0,
  "rejected": []
}
```

### Get comments — `GET /api/v1/comments?id=<post-id>`

No authentication required. Suitable to call directly from the browser.

Optional query params:
- `before=<ISO8601>` — cursor for pagination
- `limit=<n>` — default 50, max 100

```json
{
  "post_id": "hello-world-2024",
  "total": 3,
  "comments": [
    {
      "id": "https://mastodon.social/users/alice/statuses/123",
      "actor": {
        "id": "https://mastodon.social/users/alice",
        "preferred_username": "alice",
        "display_name": "Alice",
        "avatar_url": "https://...",
        "profile_url": "https://mastodon.social/@alice",
        "instance": "mastodon.social"
      },
      "content_html": "<p>Great post!</p>",
      "published_at": "2024-06-01T12:00:00Z",
      "replies": []
    }
  ],
  "next_cursor": null
}
```

## ActivityPub endpoints

| Endpoint | Description |
|---|---|
| `GET /.well-known/webfinger` | WebFinger discovery |
| `GET /actor` | Actor document |
| `POST /inbox` | Incoming activities |
| `GET /outbox` | Outbox (empty stub) |
| `GET /followers` | Follower count |
| `GET /notes/:id` | Individual Note document |

## Security

- HTTP Signatures (Cavage draft-12, `rsa-sha256`) verified on every inbox request.
- Digest header verified for body integrity.
- SSRF protection on all outbound fetches (blocks loopback, RFC 1918, link-local, CGNAT).
- HTML from remote actors sanitized with [ammonia](https://github.com/rust-ammonia/ammonia) before storage.
- Admin API protected with constant-time token comparison.

## License

MIT
