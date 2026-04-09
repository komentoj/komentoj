# Introduction

**komentoj** (Esperanto: *comments*) is a lightweight ActivityPub server that turns your static blog into a first-class Fediverse citizen.

## The problem

Static site generators are fast and simple, but they have no server-side logic. Adding comments typically means:

- Embedding a third-party widget (Disqus, Utterances) — you lose your readers' data
- Running a heavyweight CMS just for comments — defeats the purpose of going static
- Ignoring comments entirely

## The solution

komentoj sits alongside your static blog as a small Rust service. When you publish a post, it automatically announces it on the Fediverse as an ActivityPub `Note`. Readers reply to that note from their own Mastodon/Misskey/Pleroma account. Those replies are stored in PostgreSQL and served back to your blog frontend via a simple JSON API.

```
Your blog                komentoj                  Fediverse
─────────                ────────                  ─────────

POST /api/v1/posts/sync  →  Create(Note)  →  followers' feeds

GET  /api/v1/comments    ←  stored replies  ←  replies to Note
```

## What komentoj is not

- **Not a social network.** It only manages the comment/reply flow for your blog posts.
- **Not a moderation tool** (yet). All public replies to a registered post are stored as-is (after HTML sanitization).
- **Not a replacement for your blog.** It has no frontend of its own — your blog's JavaScript is responsible for rendering comments.

## Architecture overview

| Component | Role |
|---|---|
| **PostgreSQL** | Posts, comments, follower list, actor cache, processed-activity dedup |
| **Redis** | Short-lived actor document cache (default TTL: 1 hour) |
| **komentoj** | AP inbox/outbox, HTTP Signature signing/verification, fan-out delivery |
| **Your blog frontend** | Calls `GET /api/v1/comments` and renders the thread |
| **Your build pipeline** | Calls `POST /api/v1/posts/sync` after every deploy |
