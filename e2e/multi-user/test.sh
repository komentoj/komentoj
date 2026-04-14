#!/usr/bin/env bash
# E2E test for the multi-user layer — no federation involved.
#
# Brings up just komentoj + Postgres + Redis via docker-compose and exercises:
#   - POST /api/v1/admin/users  (create + list + delete)
#   - Per-user webfinger + actor document at /users/:username
#   - Per-user inbox path (404/401 behaviors, signature verification covers
#     the correct path)
#   - Per-user posts/sync authenticated by the user's own api_token
#   - Legacy /actor + /inbox still resolve to the configured owner
#
# Run from repo root:
#   docker compose -f e2e/multi-user/docker-compose.yml up -d --build
#   ./e2e/multi-user/test.sh
#   docker compose -f e2e/multi-user/docker-compose.yml down -v

set -euo pipefail

KOMENTOJ="http://127.0.0.1:18080"
ADMIN_TOKEN="e2e-multi-user-admin-token"

PASS=0
FAIL=0
_fail_msgs=()

ok()   { echo "  [PASS] $*"; PASS=$((PASS + 1)); }
fail() { echo "  [FAIL] $*"; FAIL=$((FAIL + 1)); _fail_msgs+=("$*"); }

assert_contains() {
    local label="$1" haystack="$2" needle="$3"
    if echo "$haystack" | grep -qF "$needle"; then ok "$label"
    else fail "$label — '$needle' not in response: $haystack"; fi
}

assert_http() {
    local label="$1" want="$2"
    shift 2
    local got
    got=$(curl -s -o /dev/null -w "%{http_code}" "$@")
    if [[ "$got" == "$want" ]]; then ok "$label (HTTP $got)"
    else fail "$label — HTTP $got, want $want"; fi
}

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

# ── 0. komentoj healthy ──────────────────────────────────────────────────────

section "0. komentoj reachable"

wait_for "komentoj responds to /.well-known/webfinger" 60 \
    'curl -sf "$KOMENTOJ/.well-known/webfinger?resource=acct:comments@komentoj.local"'

# ── 1. Legacy /actor still works (owner user) ────────────────────────────────

section "1. Legacy single-actor surface"

ACTOR=$(curl -sf -H "Accept: application/activity+json" "$KOMENTOJ/actor")
assert_contains "legacy /actor type=Service"    "$ACTOR" '"type":"Service"'
assert_contains "legacy /actor has publicKey"   "$ACTOR" '"publicKey"'
assert_contains "legacy /actor has inbox"       "$ACTOR" '"inbox"'

# The legacy /actor should now point to the per-user URL under the hood;
# follow the `id` to confirm it matches /users/<owner>.
OWNER_ID=$(echo "$ACTOR" | grep -oP '"id":"\K[^"]+' | head -1)
assert_contains "legacy /actor redirects to /users/<owner>" "$OWNER_ID" "/users/"

# ── 2. Admin: create a new user ──────────────────────────────────────────────

section "2. POST /api/v1/admin/users"

CREATE_RESP=$(curl -sf -X POST "$KOMENTOJ/api/v1/admin/users" \
    -H "Authorization: Bearer $ADMIN_TOKEN" \
    -H "Content-Type: application/json" \
    -d '{"username":"alice","display_name":"Alice","summary":"test user"}')

assert_contains "create user: username"     "$CREATE_RESP" '"username":"alice"'
assert_contains "create user: api_token"    "$CREATE_RESP" '"api_token"'
assert_contains "create user: plan=self_host" "$CREATE_RESP" '"plan_tier":"self_host"'

USER_TOKEN=$(echo "$CREATE_RESP" | grep -oP '"api_token":"\K[^"]+' | head -1)

# Duplicate username → 400
assert_http "create duplicate user → 400" 400 \
    -X POST "$KOMENTOJ/api/v1/admin/users" \
    -H "Authorization: Bearer $ADMIN_TOKEN" \
    -H "Content-Type: application/json" \
    -d '{"username":"alice"}'

# Invalid chars → 400
assert_http "create user invalid chars → 400" 400 \
    -X POST "$KOMENTOJ/api/v1/admin/users" \
    -H "Authorization: Bearer $ADMIN_TOKEN" \
    -H "Content-Type: application/json" \
    -d '{"username":"bad name!"}'

# Wrong admin token → 401
assert_http "create user wrong token → 401" 401 \
    -X POST "$KOMENTOJ/api/v1/admin/users" \
    -H "Authorization: Bearer wrong" \
    -H "Content-Type: application/json" \
    -d '{"username":"bob"}'

