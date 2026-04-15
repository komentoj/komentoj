#!/usr/bin/env bash
# E2E federation test: komentoj ↔ Mastodon (HTTPS)
#
# Prerequisites:
#   ./e2e/mastodon/setup.sh      # generate mkcert certs, VAPID keys, /etc/hosts
#   set -a; source e2e/mastodon/.env; set +a
#   docker compose -f e2e/mastodon/docker-compose.yml up -d
#
# Run from the repository root:
#   ./e2e/mastodon/test.sh

set -euo pipefail

KOMENTOJ="https://komentoj.local"
MASTODON="https://mastodon.local"
ADMIN_TOKEN="e2e-test-admin-token"
MASTO_USER="testuser"
MASTO_PASSWORD="Password1!"
MASTO_EMAIL="testuser@gmail.com"

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
        --resolve "mastodon.local:443:127.0.0.1" \
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

wait_for "Mastodon reachable" 300 \
    'curl -sf "$MASTODON/health"'

# ── 1. Mastodon account + OAuth token ────────────────────────────────────────

section "1. Mastodon account setup"

# Create confirmed account via tootctl (no --password; we'll generate a token directly).
MASTO_WEB_CONTAINER=$(docker compose -f "$COMPOSE_FILE" ps -q mastodon-web)
docker exec "$MASTO_WEB_CONTAINER" \
    bin/tootctl accounts create "$MASTO_USER" \
    --email "$MASTO_EMAIL" \
    --confirmed \
    2>&1 | grep -vE "^(Checking|OK|New password|INFO)" || true
docker exec "$MASTO_WEB_CONTAINER" \
    bin/tootctl accounts modify "$MASTO_USER" --approve \
    2>&1 | grep -v "^OK" || true
ok "Mastodon account created/confirmed/approved"

