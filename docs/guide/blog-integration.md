# Blog Integration

## Overview

Integrating komentoj into a static blog requires two things:

1. **Build pipeline** — call `POST /api/v1/posts/sync` on every deploy
2. **Frontend** — call `GET /api/v1/comments` and render the thread

## Build pipeline integration

Call the sync endpoint after your build completes. Pass the full list of currently published posts every time — posts absent from the list are automatically marked inactive.

### Shell script

```sh
#!/bin/sh
# deploy.sh — run after your static site build

KOMENTOJ_URL="https://comments.example.com"
ADMIN_TOKEN="your-token-here"

# Build the JSON payload from your posts
# Adjust this to match however your SSG exposes post metadata
PAYLOAD=$(cat <<EOF
{
  "posts": [
    {
      "id":      "hello-world",
      "title":   "Hello World",
      "url":     "https://example.com/hello-world/",
      "content": "Post body in Markdown."
    }
  ]
}
EOF
)

curl -sf -X POST "$KOMENTOJ_URL/api/v1/posts/sync" \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d "$PAYLOAD"
```

### GitHub Actions

```yaml
- name: Sync posts to komentoj
  env:
    KOMENTOJ_TOKEN: ${{ secrets.KOMENTOJ_TOKEN }}
  run: |
    node scripts/sync-posts.js
```

```js
// scripts/sync-posts.js
// Example for a VitePress / content-collections setup
import { createContentLoader } from 'vitepress'

const posts = await createContentLoader('posts/*.md').load()

await fetch('https://comments.example.com/api/v1/posts/sync', {
  method: 'POST',
  headers: {
    'Authorization': `Bearer ${process.env.KOMENTOJ_TOKEN}`,
    'Content-Type': 'application/json',
  },
  body: JSON.stringify({
    posts: posts.map(p => ({
      id:      p.url.replace(/^\/posts\//, '').replace(/\/$/, ''),
      title:   p.frontmatter.title,
      url:     `https://example.com${p.url}`,
      content: p.src,   // raw Markdown
    })),
  }),
})
```

## Frontend integration

### Vanilla JS

```html
<section id="comments">
  <h2>Comments</h2>
  <div id="comments-list">Loading…</div>
</section>

<script>
const postId = 'hello-world'   // must match the id used in sync

async function loadComments() {
  const res = await fetch(
    `https://comments.example.com/api/v1/comments?id=${postId}`
  )
  if (!res.ok) return
  const data = await res.json()
  renderComments(data.comments)
}

function renderComments(comments) {
  const list = document.getElementById('comments-list')
  if (!comments.length) {
    list.textContent = 'No comments yet. Reply on the Fediverse!'
    return
  }
  list.innerHTML = comments.map(c => `
    <article class="comment">
      <header>
        <img src="${c.actor.avatar_url || ''}" width="40" height="40" alt="">
        <a href="${c.actor.profile_url}" target="_blank" rel="noopener noreferrer">
          ${c.actor.display_name || c.actor.preferred_username}
        </a>
        <span class="instance">@${c.actor.instance}</span>
        <time datetime="${c.published_at}">
          ${new Date(c.published_at).toLocaleDateString()}
        </time>
      </header>
      <div class="content">${c.content_html}</div>
      ${c.replies.length ? `
        <div class="replies">
          ${c.replies.map(r => `
            <article class="reply">
              <a href="${r.actor.profile_url}" target="_blank" rel="noopener noreferrer">
                ${r.actor.display_name || r.actor.preferred_username}
              </a>
              <div class="content">${r.content_html}</div>
            </article>
          `).join('')}
        </div>
      ` : ''}
    </article>
  `).join('')
}

loadComments()
</script>
```

### Vue 3 component

```vue
<script setup>
import { ref, onMounted } from 'vue'

const props = defineProps({ postId: String })
const comments = ref([])
const loading = ref(true)
const nextCursor = ref(null)

async function load(cursor = null) {
  const url = new URL('https://comments.example.com/api/v1/comments')
  url.searchParams.set('id', props.postId)
  if (cursor) url.searchParams.set('before', cursor)

  const res = await fetch(url)
  const data = await res.json()
  comments.value.push(...data.comments)
  nextCursor.value = data.next_cursor
  loading.value = false
}

onMounted(() => load())
</script>

<template>
  <section class="comments">
    <h2>Comments</h2>
    <p v-if="loading">Loading…</p>
    <template v-else>
      <article v-for="c in comments" :key="c.id" class="comment">
        <header>
          <img v-if="c.actor.avatar_url" :src="c.actor.avatar_url" width="40" height="40">
          <a :href="c.actor.profile_url" target="_blank" rel="noopener noreferrer">
            {{ c.actor.display_name || c.actor.preferred_username }}
          </a>
          <time :datetime="c.published_at">
            {{ new Date(c.published_at).toLocaleDateString() }}
          </time>
        </header>
        <div class="content" v-html="c.content_html" />
        <div v-if="c.replies.length" class="replies">
          <article v-for="r in c.replies" :key="r.id" class="reply">
            <a :href="r.actor.profile_url" target="_blank" rel="noopener noreferrer">
              {{ r.actor.display_name || r.actor.preferred_username }}
            </a>
            <div class="content" v-html="r.content_html" />
          </article>
        </div>
      </article>
      <button v-if="nextCursor" @click="load(nextCursor)">Load more</button>
      <p v-if="!comments.length">
        No comments yet. Reply on the Fediverse!
      </p>
    </template>
  </section>
</template>
```

## Letting readers reply

On each post page, link to the post's AP Note so readers can reply from their own instance:

```js
// Fetch the actor to find the Note URL
// The Note URL is displayed in the Mastodon thread view
const noteUrl = `https://comments.example.com/notes/<uuid>`
```

::: tip
The simplest call-to-action is a plain link:

```html
<a href="https://comments.example.com/actor" target="_blank">
  Follow @comments@comments.example.com to comment
</a>
```

When your readers follow the actor, they'll see new posts in their timeline and can reply directly.
:::

## Pagination

The comments API returns up to 50 comments per request by default. For posts with many comments, use the `next_cursor` field:

```js
let cursor = null
let allComments = []

do {
  const url = new URL('https://comments.example.com/api/v1/comments')
  url.searchParams.set('id', postId)
  if (cursor) url.searchParams.set('before', cursor)

  const { comments, next_cursor } = await fetch(url).then(r => r.json())
  allComments.push(...comments)
  cursor = next_cursor
} while (cursor)
```