# ── 3. Per-user WebFinger + actor doc ────────────────────────────────────────

section "3. Per-user discovery"

WF=$(curl -sf "$KOMENTOJ/.well-known/webfinger?resource=acct:alice@komentoj.local")
assert_contains "alice WF subject"       "$WF" "acct:alice@komentoj.local"
assert_contains "alice WF actor link"    "$WF" "/users/alice"

ALICE_ACTOR=$(curl -sf -H "Accept: application/activity+json" "$KOMENTOJ/users/alice")
assert_contains "alice actor id"          "$ALICE_ACTOR" 'komentoj.local/users/alice"'
assert_contains "alice actor inbox"       "$ALICE_ACTOR" '/users/alice/inbox'
assert_contains "alice actor outbox"      "$ALICE_ACTOR" '/users/alice/outbox'
assert_contains "alice actor followers"   "$ALICE_ACTOR" '/users/alice/followers'
assert_contains "alice actor publicKey"   "$ALICE_ACTOR" '"publicKey"'
assert_contains "alice actor display name" "$ALICE_ACTOR" '"name":"Alice"'

# Unknown user → 404
assert_http "unknown user actor → 404" 404 \
    -H "Accept: application/activity+json" \
    "$KOMENTOJ/users/nonexistent"

assert_http "alice followers → 200" 200 \
    -H "Accept: application/activity+json" \
    "$KOMENTOJ/users/alice/followers"

# Per-user inbox exists (no signature → 401, not 404)
assert_http "alice inbox requires signature → 401" 401 \
    -X POST "$KOMENTOJ/users/alice/inbox" \
    -H "Content-Type: application/activity+json" \
    -d '{"type":"Create"}'

# ── 4. Per-user posts/sync using the user's own api_token ────────────────────

section "4. POST /api/v1/users/alice/posts/sync with user token"

POST_ID="alice-post-$(date +%s)"
POST_URL="https://alice.example.com/$POST_ID"
SYNC=$(curl -sf -X POST "$KOMENTOJ/api/v1/users/alice/posts/sync" \
    -H "Authorization: Bearer $USER_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{\"posts\":[{\"id\":\"$POST_ID\",\"title\":\"Hello\",\"url\":\"$POST_URL\",\"content\":\"Body.\"}]}")

assert_contains "alice sync upserted=1"   "$SYNC" '"upserted":1'
assert_contains "alice sync published=1"  "$SYNC" '"published":1'

# Admin token should also work on per-user route (OSS convenience)
assert_http "alice sync with admin token → 200" 200 \
    -X POST "$KOMENTOJ/api/v1/users/alice/posts/sync" \
    -H "Authorization: Bearer $ADMIN_TOKEN" \
    -H "Content-Type: application/json" \
    -d '{"posts":[]}'

# Wrong token on per-user route → 401
assert_http "alice sync with wrong token → 401" 401 \
    -X POST "$KOMENTOJ/api/v1/users/alice/posts/sync" \
    -H "Authorization: Bearer wrong-token" \
    -H "Content-Type: application/json" \
    -d '{"posts":[]}'

# Per-user comments endpoint
assert_http "alice comments endpoint OK" 200 \
    "$KOMENTOJ/api/v1/users/alice/comments?id=$POST_ID"

# ── 5. Admin: list users ─────────────────────────────────────────────────────

section "5. GET /api/v1/admin/users"

LIST=$(curl -sf -H "Authorization: Bearer $ADMIN_TOKEN" "$KOMENTOJ/api/v1/admin/users")
assert_contains "list contains alice"    "$LIST" '"username":"alice"'
# _bootstrap should be hidden
if echo "$LIST" | grep -qF '"_bootstrap"'; then
    fail "list leaks _bootstrap user"
else
    ok "list hides _bootstrap user"
fi

# ── 6. Admin: delete user ────────────────────────────────────────────────────

section "6. DELETE /api/v1/admin/users/alice"

assert_http "delete alice → 200" 200 \
    -X DELETE "$KOMENTOJ/api/v1/admin/users/alice" \
    -H "Authorization: Bearer $ADMIN_TOKEN"

assert_http "alice actor now 404" 404 \
    -H "Accept: application/activity+json" "$KOMENTOJ/users/alice"

# Refuse to delete owner
assert_http "delete owner → 400" 400 \
    -X DELETE "$KOMENTOJ/api/v1/admin/users/comments" \
    -H "Authorization: Bearer $ADMIN_TOKEN"

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