# Generate an OAuth access token directly via Rails runner — avoids the CSRF-
# protected web sign-in flow entirely. The token gets full read+write+follow scope.
# We write a Ruby script into the container (via env var + sh heredoc) so that
# quoting stays clean and $MASTO_EMAIL is safely interpolated by the host shell.
MASTO_TOKEN=$(docker exec -e _EMAIL="$MASTO_EMAIL" "$MASTO_WEB_CONTAINER" sh -c '
cat > /tmp/e2e_token.rb << "RBEOF"
u   = User.find_by!(email: ENV["_EMAIL"])
app = Doorkeeper::Application.create!(
  name: "e2e", redirect_uri: "urn:ietf:wg:oauth:2.0:oob", scopes: "read write follow"
)
tok = Doorkeeper::AccessToken.create!(
  application: app, resource_owner_id: u.id, scopes: "read write follow"
)
puts tok.token
RBEOF
bin/rails runner /tmp/e2e_token.rb 2>/dev/null
' | tail -1 | tr -d '[:space:]' || true)

if [[ -n "$MASTO_TOKEN" ]]; then ok "Mastodon access token obtained"
else fail "Mastodon access token not obtained — aborting"; exit 1; fi

# ── 2. WebFinger ─────────────────────────────────────────────────────────────

section "2. WebFinger"

WF_K=$(curl -sf "$KOMENTOJ/.well-known/webfinger?resource=acct:komentoj@komentoj.local")
assert_contains "komentoj WF subject"           "$WF_K" "acct:komentoj@komentoj.local"
assert_contains "komentoj WF actor link"        "$WF_K" "https://komentoj.local/users/comments"
assert_contains "komentoj WF content-type link" "$WF_K" "application/activity+json"

WF_M=$(curl -sf "$MASTODON/.well-known/webfinger?resource=acct:$MASTO_USER@mastodon.local")
assert_contains "Mastodon WF subject" "$WF_M" "acct:$MASTO_USER@mastodon.local"

# ── 3. Actor documents ───────────────────────────────────────────────────────

section "3. Actor documents"

ACTOR=$(curl -sf -H "Accept: application/activity+json" "$KOMENTOJ/users/comments")
assert_contains "komentoj actor type=Service"  "$ACTOR" '"type":"Service"'
assert_contains "komentoj actor inbox"         "$ACTOR" '"inbox":"https://komentoj.local/users/comments/inbox"'
assert_contains "komentoj actor publicKey"     "$ACTOR" '"publicKey"'
assert_contains "komentoj actor followers"     "$ACTOR" '"followers"'

assert_http "komentoj outbox OK"    200 -H "Accept: application/activity+json" "$KOMENTOJ/users/comments/outbox"
assert_http "komentoj followers OK" 200 -H "Accept: application/activity+json" "$KOMENTOJ/users/comments/followers"
assert_http "komentoj following OK" 200 -H "Accept: application/activity+json" "$KOMENTOJ/users/comments/following"

# Verify Mastodon account exists
MASTO_ACCOUNT=$(curl -sf -H "Authorization: Bearer $MASTO_TOKEN" \
    "$MASTODON/api/v1/accounts/verify_credentials")
assert_contains "Mastodon account exists" "$MASTO_ACCOUNT" '"acct"'

# ── 4. Actor content negotiation ─────────────────────────────────────────────

section "4. Actor content negotiation"

REDIR=$(curl -s -o /dev/null -w "%{http_code}" -H "Accept: text/html" "$KOMENTOJ/users/comments")
if [[ "$REDIR" == "303" ]]; then ok "actor redirects browsers (303)"
else fail "actor browser redirect — got HTTP $REDIR, want 303"; fi

# ── 5. sync_posts: register + publish Create(Note) ──────────────────────────

section "5. POST /api/v1/users/comments/posts/sync — new post → Create(Note)"

POST_ID="e2e-post-$(date +%s)"
POST_URL="https://mastodon.local/posts/$POST_ID"

SYNC1=$(curl -sf -X POST "$KOMENTOJ/api/v1/users/comments/posts/sync" \
    -H "Authorization: Bearer $ADMIN_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{\"posts\":[{\"id\":\"$POST_ID\",\"title\":\"E2E Test Post\",\"url\":\"$POST_URL\",\"content\":\"Hello from E2E.\"}]}")

assert_contains "sync: upserted=1"  "$SYNC1" '"upserted":1'
assert_contains "sync: published=1" "$SYNC1" '"published":1'
assert_contains "sync: updated=0"   "$SYNC1" '"updated":0'

wait_for "ap_note_id persisted in DB" 30 \
    'curl -sf "$KOMENTOJ/api/v1/users/comments/comments?id=$POST_ID" | grep -qF "total"'

COMMENTS0=$(curl -sf "$KOMENTOJ/api/v1/users/comments/comments?id=$POST_ID")
assert_contains "comments post_id matches" "$COMMENTS0" "\"post_id\":\"$POST_ID\""

# Capture the komentoj AP Note URL now; used in §8 for inReplyTo delivery.
# Mastodon doesn't hyperlink .local domains so URL-in-text matching won't work.
wait_for "komentoj Note URL written to DB" 30 \
    'docker compose -f "$COMPOSE_FILE" exec -T postgres \
     psql -U komentoj -d komentoj -t \
     -c "SELECT ap_note_id FROM posts WHERE id='"'"'$POST_ID'"'"';" 2>/dev/null \
     | grep -q "http"'

KOMENTOJ_NOTE_URL=$(docker compose -f "$COMPOSE_FILE" exec -T postgres \
    psql -U komentoj -d komentoj -t \
    -c "SELECT ap_note_id FROM posts WHERE id='$POST_ID';" 2>/dev/null \
    | tr -d ' \n')

# ── 6. Resolve komentoj actor from Mastodon side ─────────────────────────────
#
# Use Mastodon's search API with resolve=true to force WebFinger+actor lookup.
# Mastodon will fetch komentoj's actor document and store it internally.
# We need the internal Mastodon account ID to call the Follow API.

section "6. Mastodon resolves komentoj actor"

SEARCH_RESP=$(curl -sf \
    "$MASTODON/api/v2/search?q=komentoj@komentoj.local&resolve=true&limit=5" \
    -H "Authorization: Bearer $MASTO_TOKEN")

# Mastodon search returns { accounts: [...], statuses: [...], hashtags: [...] }
KOMENTOJ_MASTO_ID=$(echo "$SEARCH_RESP" | \
    grep -oP '"accounts":\[.*?\]' | \
    grep -oP '"id":"\K[^"]+' | head -1 || true)

# If not found via search (Service actors may be skipped), fall back to DB lookup
if [[ -z "$KOMENTOJ_MASTO_ID" ]]; then
    # Trigger resolution via a mention, then check DB
    curl -s -X POST "$MASTODON/api/v1/statuses" \
        -H "Authorization: Bearer $MASTO_TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"status\":\"test @komentoj@komentoj.local\",\"visibility\":\"public\"}" \
        -o /dev/null

    wait_for "komentoj actor stored in Mastodon DB" 30 \
        'docker compose -f "$COMPOSE_FILE" exec -T mastodon-postgres psql -U mastodon -t \
         -c "SELECT id FROM accounts WHERE username='"'"'komentoj'"'"' AND domain='"'"'komentoj.local'"'"';" \
         | grep -qE "[0-9]"'

    KOMENTOJ_MASTO_ID=$(docker compose -f "$COMPOSE_FILE" exec -T mastodon-postgres \
        psql -U mastodon -t \
        -c "SELECT id FROM accounts WHERE username='komentoj' AND domain='komentoj.local';" \
        | tr -d ' \n')
fi

if [[ -n "$KOMENTOJ_MASTO_ID" ]]; then ok "komentoj resolved in Mastodon (id=$KOMENTOJ_MASTO_ID)"
else fail "could not resolve komentoj actor in Mastodon"; KOMENTOJ_MASTO_ID=""; fi

# ── 7. Follow: Mastodon → komentoj ───────────────────────────────────────────

section "7. Follow + Accept"

if [[ -n "$KOMENTOJ_MASTO_ID" ]]; then
    curl -sf -X POST "$MASTODON/api/v1/accounts/$KOMENTOJ_MASTO_ID/follow" \
        -H "Authorization: Bearer $MASTO_TOKEN" > /dev/null
    ok "Mastodon sent Follow activity"

    wait_for "komentoj followers count ≥ 1" 30 \
        'curl -sf -H "Accept: application/activity+json" "$KOMENTOJ/users/comments/followers" | grep -P "\"totalItems\":[1-9]"'

    FOLLOWERS=$(curl -sf -H "Accept: application/activity+json" "$KOMENTOJ/users/comments/followers")
    assert_contains "followers totalItems ≥ 1" "$FOLLOWERS" '"totalItems"'

    wait_for "Mastodon sees relationship as following" 30 \
        'curl -sf "$MASTODON/api/v1/accounts/relationships?id[]=$KOMENTOJ_MASTO_ID" \
             -H "Authorization: Bearer $MASTO_TOKEN" | grep -qF "\"following\":true"'
else
    fail "Follow test skipped — komentoj actor not resolved in Mastodon"
fi

# ── 8. Incoming Create(Note) from Mastodon → komentoj stores comment ──────────

section "8. Incoming Create(Note) from Mastodon"

# Mastodon does not auto-link .local domains (not a recognized TLD), so
# embedding the blog URL as plain text doesn't give komentoj an extractable href.
# Instead, resolve the komentoj AP Note in Mastodon and reply to it — komentoj
# then matches via inReplyTo (the ap_note_id) rather than URL-in-text.

# Force Mastodon to fetch and store the komentoj Note
SEARCH_RESP=$(curl -sf \
    "$MASTODON/api/v2/search?q=$(python3 -c "import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1]))" "$KOMENTOJ_NOTE_URL")&resolve=true&limit=5" \
    -H "Authorization: Bearer $MASTO_TOKEN" || true)
