#!/usr/bin/env bash
# End-to-end smoke test against a running Vaka instance.
# Usage: VAKA=http://localhost:8080 EMAIL=... PASSWORD=... scripts/smoke.sh
set -euo pipefail
VAKA=${VAKA:-http://localhost:8080}
EMAIL=${EMAIL:-erik@example.com}
PASSWORD=${PASSWORD:-password123}
JAR=$(mktemp)
fail() { echo "FAIL: $*" >&2; exit 1; }
j() { python3 -c "import sys,json; d=json.load(sys.stdin); print(eval(sys.argv[1]))" "$1"; }

curl -sf -c "$JAR" -o /dev/null -X POST "$VAKA/ui/login" -d "email=$EMAIL&password=$PASSWORD" || fail login
TOKEN=$(curl -sf -b "$JAR" -X POST "$VAKA/api/v1/tokens" -H 'content-type: application/json' -d '{"name":"smoke","scope":"write"}' | j 'd["token"]')
A=(-H "authorization: Bearer $TOKEN" -H 'content-type: application/json')
SLUG="smoke-$RANDOM"
PROJ=$(curl -sf "${A[@]}" -X POST "$VAKA/api/v1/projects" -d "{\"name\":\"$SLUG\",\"platform\":\"javascript\"}")
KEY=$(echo "$PROJ" | j 'd["ingest_key"]')
SLUG=$(echo "$PROJ" | j 'd["slug"]')
echo "project $SLUG key $KEY"

NOW=$(date -u +%Y-%m-%dT%H:%M:%SZ)
send() { curl -sf -X POST "$VAKA/ingest/v1/batch" -H "x-vaka-key: $KEY" -H 'user-agent: Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36' -H 'content-type: application/json' -d "$1"; local r=$?; echo; return $r; }
send '{"sdk":{"name":"smoke","version":"1"},"items":[
 {"type":"error","release":"1.0.0","user":{"id":"u1"},"exception":{"type":"TypeError","value":"x is undefined for user bob@example.com","stacktrace":[{"function":"render","filename":"app.js","lineno":10,"in_app":true},{"function":"main","filename":"app.js","lineno":2}]},"breadcrumbs":[{"category":"ui.click","message":"button#buy"}]},
 {"type":"error","release":"1.0.0","exception":{"type":"TypeError","value":"x is undefined","stacktrace":[{"function":"render","filename":"app.js","lineno":11,"in_app":true},{"function":"main","filename":"app.js","lineno":2}]}},
 {"type":"error","release":"1.1.0","message":"payment failed after 3 retries"},
 {"type":"session","session_id":"s1","status":"ok","release":"1.0.0","anon_id":"a1"},
 {"type":"session","session_id":"s2","status":"crashed","release":"1.0.0","anon_id":"a2"},
 {"type":"event","name":"pageview","url":"https://shop.example.com/pricing?utm_source=hn","referrer":"https://news.ycombinator.com/item?id=1"},
 {"type":"event","name":"pageview","url":"https://shop.example.com/"},
 {"type":"event","name":"signup","url":"https://shop.example.com/","props":{"plan":"pro"}}
]}'
send '{"type":"bogus"}' && fail 'batch without items accepted' || true
sleep 2.5

ISSUES=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/issues?since=1h")
N=$(echo "$ISSUES" | j 'len(d["issues"])')
[ "$N" = "2" ] || fail "expected 2 issues, got $N: $ISSUES"
SHORT=$(echo "$ISSUES" | j '[i for i in d["issues"] if i["title"].startswith("TypeError")][0]["short_id"]')
echo "issue $SHORT"
ISSUE=$(curl -sf "${A[@]}" "$VAKA/api/v1/issues/$SHORT")
curl -sf "${A[@]}" "$VAKA/api/v1/issues/$SHORT/events" | grep -q '\[email\]' || fail "scrubbing"
[ "$(echo "$ISSUE" | j 'd["times_seen"]')" = "2" ] || fail "times_seen"

