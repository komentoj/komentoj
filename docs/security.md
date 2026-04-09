# Security

## HTTP Signatures

Every inbound inbox request is verified using [Cavage HTTP Signatures draft-12](https://datatracker.ietf.org/doc/html/draft-cavage-http-signatures-12) with `rsa-sha256`.

Verification steps, all of which must pass:

1. **Signature header present** — rejected with `401` if absent
2. **All listed headers present** — if the `headers=` list names a header that is absent from the request, verification fails. This prevents signing over empty values (e.g. an empty `Date` to bypass freshness checks)
3. **Date freshness** — the `Date` header must be within ±5 minutes of server time. Protects against replay attacks
4. **Body digest** — the `Digest` (RFC 3230) or `Content-Digest` (RFC 9530) header must match the SHA-256 hash of the request body, and must contain a SHA-256 entry (weaker algorithms like MD5 are rejected)
5. **RSA signature valid** — the signing string is reconstructed from the listed headers and verified against the actor's public key
6. **Actor match** — the actor in the payload must exactly match the actor derived from the signing key's `keyId`. Prefix matching is not used, preventing `alice` from forging activities for `alice2`

### Key rotation

When signature verification fails with a cached key, komentoj re-fetches the actor document and retries once. Keys are cached in Redis for 1 hour by default (configurable via `redis.actor_cache_ttl`).

## SSRF protection

All outbound HTTP requests go through a blocklist that rejects:

| Range | Blocked |
|---|---|
| `127.0.0.0/8`, `::1` | Loopback |
| `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16` | Private (RFC 1918) |
| `169.254.0.0/16`, `fe80::/10` | Link-local |
| `100.64.0.0/10` | CGNAT |
| `fc00::/7` | IPv6 ULA |
| Non-HTTP(S) schemes | Protocol restriction |

**Redirect following** is also guarded — each redirect destination is validated before following, so a public-looking URL cannot bounce the fetcher into an internal network address.

## HTML sanitization

All HTML content received from remote actors is sanitized by [ammonia](https://github.com/rust-ammonia/ammonia) before storage and serving. Only an explicit allowlist of tags is permitted:

```
p  br  blockquote  pre  code  a  h1-h6  ul  ol  li  span
table  thead  tbody  tr  th  td  del  strong  em
```

All `<a>` tags have `rel="nofollow noreferrer noopener"` added automatically. Arbitrary attributes, `<script>`, `<style>`, `<iframe>`, and all other tags are stripped.

## SQL injection

komentoj uses parameterised queries exclusively via `sqlx`. No string interpolation is used in SQL statements.

## Admin token

The `admin.token` config value is compared using a constant-time equality function to prevent timing oracle attacks. Use a randomly generated token of at least 32 bytes:

```sh
openssl rand -hex 32
```

Store it as an environment variable or in a secrets manager rather than directly in the config file when deploying to shared infrastructure.

## TLS

komentoj itself does not terminate TLS. Deploy it behind a reverse proxy (nginx, Caddy, etc.) that handles HTTPS. ActivityPub requires HTTPS — an HTTP-only deployment will not be able to federate.

## Threat model

| Threat | Mitigation |
|---|---|
| Forged inbox activity (wrong actor) | HTTP Signature verification + actor equality check |
| Replayed inbox activity | `processed_activities` dedup + Date freshness window |
| Body tampering in transit | Digest header verification |
| SSRF via crafted AP object URLs | IP blocklist + redirect validation |
| XSS via comment content | ammonia HTML sanitization |
| Timing oracle on admin token | Constant-time comparison |
| Unauthorised post sync | Bearer token required |
| Follower subscription spam | Follow object must target our actor exactly |
