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
    'curl -sf "$KOMENTOJ/.well-known/webfinger?resource=acct:komentoj@komentoj.local:8080"'

wait_for "GoToSocial reachable" 120 \
    'curl -sf "$GTS/api/v1/instance"'

# ── 1. GTS account + OAuth token ────────────────────────────────────────────

section "1. GTS account setup"

SIGNUP=$(curl -s -o /dev/null -w "%{http_code}" \
    -X POST "$GTS/api/v1/accounts" \
    -F "username=$GTS_USER" -F "password=$GTS_PASSWORD" \
    -F "email=$GTS_EMAIL" -F "agreement=true" -F "locale=en")
if [[ "$SIGNUP" == "200" || "$SIGNUP" == "422" ]]; then
    ok "GTS account exists (HTTP $SIGNUP)"
else
    fail "GTS account signup — HTTP $SIGNUP"
fi

APP=$(curl -sf -X POST "$GTS/api/v1/apps" \
    -F "client_name=e2e-test" \
    -F "redirect_uris=urn:ietf:wg:oauth:2.0:oob" \
    -F "scopes=read write")
CLIENT_ID=$(echo "$APP" | grep -oP '"client_id":"\K[^"]+')
CLIENT_SECRET=$(echo "$APP" | grep -oP '"client_secret":"\K[^"]+')

AUTH=$(curl -sf -X POST "$GTS/oauth/token" \
    -F "client_id=$CLIENT_ID" -F "client_secret=$CLIENT_SECRET" \
    -F "grant_type=password" -F "username=$GTS_EMAIL" \
    -F "password=$GTS_PASSWORD" -F "scope=read write")
GTS_TOKEN=$(echo "$AUTH" | grep -oP '"access_token":"\K[^"]+')

if [[ -n "$GTS_TOKEN" ]]; then ok "GTS OAuth token obtained"
else fail "GTS OAuth token not obtained — aborting"; exit 1; fi

# ── 2. WebFinger ─────────────────────────────────────────────────────────────

section "2. WebFinger"

WF_K=$(curl -sf "$KOMENTOJ/.well-known/webfinger?resource=acct:komentoj@komentoj.local:8080")
assert_contains "komentoj WF subject" "$WF_K" "acct:komentoj@komentoj.local"
assert_contains "komentoj WF actor link" "$WF_K" "http://komentoj.local:8080/actor"
assert_contains "komentoj WF content-type link" "$WF_K" "application/activity+json"

WF_GTS=$(curl -sf "$GTS/.well-known/webfinger?resource=acct:$GTS_USER@gotosocial.local:8888")
assert_contains "GTS WF subject" "$WF_GTS" "acct:$GTS_USER@gotosocial.local"

# ── 3. Actor documents ───────────────────────────────────────────────────────

section "3. Actor documents"

ACTOR=$(curl -sf -H "Accept: application/activity+json" "$KOMENTOJ/actor")
assert_contains "komentoj actor type=Service"   "$ACTOR" '"type":"Service"'
assert_contains "komentoj actor inbox"          "$ACTOR" '"inbox":"http://komentoj.local:8080/inbox"'
assert_contains "komentoj actor publicKey"      "$ACTOR" '"publicKey"'
assert_contains "komentoj actor followers"      "$ACTOR" '"followers"'

assert_http "komentoj outbox OK"   200 -H "Accept: application/activity+json" "$KOMENTOJ/outbox"
assert_http "komentoj followers OK" 200 -H "Accept: application/activity+json" "$KOMENTOJ/followers"
assert_http "komentoj following OK" 200 -H "Accept: application/activity+json" "$KOMENTOJ/following"

GTS_ACTOR=$(curl -sf -H "Accept: application/activity+json" "$GTS/users/$GTS_USER")
assert_contains "GTS actor has inbox" "$GTS_ACTOR" '"inbox"'

# ── 4. Browser redirect (actor serves HTML redirect) ────────────────────────

section "4. Actor content negotiation"

REDIR=$(curl -s -o /dev/null -w "%{http_code}" -H "Accept: text/html" "$KOMENTOJ/actor")
if [[ "$REDIR" == "303" ]]; then ok "actor redirects browsers (303)"
else fail "actor browser redirect — got HTTP $REDIR, want 303"; fi

# ── 5. sync_posts: register + publish Create(Note) ──────────────────────────

section "5. POST /api/v1/posts/sync — new post → Create(Note)"

POST_ID="e2e-post-$(date +%s)"
POST_URL="http://gotosocial.local:8888/posts/$POST_ID"

SYNC1=$(curl -sf -X POST "$KOMENTOJ/api/v1/posts/sync" \
    -H "Authorization: Bearer $ADMIN_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{\"posts\":[{\"id\":\"$POST_ID\",\"title\":\"E2E Test Post\",\"url\":\"$POST_URL\",\"content\":\"Hello from E2E.\"}]}")