SUM=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/analytics/summary?since=1h")
[ "$(echo "$SUM" | j 'd["totals"]["pageviews"]')" = "2" ] || fail "pageviews: $SUM"
REF=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/analytics/breakdown?dimension=referrer&since=1h")
echo "$REF" | grep -q news.ycombinator.com || fail "referrer breakdown: $REF"
PROPS=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/analytics/breakdown?dimension=prop:plan&type=custom&since=1h")
echo "$PROPS" | grep -q '"pro"' || fail "prop breakdown: $PROPS"
# Product analytics: two people, one converts (pageview / -> /pricing -> signup), one drops off.
T0=$(date +%s)
send "{\"items\":[
 {\"type\":\"event\",\"name\":\"pageview\",\"url\":\"https://ex.com/\",\"anon_id\":\"p1\",\"user_id\":\"u-funnel\",\"timestamp\":$((T0-3))},
 {\"type\":\"event\",\"name\":\"pageview\",\"url\":\"https://ex.com/pricing\",\"anon_id\":\"p1\",\"user_id\":\"u-funnel\",\"timestamp\":$((T0-2))},
 {\"type\":\"event\",\"name\":\"signup\",\"url\":\"https://ex.com/pricing\",\"props\":{\"plan\":\"pro\"},\"anon_id\":\"p1\",\"user_id\":\"u-funnel\",\"timestamp\":$((T0-1))},
 {\"type\":\"event\",\"name\":\"pageview\",\"url\":\"https://ex.com/\",\"anon_id\":\"p2\",\"timestamp\":$((T0-3))},
 {\"type\":\"event\",\"name\":\"pageview\",\"url\":\"https://ex.com/about\",\"anon_id\":\"p2\",\"timestamp\":$((T0-2))},
 {\"type\":\"error\",\"release\":\"1.0.0\",\"session_id\":\"s1\",\"fatal\":false,\"handled\":false,\"tags\":{\"mechanism\":\"anr\"},\"exception\":{\"type\":\"ANR\",\"value\":\"Application not responding for 5000 ms\",\"stacktrace\":[{\"function\":\"onClick\",\"in_app\":true}]}}]}" >/dev/null || fail "funnel events"
sleep 2
FUN=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/analytics/funnel?since=1h&filter=hostname:ex.com&step=pageview%7Cpath:/&step=pageview%7Cpath:/pricing&step=signup")
[ "$(echo "$FUN" | j '[s["count"] for s in d["steps"]][1:]')" = "[1, 1]" ] || fail "funnel: $FUN"
RET=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/analytics/retention?start=signup&period=day&periods=1&identity=user")
[ "$(echo "$RET" | j 'd["cohorts"][-1]["size"]')" = "1" ] || fail "retention: $RET"
PATHS=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/analytics/paths?since=1h&from=pageview%20/pricing&depth=1")
echo "$PATHS" | grep -q 'pageview /pricing → signup' || fail "paths: $PATHS"
curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/analytics/events?since=1h" | grep -q '"signup"' || fail "event catalog"
curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/users?since=1h" | grep -q 'u-funnel' || fail "identified users"
curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/users/u-funnel/timeline?since=1h" | j 'len(d["timeline"])' | grep -q 3 || fail "user timeline"
# Goals, revenue, real-time, adoption
send "{\"items\":[{\"type\":\"event\",\"name\":\"purchase\",\"url\":\"https://ex.com/checkout\",\"referrer\":\"https://news.ycombinator.com/\",\"props\":{\"revenue\":\"19.5\",\"currency\":\"usd\"},\"anon_id\":\"p1\"},
 {\"type\":\"event\",\"name\":\"purchase\",\"url\":\"https://ex.com/checkout\",\"props\":{\"revenue\":10,\"currency\":\"USD\"},\"anon_id\":\"p2\"},
 {\"type\":\"event\",\"name\":\"purchase\",\"url\":\"https://ex.com/checkout\",\"props\":{\"revenue\":\"99\",\"currency\":\"SEK\"},\"anon_id\":\"p2\"}]}" >/dev/null || fail "purchase events"
sleep 2
curl -sf "${A[@]}" -X POST "$VAKA/api/v1/projects/$SLUG/goals" -d '{"name":"Purchase","step":"purchase","value_prop":"revenue"}' | j 'd["name"]' | grep -q Purchase || fail "create goal"
curl -s "${A[@]}" -X POST "$VAKA/api/v1/projects/$SLUG/goals" -d '{"name":"Purchase","step":"purchase"}' | grep -q 'already exists' || fail "duplicate goal"
curl -sf "${A[@]}" -X POST "$VAKA/api/v1/projects/$SLUG/goals" -d '{"name":"Pricing","step":"pageview|path:/pricing"}' >/dev/null || fail "create goal 2"
GOALS=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/goals?since=1h")
[ "$(echo "$GOALS" | j '[(g["name"], g["conversions"]) for g in d["goals"]]')" = "[('Pricing', 2), ('Purchase', 3)]" ] || fail "goals: $GOALS"
[ "$(echo "$GOALS" | j '[r["revenue"] for g in d["goals"] if g["name"] == "Purchase" for r in g["revenue"] if r["currency"] == "USD"][0]')" = "29.5" ] || fail "goal revenue: $GOALS"
REV=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/analytics/revenue?since=1h&breakdown=referrer")
[ "$(echo "$REV" | j '[(r["currency"], float(r["revenue"]), r["conversions"]) for r in d["totals"]]')" = "[('SEK', 99.0, 1), ('USD', 29.5, 2)]" ] || fail "revenue: $REV"
echo "$REV" | j '[r["value"] for r in d["breakdown"]]' | grep -q news.ycombinator.com || fail "revenue breakdown: $REV"
curl -sf "${A[@]}" -X DELETE "$VAKA/api/v1/projects/$SLUG/goals/Pricing" >/dev/null || fail "delete goal"
RT=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/analytics/realtime")
[ "$(echo "$RT" | j 'd["visitors"] >= 2 and len(d["top_pages"]) > 0 and d["errors"] >= 1')" = "True" ] || fail "realtime: $RT"
ADOPT=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/releases/adoption?since=1d")
[ "$(echo "$ADOPT" | j 'd["series"][-1]["releases"]["1.0.0"]["share_sessions"]')" = "1.0" ] || fail "adoption: $ADOPT"
HEALTH=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/releases/health")
[ "$(echo "$HEALTH" | j 'd["releases"][0]["crash_free_sessions"]')" = "0.5" ] || fail "health: $HEALTH"
[ "$(echo "$HEALTH" | j '[r["anr_rate_sessions"] for r in d["releases"] if r["release"] == "1.0.0"][0]')" = "0.5" ] || fail "anr rate: $HEALTH"
curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/explain-spike?window=1h" | j 'd["totals"]' >/dev/null || fail spike
curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/releases/compare?base=1.0.0&target=1.1.0" | j 'len(d["issues_new_in_target"])' | grep -q 1 || fail compare

# Source maps (and, with ARTIFACT_DIR set, S3: local copies are deleted so the server must fetch them)
SMAP='{"version":3,"file":"app.js","sources":["src/checkout/pay.ts"],"sourcesContent":["export function pay() {\n  throw new Error(\"declined\");\n}"],"names":["pay"],"mappings":"AAAA;AACE"}'
SMF=$(mktemp); echo "$SMAP" > "$SMF"
curl -sf -H "authorization: Bearer $TOKEN" -X POST "$VAKA/api/v1/projects/$SLUG/artifacts" -F kind=sourcemap -F release=3.0.0 -F name=/assets/app.js -F "file=@$SMF" | j 'd["artifacts"][0]["name"]' | grep -q '/assets/app.js' || fail "sourcemap upload"
if [ -n "${ARTIFACT_DIR:-}" ]; then find "$ARTIFACT_DIR" -type f -delete; fi
send '{"items":[{"type":"error","release":"3.0.0","exception":{"type":"Error","value":"declined","stacktrace":[{"function":"a","abs_path":"https://ex.com/assets/app.js","lineno":2,"colno":3,"in_app":true}]}}]}' >/dev/null
sleep 2
SMI=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/issues?since=1h&release=3.0.0" | j 'd["issues"][0]["short_id"]')
curl -sf "${A[@]}" "$VAKA/api/v1/issues/$SMI" | j 'd["latest_event"]["exceptions"][0]["stacktrace"][0]["filename"]' | grep -q 'src/checkout/pay.ts' || fail "source map symbolication: $(curl -sf "${A[@]}" "$VAKA/api/v1/issues/$SMI" | j 'd["latest_event"]["exceptions"][0]["stacktrace"]')"

# Ownership rules + suspect commits
printf -v RULES 'src/** #web\\nsrc/checkout/** %s\\ntype:TimeoutError #ops\\n' "$EMAIL"
curl -sf "${A[@]}" -X PUT "$VAKA/api/v1/projects/$SLUG/ownership" -d "{\"rules\":\"$RULES\"}" | j 'd["rules"]' | grep -q 3 || fail "ownership put"
curl -s "${A[@]}" -X PUT "$VAKA/api/v1/projects/$SLUG/ownership" -d '{"rules":"nobody-owns-this"}' | grep -q 'no owners' || fail "ownership validation"
curl -sf "${A[@]}" -X PUT "$VAKA/api/v1/projects/$SLUG/releases/2.0.0/commits" -d '{"repository":"https://github.com/acme/shop","commits":[
 {"sha":"c0ffee1","message":"Docs","author_name":"Bo","files":["README.md"]},
 {"sha":"deadbee","message":"Refactor cart totals\n\nlonger body","author_name":"Ada","author_email":"ada@x.se","timestamp":1700000000,"files":["src/checkout/cart.ts","src/checkout/total.ts"]}]}' | j 'd["commits"]' | grep -q 2 || fail "set commits"
send '{"items":[{"type":"error","release":"2.0.0","exception":{"type":"RangeError","value":"bad total","stacktrace":[{"function":"total","abs_path":"webpack:///./src/checkout/total.ts","filename":"src/checkout/total.ts","lineno":4,"in_app":true},{"function":"render","abs_path":"webpack:///./src/ui/app.ts","filename":"src/ui/app.ts","lineno":9,"in_app":true}]}}]}' >/dev/null
sleep 2
OWNED=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/issues?since=1h&query=RangeError" | j '[i for i in d["issues"] if i["title"].startswith("RangeError")][0]["short_id"]')
OI=$(curl -sf "${A[@]}" "$VAKA/api/v1/issues/$OWNED")
[ "$(echo "$OI" | j 'd["owners"]')" = "['$EMAIL']" ] || fail "owners: $(echo "$OI" | j 'd["owners"]')"
[ "$(echo "$OI" | j 'd["assignee"]')" = "$EMAIL" ] || fail "auto-assign"
[ "$(echo "$OI" | j 'd["suspect_commits"][0]["sha"]')" = "deadbee" ] || fail "suspect commits: $(echo "$OI" | j 'd["suspect_commits"]')"
echo "$OI" | j 'd["suspect_commits"][0]["url"]' | grep -q 'github.com/acme/shop/commit/deadbee' || fail "commit url"
[ "$(echo "$OI" | j 'len(d["suspect_commits"])')" = "1" ] || fail "unrelated commit suspected"

# Source links, fix context, issue links, fix-by-commit
curl -sf "${A[@]}" -X PUT "$VAKA/api/v1/projects/$SLUG/code-mappings" -d '{"mappings":[{"stack_root":"src/","source_root":"web/src/","repository":"https://github.com/acme/shop","default_branch":"main"}]}' | j 'len(d["mappings"])' | grep -q 1 || fail "code mappings"
OI=$(curl -sf "${A[@]}" "$VAKA/api/v1/issues/$OWNED")
[ "$(echo "$OI" | j 'd["latest_event"]["exceptions"][0]["stacktrace"][0]["source"]["url"]')" = "https://github.com/acme/shop/blob/c0ffee1/web/src/checkout/total.ts#L4" ] || fail "source link: $(echo "$OI" | j 'd["latest_event"]["exceptions"][0]["stacktrace"][0]')"
FIX=$(curl -sf "${A[@]}" "$VAKA/api/v1/issues/$OWNED/fix-context")
[ "$(echo "$FIX" | j 'd["suggested"]["commit_trailer"]')" = "Fixes $OWNED" ] || fail "fix context: $FIX"
[ "$(echo "$FIX" | j 'd["exceptions"][0]["in_app_frames"][0]["source"]["path"]')" = "web/src/checkout/total.ts" ] || fail "fix context frames: $FIX"
[ "$(echo "$FIX" | j 'd["suspect_commits"][0]["sha"]')" = "deadbee" ] || fail "fix context suspects"
curl -sf "${A[@]}" -X POST "$VAKA/api/v1/issues/$OWNED/links" -d '{"url":"https://github.com/acme/shop/pull/7","title":"Fix total"}' | j 'd["links"][0]["kind"]' | grep -q pull_request || fail "issue link"
curl -sf "${A[@]}" -X POST "$VAKA/api/v1/projects/$SLUG/ownership/apply" | j 'd["checked"]' >/dev/null || fail "ownership apply"
FBC=$(curl -sf "${A[@]}" -X PUT "$VAKA/api/v1/projects/$SLUG/releases/2.0.1/commits" -d "{\"repository\":\"https://github.com/acme/shop\",\"commits\":[{\"sha\":\"f1x0000\",\"message\":\"Guard totals\\n\\nFixes $OWNED\",\"files\":[\"src/checkout/total.ts\"]}]}")
[ "$(echo "$FBC" | j 'd["resolved_issues"]')" = "['$OWNED']" ] || fail "fix-by-commit: $FBC"
OI=$(curl -sf "${A[@]}" "$VAKA/api/v1/issues/$OWNED")
[ "$(echo "$OI" | j '(d["status"], d["resolved_in_release"], len(d["links"]))')" = "('resolved', '2.0.1', 2)" ] || fail "fix-by-commit issue: $(echo "$OI" | j '(d["status"], d["resolved_in_release"], d["links"])')"
RANGE_ERR='{"type":"RangeError","value":"bad total","stacktrace":[{"function":"total","abs_path":"webpack:///./src/checkout/total.ts","filename":"src/checkout/total.ts","lineno":4,"in_app":true},{"function":"render","abs_path":"webpack:///./src/ui/app.ts","filename":"src/ui/app.ts","lineno":9,"in_app":true}]}'
send "{\"items\":[{\"type\":\"error\",\"release\":\"2.0.0\",\"exception\":$RANGE_ERR}]}" >/dev/null
sleep 2
[ "$(curl -sf "${A[@]}" "$VAKA/api/v1/issues/$OWNED" | j 'd["status"]')" = "resolved" ] || fail "event from an older release regressed the issue"
send "{\"items\":[{\"type\":\"error\",\"release\":\"2.0.2\",\"exception\":$RANGE_ERR}]}" >/dev/null
sleep 2
[ "$(curl -sf "${A[@]}" "$VAKA/api/v1/issues/$OWNED" | j 'd["status"]')" = "open" ] || fail "event from a newer release did not regress the issue"

# Slack alerts and digests, delivered to a local receiver
HOOKS=$(mktemp); HOOK_PORT=${HOOK_PORT:-18099}
python3 -c '
import http.server, sys
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers["content-length"] or 0))
        event = (self.headers.get("x-vaka-event") or "-").encode()
        open(sys.argv[1], "ab").write(self.path.encode() + b" " + event + b" " + body + b"\n")
        if self.path.startswith("/fail"):
            self.send_response(500); self.end_headers(); self.wfile.write(b"boom"); return
        self.send_response(200); self.end_headers()
        if self.path.startswith("/gh/") and self.path.endswith("/issues"):
            self.wfile.write(b"{\"html_url\": \"https://github.com/acme/shop/issues/1\", \"number\": 1}")
        else:
            self.wfile.write(b"ok")
    def log_message(self, *a): pass
http.server.HTTPServer(("127.0.0.1", int(sys.argv[2])), H).serve_forever()
' "$HOOKS" "$HOOK_PORT" & HOOK_PID=$!
trap 'kill $HOOK_PID 2>/dev/null || true' EXIT
sleep 0.5
curl -sf "${A[@]}" -X POST "$VAKA/api/v1/alert-rules" -d "{\"project\":\"$SLUG\",\"trigger\":\"new_issue\",\"channel\":\"slack\",\"target\":\"http://127.0.0.1:$HOOK_PORT/slack\"}" >/dev/null || fail "slack rule"
send '{"items":[{"type":"error","release":"2.0.0","exception":{"type":"SlackTestError","value":"for slack","stacktrace":[{"function":"total","filename":"src/checkout/total.ts","lineno":7,"in_app":true}]}}]}' >/dev/null
DIGEST=$(curl -sf "${A[@]}" -X POST "$VAKA/api/v1/alert-rules" -d "{\"project\":\"$SLUG\",\"trigger\":\"digest\",\"channel\":\"webhook\",\"target\":\"http://127.0.0.1:$HOOK_PORT/digest\"}" | j 'd["id"]')
curl -sf "${A[@]}" -X POST "$VAKA/api/v1/alert-rules/$DIGEST/test" >/dev/null || fail "digest test"
grep '^/digest ' "$HOOKS" | grep -q '"top_issues"' || fail "digest payload: $(cat "$HOOKS")"
grep '^/digest ' "$HOOKS" | grep -q "\"slug\":\"$SLUG\"" || fail "digest project"
for i in $(seq 1 10); do grep -q '^/slack ' "$HOOKS" && break; sleep 0.5; done
grep '^/slack ' "$HOOKS" | grep -q 'New issue' || fail "slack alert: $(cat "$HOOKS")"
grep '^/slack ' "$HOOKS" | grep -q "owners $EMAIL" || fail "slack owners: $(cat "$HOOKS")"

# Webhooks and agent triggers
rule() { curl -sf "${A[@]}" -X POST "$VAKA/api/v1/alert-rules" -d "$1" | j 'd["id"]'; }
rule "{\"project\":\"$SLUG\",\"trigger\":\"new_issue\",\"channel\":\"webhook\",\"target\":\"http://127.0.0.1:$HOOK_PORT/hook\",\"secret\":\"s3cret\",\"filters\":{\"min_level\":\"error\",\"environment\":\"production\"}}" >/dev/null || fail "webhook rule"
rule "{\"project\":\"$SLUG\",\"trigger\":\"new_issue\",\"channel\":\"webhook\",\"target\":\"http://127.0.0.1:$HOOK_PORT/filtered\",\"filters\":{\"environment\":\"staging\"}}" >/dev/null || fail "filtered rule"
rule "{\"project\":\"$SLUG\",\"trigger\":\"new_issue\",\"channel\":\"github_dispatch\",\"target\":\"acme/shop\",\"secret\":\"ghp_test\"}" >/dev/null || fail "dispatch rule"
rule "{\"project\":\"$SLUG\",\"trigger\":\"new_issue\",\"channel\":\"github_issue\",\"target\":\"https://github.com/acme/shop\",\"secret\":\"ghp_test\",\"filters\":{\"mention\":\"@agent please fix\"}}" >/dev/null || fail "github issue rule"
FAILRULE=$(rule "{\"project\":\"$SLUG\",\"trigger\":\"issue_commented\",\"channel\":\"webhook\",\"target\":\"http://127.0.0.1:$HOOK_PORT/fail\"}") || fail "failing rule"
rule "{\"project\":\"$SLUG\",\"trigger\":\"release_created\",\"channel\":\"webhook\",\"target\":\"http://127.0.0.1:$HOOK_PORT/release\"}" >/dev/null || fail "release rule"
curl -s "${A[@]}" -X POST "$VAKA/api/v1/alert-rules" -d '{"trigger":"new_issue","channel":"webhook","target":"http://x","filters":{"bogus":1}}' | grep -q 'unknown filter' || fail "filter validation"
send '{"items":[{"type":"error","release":"2.0.0","environment":"production","exception":{"type":"HookError","value":"for hooks","stacktrace":[{"function":"total","abs_path":"webpack:///./src/checkout/total.ts","filename":"src/checkout/total.ts","lineno":9,"in_app":true}]}}]}' >/dev/null
for i in $(seq 1 20); do grep -q '^/gh/repos/acme/shop/issues ' "$HOOKS" && grep -q '^/hook ' "$HOOKS" && break; sleep 0.5; done
grep '^/hook new_issue ' "$HOOKS" | grep 'HookError' | grep -q '"commit_trailer":"Fixes ' || fail "webhook fix context: $(grep '^/hook' "$HOOKS" | head -c 600)"
grep -q '^/filtered ' "$HOOKS" && fail "environment filter ignored"
grep '^/gh/repos/acme/shop/dispatches ' "$HOOKS" | grep -q '"event_type":"vaka.new_issue"' || fail "repository_dispatch: $(grep '^/gh' "$HOOKS" | head -c 600)"
grep '^/gh/repos/acme/shop/issues ' "$HOOKS" | grep 'HookError' | grep -q '@agent please fix' || fail "github issue body"
HOOKI=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/issues?since=1h&query=HookError" | j 'd["issues"][0]["short_id"]')
for i in $(seq 1 10); do curl -sf "${A[@]}" "$VAKA/api/v1/issues/$HOOKI/links" | grep -q 'issues/1' && break; sleep 0.5; done
curl -sf "${A[@]}" "$VAKA/api/v1/issues/$HOOKI/links" | grep -q 'https://github.com/acme/shop/issues/1' || fail "github issue link"
curl -sf "${A[@]}" -X POST "$VAKA/api/v1/issues/$HOOKI/comments" -d '{"body":"looking"}' >/dev/null || fail comment
for i in $(seq 1 10); do curl -sf "${A[@]}" "$VAKA/api/v1/alert-rules/$FAILRULE/deliveries" | grep -q retrying && break; sleep 0.5; done
DEL=$(curl -sf "${A[@]}" "$VAKA/api/v1/alert-rules/$FAILRULE/deliveries?payload=true")
[ "$(echo "$DEL" | j '(d["deliveries"][0]["status"], d["deliveries"][0]["attempts"], d["deliveries"][0]["payload"]["event"]["comment"])')" = "('retrying', 1, 'looking')" ] || fail "retrying delivery: $DEL"
DID=$(echo "$DEL" | j 'd["deliveries"][0]["id"]')
curl -sf "${A[@]}" -X PATCH "$VAKA/api/v1/alert-rules/$FAILRULE" -d "{\"target\":\"http://127.0.0.1:$HOOK_PORT/fixed\"}" >/dev/null || fail "update rule"
curl -sf "${A[@]}" -X POST "$VAKA/api/v1/alert-rules/$FAILRULE/deliveries/$DID/redeliver" | j 'd["ok"]' | grep -q True || fail "redeliver"
grep '^/fixed issue_commented ' "$HOOKS" | grep -q looking || fail "redelivered payload"
curl -sf "${A[@]}" -X PUT "$VAKA/api/v1/projects/$SLUG/releases/9.9.9/commits" -d '{"commits":[{"sha":"aaa1111","message":"x"}]}' >/dev/null
for i in $(seq 1 10); do grep -q '^/release ' "$HOOKS" && break; sleep 0.5; done
grep '^/release release_created ' "$HOOKS" | grep -q '"version":"9.9.9"' || fail "release_created: $(grep '^/release' "$HOOKS")"

# MCP
M=(-H "authorization: Bearer $TOKEN" -H 'content-type: application/json' -H 'accept: application/json, text/event-stream')
curl -sf "${M[@]}" -X POST "$VAKA/mcp" -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"1"}}}' | grep -q '"vaka"' || fail mcp-init
TOOLS=$(curl -sf "${M[@]}" -X POST "$VAKA/mcp" -d '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' | j 'len(d["result"]["tools"])')
echo "mcp tools: $TOOLS"
curl -sf "${M[@]}" -X POST "$VAKA/mcp" -d '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"list_alert_rules","arguments":{}}}' | j 'd["result"]["structuredContent"]["channels"]' | grep -q github_dispatch || fail mcp-alert-rules
curl -sf "${M[@]}" -X POST "$VAKA/mcp" -d "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"get_fix_context\",\"arguments\":{\"issue\":\"$OWNED\"}}}" | j 'd["result"]["structuredContent"]["suggested"]["branch"]' | grep -q "^vaka/" || fail mcp-fix-context
curl -sf "${M[@]}" -X POST "$VAKA/mcp" -d "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"resolve_issue\",\"arguments\":{\"issue\":\"$SHORT\",\"resolved_in_release\":\"1.0.1\"}}}" | j 'd["result"]["structuredContent"]["status"]' | grep -q resolved || fail mcp-resolve
# Regression
send '{"items":[{"type":"error","release":"1.0.2","exception":{"type":"TypeError","value":"x is undefined","stacktrace":[{"function":"render","filename":"app.js","lineno":12,"in_app":true},{"function":"main","filename":"app.js","lineno":2}]}}]}'
sleep 2
[ "$(curl -sf "${A[@]}" "$VAKA/api/v1/issues/$SHORT" | j 'd["status"]')" = "open" ] || fail regression
# GDPR
curl -sf "${A[@]}" -X DELETE "$VAKA/api/v1/projects/$SLUG/users/u1" | j 'd["errors"]' | grep -q 1 || fail gdpr
echo "SMOKE OK"