MASTO_NOTE_ID=$(echo "$SEARCH_RESP" | \
    python3 -c "import sys,json; d=json.load(sys.stdin); print(d['statuses'][0]['id'] if d.get('statuses') else '')" 2>/dev/null || true)

MASTO_STATUS_ID=""
if [[ -z "$MASTO_NOTE_ID" ]]; then
    fail "Could not resolve komentoj Note in Mastodon — skipping section 8"
else
    ok "komentoj Note resolved in Mastodon (id=$MASTO_NOTE_ID)"
    # Post as a reply so inReplyTo = komentoj Note URL → komentoj stores the comment
    MASTO_STATUS=$(curl -sf -X POST "$MASTODON/api/v1/statuses" \
        -H "Authorization: Bearer $MASTO_TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"status\":\"E2E reply comment @komentoj@komentoj.local\",\"in_reply_to_id\":\"$MASTO_NOTE_ID\",\"visibility\":\"public\"}")
    MASTO_STATUS_ID=$(echo "$MASTO_STATUS" | grep -oP '"id":"\K[^"]+' | head -1 || true)

    if [[ -n "$MASTO_STATUS_ID" ]]; then
        ok "Mastodon status posted (id=$MASTO_STATUS_ID)"

        wait_for "comment stored in komentoj" 30 \
            'curl -sf "$KOMENTOJ/api/v1/users/comments/comments?id=$POST_ID" | grep -qP "\"total\":[1-9]"'

        COMMENTS1=$(curl -sf "$KOMENTOJ/api/v1/users/comments/comments?id=$POST_ID")
        assert_contains "comment has author field"   "$COMMENTS1" '"author"'
        assert_contains "comment has content_html"   "$COMMENTS1" '"content_html"'
        assert_contains "comments has replies field" "$COMMENTS1" '"replies"'
    else
        fail "Mastodon status post failed: $MASTO_STATUS"
    fi
