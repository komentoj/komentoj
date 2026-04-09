# ActivityPub Endpoints

komentoj exposes the following public ActivityPub endpoints. All responses use `Content-Type: application/activity+json` unless noted.

## WebFinger

```
GET /.well-known/webfinger?resource=<resource>
Content-Type: application/jrd+json
```

Accepts both `acct:` URIs and actor URLs as the `resource` parameter:

- `acct:comments@comments.example.com`
- `https://comments.example.com/actor`

**Response:**

```json
{
  "subject": "acct:comments@comments.example.com",
  "aliases": ["https://comments.example.com/actor"],
  "links": [
    {
      "rel": "http://webfinger.net/rel/profile-page",
      "type": "text/html",
      "href": "https://comments.example.com/actor"
    },
    {
      "rel": "self",
      "type": "application/activity+json",
      "href": "https://comments.example.com/actor"
    }
  ]
}
```

## Actor

```
GET /actor
Accept: application/activity+json
```

Returns the instance actor document (`type: Service`). Browsers that do not send an AP Accept header are redirected to `https://<instance.domain>`.

**Response:**

```json
{
  "@context": ["https://www.w3.org/ns/activitystreams", ...],
  "id": "https://comments.example.com/actor",
  "type": "Service",
  "preferredUsername": "comments",
  "name": "Blog Comments",
  "summary": "Fediverse comment bot for example.com",
  "inbox": "https://comments.example.com/inbox",
  "outbox": "https://comments.example.com/outbox",
  "followers": "https://comments.example.com/followers",
  "following": "https://comments.example.com/following",
  "publicKey": {
    "id": "https://comments.example.com/actor#main-key",
    "owner": "https://comments.example.com/actor",
    "publicKeyPem": "-----BEGIN PUBLIC KEY-----\n..."
  },
  "manuallyApprovesFollowers": false,
  "discoverable": true
}
```

## Inbox

```
POST /inbox
```

Accepts incoming ActivityPub activities. All requests must carry a valid HTTP Signature.

Handled activity types:

| Type | Action |
|---|---|
| `Create(Note)` | Store as a comment if it replies to a known post |
| `Update(Note)` | Update stored comment content |
| `Delete` | Soft-delete the comment |
| `Follow` | Store follower, send `Accept(Follow)` |
| `Undo(Follow)` | Remove follower |

All other types are silently ignored. Returns `202 Accepted` on success.

## Outbox

```
GET /outbox
```

Returns an empty `OrderedCollection`. komentoj does not paginate its outbox.

## Followers

```
GET /followers
```

Returns an `OrderedCollection` with the current follower count.

## Following

```
GET /following
```

Returns an empty `OrderedCollection` (komentoj does not follow anyone).

## Notes

```
GET /notes/:id
```

Returns a single `Note` document for a registered post. Remote AP servers fetch this URL when they need to verify a reply's `inReplyTo` reference.

**Response:**

```json
{
  "@context": "https://www.w3.org/ns/activitystreams",
  "id": "https://comments.example.com/notes/550e8400-e29b-41d4-a716-446655440000",
  "type": "Note",
  "attributedTo": "https://comments.example.com/actor",
  "content": "<p><strong><a href=\"https://example.com/hello-world/\">Hello World</a></strong></p><p>Post body…</p>",
  "url": "https://example.com/hello-world/",
  "published": "2024-06-01T00:00:00Z",
  "to": ["https://www.w3.org/ns/activitystreams#Public"],
  "cc": ["https://comments.example.com/followers"],
  "source": {
    "content": "Post body…",
    "mediaType": "text/markdown"
  }
}
```
