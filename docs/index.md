---
layout: home

hero:
  name: komentoj
  text: Fediverse comments for static blogs
  tagline: Write a post. The Fediverse replies. Your blog shows the thread.
  actions:
    - theme: brand
      text: Get Started
      link: /guide/getting-started
    - theme: alt
      text: View on GitHub
      link: https://github.com/komentoj/komentoj

features:
  - icon: 🌐
    title: ActivityPub native
    details: A real AP actor your readers can follow from Mastodon, Misskey, Pleroma, or any compatible client.
  - icon: ⚡
    title: Static-blog friendly
    details: One sync call from your build pipeline. Comments are fetched at page load via a simple REST API — no backend required on the blog side.
  - icon: 🔒
    title: Secure by default
    details: HTTP Signatures on every request, body digest verification, SSRF protection on all outbound fetches, and allowlist HTML sanitization.
  - icon: 🦀
    title: Single binary
    details: A self-contained Rust binary. Bring PostgreSQL and Redis — that's all you need.
---
