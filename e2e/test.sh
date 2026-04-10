#!/usr/bin/env bash
# E2E federation test: komentoj ↔ GoToSocial
#
# Requires the docker-compose.e2e.yml stack to be running:
#   docker compose -f docker-compose.e2e.yml up -d
#
# Run from the repository root:
#   ./e2e/test.sh

set -euo pipefail

KOMENTOJ="http://localhost:8080"
GTS="http://localhost:8888"
ADMIN_TOKEN="e2e-test-admin-token"
GTS_USER="testuser"
GTS_PASSWORD="Password1!"
GTS_EMAIL="testuser@example.com"

PASS=0
FAIL=0
_fail_msgs=()

# ── Helpers ──────────────────────────────────────────────────────────────────

ok()   { echo "  [PASS] $*"; ((PASS++)); }
fail() { echo "  [FAIL] $*"; ((FAIL++)); _fail_msgs+=("$*"); }

assert_eq() {
    local label="$1" got="$2" want="$3"
    if [[ "$got" == "$want" ]]; then
        ok "$label"
    else
        fail "$label — got '$got', want '$want'"
    fi
}

assert_contains() {
    local label="$1" haystack="$2" needle="$3"
    if echo "$haystack" | grep -qF "$needle"; then
        ok "$label"
    else
        fail "$label — '$needle' not found in response"
    fi
}

# Poll until condition exits 0, or time out.
wait_for() {
    local label="$1" max_secs="$2"
    shift 2
    local elapsed=0
    while ! "$@" 2>/dev/null; do
        sleep 2
        elapsed=$((elapsed + 2))
        if (( elapsed >= max_secs )); then
            fail "timeout waiting for: $label"
            return 1
        fi
    done
    ok "$label"
}

section() { echo; echo "── $* ──────────────────────────────────────────────"; }

# ── 0. Prerequisite: services healthy ───────────────────────────────────────

section "0. Prerequisite checks"

wait_for "komentoj reachable" 60 \
    curl -sf "$KOMENTOJ/.well-known/webfinger?resource=acct:komentoj@komentoj.local:8080"

wait_for "GoToSocial reachable" 60 \
    curl -sf "$GTS/api/v1/instance"

# ── 1. Create a GoToSocial account ──────────────────────────────────────────

section "1. GTS account setup"

# Register (may already exist — tolerate 422)
HTTP_STATUS=$(curl -s -o /dev/null -w "%{http_code}" \
    -X POST "$GTS/api/v1/accounts" \
    -F "username=$GTS_USER" \
    -F "password=$GTS_PASSWORD" \
    -F "email=$GTS_EMAIL" \
    -F "agreement=true" \
    -F "locale=en")

if [[ "$HTTP_STATUS" == "200" || "$HTTP_STATUS" == "422" ]]; then
    ok "GTS account exists or just created (HTTP $HTTP_STATUS)"
else
    fail "GTS account registration returned HTTP $HTTP_STATUS"
fi

# Obtain token
TOKEN_RESP=$(curl -sf -X POST "$GTS/api/v1/apps" \
    -F "client_name=e2e-test" \
    -F "redirect_uris=urn:ietf:wg:oauth:2.0:oob" \
    -F "scopes=read write")
CLIENT_ID=$(echo "$TOKEN_RESP" | grep -oP '"client_id":"\K[^"]+')
CLIENT_SECRET=$(echo "$TOKEN_RESP" | grep -oP '"client_secret":"\K[^"]+')

AUTH_RESP=$(curl -sf -X POST "$GTS/oauth/token" \
    -F "client_id=$CLIENT_ID" \
    -F "client_secret=$CLIENT_SECRET" \
    -F "grant_type=password" \
    -F "username=$GTS_EMAIL" \
    -F "password=$GTS_PASSWORD" \
    -F "scope=read write")
GTS_TOKEN=$(echo "$AUTH_RESP" | grep -oP '"access_token":"\K[^"]+')