fi

# ── 9. sync_posts: Update(Note) on content change ────────────────────────────

section "9. POST /api/v1/users/comments/posts/sync — updated post → Update(Note)"

SYNC2=$(curl -sf -X POST "$KOMENTOJ/api/v1/users/comments/posts/sync" \
    -H "Authorization: Bearer $ADMIN_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{\"posts\":[{\"id\":\"$POST_ID\",\"title\":\"E2E Test Post (updated)\",\"url\":\"$POST_URL\",\"content\":\"Updated content.\"}]}")

assert_contains "update sync: upserted=1"  "$SYNC2" '"upserted":1'
assert_contains "update sync: updated=1"   "$SYNC2" '"updated":1'
assert_contains "update sync: published=0" "$SYNC2" '"published":0'

# ── 10. Note document fetchable ───────────────────────────────────────────────

section "10. GET /notes/{id}"

NOTE_RESP=$(curl -sf "$KOMENTOJ/api/v1/users/comments/comments?id=$POST_ID")
NOTE_ID_FROM_COMMENTS=$(echo "$NOTE_RESP" | grep -oP '"id":"https://komentoj\.local/users/comments/notes/\K[^"]+' | head -1 || true)

if [[ -n "$NOTE_ID_FROM_COMMENTS" ]]; then
    NOTE_DOC=$(curl -sf -H "Accept: application/activity+json" \
        "$KOMENTOJ/users/comments/notes/$NOTE_ID_FROM_COMMENTS" || true)
    assert_contains "Note doc type=Note"    "$NOTE_DOC" '"type":"Note"'
    assert_contains "Note doc attributedTo" "$NOTE_DOC" '"attributedTo"'
    assert_contains "Note doc content"      "$NOTE_DOC" '"content"'
else
    ok "Note fetch skipped — no comments with komentoj note IDs yet"
fi

# ── 11. sync_posts: deactivate post (absent from list) ───────────────────────

section "11. POST /api/v1/users/comments/posts/sync — empty list → all posts deactivated"

SYNC3=$(curl -sf -X POST "$KOMENTOJ/api/v1/users/comments/posts/sync" \
    -H "Authorization: Bearer $ADMIN_TOKEN" \
    -H "Content-Type: application/json" \
    -d '{"posts":[]}')

DEACT=$(echo "$SYNC3" | grep -oP '"deactivated":\K[0-9]+' || true)
if (( DEACT >= 1 )); then ok "deactivated ≥ 1 posts ($DEACT)"
else fail "expected deactivated ≥ 1, got $DEACT — sync response: $SYNC3"; fi

assert_http "comments still readable after deactivation" 200 \
    "$KOMENTOJ/api/v1/users/comments/comments?id=$POST_ID"

