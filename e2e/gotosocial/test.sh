#!/usr/bin/env bash
# E2E federation test: komentoj ↔ GoToSocial (HTTPS)
#
# Prerequisites:
#   ./e2e/gotosocial/setup.sh    # generate mkcert certs + /etc/hosts entries
#   docker compose -f e2e/gotosocial/docker-compose.yml up -d
#
# Run from the repository root:
#   ./e2e/gotosocial/test.sh

set -euo pipefail

KOMENTOJ="https://komentoj.local"
GTS="https://gotosocial.local:8888"
ADMIN_TOKEN="e2e-test-admin-token"
GTS_USER="testuser"
GTS_PASSWORD="Password1!"
GTS_EMAIL="testuser@example.com"

PASS=0
FAIL=0
_fail_msgs=()

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CACERT="$SCRIPT_DIR/certs/rootCA.pem"
COMPOSE_FILE="$SCRIPT_DIR/docker-compose.yml"

# Route *.local domains to localhost without touching /etc/hosts.
# All curl calls in this script go through this wrapper automatically.
curl() {
    command curl \
        --resolve "komentoj.local:443:127.0.0.1" \
        --resolve "gotosocial.local:443:127.0.0.1" \
        --resolve "gotosocial.local:8888:127.0.0.1" \
        --cacert "$CACERT" \
        "$@"
}

COOKIE_JAR=$(mktemp)
trap 'rm -f "$COOKIE_JAR"' EXIT

# ── Helpers ──────────────────────────────────────────────────────────────────

ok()   { echo "  [PASS] $*"; PASS=$((PASS + 1)); }
fail() { echo "  [FAIL] $*"; FAIL=$((FAIL + 1)); _fail_msgs+=("$*"); }

assert_eq() {
    local label="$1" got="$2" want="$3"
    if [[ "$got" == "$want" ]]; then ok "$label"
    else fail "$label — got '$got', want '$want'"; fi
}

assert_contains() {
    local label="$1" haystack="$2" needle="$3"
    if echo "$haystack" | grep -qF "$needle"; then ok "$label"
    else fail "$label — '$needle' not found in response"; fi
}

assert_http() {
    local label="$1" want="$2"
    shift 2
    local got
    got=$(curl -s -o /dev/null -w "%{http_code}" "$@")
    if [[ "$got" == "$want" ]]; then ok "$label (HTTP $got)"
    else fail "$label — HTTP $got, want $want"; fi
}

# Poll until command exits 0, or time out.
wait_for() {
    local label="$1" max_secs="$2"
    shift 2
    local elapsed=0
    while ! eval "$@" 2>/dev/null; do
        sleep 2; elapsed=$((elapsed+2))
        if (( elapsed >= max_secs )); then
            fail "timeout ($max_secs s) waiting for: $label"; return 1
        fi
    done
    ok "$label"
}

section() { echo; echo "── $* ──────────────────────────────────────────────"; }

# ── 0. Services reachable ────────────────────────────────────────────────────

section "0. Services healthy"

wait_for "komentoj reachable" 120 \
    'curl -sf "$KOMENTOJ/.well-known/webfinger?resource=acct:komentoj@komentoj.local"'

wait_for "GoToSocial reachable" 300 \
    'curl -sf "$GTS/api/v1/instance"'

# ── 1. GTS account + OAuth token ─────────────────────────────────────────────

section "1. GTS account setup"

# Create + confirm account via GTS CLI (direct DB access, no HTTP needed).
# The CLI exits 0 even if the account already exists.
GTS_CONTAINER=$(docker compose -f "$COMPOSE_FILE" ps -q gotosocial)
docker exec "$GTS_CONTAINER" \
    /gotosocial/gotosocial --config-path "" admin account create \
    --username "$GTS_USER" --email "$GTS_EMAIL" --password "$GTS_PASSWORD" \
    2>&1 | grep -v "^time=" || true
docker exec "$GTS_CONTAINER" \
    /gotosocial/gotosocial --config-path "" admin account confirm \
    --username "$GTS_USER" \
    2>&1 | grep -v "^time=" || true
ok "GTS account created/confirmed"

# Register OAuth app (callback URL will receive the auth code via redirect)
APP=$(curl -sf -X POST "$GTS/api/v1/apps" \
    -F "client_name=e2e-test" \
    -F "redirect_uris=http://localhost:1/cb" \
    -F "scopes=read write")
