# How It Works

## The publish flow

When you sync a new post, komentoj publishes a `Create(Note)` activity to every follower's inbox.

```
POST /api/v1/posts/sync  ─────────────────────────────────────────────┐
                                                                       │
komentoj:                                                              │
  1. Upsert post row (posts.id = "hello-world")                        │
  2. Assign a permanent Note ID (UUID):                                │
       https://comments.example.com/notes/<uuid>                       │
  3. Store Note ID in posts.ap_note_id                                 │
  4. Build Create(Note) activity                                       │
  5. Fan-out to all followers concurrently (max 10 in parallel)        │
     ├── Sign request with instance RSA key (HTTP Signatures)          │
     ├── POST to inbox.mastodon.social                                 │
     ├── POST to inbox.fosstodon.org                                   │
     └── …                                                             │
  6. Return HTTP 200 to the caller  ◄───────────────────────────────── ┘
     (fan-out continues in the background)
```

### Note permanence

The Note's `id` (UUID-based URL) is permanent and never changes. If you update a post's title, URL, or content, komentoj sends an `Update(Note)` with the same `id` — remote servers know to replace their cached copy.

## The reply flow

When a Fediverse user replies to the Note from their client:

```
User on Mastodon replies to the Note
         │
         ▼
mastodon.social POSTs Create(Note) to https://comments.example.com/inbox
         │
         ▼
komentoj inbox handler:
  1. Buffer request body
  2. Parse Signature header → extract keyId → derive actor URL
  3. Look up actor's public key (Redis → DB → remote fetch)
  4. Verify HTTP Signature (rsa-sha256)
  5. Verify body Digest header (SHA-256)
  6. Check actor mismatch (payload.actor must match signing key's actor)
  7. Deduplicate (activity_id → processed_activities table)
  8. Return 202 Accepted
  9. Background task:
     ├── Resolve which post the reply belongs to:
     │     a. inReplyTo → posts.ap_note_id     (direct reply to post Note)
     │     b. inReplyTo → comments.id          (reply to existing comment)
     │     c. URL in content → posts.url        (mention fallback)
     ├── Sanitize content HTML (ammonia allowlist)
     └── Insert into comments table
```

### Key rotation handling

If signature verification fails with the cached key, komentoj automatically re-fetches the actor document and retries verification once. This handles key rotations without manual intervention.

## HTTP Signatures

komentoj implements [Cavage HTTP Signatures draft-12](https://datatracker.ietf.org/doc/html/draft-cavage-http-signatures-12) with `rsa-sha256`.

**Outbound requests (signed by komentoj):**

| Header | POST to inbox | GET actor/note |
|---|---|---|
| `(request-target)` | ✓ | ✓ |
| `host` | ✓ | ✓ |
| `date` | ✓ | ✓ |
| `digest` | ✓ | — |

**Inbound verification:**

komentoj verifies:
1. Signature header is present and parseable
2. `date` header is within ±5 minutes of current time (replay protection)
3. `digest` header matches SHA-256 of request body (body integrity)
4. All headers listed in `headers=` are present in the request
5. RSA-SHA256 signature is valid for the reconstructed signing string

Both RFC 3230 (`Digest: SHA-256=<base64>`) and RFC 9530 (`Content-Digest: sha-256=:<base64>:`) are accepted.

## SSRF protection

All outbound HTTP requests (actor fetches, Note fetches, Accept(Follow) delivery) go through an SSRF guard that blocks:

- Loopback addresses (`127.0.0.0/8`, `::1`)
- Private ranges (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`)
- Link-local (`169.254.0.0/16`, `fe80::/10`)
- CGNAT (`100.64.0.0/10`)
- ULA (`fc00::/7`)
- Non-HTTP(S) schemes

Redirect destinations are also validated before following — a public-looking URL cannot bounce the fetcher into an internal address.

## Fan-out delivery

When publishing to followers, komentoj:

1. Queries `DISTINCT COALESCE(shared_inbox_url, inbox_url)` — uses shared inboxes where available to reduce duplicate deliveries to the same server
2. Delivers to up to **10 inboxes concurrently**
3. Logs a warning for each failed delivery (but does not retry automatically)

## Activity deduplication

Every received activity `id` is written to `processed_activities` before processing. Duplicate deliveries (which are normal in AP) return `200` immediately without reprocessing.

If the background processing task fails (e.g. a network error fetching a referenced object), the activity ID is removed from `processed_activities` so the remote server's retry will be processed correctly.
