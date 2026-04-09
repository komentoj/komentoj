# Getting Started

## Requirements

| Dependency | Minimum version |
|---|---|
| Rust | 1.75 |
| PostgreSQL | 14 |
| Redis | 6 |

You also need a **public domain with HTTPS**. ActivityPub requires HTTPS for all actor and inbox URLs.

## 1. Build

```sh
git clone https://github.com/komentoj/komentoj.git
cd komentoj
cargo build --release
# binary: ./target/release/komentoj
```

## 2. Database

Create a database:

```sql
CREATE DATABASE komentoj;
```

komentoj runs its own migrations on startup — no manual SQL needed.

## 3. Configure

Copy the example config and edit it:

```sh
cp config.toml my-config.toml
$EDITOR my-config.toml
```

Minimum required fields:

```toml
[instance]
domain   = "comments.example.com"   # public hostname, no https://
username = "comments"

[database]
url = "postgresql://user:pass@localhost/komentoj"

[admin]
token = ""   # openssl rand -hex 32
```

See the [Configuration reference](/guide/configuration) for all options.

## 4. Run

```sh
KOMENTOJ_CONFIG=my-config.toml ./target/release/komentoj
```

On first start komentoj will:
1. Run database migrations
2. Generate a 2048-bit RSA keypair and store it in the database
3. Start listening on `0.0.0.0:8080` (configurable)

You should see:

```
INFO komentoj: starting komentoj for @comments@comments.example.com
INFO komentoj: listening on 0.0.0.0:8080
```

## 5. Verify

Check that WebFinger works:

```sh
curl "https://comments.example.com/.well-known/webfinger?resource=acct:comments@comments.example.com"
```

Expected response:

```json
{
  "subject": "acct:comments@comments.example.com",
  "aliases": ["https://comments.example.com/actor"],
  "links": [...]
}
```

Then search for `@comments@comments.example.com` in your Mastodon client — the account should appear.

## 6. Sync your first post

```sh
curl -X POST https://comments.example.com/api/v1/posts/sync \
  -H "Authorization: Bearer <your-admin-token>" \
  -H "Content-Type: application/json" \
  -d '{
    "posts": [{
      "id":      "hello-world",
      "title":   "Hello World",
      "url":     "https://example.com/hello-world/",
      "content": "My first post."
    }]
  }'
```

komentoj will publish a `Create(Note)` to all current followers (none yet on first run). From now on any Fediverse user can reply to that Note and the reply will appear at `GET /api/v1/comments?id=hello-world`.

## Next steps

- [Configure](/guide/configuration) allowed origins, blog domains, Redis TTL, etc.
- [Integrate](/guide/blog-integration) the comments widget into your blog frontend
- [Deploy](/guide/deployment) behind nginx with systemd