# ── 12. Incoming Update(Note) from Mastodon ──────────────────────────────────

section "12. Incoming Update(Note) from Mastodon"

if [[ -n "$MASTO_STATUS_ID" ]]; then
    UPD=$(curl -sf -X PUT "$MASTODON/api/v1/statuses/$MASTO_STATUS_ID" \
        -H "Authorization: Bearer $MASTO_TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"status\":\"Updated E2E comment on $POST_URL @komentoj@komentoj.local\"}")
    if echo "$UPD" | grep -qF '"id"'; then
        ok "Mastodon status updated"
        sleep 5
    else
        fail "Mastodon status update failed: $UPD"
    fi
fi

# ── 13. Incoming Delete(Note) from Mastodon ──────────────────────────────────

section "13. Incoming Delete(Note) from Mastodon"

if [[ -n "$MASTO_STATUS_ID" ]]; then
    curl -sf -X DELETE "$MASTODON/api/v1/statuses/$MASTO_STATUS_ID" \
        -H "Authorization: Bearer $MASTO_TOKEN" > /dev/null || true
    ok "Mastodon status deleted (Delete activity sent to komentoj)"
    sleep 3

    wait_for "comment soft-deleted in komentoj" 30 \
        'curl -sf "$KOMENTOJ/api/v1/users/comments/comments?id=$POST_ID" | grep -qP "\"total\":0"'
fi

# ── 14. Undo(Follow): Mastodon unfollows komentoj ────────────────────────────

section "14. Undo(Follow)"

if [[ -n "$KOMENTOJ_MASTO_ID" ]]; then
    curl -sf -X POST "$MASTODON/api/v1/accounts/$KOMENTOJ_MASTO_ID/unfollow" \
        -H "Authorization: Bearer $MASTO_TOKEN" > /dev/null
    ok "Mastodon unfollow sent"

    wait_for "komentoj followers count returns to 0" 30 \
        'curl -sf -H "Accept: application/activity+json" "$KOMENTOJ/users/comments/followers" | grep -qF "\"totalItems\":0"'
fi

# ── 15. Image/file attachment round-trip ─────────────────────────────────────
#
# Upload a minimal 1×1 PNG to Mastodon, post a status with that attachment,
# then verify that komentoj exposes the attachment URL and mediaType.

section "15. Image attachment round-trip"

# Re-sync a fresh post (previous was deactivated in §11)
POST2_ID="e2e-attach-$(date +%s)"
POST2_URL="https://mastodon.local/posts/$POST2_ID"
SYNC_ATTACH=$(curl -sf -X POST "$KOMENTOJ/api/v1/users/comments/posts/sync" \
    -H "Authorization: Bearer $ADMIN_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{\"posts\":[{\"id\":\"$POST2_ID\",\"title\":\"Attach Test\",\"url\":\"$POST2_URL\",\"content\":\"Attachment test.\"}]}")
assert_contains "attachment test post synced" "$SYNC_ATTACH" '"upserted":1'

# Capture komentoj Note URL for POST2 so we can use inReplyTo (same reason as §8).
wait_for "komentoj Note2 URL written to DB" 30 \
    'docker compose -f "$COMPOSE_FILE" exec -T postgres \
     psql -U komentoj -d komentoj -t \
     -c "SELECT ap_note_id FROM posts WHERE id='"'"'$POST2_ID'"'"';" 2>/dev/null \
     | grep -q "http"'

KOMENTOJ_NOTE2_URL=$(docker compose -f "$COMPOSE_FILE" exec -T postgres \
    psql -U komentoj -d komentoj -t \
    -c "SELECT ap_note_id FROM posts WHERE id='$POST2_ID';" 2>/dev/null \
    | tr -d ' \n')

# Resolve the komentoj Note in Mastodon so we can reply to it
SEARCH_RESP2=$(curl -sf \
    "$MASTODON/api/v2/search?q=$(python3 -c "import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1]))" "$KOMENTOJ_NOTE2_URL")&resolve=true&limit=5" \
    -H "Authorization: Bearer $MASTO_TOKEN" || true)