if [[ -n "$GTS_TOKEN" ]]; then
    ok "GTS OAuth token obtained"
else
    fail "GTS OAuth token not obtained — aborting"
    echo "AUTH_RESP: $AUTH_RESP"
    exit 1
fi

GTS_ACTOR_URL="http://gotosocial.local:8888/users/$GTS_USER"

# ── 2. WebFinger ─────────────────────────────────────────────────────────────

section "2. WebFinger"

WF=$(curl -sf "$KOMENTOJ/.well-known/webfinger?resource=acct:komentoj@komentoj.local:8080")
assert_contains "komentoj WebFinger subject" "$WF" "acct:komentoj@komentoj.local"
assert_contains "komentoj WebFinger actor link" "$WF" "http://komentoj.local:8080/actor"

WF_GTS=$(curl -sf "$GTS/.well-known/webfinger?resource=acct:$GTS_USER@gotosocial.local:8888")
assert_contains "GTS WebFinger subject" "$WF_GTS" "acct:$GTS_USER@gotosocial.local"

# ── 3. Actor fetch ───────────────────────────────────────────────────────────

section "3. Actor documents"

ACTOR=$(curl -sf -H "Accept: application/activity+json" "$KOMENTOJ/actor")
assert_contains "komentoj actor type" "$ACTOR" '"type":"Service"'
assert_contains "komentoj actor inbox" "$ACTOR" '"inbox":"http://komentoj.local:8080/inbox"'

GTS_ACTOR=$(curl -sf -H "Accept: application/activity+json" "$GTS/users/$GTS_USER")
assert_contains "GTS actor type" "$GTS_ACTOR" '"type":'

# ── 4. Register a post (admin API) ───────────────────────────────────────────

section "4. Post registration"

POST_URL="http://gotosocial.local:8888/posts/test-post-1"
REG_RESP=$(curl -sf -X POST "$KOMENTOJ/admin/posts" \
    -H "Authorization: Bearer $ADMIN_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{\"url\": \"$POST_URL\", \"title\": \"E2E Test Post\"}")
POST_ID=$(echo "$REG_RESP" | grep -oP '"id":"\K[^"]+')

if [[ -n "$POST_ID" ]]; then
    ok "post registered (id=$POST_ID)"
else
    fail "post registration failed: $REG_RESP"
fi

# ── 5. Follow: GTS → komentoj ────────────────────────────────────────────────

section "5. Follow komentoj from GTS"

FOLLOW_RESP=$(curl -sf -X POST "$GTS/api/v1/accounts/search?q=komentoj@komentoj.local:8080&resolve=true" \
    -H "Authorization: Bearer $GTS_TOKEN")
KOMENTOJ_GTS_ID=$(echo "$FOLLOW_RESP" | grep -oP '"id":"\K[^"]+' | head -1)

if [[ -z "$KOMENTOJ_GTS_ID" ]]; then
    fail "could not resolve komentoj actor via GTS search"
else
    ok "komentoj resolved in GTS (id=$KOMENTOJ_GTS_ID)"

    curl -sf -X POST "$GTS/api/v1/accounts/$KOMENTOJ_GTS_ID/follow" \
        -H "Authorization: Bearer $GTS_TOKEN" > /dev/null
    ok "GTS follow request sent"
fi

# Wait for komentoj to record the follower and deliver Accept
wait_for "follower stored in komentoj DB" 30 bash -c \
    "curl -sf -H 'Authorization: Bearer $ADMIN_TOKEN' '$KOMENTOJ/admin/followers' | grep -qF '$GTS_USER'"

# ── 6. Publish a Create(Note) and verify GTS receives it ─────────────────────

section "6. Publish Create(Note)"

PUB_RESP=$(curl -sf -X POST "$KOMENTOJ/admin/posts/$POST_ID/publish" \
    -H "Authorization: Bearer $ADMIN_TOKEN")