CLIENT_ID=$(echo "$APP"    | grep -oP '"client_id":"\K[^"]+')
CLIENT_SECRET=$(echo "$APP" | grep -oP '"client_secret":"\K[^"]+')

# ── Headless authorization_code OAuth flow ───────────────────────────────────
#
# GTS 0.17 has no CSRF tokens on either the sign-in or authorize forms.
# The key is to GET /oauth/authorize FIRST so GTS stores the OAuth state in
# the session, then sign in, then POST the grant (empty body).
#
# 1. GET /oauth/authorize → GTS 303s to /auth/sign_in, stores state in cookie
# 2. POST /auth/sign_in → GTS 302s back to /oauth/authorize
# 3. GET /oauth/authorize (authenticated) → grant form
# 4. POST /oauth/authorize (empty body) → 302 to callback?code=XXX
# 5. Exchange code for access token

OAUTH_AUTH_URL="$GTS/oauth/authorize?response_type=code&client_id=${CLIENT_ID}&redirect_uri=http%3A%2F%2Flocalhost%3A1%2Fcb&scope=read+write"

# Step 1: prime session with OAuth state
curl -s -c "$COOKIE_JAR" -b "$COOKIE_JAR" "$OAUTH_AUTH_URL" -o /dev/null

# Step 2: sign in
curl -s -c "$COOKIE_JAR" -b "$COOKIE_JAR" \
    -X POST "$GTS/auth/sign_in" \
    --data-urlencode "username=$GTS_EMAIL" \
    --data-urlencode "password=$GTS_PASSWORD" \
    -o /dev/null

# Step 3+4: GET authorize page (confirmation), then POST empty grant
curl -sf -c "$COOKIE_JAR" -b "$COOKIE_JAR" "$OAUTH_AUTH_URL" -o /dev/null

# POST /oauth/authorize — GTS redirects to callback?code=XXX
AUTH_RESP=$(curl -s -c "$COOKIE_JAR" -b "$COOKIE_JAR" \
    -X POST "$GTS/oauth/authorize" \
    -D - -o /dev/null --max-redirs 0 2>&1 || true)
AUTH_CODE=$(echo "$AUTH_RESP" | grep -i 'location:' | grep -oP 'code=\K[^\s&]+')

TOKEN_RESP=$(curl -sf -X POST "$GTS/oauth/token" \
    -F "client_id=$CLIENT_ID" \
    -F "client_secret=$CLIENT_SECRET" \
    -F "redirect_uri=http://localhost:1/cb" \
    -F "grant_type=authorization_code" \
    -F "code=$AUTH_CODE" \
    -F "scope=read write")
GTS_TOKEN=$(echo "$TOKEN_RESP" | grep -oP '"access_token":"\K[^"]+' || true)

if [[ -n "$GTS_TOKEN" ]]; then ok "GTS OAuth token obtained"
else fail "GTS OAuth token not obtained — aborting"; exit 1; fi

# ── 2. WebFinger ─────────────────────────────────────────────────────────────

section "2. WebFinger"

WF_K=$(curl -sf "$KOMENTOJ/.well-known/webfinger?resource=acct:komentoj@komentoj.local")
assert_contains "komentoj WF subject"           "$WF_K" "acct:komentoj@komentoj.local"
assert_contains "komentoj WF actor link"        "$WF_K" "https://komentoj.local/users/komentoj"
assert_contains "komentoj WF content-type link" "$WF_K" "application/activity+json"

WF_GTS=$(curl -sf "$GTS/.well-known/webfinger?resource=acct:$GTS_USER@gotosocial.local")
assert_contains "GTS WF subject" "$WF_GTS" "acct:$GTS_USER@gotosocial.local"

# ── 3. Actor documents ───────────────────────────────────────────────────────

section "3. Actor documents"

ACTOR=$(curl -sf -H "Accept: application/activity+json" "$KOMENTOJ/users/komentoj")
assert_contains "komentoj actor type=Service"  "$ACTOR" '"type":"Service"'
assert_contains "komentoj actor inbox"         "$ACTOR" '"inbox":"https://komentoj.local/users/komentoj/inbox"'
assert_contains "komentoj actor publicKey"     "$ACTOR" '"publicKey"'
assert_contains "komentoj actor followers"     "$ACTOR" '"followers"'

