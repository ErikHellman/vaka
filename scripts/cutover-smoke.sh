#!/usr/bin/env bash
# End-to-end check of the SQLite -> Postgres cutover (docs/self-hosting.md) against a running
# SQLite-backed Vaka. Starts a second server on the copied Postgres database at the end.
# Usage: VAKA=http://127.0.0.1:8080 EMAIL=... PASSWORD=... SQLITE_URL=sqlite://... PG_URL=postgres://... \
#        BIN=./target/debug/vaka-server scripts/cutover-smoke.sh   (plus the VAKA_CLICKHOUSE_* env of the server)
set -euo pipefail
: "${VAKA:?}" "${EMAIL:?}" "${PASSWORD:?}" "${SQLITE_URL:?}" "${PG_URL:?}"
BIN=${BIN:-./target/debug/vaka-server}
PG_LISTEN=${PG_LISTEN:-127.0.0.1:8082}
JAR=$(mktemp)
fail() { echo "FAIL: $*" >&2; exit 1; }
j() { python3 -c "import sys,json; d=json.load(sys.stdin); print(eval(sys.argv[1]))" "$1"; }
code() { curl -s -o /dev/null -w '%{http_code}' "$@"; }

curl -sf -c "$JAR" -o /dev/null -X POST "$VAKA/ui/login" -d "email=$EMAIL&password=$PASSWORD" || fail login
TOKEN=$(curl -sf -b "$JAR" -X POST "$VAKA/api/v1/tokens" -H 'content-type: application/json' -d '{"name":"cutover","scope":"write"}' | j 'd["token"]')
A=(-H "authorization: Bearer $TOKEN" -H 'content-type: application/json')
PROJ=$(curl -sf "${A[@]}" -X POST "$VAKA/api/v1/projects" -d '{"name":"cutover","platform":"javascript"}')
KEY=$(echo "$PROJ" | j 'd["ingest_key"]')
ING=(-H "x-vaka-key: $KEY" -H 'content-type: application/json')

echo "== 1. copy while live"
"$BIN" migrate-db --from "$SQLITE_URL" --to "$PG_URL" || fail "live copy"

echo "== 2. read-only mode"
RO=$(curl -sf "${A[@]}" -X PUT "$VAKA/api/v1/admin/read-only" -d '{"enabled":true}')
echo "$RO"
[ "$(echo "$RO" | j 'd["enforced"]')" = "True" ] || fail "read-only not enforced: $RO"
curl -sf "$VAKA/health" | grep -q '"read_only":true' || fail "health does not show read-only"
R=$(curl -s -w ' %{http_code}' "${A[@]}" -X POST "$VAKA/api/v1/projects" -d '{"name":"blocked","platform":"go"}')
echo "$R" | grep -q 'read-only' || fail "project creation not refused clearly: $R"
echo "$R" | grep -q ' 503$' || fail "expected 503: $R"
[ "$(code "${ING[@]}" -X POST "$VAKA/ingest/v1/batch" -d '{"items":[{"type":"event","name":"pageview","url":"https://ex.com/"}]}')" = 202 ] || fail "events must keep flowing"
H=$(curl -s -D - -o /dev/null "${ING[@]}" -X POST "$VAKA/ingest/v1/batch" -d '{"items":[{"type":"error","message":"boom"}]}')
echo "$H" | grep -q ' 503' || fail "error items must get a retryable 503: $H"
echo "$H" | grep -qi '^retry-after:' || fail "no Retry-After: $H"
PROJECTS=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects" | j 'len(d["projects"])')

echo "== 3. final copy"
OUT=$("$BIN" migrate-db --from "$SQLITE_URL" --to "$PG_URL" --replace) || fail "final copy"
echo "$OUT"
echo "$OUT" | grep -q 'source changed' && fail "source changed while read-only"

echo "== back to writable (the old server stays usable if the cutover is abandoned)"
curl -sf "${A[@]}" -X PUT "$VAKA/api/v1/admin/read-only" -d '{"enabled":false}' >/dev/null || fail "disable read-only"
curl -sf -o /dev/null "${A[@]}" -X POST "$VAKA/api/v1/projects" -d '{"name":"after","platform":"go"}' || fail "writes after read-only"

echo "== start on Postgres"
VAKA_DB_URL="$PG_URL" VAKA_LISTEN="$PG_LISTEN" VAKA_DATA_DIR="$(mktemp -d)" "$BIN" > /tmp/vaka-cutover.log 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null || true' EXIT
for _ in $(seq 1 60); do curl -sf "http://$PG_LISTEN/health" >/dev/null && break; sleep 1; done
B=(-H "authorization: Bearer $TOKEN")
N=$(curl -sf "${B[@]}" "http://$PG_LISTEN/api/v1/projects" | j 'len(d["projects"])') || fail "token does not work on Postgres"
[ "$N" = "$PROJECTS" ] || fail "projects on Postgres: $N, expected $PROJECTS"
JAR2=$(mktemp)
curl -sf -c "$JAR2" -o /dev/null -X POST "http://$PG_LISTEN/ui/login" -d "email=$EMAIL&password=$PASSWORD" || fail "login on Postgres"
curl -sf -b "$JAR2" "http://$PG_LISTEN/api/v1/me" | grep -q "$EMAIL" || fail "session after login on Postgres"
[ "$(code "${ING[@]}" -X POST "http://$PG_LISTEN/ingest/v1/batch" -d '{"items":[{"type":"error","message":"retried after cutover"}]}')" = 202 ] || fail "ingest key on Postgres"
echo "cutover smoke OK ($N projects)"