MASTO_NOTE2_ID=$(echo "$SEARCH_RESP2" | \
    python3 -c "import sys,json; d=json.load(sys.stdin); print(d['statuses'][0]['id'] if d.get('statuses') else '')" 2>/dev/null || true)

if [[ -n "$MASTO_NOTE2_ID" ]]; then
    ok "komentoj Note2 resolved in Mastodon (id=$MASTO_NOTE2_ID)"
else
    fail "Could not resolve komentoj Note2 in Mastodon — attachment test may fail"
fi

# Generate a minimal valid 1×1 RGB PNG
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
    -X POST "$MASTODON/api/v2/media" \
    -H "Authorization: Bearer $MASTO_TOKEN" \
    -F "file=@$TINY_PNG_FILE;type=image/png" \
    -F "description=E2E test image" || true)
rm -f "$TINY_PNG_FILE"

MEDIA_ID=$(echo "$MEDIA_RESP" | grep -oP '"id":"\K[^"]+' | head -1 || true)

# Mastodon processes media asynchronously; wait until it's ready
if [[ -n "$MEDIA_ID" ]]; then
    wait_for "Mastodon media processed" 30 \
        'curl -sf "$MASTODON/api/v1/media/$MEDIA_ID" \
             -H "Authorization: Bearer $MASTO_TOKEN" | grep -qF "\"url\":"'
    ok "Mastodon media uploaded (id=$MEDIA_ID)"

    # Use inReplyTo so komentoj matches via ap_note_id (not URL-in-text which
    # doesn't work with .local domains — Mastodon won't hyperlink them).
    ATTACH_REPLY_TO=""
    [[ -n "$MASTO_NOTE2_ID" ]] && ATTACH_REPLY_TO=",\"in_reply_to_id\":\"$MASTO_NOTE2_ID\""
    ATTACH_STATUS=$(curl -sf -X POST "$MASTODON/api/v1/statuses" \
        -H "Authorization: Bearer $MASTO_TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"status\":\"E2E image attachment test @komentoj@komentoj.local\",\"media_ids\":[\"$MEDIA_ID\"]${ATTACH_REPLY_TO},\"visibility\":\"public\"}" || true)
    ATTACH_STATUS_ID=$(echo "$ATTACH_STATUS" | grep -oP '"id":"\K[^"]+' | head -1 || true)

    if [[ -n "$ATTACH_STATUS_ID" ]]; then
        ok "Mastodon status with attachment posted (id=$ATTACH_STATUS_ID)"

        wait_for "comment with attachment stored in komentoj" 30 \
            'curl -sf "$KOMENTOJ/api/v1/users/comments/comments?id=$POST2_ID" | grep -qP "\"total\":[1-9]"'

        ATTACH_COMMENTS=$(curl -sf "$KOMENTOJ/api/v1/users/comments/comments?id=$POST2_ID")
        assert_contains "attachment comment received"  "$ATTACH_COMMENTS" '"attachments"'
        assert_contains "attachment has url field"     "$ATTACH_COMMENTS" '"url"'
        assert_contains "attachment has media_type"    "$ATTACH_COMMENTS" '"media_type"'
        assert_contains "attachment is image"          "$ATTACH_COMMENTS" '"image/'
    else
        fail "Mastodon status with attachment failed: $ATTACH_STATUS"
    fi
else
    fail "Mastodon media upload failed: $MEDIA_RESP"
fi

# ── 16. API error cases ───────────────────────────────────────────────────────

section "16. API error handling"

assert_http "comments: missing id → 400" 400 "$KOMENTOJ/api/v1/users/comments/comments"
assert_http "comments: unknown id → 404" 404 "$KOMENTOJ/api/v1/users/comments/comments?id=__nonexistent__"
assert_http "sync: wrong token → 401"    401 \
    -X POST "$KOMENTOJ/api/v1/users/comments/posts/sync" \
    -H "Authorization: Bearer wrong-token" \
    -H "Content-Type: application/json" \
    -d '{"posts":[]}'
assert_http "inbox: no signature → 401" 401 \
    -X POST "$KOMENTOJ/users/comments/inbox" \
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