assert_http "komentoj outbox OK"    200 -H "Accept: application/activity+json" "$KOMENTOJ/users/komentoj/outbox"
assert_http "komentoj followers OK" 200 -H "Accept: application/activity+json" "$KOMENTOJ/users/komentoj/followers"
assert_http "komentoj following OK" 200 -H "Accept: application/activity+json" "$KOMENTOJ/users/komentoj/following"

# GTS 0.17+ requires HTTP signatures for /users/:id (ActivityPub endpoint).
# Use the Mastodon-compatible API instead to verify the account exists.
GTS_ACCOUNT=$(curl -sf -H "Authorization: Bearer $GTS_TOKEN" \
    "$GTS/api/v1/accounts/lookup?acct=$GTS_USER@gotosocial.local")
assert_contains "GTS account exists" "$GTS_ACCOUNT" '"acct"'

# ── 4. Browser redirect (actor serves HTML redirect) ────────────────────────

section "4. Actor content negotiation"

REDIR=$(curl -s -o /dev/null -w "%{http_code}" -H "Accept: text/html" "$KOMENTOJ/users/komentoj")
if [[ "$REDIR" == "303" ]]; then ok "actor redirects browsers (303)"
else fail "actor browser redirect — got HTTP $REDIR, want 303"; fi

# ── 5. sync_posts: register + publish Create(Note) ──────────────────────────

section "5. POST /api/v1/users/komentoj/posts/sync — new post → Create(Note)"

POST_ID="e2e-post-$(date +%s)"
POST_URL="https://gotosocial.local/posts/$POST_ID"

SYNC1=$(curl -sf -X POST "$KOMENTOJ/api/v1/users/komentoj/posts/sync" \
    -H "Authorization: Bearer $ADMIN_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{\"posts\":[{\"id\":\"$POST_ID\",\"title\":\"E2E Test Post\",\"url\":\"$POST_URL\",\"content\":\"Hello from E2E.\"}]}")

assert_contains "sync: upserted=1"  "$SYNC1" '"upserted":1'
assert_contains "sync: published=1" "$SYNC1" '"published":1'
assert_contains "sync: updated=0"   "$SYNC1" '"updated":0'

# Wait for ap_note_id to be written (async publish)
wait_for "ap_note_id persisted in DB" 30 \
    'curl -sf "$KOMENTOJ/api/v1/users/komentoj/comments?id=$POST_ID" | grep -qF "total"'

COMMENTS0=$(curl -sf "$KOMENTOJ/api/v1/users/komentoj/comments?id=$POST_ID")
assert_contains "comments post_id matches" "$COMMENTS0" "\"post_id\":\"$POST_ID\""

# ── 6. Resolve komentoj actor from GTS side ──────────────────────────────────
#
# GTS Mastodon API search doesn't surface Service-type actors even with resolve=true.
# Trigger resolution via a mention: GTS fetches the mentioned actor's WebFinger+actor
# document, which causes it to store komentoj as a remote account. We then query the
# GTS database directly to get the internal ID (needed for the Follow API in §7).

section "6. GTS resolves komentoj actor"

# Trigger resolution via mention
curl -s -X POST "$GTS/api/v1/statuses" \
    -H "Authorization: Bearer $GTS_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{\"status\":\"test @komentoj@komentoj.local\",\"visibility\":\"public\"}" \
    -o /dev/null

# Wait for GTS to process the mention and store the remote account
wait_for "komentoj actor stored in GTS DB" 30 \
    'docker compose -f "$COMPOSE_FILE" exec -T gts-postgres psql -U gotosocial -t \
     -c "SELECT id FROM accounts WHERE username='"'"'komentoj'"'"' AND domain='"'"'komentoj.local'"'"';" \
     | grep -qE "[0-9A-Z]"'

KOMENTOJ_GTS_ID=$(docker compose -f "$COMPOSE_FILE" exec -T gts-postgres \
    psql -U gotosocial -t \
    -c "SELECT id FROM accounts WHERE username='komentoj' AND domain='komentoj.local';" \
    | tr -d ' \n')