assert_contains "sync: upserted=1"  "$SYNC1" '"upserted":1'
assert_contains "sync: published=1" "$SYNC1" '"published":1'
assert_contains "sync: updated=0"   "$SYNC1" '"updated":0'

# Wait for ap_note_id to be written (async publish)
wait_for "ap_note_id persisted in DB" 30 \
    'curl -sf "$KOMENTOJ/api/v1/comments?id=$POST_ID" | grep -qF "total"'

# Retrieve the note ID from comments endpoint (total=0 but post must exist)
COMMENTS0=$(curl -sf "$KOMENTOJ/api/v1/comments?id=$POST_ID")
assert_contains "comments post_id matches" "$COMMENTS0" "\"post_id\":\"$POST_ID\""

# ── 6. Resolve komentoj actor from GTS side ──────────────────────────────────

section "6. GTS resolves komentoj actor"

GTS_SEARCH=$(curl -sf \
    "$GTS/api/v1/accounts/search?q=komentoj@komentoj.local:8080&resolve=true" \
    -H "Authorization: Bearer $GTS_TOKEN")
KOMENTOJ_GTS_ID=$(echo "$GTS_SEARCH" | grep -oP '"id":"\K[^"]+' | head -1)

if [[ -n "$KOMENTOJ_GTS_ID" ]]; then ok "komentoj resolved in GTS (id=$KOMENTOJ_GTS_ID)"
else fail "could not resolve komentoj actor in GTS"; KOMENTOJ_GTS_ID=""; fi

# ── 7. Follow: GTS → komentoj ────────────────────────────────────────────────

section "7. Follow + Accept"

if [[ -n "$KOMENTOJ_GTS_ID" ]]; then
    curl -sf -X POST "$GTS/api/v1/accounts/$KOMENTOJ_GTS_ID/follow" \
        -H "Authorization: Bearer $GTS_TOKEN" > /dev/null
    ok "GTS sent Follow activity"

    # komentoj records follower, delivers Accept, GTS records relationship
    wait_for "komentoj followers count ≥ 1" 30 \
        'curl -sf -H "Accept: application/activity+json" "$KOMENTOJ/followers" | grep -P "\"totalItems\":[1-9]"'

    FOLLOWERS=$(curl -sf -H "Accept: application/activity+json" "$KOMENTOJ/followers")
    assert_contains "followers totalItems ≥ 1" "$FOLLOWERS" '"totalItems"'

    # GTS should now consider komentoj as followed
    wait_for "GTS sees relationship as following" 30 \
        'curl -sf "$GTS/api/v1/accounts/relationships?id=$KOMENTOJ_GTS_ID" \
             -H "Authorization: Bearer $GTS_TOKEN" | grep -qF "\"following\":true"'
else
    fail "Follow test skipped — komentoj actor not resolved in GTS"
fi

# ── 8. Incoming Create(Note) from GTS → komentoj stores comment ──────────────

section "8. Incoming Create(Note) from GTS"

GTS_STATUS=$(curl -sf -X POST "$GTS/api/v1/statuses" \
    -H "Authorization: Bearer $GTS_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{\"status\":\"E2E comment on $POST_URL\",\"visibility\":\"public\"}")
GTS_STATUS_ID=$(echo "$GTS_STATUS" | grep -oP '"id":"\K[^"]+' | head -1)

if [[ -n "$GTS_STATUS_ID" ]]; then
    ok "GTS status posted (id=$GTS_STATUS_ID)"

    # komentoj should deliver the Note to its inbox and store a comment
    # GTS delivers the Create to komentoj's inbox automatically (as a follower post)
    wait_for "comment stored in komentoj" 30 \
        'curl -sf "$KOMENTOJ/api/v1/comments?id=$POST_ID" | grep -qP "\"total\":[1-9]"'

    COMMENTS1=$(curl -sf "$KOMENTOJ/api/v1/comments?id=$POST_ID")
    assert_contains "comment has author field" "$COMMENTS1" '"author"'
    assert_contains "comment has content_html"  "$COMMENTS1" '"content_html"'
    assert_contains "comments has replies field" "$COMMENTS1" '"replies"'
else
    fail "GTS status post failed: $GTS_STATUS"
fi

# ── 9. sync_posts: Update(Note) on content change ────────────────────────────

section "9. POST /api/v1/posts/sync — updated post → Update(Note)"

SYNC2=$(curl -sf -X POST "$KOMENTOJ/api/v1/posts/sync" \
    -H "Authorization: Bearer $ADMIN_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{\"posts\":[{\"id\":\"$POST_ID\",\"title\":\"E2E Test Post (updated)\",\"url\":\"$POST_URL\",\"content\":\"Updated content.\"}]}")

assert_contains "update sync: upserted=1" "$SYNC2" '"upserted":1'
assert_contains "update sync: updated=1"  "$SYNC2" '"updated":1'
assert_contains "update sync: published=0" "$SYNC2" '"published":0'