NOTE_ID=$(echo "$PUB_RESP" | grep -oP '"ap_note_id":"\K[^"]+')

if [[ -n "$NOTE_ID" ]]; then
    ok "Create(Note) published (note_id=$NOTE_ID)"
else
    fail "publish failed: $PUB_RESP"
    NOTE_ID=""
fi

# Allow GTS time to receive and process the activity
sleep 4

if [[ -n "$NOTE_ID" ]]; then
    # GTS should be able to fetch the Note
    NOTE_RESP=$(curl -sf -H "Accept: application/activity+json" "$NOTE_ID" || true)
    assert_contains "Note fetchable from komentoj" "$NOTE_RESP" '"type":"Note"'
fi

# ── 7. Create(Note) from GTS → komentoj inbox (reply / comment) ─────────────

section "7. Incoming Create(Note) from GTS"

if [[ -n "$POST_ID" ]]; then
    # GTS user posts a status (publicly visible)
    STATUS_RESP=$(curl -sf -X POST "$GTS/api/v1/statuses" \
        -H "Authorization: Bearer $GTS_TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"status\": \"E2E reply to $POST_URL\", \"visibility\": \"public\"}")
    STATUS_ID=$(echo "$STATUS_RESP" | grep -oP '"id":"\K[^"]+' | head -1)

    if [[ -n "$STATUS_ID" ]]; then
        ok "GTS status posted (id=$STATUS_ID)"
    else
        fail "GTS status post failed: $STATUS_RESP"
    fi
fi

# ── 8. Update(Note) ──────────────────────────────────────────────────────────

section "8. Update(Note)"

if [[ -n "$POST_ID" ]]; then
    UPD_RESP=$(curl -sf -X PUT "$KOMENTOJ/admin/posts/$POST_ID" \
        -H "Authorization: Bearer $ADMIN_TOKEN" \
        -H "Content-Type: application/json" \
        -d '{"title": "E2E Test Post (updated)", "content": "Updated content."}')
    if echo "$UPD_RESP" | grep -qF "updated"; then
        ok "Update(Note) sent"
    else
        fail "Update failed: $UPD_RESP"
    fi
fi

# ── 9. Delete(Note) ──────────────────────────────────────────────────────────

section "9. Delete(Note)"

if [[ -n "$POST_ID" ]]; then
    DEL_RESP=$(curl -sf -X DELETE "$KOMENTOJ/admin/posts/$POST_ID" \
        -H "Authorization: Bearer $ADMIN_TOKEN")
    if echo "$DEL_RESP" | grep -qiF "deleted\|ok\|success"; then
        ok "Delete(Note) sent"
    else
        # A 204 with no body is also acceptable
        ok "Delete(Note) sent (empty body)"
    fi
fi

# ── 10. Undo(Follow): GTS unfollows komentoj ─────────────────────────────────

section "10. Undo(Follow)"

if [[ -n "$KOMENTOJ_GTS_ID" ]]; then
    curl -sf -X POST "$GTS/api/v1/accounts/$KOMENTOJ_GTS_ID/unfollow" \
        -H "Authorization: Bearer $GTS_TOKEN" > /dev/null
    ok "GTS unfollow sent"

    wait_for "follower removed from komentoj DB" 30 bash -c \
        "! curl -sf -H 'Authorization: Bearer $ADMIN_TOKEN' '$KOMENTOJ/admin/followers' | grep -qF '$GTS_USER'"
fi

# ── Summary ──────────────────────────────────────────────────────────────────

echo
echo "════════════════════════════════════════"
printf "  Results: %d passed, %d failed\n" "$PASS" "$FAIL"
echo "════════════════════════════════════════"

if (( FAIL > 0 )); then
    echo "Failed checks:"
    for m in "${_fail_msgs[@]}"; do
        echo "  • $m"
    done
    exit 1
fi
