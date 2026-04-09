# POST /api/v1/posts/sync

Synchronise the full list of published blog posts with komentoj. This is a **full replacement** operation: posts absent from the submitted list are marked inactive.

Call this endpoint from your build pipeline after every deploy.

## Authentication

```
Authorization: Bearer <admin_token>
```

The token must match `admin.token` in your config file. Compared in constant time.

## Request

```
POST /api/v1/posts/sync
Content-Type: application/json
Authorization: Bearer <token>
```

```ts
interface SyncRequest {
  posts: PostInput[]
}

interface PostInput {
  /** Unique identifier for this post. Recommended: your URL slug. */
  id: string
  /** Optional display title. Shown as a bold link in the AP Note. */
  title?: string
  /** Canonical URL of the blog post. Shown as the clickable link in Mastodon. */
  url: string
  /** Post body in Markdown. Rendered to HTML in the AP Note. */
  content: string
}
```

### Example

```json
{
  "posts": [
    {
      "id":      "building-a-comment-system",
      "title":   "Building a Comment System with ActivityPub",
      "url":     "https://example.com/building-a-comment-system/",
      "content": "ActivityPub is a W3C standard for decentralised social networking..."
    },
    {
      "id":      "hello-world",
      "title":   "Hello World",
      "url":     "https://example.com/hello-world/",
      "content": "First post."
    }
  ]
}
```

## Response

```ts
interface SyncResponse {
  /** Total rows written (new + updated). */
  upserted: number
  /** New posts for which a Create(Note) was published. */
  published: number
  /** Existing posts whose title/url/content changed — Update(Note) sent. */
  updated: number
  /** Posts that were active before this sync but absent from the list. */
  deactivated: number
  /** Posts rejected before processing (e.g. empty id). */
  rejected: RejectedPost[]
}

interface RejectedPost {
  id: string
  reason: string
}
```

### Example

```json
{
  "upserted": 2,
  "published": 1,
  "updated": 1,
  "deactivated": 0,
  "rejected": []
}
```

## Behaviour per post

| Situation | Action |
|---|---|
| New `id` | Insert row → publish `Create(Note)` to all followers |
| Existing `id`, content changed | Update row → publish `Update(Note)` to all followers |
| Existing `id`, nothing changed | Update row → no AP activity sent |
| `id` absent from submitted list | Mark `active = false` → no AP activity sent |
| Empty submitted list | All active posts marked inactive |

## Note content

The AP Note published for each post is structured as follows:

```
<strong><a href="{url}">{title}</a></strong>

{content rendered as HTML from Markdown (GFM)}
```

The original Markdown is also stored in the Note's `source` field (mediaType `text/markdown`) for clients that support it.

## HTTP Signatures

Fan-out delivery to followers is performed asynchronously after the HTTP response is returned. Each delivery is signed with the instance RSA key.

## Error codes

| Code | Meaning |
|---|---|
| `200` | Sync completed |
| `400` | Malformed JSON |
| `401` | Missing or invalid admin token |
| `500` | Database error |
