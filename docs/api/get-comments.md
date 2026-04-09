# GET /api/v1/comments

Fetch the comment thread for a registered post. Designed to be called directly from your blog's JavaScript — no authentication required, CORS is configured for your allowed origins.

## Request

```
GET /api/v1/comments?id=<post-id>[&before=<cursor>][&limit=<n>]
```

### Query parameters

| Parameter | Type | Required | Description |
|---|---|---|---|
| `id` | string | yes | Post identifier as provided to the sync API |
| `before` | ISO 8601 datetime | no | Return comments published before this timestamp (pagination cursor) |
| `limit` | integer | no | Max comments to return. Default: `50`. Max: `100` |

### Example

```sh
curl "https://comments.example.com/api/v1/comments?id=hello-world"
```

With pagination:

```sh
curl "https://comments.example.com/api/v1/comments?id=hello-world&before=2024-06-01T12:00:00Z&limit=20"
```

## Response

```ts
interface CommentsResponse {
  post_id:     string
  total:       number        // total comments (ignoring pagination)
  comments:    Comment[]
  next_cursor: string | null // ISO 8601, pass as `before` for next page
}

interface Comment {
  id:           string   // AP Note URL of the comment
  post_id:      string
  actor:        Actor
  content_html: string   // sanitized HTML
  published_at: string   // ISO 8601
  in_reply_to:  string | null
  replies:      Comment[]  // one level of nested replies
}

interface Actor {
  id:                 string
  preferred_username: string
  display_name:       string | null
  avatar_url:         string | null
  profile_url:        string | null
  instance:           string   // e.g. "mastodon.social"
}
```

### Example response

```json
{
  "post_id": "hello-world",
  "total": 3,
  "comments": [
    {
      "id": "https://mastodon.social/users/alice/statuses/112345678",
      "post_id": "hello-world",
      "actor": {
        "id": "https://mastodon.social/users/alice",
        "preferred_username": "alice",
        "display_name": "Alice Wonderland",
        "avatar_url": "https://files.mastodon.social/accounts/avatars/.../alice.jpg",
        "profile_url": "https://mastodon.social/@alice",
        "instance": "mastodon.social"
      },
      "content_html": "<p>Great post! Really enjoyed the section on HTTP Signatures.</p>",
      "published_at": "2024-06-01T10:30:00Z",
      "in_reply_to": null,
      "replies": [
        {
          "id": "https://fosstodon.org/users/bob/statuses/998877",
          "post_id": "hello-world",
          "actor": {
            "id": "https://fosstodon.org/users/bob",
            "preferred_username": "bob",
            "display_name": "Bob",
            "avatar_url": null,
            "profile_url": "https://fosstodon.org/@bob",
            "instance": "fosstodon.org"
          },
          "content_html": "<p><span class=\"h-card\">@alice</span> Agreed!</p>",
          "published_at": "2024-06-01T11:05:00Z",
          "in_reply_to": "https://mastodon.social/users/alice/statuses/112345678",
          "replies": []
        }
      ]
    }
  ],
  "next_cursor": null
}
```

## Reply nesting

Comments are returned as a flat list of **top-level comments** (those replying directly to the post's AP Note). Each top-level comment has a `replies` array containing **one level of nested replies**.

Deeper threads (replies to replies to replies) are flattened into the nearest top-level comment's `replies` array.

## Pagination

Results are ordered by `published_at` descending (newest first). To paginate:

1. Check `next_cursor` in the response
2. If non-null, pass it as `before=<cursor>` in the next request
3. Repeat until `next_cursor` is null

```js
async function loadAll(postId) {
  let cursor = null
  const all = []

  do {
    const url = new URL('https://comments.example.com/api/v1/comments')
    url.searchParams.set('id', postId)
    if (cursor) url.searchParams.set('before', cursor)

    const data = await fetch(url).then(r => r.json())
    all.push(...data.comments)
    cursor = data.next_cursor
  } while (cursor)

  return all
}
```

## `content_html` safety

All HTML in `content_html` is sanitized by [ammonia](https://github.com/rust-ammonia/ammonia) before storage. Allowed tags: `p`, `br`, `blockquote`, `pre`, `code`, `a`, `h1`–`h6`, `ul`, `ol`, `li`, `span`, `table`, `thead`, `tbody`, `tr`, `th`, `td`, `del`, `strong`, `em`. All `<a>` tags have `rel="nofollow noreferrer noopener"` added automatically.

It is safe to render `content_html` directly via `innerHTML` / `v-html`.

## Error codes

| Code | Meaning |
|---|---|
| `200` | OK |
| `400` | Missing or invalid `id` parameter |
| `404` | No post with the given `id` |
| `500` | Database error |