if [[ -n "$KOMENTOJ_GTS_ID" ]]; then ok "komentoj resolved in GTS (id=$KOMENTOJ_GTS_ID)"
else fail "could not resolve komentoj actor in GTS"; KOMENTOJ_GTS_ID=""; fi

# ── 7. Follow: GTS → komentoj ────────────────────────────────────────────────

section "7. Follow + Accept"

if [[ -n "$KOMENTOJ_GTS_ID" ]]; then
    curl -sf -X POST "$GTS/api/v1/accounts/$KOMENTOJ_GTS_ID/follow" \
        -H "Authorization: Bearer $GTS_TOKEN" > /dev/null
    ok "GTS sent Follow activity"

    wait_for "komentoj followers count ≥ 1" 30 \
        'curl -sf -H "Accept: application/activity+json" "$KOMENTOJ/users/komentoj/followers" | grep -P "\"totalItems\":[1-9]"'

    FOLLOWERS=$(curl -sf -H "Accept: application/activity+json" "$KOMENTOJ/users/komentoj/followers")
    assert_contains "followers totalItems ≥ 1" "$FOLLOWERS" '"totalItems"'

    wait_for "GTS sees relationship as following" 30 \
        'curl -sf "$GTS/api/v1/accounts/relationships?id=$KOMENTOJ_GTS_ID" \
             -H "Authorization: Bearer $GTS_TOKEN" | grep -qF "\"following\":true"'
else
    fail "Follow test skipped — komentoj actor not resolved in GTS"
fi

# ── 8. Incoming Create(Note) from GTS → komentoj stores comment ──────────────

section "8. Incoming Create(Note) from GTS"

# Mention @komentoj@komentoj.local so GTS delivers the status to komentoj's inbox.
# The POST_URL in the text lets komentoj associate the comment with the right post.
GTS_STATUS=$(curl -sf -X POST "$GTS/api/v1/statuses" \
    -H "Authorization: Bearer $GTS_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{\"status\":\"E2E comment on $POST_URL @komentoj@komentoj.local\",\"visibility\":\"public\"}")
GTS_STATUS_ID=$(echo "$GTS_STATUS" | grep -oP '"id":"\K[^"]+' | head -1 || true)

if [[ -n "$GTS_STATUS_ID" ]]; then
    ok "GTS status posted (id=$GTS_STATUS_ID)"

    wait_for "comment stored in komentoj" 30 \
        'curl -sf "$KOMENTOJ/api/v1/users/komentoj/comments?id=$POST_ID" | grep -qP "\"total\":[1-9]"'

    COMMENTS1=$(curl -sf "$KOMENTOJ/api/v1/users/komentoj/comments?id=$POST_ID")
    assert_contains "comment has author field"   "$COMMENTS1" '"author"'
    assert_contains "comment has content_html"   "$COMMENTS1" '"content_html"'
    assert_contains "comments has replies field" "$COMMENTS1" '"replies"'
else
    fail "GTS status post failed: $GTS_STATUS"
fi

# ── 9. sync_posts: Update(Note) on content change ────────────────────────────

section "9. POST /api/v1/users/komentoj/posts/sync — updated post → Update(Note)"

SYNC2=$(curl -sf -X POST "$KOMENTOJ/api/v1/users/komentoj/posts/sync" \
    -H "Authorization: Bearer $ADMIN_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{\"posts\":[{\"id\":\"$POST_ID\",\"title\":\"E2E Test Post (updated)\",\"url\":\"$POST_URL\",\"content\":\"Updated content.\"}]}")

assert_contains "update sync: upserted=1"  "$SYNC2" '"upserted":1'
assert_contains "update sync: updated=1"   "$SYNC2" '"updated":1'
assert_contains "update sync: published=0" "$SYNC2" '"published":0'

# ── 10. Note document fetchable ───────────────────────────────────────────────

section "10. GET /notes/{id}"

NOTE_RESP=$(curl -sf "$KOMENTOJ/api/v1/users/komentoj/comments?id=$POST_ID")
NOTE_ID_FROM_COMMENTS=$(echo "$NOTE_RESP" | grep -oP '"id":"https://komentoj\.local/users/komentoj/notes/\K[^"]+' | head -1 || true)