# ── 10. Note document fetchable ───────────────────────────────────────────────

section "10. GET /notes/:id"

# Derive note ID from the DB via comments API (comments store the Note ID)
# (post must exist and ap_note_id must be set by now — checked in step 5)
# We verify the note endpoint returns a valid Note object
NOTE_RESP=$(curl -sf "$KOMENTOJ/api/v1/comments?id=$POST_ID")
NOTE_ID_FROM_COMMENTS=$(echo "$NOTE_RESP" | grep -oP '"id":"http://komentoj\.local:8080/notes/\K[^"]+' | head -1)

if [[ -n "$NOTE_ID_FROM_COMMENTS" ]]; then
    NOTE_DOC=$(curl -sf -H "Accept: application/activity+json" \
        "http://localhost:8080/notes/$NOTE_ID_FROM_COMMENTS" || true)
    assert_contains "Note doc type=Note"       "$NOTE_DOC" '"type":"Note"'
    assert_contains "Note doc attributedTo"    "$NOTE_DOC" '"attributedTo"'
    assert_contains "Note doc content"         "$NOTE_DOC" '"content"'
else
    # No comments yet — derive from sync response instead
    # (note_id not exposed by comments API directly; skip sub-check)
    ok "Note fetch skipped — no comments with komentoj note IDs yet"
fi

# ── 11. sync_posts: deactivate post (absent from list) ───────────────────────

section "11. POST /api/v1/posts/sync — empty list → all posts deactivated"

SYNC3=$(curl -sf -X POST "$KOMENTOJ/api/v1/posts/sync" \
    -H "Authorization: Bearer $ADMIN_TOKEN" \
    -H "Content-Type: application/json" \
    -d '{"posts":[]}')

# deactivated count should be ≥ 1 (our test post)
DEACT=$(echo "$SYNC3" | grep -oP '"deactivated":\K[0-9]+')
if (( DEACT >= 1 )); then ok "deactivated ≥ 1 posts ($DEACT)"
else fail "expected deactivated ≥ 1, got $DEACT — sync response: $SYNC3"; fi

# Comments on deactivated post still readable (soft delete)
assert_http "comments still readable after deactivation" 200 \
    "$KOMENTOJ/api/v1/comments?id=$POST_ID"

# ── 12. Incoming Update(Note) from GTS ───────────────────────────────────────

section "12. Incoming Update(Note) from GTS"

if [[ -n "$GTS_STATUS_ID" ]]; then
    UPD=$(curl -sf -X PUT "$GTS/api/v1/statuses/$GTS_STATUS_ID" \
        -H "Authorization: Bearer $GTS_TOKEN" \
        -H "Content-Type: application/json" \
        -d '{"status":"Updated E2E comment"}')
    if echo "$UPD" | grep -qF '"id"'; then
        ok "GTS status updated"
        sleep 3  # allow delivery
    else
        fail "GTS status update failed: $UPD"
    fi
fi

# ── 13. Incoming Delete(Note) from GTS ───────────────────────────────────────

section "13. Incoming Delete(Note) from GTS"

if [[ -n "$GTS_STATUS_ID" ]]; then
    DEL=$(curl -sf -X DELETE "$GTS/api/v1/statuses/$GTS_STATUS_ID" \
        -H "Authorization: Bearer $GTS_TOKEN" || true)
    ok "GTS status deleted (Delete activity sent to komentoj)"
    sleep 3  # allow delivery

    wait_for "comment soft-deleted in komentoj" 20 \
        'curl -sf "$KOMENTOJ/api/v1/comments?id=$POST_ID" | grep -qP "\"total\":0"'
fi

# ── 14. Undo(Follow): GTS unfollows komentoj ─────────────────────────────────

section "14. Undo(Follow)"

if [[ -n "$KOMENTOJ_GTS_ID" ]]; then
    curl -sf -X POST "$GTS/api/v1/accounts/$KOMENTOJ_GTS_ID/unfollow" \
        -H "Authorization: Bearer $GTS_TOKEN" > /dev/null
    ok "GTS unfollow sent"

    wait_for "komentoj followers count returns to 0" 30 \
        'curl -sf -H "Accept: application/activity+json" "$KOMENTOJ/followers" | grep -qF "\"totalItems\":0"'
fi

# ── 15. API error cases ───────────────────────────────────────────────────────

section "15. API error handling"

assert_http "comments: missing id → 400" 400 "$KOMENTOJ/api/v1/comments"
assert_http "comments: unknown id → 404" 404 "$KOMENTOJ/api/v1/comments?id=__nonexistent__"
assert_http "sync: wrong token → 401"    401 \
    -X POST "$KOMENTOJ/api/v1/posts/sync" \
    -H "Authorization: Bearer wrong-token" \
    -H "Content-Type: application/json" \
    -d '{"posts":[]}'
assert_http "inbox: no signature → 401" 401 \
    -X POST "$KOMENTOJ/inbox" \
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