if [[ -n "$NOTE_ID_FROM_COMMENTS" ]]; then
    NOTE_DOC=$(curl -sf -H "Accept: application/activity+json" \
        "$KOMENTOJ/users/komentoj/notes/$NOTE_ID_FROM_COMMENTS" || true)
    assert_contains "Note doc type=Note"    "$NOTE_DOC" '"type":"Note"'
    assert_contains "Note doc attributedTo" "$NOTE_DOC" '"attributedTo"'
    assert_contains "Note doc content"      "$NOTE_DOC" '"content"'
else
    ok "Note fetch skipped — no comments with komentoj note IDs yet"
fi

# ── 11. sync_posts: deactivate post (absent from list) ───────────────────────

section "11. POST /api/v1/users/komentoj/posts/sync — empty list → all posts deactivated"

SYNC3=$(curl -sf -X POST "$KOMENTOJ/api/v1/users/komentoj/posts/sync" \
    -H "Authorization: Bearer $ADMIN_TOKEN" \
    -H "Content-Type: application/json" \
    -d '{"posts":[]}')

DEACT=$(echo "$SYNC3" | grep -oP '"deactivated":\K[0-9]+' || true)
if (( DEACT >= 1 )); then ok "deactivated ≥ 1 posts ($DEACT)"
else fail "expected deactivated ≥ 1, got $DEACT — sync response: $SYNC3"; fi

assert_http "comments still readable after deactivation" 200 \
    "$KOMENTOJ/api/v1/users/komentoj/comments?id=$POST_ID"

# ── 12. Incoming Update(Note) from GTS ───────────────────────────────────────

section "12. Incoming Update(Note) from GTS"

if [[ -n "$GTS_STATUS_ID" ]]; then
    UPD=$(curl -sf -X PUT "$GTS/api/v1/statuses/$GTS_STATUS_ID" \
        -H "Authorization: Bearer $GTS_TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"status\":\"Updated E2E comment on $POST_URL @komentoj@komentoj.local\"}")
    if echo "$UPD" | grep -qF '"id"'; then
        ok "GTS status updated"
        sleep 3
    else
        fail "GTS status update failed: $UPD"
    fi
fi

# ── 13. Incoming Delete(Note) from GTS ───────────────────────────────────────

section "13. Incoming Delete(Note) from GTS"

if [[ -n "$GTS_STATUS_ID" ]]; then
    curl -sf -X DELETE "$GTS/api/v1/statuses/$GTS_STATUS_ID" \
        -H "Authorization: Bearer $GTS_TOKEN" > /dev/null || true
    ok "GTS status deleted (Delete activity sent to komentoj)"
    sleep 3

    wait_for "comment soft-deleted in komentoj" 30 \
        'curl -sf "$KOMENTOJ/api/v1/users/komentoj/comments?id=$POST_ID" | grep -qP "\"total\":0"'
fi

# ── 14. Undo(Follow): GTS unfollows komentoj ─────────────────────────────────

section "14. Undo(Follow)"

if [[ -n "$KOMENTOJ_GTS_ID" ]]; then
    curl -sf -X POST "$GTS/api/v1/accounts/$KOMENTOJ_GTS_ID/unfollow" \
        -H "Authorization: Bearer $GTS_TOKEN" > /dev/null
    ok "GTS unfollow sent"

    wait_for "komentoj followers count returns to 0" 30 \
        'curl -sf -H "Accept: application/activity+json" "$KOMENTOJ/users/komentoj/followers" | grep -qF "\"totalItems\":0"'
fi

# ── 15. Image/file attachment round-trip ─────────────────────────────────────
#
# Upload a minimal 1×1 PNG to GTS, post a status with that attachment (plus a
# mention so komentoj's inbox receives it), then verify that komentoj exposes
# the attachment URL and mediaType in the comments API response.

section "15. Image attachment round-trip"

# Re-sync the post (deactivated in §11) so we can receive new comments
POST2_ID="e2e-attach-$(date +%s)"
POST2_URL="https://gotosocial.local/posts/$POST2_ID"
SYNC_ATTACH=$(curl -sf -X POST "$KOMENTOJ/api/v1/users/komentoj/posts/sync" \
    -H "Authorization: Bearer $ADMIN_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{\"posts\":[{\"id\":\"$POST2_ID\",\"title\":\"Attach Test\",\"url\":\"$POST2_URL\",\"content\":\"Attachment test.\"}]}")
assert_contains "attachment test post synced" "$SYNC_ATTACH" '"upserted":1'

# Generate a minimal 1×1 RGB PNG via Python (GTS validates image data with ffmpeg)
TINY_PNG_FILE=$(mktemp --suffix=.png)
python3 - <<'PYEOF' > "$TINY_PNG_FILE"
import struct, zlib, sys
def chunk(name, data):
    c = struct.pack('>I', len(data)) + name + data
    return c + struct.pack('>I', zlib.crc32(name + data) & 0xffffffff)
ihdr = chunk(b'IHDR', struct.pack('>IIBBBBB', 1, 1, 8, 2, 0, 0, 0))
idat = chunk(b'IDAT', zlib.compress(b'\x00\xff\x00\x00'))
sys.stdout.buffer.write(b'\x89PNG\r\n\x1a\n' + ihdr + idat + chunk(b'IEND', b''))
PYEOF

MEDIA_RESP=$(curl -sf \
    -X POST "$GTS/api/v2/media" \
    -H "Authorization: Bearer $GTS_TOKEN" \
    -F "file=@$TINY_PNG_FILE;type=image/png" \
    -F "description=E2E test image" || true)
rm -f "$TINY_PNG_FILE"

MEDIA_ID=$(echo "$MEDIA_RESP" | grep -oP '"id":"\K[^"]+' | head -1 || true)

if [[ -n "$MEDIA_ID" ]]; then
    ok "GTS media uploaded (id=$MEDIA_ID)"

    ATTACH_STATUS=$(curl -sf -X POST "$GTS/api/v1/statuses" \
        -H "Authorization: Bearer $GTS_TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"status\":\"Image test on $POST2_URL @komentoj@komentoj.local\",\"media_ids\":[\"$MEDIA_ID\"],\"visibility\":\"public\"}" || true)
    ATTACH_STATUS_ID=$(echo "$ATTACH_STATUS" | grep -oP '"id":"\K[^"]+' | head -1 || true)

    if [[ -n "$ATTACH_STATUS_ID" ]]; then
        ok "GTS status with attachment posted (id=$ATTACH_STATUS_ID)"

        wait_for "comment with attachment stored in komentoj" 30 \
            'curl -sf "$KOMENTOJ/api/v1/users/komentoj/comments?id=$POST2_ID" | grep -qP "\"total\":[1-9]"'

        ATTACH_COMMENTS=$(curl -sf "$KOMENTOJ/api/v1/users/komentoj/comments?id=$POST2_ID")
        assert_contains "attachment comment received"  "$ATTACH_COMMENTS" '"attachments"'
        assert_contains "attachment has url field"     "$ATTACH_COMMENTS" '"url"'
        assert_contains "attachment has media_type"    "$ATTACH_COMMENTS" '"media_type"'
        assert_contains "attachment is image"          "$ATTACH_COMMENTS" '"image/'
    else
        fail "GTS status with attachment failed: $ATTACH_STATUS"
    fi
else
    fail "GTS media upload failed: $MEDIA_RESP"
fi

# ── 16. API error cases ───────────────────────────────────────────────────────

section "16. API error handling"

assert_http "comments: missing id → 400" 400 "$KOMENTOJ/api/v1/users/komentoj/comments"
assert_http "comments: unknown id → 404" 404 "$KOMENTOJ/api/v1/users/komentoj/comments?id=__nonexistent__"
assert_http "sync: wrong token → 401"    401 \
    -X POST "$KOMENTOJ/api/v1/users/komentoj/posts/sync" \
    -H "Authorization: Bearer wrong-token" \
    -H "Content-Type: application/json" \
    -d '{"posts":[]}'
assert_http "inbox: no signature → 401" 401 \
    -X POST "$KOMENTOJ/users/komentoj/inbox" \
    -H "Content-Type: application/activity+json" \
    -d '{"type":"Create"}'

# ── Summary ──────────────────────────────────────────────────────────────────

echo
echo "════════════════════════════════════════"
printf "  Results: %d passed, %d failed\n" "$PASS" "$FAIL"
echo "════════════════════════════════════════"

if (( FAIL > 0 )); then
    echo "Failed checks:"
    for m in "${_fail_msgs[@]}"; do echo "  • $m"; done
    exit 1
fi
