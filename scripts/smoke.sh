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
# Cohorts, stickiness, lifecycle
send "{\"items\":[
 {\"type\":\"event\",\"name\":\"cohort_buy\",\"anon_id\":\"c1\",\"user_id\":\"c-1\",\"timestamp\":$((T0-30))},
 {\"type\":\"event\",\"name\":\"cohort_buy\",\"anon_id\":\"c1\",\"user_id\":\"c-1\",\"timestamp\":$((T0-20))},
 {\"type\":\"event\",\"name\":\"cohort_buy\",\"anon_id\":\"c1\",\"user_id\":\"c-1\",\"timestamp\":$((T0-10))},
 {\"type\":\"event\",\"name\":\"cohort_buy\",\"anon_id\":\"c2\",\"user_id\":\"c-2\",\"timestamp\":$((T0-10))},
 {\"type\":\"event\",\"name\":\"cohort_login\",\"anon_id\":\"c1\",\"user_id\":\"c-1\",\"timestamp\":$((T0-5))},
 {\"type\":\"event\",\"name\":\"cohort_login\",\"anon_id\":\"c3\",\"user_id\":\"c-3\",\"timestamp\":$((T0-10))}]}" >/dev/null || fail "cohort events"
sleep 2
CO=$(curl -sf "${A[@]}" -X POST "$VAKA/api/v1/projects/$SLUG/cohorts" -d '{"name":"buyers","identity":"user","criteria":[{"event":"cohort_buy","min_count":2}]}')
[ "$(echo "$CO" | j 'd["size"]')" = "1" ] || fail "cohort size: $CO"
[ "$(curl -sf "${A[@]}" -X POST "$VAKA/api/v1/projects/$SLUG/cohorts" -d '{"name":"quiet","identity":"user","criteria":[{"event":"cohort_login"},{"step":"cohort_buy","negate":true}]}' | j 'd["size"]')" = "1" ] || fail "negated cohort"
curl -s "${A[@]}" -X POST "$VAKA/api/v1/projects/$SLUG/cohorts" -d '{"name":"empty"}' | grep -q 'at least one criterion' || fail "cohort validation"
[ "$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/cohorts/buyers" | j 'd["sample_members"][0]["id"]')" = "c-1" ] || fail "cohort members"
[ "$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/analytics/summary?since=1h&filter=cohort:buyers" | j 'd["totals"]["events"]')" = "4" ] || fail "cohort filter"
[ "$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/analytics/summary?since=1h&filter=cohort:!buyers&filter=event:cohort_buy" | j 'd["totals"]["events"]')" = "1" ] || fail "negated cohort filter"
CF=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/analytics/funnel?since=1h&identity=user&step=cohort_buy&step=cohort_login&breakdown=cohort:buyers")
[ "$(echo "$CF" | j 'sorted((r["value"], [s["count"] for s in r["steps"]]) for r in d["breakdown"]["rows"])')" = "[('in buyers', [1, 1]), ('not in buyers', [1, 0])]" ] || fail "cohort funnel breakdown: $CF"
[ "$(echo "$CF" | j '(d["conversion_time"]["converted"], d["conversion_time"]["distribution"]["under_1m"])')" = "(1, 1)" ] || fail "conversion time: $CF"
[ "$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/analytics/stickiness?since=1h&identity=user&event=cohort_buy" | j 'd["active_in_window"]')" = "2" ] || fail "stickiness"
LC=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/analytics/lifecycle?period=day&identity=user&event=cohort_buy")
[ "$(echo "$LC" | j '(d["periods"][-1]["new"], d["periods"][-1]["active"])')" = "(2, 2)" ] || fail "lifecycle: $LC"
# Feature flags and experiments (own project, so its sessions and events don't skew the other checks)
FPROJ=$(curl -sf "${A[@]}" -X POST "$VAKA/api/v1/projects" -d "{\"name\":\"$SLUG-flags\",\"platform\":\"javascript\"}")
FKEY=$(echo "$FPROJ" | j 'd["ingest_key"]')
FSLUG=$(echo "$FPROJ" | j 'd["slug"]')
FL="$VAKA/api/v1/projects/$FSLUG/flags"
curl -sf "${A[@]}" -X POST "$FL" -d '{"key":"smoke_bool"}' >/dev/null || fail "create bool flag"
curl -sf "${A[@]}" -X POST "$FL" -d '{"key":"pricing","variants":[{"key":"a"},{"key":"b"}],"rules":[{"user_ids":["vip"],"variant":"b"}]}' >/dev/null || fail "create variant flag"
curl -s "${A[@]}" -X POST "$FL" -d '{"key":"bad","variants":[{"key":"a"}]}' | grep -q '2 to 10' || fail "flag validation"
EV=$(curl -sf -X POST "$VAKA/ingest/v1/flags" -H "x-vaka-key: $FKEY" -d '{"distinct_id":"d1","user_id":"vip"}')
[ "$(echo "$EV" | j '(d["flags"]["pricing"], d["flags"]["smoke_bool"], d["details"]["pricing"]["reason"])')" = "('b', True, 'rule')" ] || fail "flag evaluation: $EV"
curl -sf "${A[@]}" -X PATCH "$FL/smoke_bool" -d '{"enabled":false}' >/dev/null || fail "update flag"
[ "$(curl -sf -X POST "$VAKA/ingest/v1/flags" -H "x-vaka-key: $FKEY" -d '{"distinct_id":"d1"}' | j 'd["flags"]["smoke_bool"]')" = "False" ] || fail "flag kill switch"
FLAGITEMS=$(python3 - <<'PY'
import json, time
t = int(time.time())
items = []
for v in ("a", "b"):
    for i in range(10):
        items.append({"type": "session", "session_id": f"fs-{v}-{i}", "status": "crashed" if v == "b" and i < 5 else "exited", "release": "f1", "flags": {"pricing": v}, "timestamp": t - 5})
for i in range(5):
    items.append({"type": "error", "release": "f1", "session_id": f"fs-b-{i}", "flags": {"pricing": "b"}, "exception": {"type": "FlagCrash", "value": "variant b", "stacktrace": [{"function": "price", "filename": "p.ts", "lineno": 1, "in_app": True}]}})
for v, buyers in (("a", 4), ("b", 20)):
    for i in range(40):
        items.append({"type": "event", "name": "exp_view", "user_id": f"e-{v}-{i}", "flags": {"pricing": v}, "timestamp": t - 20})
        if i < buyers:
            items.append({"type": "event", "name": "exp_buy", "user_id": f"e-{v}-{i}", "flags": {"pricing": v}, "timestamp": t - 10})
print(json.dumps({"items": items}))
PY
)
curl -sf -X POST "$VAKA/ingest/v1/batch" -H "x-vaka-key: $FKEY" -H 'content-type: application/json' -d "$FLAGITEMS" >/dev/null || fail "flag items"
sleep 2
FH=$(curl -sf "${A[@]}" "$FL/pricing/health?since=1h")
[ "$(echo "$FH" | j '[(v["variant"], v["sessions"], v["crash_free_sessions"]) for v in d["variants"]]')" = "[('a', 10, 1.0), ('b', 10, 0.5)]" ] || fail "flag health: $FH"
[ "$(echo "$FH" | j '(d["over_represented_issues"][0]["variant"], d["over_represented_issues"][0]["title"].startswith("FlagCrash"))')" = "('b', True)" ] || fail "flag issues: $FH"
curl -sf "${A[@]}" -X POST "$VAKA/api/v1/projects/$FSLUG/experiments" -d '{"name":"price test","flag":"pricing","goal":"exp_buy","control":"a"}' >/dev/null || fail "create experiment"
ER=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$FSLUG/experiments/price%20test/results?since=1h")
[ "$(echo "$ER" | j '(d["winner"], [(v["variant"], v["exposed"], v["converted"]) for v in d["variants"]])')" = "('b', [('a', 40, 4), ('b', 40, 20)])" ] || fail "experiment results: $ER"
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

# SMS alerts (CI points VAKA_SMS_API_URL at the receiver and sets SMS_PATH)
curl -sf "${A[@]}" -X POST "$VAKA/api/v1/alert-rules" -d '{"trigger":"new_issue","channel":"sms","target":"0701234567"}' >/dev/null && fail "sms accepted a non-E.164 number"
SMS=$(curl -sf "${A[@]}" -X POST "$VAKA/api/v1/alert-rules" -d "{\"project\":\"$SLUG\",\"trigger\":\"new_issue\",\"channel\":\"sms\",\"target\":\"+46 70 123 45 67\"}") || fail "sms rule"
[ "$(echo "$SMS" | j 'd["max_per_hour"]')" = "10" ] || fail "sms default max_per_hour: $SMS"
if [ -n "${SMS_PATH:-}" ]; then
  curl -sf "${A[@]}" -X POST "$VAKA/api/v1/alert-rules/$(echo "$SMS" | j 'd["id"]')/test" >/dev/null || fail "sms test"
  grep "^$SMS_PATH " "$HOOKS" | grep -q 'Vaka+Test+alert' || fail "sms delivery: $(grep -v '^/digest' "$HOOKS" | tail -c 600)"
  grep "^$SMS_PATH " "$HOOKS" | grep -q '%2B46701234567' || fail "sms number"
fi

# Anomaly detection and digests (own project): three quiet weeks, then a burst today
APROJ=$(curl -sf "${A[@]}" -X POST "$VAKA/api/v1/projects" -d "{\"name\":\"$SLUG-anomaly\",\"platform\":\"javascript\"}")
AKEY=$(echo "$APROJ" | j 'd["ingest_key"]'); ASLUG=$(echo "$APROJ" | j 'd["slug"]')
AITEMS=$(python3 - <<'PY'
import json, time
t = int(time.time())
items = []
def err(ts, kind, release):
    return {"type": "error", "release": release, "timestamp": ts, "exception": {"type": kind, "value": "boom", "stacktrace": [{"function": kind.lower(), "filename": "a.ts", "lineno": 1, "in_app": True}]}}
for k in (1, 2, 3):
    base = t - k * 7 * 86400 - 20 * 3600
    items += [err(base + i, "AnomSteady", "r1") for i in range(2)]
    items += [{"type": "event", "name": "pageview", "url": "https://anom.example.com/", "timestamp": base + i} for i in range(60)]
    items += [{"type": "session", "session_id": f"as-{k}-{i}", "status": "exited", "release": "r1", "started": base + i, "timestamp": base + i + 5} for i in range(30)]
now = t - 600
items += [err(now + i, "AnomBurst", "r2") for i in range(40)]
items += [err(now, "AnomSteady", "r2") for i in range(2)]
items += [{"type": "event", "name": "pageview", "url": "https://anom.example.com/", "timestamp": now + i} for i in range(5)]
items += [{"type": "session", "session_id": f"as-0-{i}", "status": "crashed" if i < 10 else "exited", "release": "r2", "started": now + i, "timestamp": now + i + 5} for i in range(30)]
items += [{"type": "session", "session_id": f"as-0-r1-{i}", "status": "exited", "release": "r1", "started": now + i, "timestamp": now + i + 5} for i in range(30)]
print(json.dumps({"items": items}))
PY
)
curl -sf -X POST "$VAKA/ingest/v1/batch" -H "x-vaka-key: $AKEY" -H 'content-type: application/json' -d "$AITEMS" >/dev/null || fail "anomaly items"
sleep 2
AN=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$ASLUG/anomalies?window=24h")
[ "$(echo "$AN" | j 'sorted({a["metric"] for a in d["anomalies"]} & {"errors", "issue_errors", "crash_free_sessions", "release_crash_free", "pageviews"})')" = "['crash_free_sessions', 'errors', 'issue_errors', 'pageviews', 'release_crash_free']" ] || fail "anomalies: $AN"
[ "$(echo "$AN" | j '[a["subject"]["title"].split(":")[0] for a in d["anomalies"] if a["metric"] == "issue_errors"]')" = "['AnomBurst']" ] || fail "issue anomaly: $AN"
echo "$AN" | j 'd["baseline"]' | grep -q '3 weeks' || fail "anomaly baseline: $AN"
echo "$AN" | j '[a["next_tool"]["tool"] for a in d["anomalies"] if a["metric"] == "errors"][0]' | grep -q explain_spike || fail "anomaly next tool"
[ "$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/anomalies?window=9d" -o /dev/null -w '%{http_code}' || true)" != "200" ] || fail "anomaly window validation"
PV=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$ASLUG/digest/preview")
[ "$(echo "$PV" | j '(d["narrative_source"], d["title"], "Unusual:" in d["text"])')" = "('template', 'Vaka daily digest', True)" ] || fail "digest preview: $PV"
curl -s "${A[@]}" -X POST "$VAKA/api/v1/alert-rules" -d "{\"project\":\"$ASLUG\",\"trigger\":\"digest\",\"channel\":\"webhook\",\"target\":\"http://127.0.0.1:1/x\",\"schedule\":{\"timezone\":\"Mars/Base\"}}" | grep -q 'unknown time zone' || fail "schedule validation"
ADIG=$(curl -sf "${A[@]}" -X POST "$VAKA/api/v1/alert-rules" -d "{\"project\":\"$ASLUG\",\"trigger\":\"digest\",\"channel\":\"webhook\",\"target\":\"http://127.0.0.1:$HOOK_PORT/anomaly-digest\",\"schedule\":{\"frequency\":\"weekly\",\"weekday\":\"monday\",\"hour\":9,\"timezone\":\"Europe/Stockholm\"}}")
[ "$(echo "$ADIG" | j 'd["schedule"]["timezone"]')" = "Europe/Stockholm" ] || fail "digest schedule: $ADIG"
curl -sf "${A[@]}" -X POST "$VAKA/api/v1/alert-rules/$(echo "$ADIG" | j 'd["id"]')/test" >/dev/null || fail "anomaly digest test"
grep '^/anomaly-digest ' "$HOOKS" | grep -q '"issue_errors"' || fail "anomaly digest payload: $(grep '^/anomaly-digest ' "$HOOKS")"
grep '^/anomaly-digest ' "$HOOKS" | grep -q '"title":"Vaka weekly digest"' || fail "weekly digest title"
grep '^/anomaly-digest ' "$HOOKS" | grep -q '"narrative":"' || fail "digest narrative"

# Session timelines and crash precursors
T1=$(date +%s)
send "{\"items\":[
 {\"type\":\"session\",\"session_id\":\"tl-1\",\"status\":\"crashed\",\"release\":\"5.0.0\",\"started\":$((T1-5)),\"timestamp\":$((T1-1))},
 {\"type\":\"event\",\"name\":\"pageview\",\"url\":\"https://ex.com/cart\",\"session_id\":\"tl-1\",\"timestamp\":$((T1-4))},
 {\"type\":\"event\",\"name\":\"add_to_cart\",\"props\":{\"sku\":\"a1\"},\"session_id\":\"tl-1\",\"timestamp\":$((T1-3))},
 {\"type\":\"error\",\"release\":\"5.0.0\",\"session_id\":\"tl-1\",\"fatal\":true,\"timestamp\":$((T1-1)),\"breadcrumbs\":[{\"category\":\"fetch\",\"message\":\"POST /api/pay 502\",\"level\":\"warn\",\"timestamp\":$((T1-2))}],
  \"exception\":{\"type\":\"PayError\",\"value\":\"gateway\",\"stacktrace\":[{\"function\":\"pay\",\"filename\":\"src/pay.ts\",\"lineno\":3,\"in_app\":true}]}}]}" >/dev/null
sleep 2
TL=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/sessions/tl-1/timeline")
[ "$(echo "$TL" | j '[e["kind"] for e in d["timeline"]]')" = "['session_start', 'pageview', 'event', 'breadcrumb', 'crash', 'session_end']" ] || fail "session timeline: $TL"
echo "$TL" | j '[e["title"] for e in d["timeline"] if e["kind"] == "breadcrumb"][0]' | grep -q '^http: POST /api/pay 502' || fail "breadcrumb normalization: $TL"
PAYI=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/issues?since=1h&query=PayError" | j 'd["issues"][0]["short_id"]')
[ "$(curl -sf "${A[@]}" "$VAKA/api/v1/issues/$PAYI" | j '[e["kind"] for e in d["timeline_before_crash"]][-2:]')" = "['breadcrumb', 'crash']" ] || fail "timeline before crash"
PRE=$(curl -sf "${A[@]}" "$VAKA/api/v1/issues/$PAYI/precursors?minutes=5")
[ "$(echo "$PRE" | j 'sorted(s["step"] for s in d["steps"])')" = "['add_to_cart', 'pageview /cart']" ] || fail "precursors: $PRE"
echo "$PRE" | j 'd["breadcrumbs"][0]["breadcrumb"]' | grep -q 'http: POST /api/pay 502' || fail "precursor breadcrumbs: $PRE"

# Performance tracing
SPANS=$(python3 - <<'PY'
import json, time
now = int(time.time() * 1000)
items = []
for rel, base, dur in (("p1", now - 600_000, 100), ("p2", now - 300_000, 300)):
    for n in range(25):
        tid = f"{rel}{n:030d}"
        st = base + n * 1000
        items.append({"type": "span", "trace_id": tid, "span_id": f"{n:016d}", "name": f"GET /api/carts/{1000 + n}", "op": "http.server", "release": rel,
                      "start_timestamp": st, "duration_ms": dur + n, "http": {"method": "GET", "status_code": 500 if n == 0 else 200}})
        items.append({"type": "span", "trace_id": tid, "span_id": f"{n:015d}d", "parent_span_id": f"{n:016d}", "name": "SELECT carts", "op": "db", "release": rel,
                      "start_timestamp": st + 5, "duration_ms": dur * 0.8})
        items.append({"type": "span", "trace_id": f"w{rel}{n:029d}", "name": "/checkout/42", "op": "web.vitals", "transaction": "/checkout/42", "release": rel,
                      "start_timestamp": st, "duration_ms": 0, "measurements": {"lcp": 2000 if rel == "p1" else 4500, "cls": 0.05, "inp": 120}})
    items.append({"type": "span", "trace_id": f"s{rel}" + "0" * 29, "name": "app start", "op": "app.start.cold", "release": rel, "start_timestamp": base, "duration_ms": 800})
items.append({"type": "error", "release": "p2", "trace_id": "p2" + "0" * 30, "exception": {"type": "CartError", "value": "slow cart", "stacktrace": [{"function": "cart", "filename": "cart.rs", "lineno": 1, "in_app": True}]}})
print(json.dumps({"items": items}))
PY
)
[ "$(send "$SPANS" | j 'd["accepted"]')" = "153" ] || fail "span ingest"
curl -s -X POST "$VAKA/ingest/v1/spans" -H "x-vaka-key: $KEY" -H 'content-type: application/json' -d '[{"trace_id":"","name":"x"}]' | grep -q 'trace_id' || fail "span validation"
sleep 2
PERF="$VAKA/api/v1/projects/$SLUG/performance"
SUM=$(curl -sf "${A[@]}" "$PERF/summary?since=1h")
[ "$(echo "$SUM" | j '[(t["transaction"], t["count"]) for t in d["transactions"] if t["op"] == "http.server"]')" = "[('GET /api/carts/:id', 50)]" ] || fail "perf summary: $SUM"
EP=$(curl -sf "${A[@]}" "$PERF/endpoints?since=1h")
[ "$(echo "$EP" | j '(d["endpoints"][0]["endpoint"], d["endpoints"][0]["server_errors"])')" = "('GET /api/carts/:id', 2)" ] || fail "slow endpoints: $EP"
curl -sf "${A[@]}" "$PERF/app-start?since=1h" | j 'sorted((a["release"], a["start_type"], a["count"]) for a in d["app_starts"])' | grep -q "('p1', 'cold', 1)" || fail "app start"
WV=$(curl -sf "${A[@]}" "$PERF/web-vitals?since=1h&release=p2")
[ "$(echo "$WV" | j '[(m["metric"], m["rating"]) for m in d["overall"]]')" = "[('cls', 'good'), ('inp', 'good'), ('lcp', 'poor')]" ] || fail "web vitals: $WV"
[ "$(echo "$WV" | j 'd["pages"][0]["page"]')" = "/checkout/:id" ] || fail "web vitals pages: $WV"
TID=$(curl -sf "${A[@]}" "$PERF/traces?since=1h&op=http.server&limit=1" | j 'd["traces"][0]["trace_id"]')
[ "$TID" = "p2000000000000000000000000000024" ] || fail "slow traces: $TID"
TR=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/traces/$TID")
[ "$(echo "$TR" | j '[(s["op"], s["depth"]) for s in d["spans"]]')" = "[('http.server', 0), ('db', 1)]" ] || fail "trace tree: $TR"
curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/traces/p2000000000000000000000000000000" | j 'd["errors"][0]["title"]' | grep -q CartError || fail "trace errors"
BD=$(curl -sf "${A[@]}" "$PERF/breakdown?since=1h&transaction=GET%20/api/carts/:id")
[ "$(echo "$BD" | j '(d["spans"][0]["op"], d["spans"][0]["per_transaction"])')" = "('db', 1.0)" ] || fail "span breakdown: $BD"
CMP=$(curl -sf "${A[@]}" "$PERF/compare?base=p1&target=p2")
[ "$(echo "$CMP" | j '(d["regressed"], [r["transaction"] for r in d["regressions"]], [v["metric"] for v in d["web_vitals"] if v["regressed"]])')" = "(True, ['GET /api/carts/:id'], ['lcp'])" ] || fail "compare performance: $CMP"
PR=$(curl -sf "${A[@]}" -X POST "$VAKA/api/v1/alert-rules" -d "{\"project\":\"$SLUG\",\"trigger\":\"perf_regression\",\"channel\":\"webhook\",\"target\":\"http://127.0.0.1:$HOOK_PORT/perf\"}" | j 'd["id"]') || fail "perf rule"
curl -sf "${A[@]}" -X POST "$VAKA/api/v1/alert-rules/$PR/test" >/dev/null || fail "perf rule test"
for i in $(seq 1 10); do grep -q '^/perf ' "$HOOKS" && break; sleep 0.5; done
grep '^/perf perf_regression ' "$HOOKS" | grep -q '"target":"p2"' || fail "perf_regression webhook: $(grep '^/perf' "$HOOKS")"

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

# OpenTelemetry (OTLP/HTTP JSON) in its own project
OPROJ=$(curl -sf "${A[@]}" -X POST "$VAKA/api/v1/projects" -d "{\"name\":\"$SLUG-otel\",\"platform\":\"jvm\"}")
OKEY=$(echo "$OPROJ" | j 'd["ingest_key"]'); OSLUG=$(echo "$OPROJ" | j 'd["slug"]')
OTID=4bf92f3577b34da6a3ce929d0e0e4736
NS=$(python3 -c 'import time; print(time.time_ns())')
OTRACES=$(python3 - "$NS" "$OTID" <<'PY'
import json, sys
ns, tid = int(sys.argv[1]), sys.argv[2]
kv = lambda k, v: {"key": k, "value": {"intValue": str(v)} if isinstance(v, int) else {"stringValue": v}}
res = {"attributes": [kv("service.name", "cart-svc"), kv("service.version", "o1"), kv("deployment.environment.name", "production"), kv("telemetry.sdk.language", "java")]}
spans = [
  {"traceId": tid, "spanId": "00f067aa0ba902b7", "name": "GET", "kind": 2, "startTimeUnixNano": str(ns - 300_000_000), "endTimeUnixNano": str(ns),
   "status": {"code": 2}, "attributes": [kv("http.request.method", "GET"), kv("http.route", "/carts/{id}"), kv("http.response.status_code", 500)],
   "events": [{"name": "exception", "timeUnixNano": str(ns - 1000), "attributes": [kv("exception.type", "com.shop.OtelCartError"), kv("exception.message", "cart vanished"),
     kv("exception.stacktrace", "com.shop.OtelCartError: cart vanished\n\tat com.shop.Carts.load(Carts.java:31)\n\tat com.shop.Api.get(Api.java:12)")]}]},
  {"traceId": tid, "spanId": "00f067aa0ba902b8", "parentSpanId": "00f067aa0ba902b7", "name": "SELECT carts", "kind": 3,
   "startTimeUnixNano": str(ns - 250_000_000), "endTimeUnixNano": str(ns - 50_000_000), "attributes": [kv("db.system", "postgresql")]},
]
print(json.dumps({"resourceSpans": [{"resource": res, "scopeSpans": [{"scope": {"name": "smoke"}, "spans": spans}]}]}))
PY
)
OLOGS=$(python3 - "$NS" "$OTID" <<'PY'
import json, sys
ns, tid = int(sys.argv[1]), sys.argv[2]
res = {"attributes": [{"key": "service.name", "value": {"stringValue": "cart-svc"}}]}
logs = [
  {"timeUnixNano": str(ns - 200_000_000), "severityNumber": 9, "severityText": "INFO", "body": {"stringValue": "loading cart 42"}, "traceId": tid, "spanId": "00f067aa0ba902b7"},
  {"timeUnixNano": str(ns - 100_000_000), "severityNumber": 17, "severityText": "ERROR", "body": {"stringValue": "otel smoke payment declined"}, "traceId": tid, "spanId": "00f067aa0ba902b7"},
]
print(json.dumps({"resourceLogs": [{"resource": res, "scopeLogs": [{"scope": {"name": "com.shop.Payments"}, "logRecords": logs}]}]}))
PY
)
[ "$(curl -sf -X POST "$VAKA/ingest/otlp/v1/traces" -H "x-vaka-key: $OKEY" -H 'content-type: application/json' -d "$OTRACES")" = "{}" ] || fail "otlp traces"
[ "$(curl -sf -X POST "$VAKA/ingest/otlp/v1/logs" -H "authorization: Bearer $OKEY" -H 'content-type: application/json' -d "$OLOGS")" = "{}" ] || fail "otlp logs (bearer)"
[ "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$VAKA/ingest/otlp/v1/traces" -H "x-vaka-key: $OKEY" -H 'content-type: text/plain' -d 'x')" = "415" ] || fail "otlp content type"
[ "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$VAKA/ingest/otlp/v1/traces" -H 'content-type: application/json' -d "$OTRACES")" = "401" ] || fail "otlp auth"
curl -sf -X POST "$VAKA/ingest/otlp/v1/metrics" -H "x-vaka-key: $OKEY" -H 'content-type: application/json' -d '{}' >/dev/null || fail "otlp metrics"
sleep 2
OTR=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$OSLUG/traces/$OTID")
[ "$(echo "$OTR" | j '[(s["op"], s["name"], s["depth"]) for s in d["spans"]]')" = "[('http.server', 'GET /carts/{id}', 0), ('db', 'SELECT carts', 1)]" ] || fail "otlp trace: $OTR"
[ "$(echo "$OTR" | j '[l["body"] for l in d["logs"]]')" = "['loading cart 42', 'otel smoke payment declined']" ] || fail "trace logs: $OTR"
echo "$OTR" | j '[e["title"] for e in d["errors"]]' | grep -q OtelCartError || fail "otlp exception: $OTR"
OIS=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$OSLUG/issues?since=1h")
echo "$OIS" | j '[i["title"] for i in d["issues"]]' | grep -q 'otel smoke payment declined' || fail "error log issue: $OIS"
echo "$OIS" | j '[i["culprit"] for i in d["issues"] if "OtelCartError" in i["title"]]' | grep -q 'Carts' || fail "otlp culprit: $OIS"
OL=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$OSLUG/logs?since=1h&severity=error")
[ "$(echo "$OL" | j '([l["body"] for l in d["logs"]], d["counts"].get("error"), d["logs"][0]["service"])')" = "(['otel smoke payment declined'], 1, 'cart-svc')" ] || fail "log search: $OL"
[ "$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$OSLUG/logs?since=1h&q=CART%2042" | j 'len(d["logs"])')" = "1" ] || fail "log text search"
curl -sf "${A[@]}" "$VAKA/api/v1/projects/$OSLUG/performance/summary?since=1h" | grep -q 'GET /carts/{id}' || fail "otlp perf summary"

# MCP
M=(-H "authorization: Bearer $TOKEN" -H 'content-type: application/json' -H 'accept: application/json, text/event-stream')
curl -sf "${M[@]}" -X POST "$VAKA/mcp" -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"1"}}}' | grep -q '"vaka"' || fail mcp-init
TOOLS=$(curl -sf "${M[@]}" -X POST "$VAKA/mcp" -d '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' | j 'len(d["result"]["tools"])')
echo "mcp tools: $TOOLS"
curl -sf "${M[@]}" -X POST "$VAKA/mcp" -d "{\"jsonrpc\":\"2.0\",\"id\":6,\"method\":\"tools/call\",\"params\":{\"name\":\"compare_release_performance\",\"arguments\":{\"project\":\"$SLUG\",\"base\":\"p1\",\"target\":\"p2\"}}}" | j 'd["result"]["structuredContent"]["regressed"]' | grep -q True || fail mcp-perf-compare
curl -sf "${M[@]}" -X POST "$VAKA/mcp" -d "{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"tools/call\",\"params\":{\"name\":\"list_cohorts\",\"arguments\":{\"project\":\"$SLUG\"}}}" | j '[c["name"] for c in d["result"]["structuredContent"]["cohorts"]]' | grep -q buyers || fail mcp-cohorts
curl -sf "${M[@]}" -X POST "$VAKA/mcp" -d "{\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"tools/call\",\"params\":{\"name\":\"experiment_results\",\"arguments\":{\"project\":\"$FSLUG\",\"flag\":\"pricing\",\"goal\":\"exp_buy\",\"control\":\"a\",\"since\":\"1h\"}}}" | j 'd["result"]["structuredContent"]["winner"]' | grep -q b || fail mcp-experiment
curl -sf "${M[@]}" -X POST "$VAKA/mcp" -d "{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"tools/call\",\"params\":{\"name\":\"detect_anomalies\",\"arguments\":{\"project\":\"$ASLUG\"}}}" | j '[a["metric"] for a in d["result"]["structuredContent"]["anomalies"]]' | grep -q issue_errors || fail mcp-anomalies
curl -sf "${M[@]}" -X POST "$VAKA/mcp" -d "{\"jsonrpc\":\"2.0\",\"id\":10,\"method\":\"tools/call\",\"params\":{\"name\":\"search_logs\",\"arguments\":{\"project\":\"$OSLUG\",\"trace_id\":\"$OTID\",\"since\":\"1h\"}}}" | j 'len(d["result"]["structuredContent"]["logs"])' | grep -qx 2 || fail mcp-search-logs
curl -sf "${M[@]}" -X POST "$VAKA/mcp" -d '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"list_alert_rules","arguments":{}}}' | j 'd["result"]["structuredContent"]["channels"]' | grep -q github_dispatch || fail mcp-alert-rules
curl -sf "${M[@]}" -X POST "$VAKA/mcp" -d "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"get_fix_context\",\"arguments\":{\"issue\":\"$OWNED\"}}}" | j 'd["result"]["structuredContent"]["suggested"]["branch"]' | grep -q "^vaka/" || fail mcp-fix-context
curl -sf "${M[@]}" -X POST "$VAKA/mcp" -d "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"resolve_issue\",\"arguments\":{\"issue\":\"$SHORT\",\"resolved_in_release\":\"1.0.1\"}}}" | j 'd["result"]["structuredContent"]["status"]' | grep -q resolved || fail mcp-resolve
# Regression
send '{"items":[{"type":"error","release":"1.0.2","exception":{"type":"TypeError","value":"x is undefined","stacktrace":[{"function":"render","filename":"app.js","lineno":12,"in_app":true},{"function":"main","filename":"app.js","lineno":2}]}}]}'
sleep 2
[ "$(curl -sf "${A[@]}" "$VAKA/api/v1/issues/$SHORT" | j 'd["status"]')" = "open" ] || fail regression
# User feedback
FHOOK=$(curl -sf "${A[@]}" -X POST "$VAKA/api/v1/alert-rules" -d "{\"project\":\"$SLUG\",\"trigger\":\"new_feedback\",\"channel\":\"webhook\",\"target\":\"http://127.0.0.1:$HOOK_PORT/feedback\",\"filters\":{\"linked_to_issue\":true}}" | j 'd["id"]') || fail "feedback rule"
curl -s "${A[@]}" -X POST "$VAKA/api/v1/alert-rules" -d "{\"project\":\"$SLUG\",\"trigger\":\"new_feedback\",\"channel\":\"webhook\",\"target\":\"http://127.0.0.1:1/x\",\"filters\":{\"sentiment\":\"angry\"}}" | grep -q 'sentiment must be' || fail "feedback filter validation"
EID=$(python3 -c 'import uuid; print(uuid.uuid4())')
send "{\"items\":[{\"type\":\"error\",\"event_id\":\"$EID\",\"release\":\"1.0.2\",\"session_id\":\"fs1\",\"user\":{\"id\":\"u9\"},\"exception\":{\"type\":\"FeedbackSmokeError\",\"value\":\"boom\",\"stacktrace\":[{\"function\":\"pay\",\"filename\":\"app.js\",\"lineno\":7,\"in_app\":true}]}}]}"
sleep 2
FB1="{\"items\":[{\"type\":\"feedback\",\"id\":\"fb-smoke-1\",\"message\":\"It crashed when I paid. token=abc123secret\",\"email\":\"Bob@Example.com\",\"event_id\":\"$EID\",\"source\":\"crash_prompt\",\"release\":\"1.0.2\",\"session_id\":\"fs1\",\"user_id\":\"u1\",\"flags\":{\"new_checkout\":\"b\"}}]}"
send "$FB1" | grep -q '"accepted":1' || fail "feedback batch"
send "$FB1" | grep -q '"accepted":1' || fail "feedback retry"
PNG=$(mktemp --suffix=.png)
python3 -c '
import struct, sys, zlib
w, h = 64, 40
raw = b"".join(b"\x00" + b"\x20\x60\xc0\xff" * w for _ in range(h))
def chunk(t, d): return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
open(sys.argv[1], "wb").write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))
' "$PNG"
FB2=$(curl -sf -X POST "$VAKA/ingest/v1/feedback" -H "x-vaka-key: $KEY" -H 'x-vaka-sdk: smoke/1' -F 'feedback={"message":"Love the new dark mode!","session_id":"fs2","release":"1.0.2","flags":{"new_checkout":"a"},"url":"https://shop.example.com/settings?ref=x"};type=application/json' -F "screenshot=@$PNG;type=image/png") || fail "feedback multipart"
FB2ID=$(echo "$FB2" | j 'd["id"]')
[ "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$VAKA/ingest/v1/feedback" -H "x-vaka-key: $KEY" -F 'feedback={"message":"x"};type=application/json' -F "screenshot=@$0;type=image/png")" = "400" ] || fail "feedback non-image accepted"
[ "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$VAKA/ingest/v1/feedback" -H "x-vaka-key: $KEY" -H 'content-type: application/json' -d '{"message":"  "}')" = "400" ] || fail "empty feedback accepted"
[ "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$VAKA/ingest/v1/feedback" -H 'content-type: application/json' -d '{"message":"hi"}')" = "401" ] || fail "feedback without key"
for i in 1 2 3 4 5; do curl -sf -o /dev/null -X POST "$VAKA/ingest/v1/feedback" -H "x-vaka-key: $KEY" -H 'content-type: application/json' -d "{\"message\":\"spam $i\",\"session_id\":\"fs3\"}" || fail "feedback $i"; done
[ "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$VAKA/ingest/v1/feedback" -H "x-vaka-key: $KEY" -H 'content-type: application/json' -d '{"message":"spam 6","session_id":"fs3"}')" = "429" ] || fail "feedback session rate limit"
FL=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/feedback?session=fs1")
[ "$(echo "$FL" | j 'len(d["feedback"])')" = "1" ] || fail "feedback dedupe: $FL"
[ "$(echo "$FL" | j 'd["feedback"][0]["contact_email"]')" = "bob@example.com" ] || fail "feedback email: $FL"
echo "$FL" | j 'd["feedback"][0]["message"]' | grep -q 'token=\[Filtered\]' || fail "feedback scrub: $FL"
FISSUE=$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/issues?since=1h&query=FeedbackSmokeError" | j 'd["issues"][0]["short_id"]')
[ "$(echo "$FL" | j 'd["feedback"][0]["issue_short_id"]')" = "$FISSUE" ] || fail "feedback crash link ($FISSUE): $FL"
FSHORT=$(echo "$FL" | j 'd["feedback"][0]["short_id"]')
echo "feedback $FSHORT about $FISSUE"
[ "$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/feedback?has_screenshot=true" | j '[f["message"] for f in d["feedback"]]')" = "['Love the new dark mode!']" ] || fail "feedback screenshot filter"
[ "$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/feedback?flag=new_checkout=b" | j 'len(d["feedback"])')" = "1" ] || fail "feedback flag filter"
[ "$(curl -sf "${A[@]}" "$VAKA/api/v1/feedback/$FB2ID/attachments/0" | wc -c)" = "$(wc -c < "$PNG")" ] || fail "feedback attachment bytes"
curl -sfI "${A[@]}" "$VAKA/api/v1/feedback/$FB2ID/attachments/0" | grep -qi 'content-type: image/png' || fail "feedback attachment type"
[ "$(curl -s -o /dev/null -w '%{http_code}' "$VAKA/api/v1/feedback/$FB2ID/attachments/0")" = "401" ] || fail "attachment without auth"
[ "$(curl -sf "${A[@]}" -X PATCH "$VAKA/api/v1/feedback/$FSHORT" -d '{"status":"triaged","topics":["Checkout","payments"]}' | j '(d["status"], d["topics"])')" = "('triaged', ['checkout', 'payments'])" ] || fail "feedback update"
[ "$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/feedback/summary?by=flag&flag=new_checkout" | j 'sorted(b["key"] for b in d["buckets"])')" = "['(not set)', 'a', 'b']" ] || fail "feedback summary"
[ "$(curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/feedback/summary?by=topic" | j '[b["key"] for b in d["buckets"]][:2]')" = "['(none)', 'checkout']" ] || fail "feedback topic summary"
[ "$(curl -sf "${A[@]}" "$VAKA/api/v1/issues/$FISSUE" | j 'd["feedback"]["count"]')" = "1" ] || fail "issue feedback"
curl -sf "${A[@]}" "$VAKA/api/v1/projects/$SLUG/sessions/fs1/timeline" | j '[e["kind"] for e in d["timeline"]]' | grep -q feedback || fail "timeline feedback"
curl -sf -b "$JAR" "$VAKA/ui/projects/$SLUG/feedback" | grep -q 'Love the new dark mode' || fail "feedback admin page"
MF=$(curl -sf -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' -X POST "$VAKA/mcp" -d "{\"jsonrpc\":\"2.0\",\"id\":11,\"method\":\"tools/call\",\"params\":{\"name\":\"get_feedback\",\"arguments\":{\"feedback\":\"$FB2ID\"}}}")
[ "$(echo "$MF" | j '[(c["type"], c.get("mimeType")) for c in d["result"]["content"]]')" = "[('text', None), ('image', 'image/png')]" ] || fail "mcp get_feedback image"
for i in $(seq 1 30); do grep -q '^/feedback ' "$HOOKS" && break; sleep 1; done
grep '^/feedback ' "$HOOKS" | grep -q "\"short_id\":\"$FSHORT\"" || fail "feedback alert: $(grep '^/feedback ' "$HOOKS")"
grep '^/feedback ' "$HOOKS" | grep -q 'bob@example.com' && fail "feedback alert leaked the email"
[ "$(grep -c '^/feedback ' "$HOOKS")" = "1" ] || fail "feedback alert filter: $(grep '^/feedback ' "$HOOKS")"
# Backend SDKs: link by issue short id, end user's address in X-Forwarded-For
FB3=$(curl -sf -X POST "$VAKA/ingest/v1/feedback" -H "x-vaka-key: $KEY" -H 'x-forwarded-for: 198.51.100.7' -H 'content-type: application/json' -d "{\"message\":\"Support form: export is broken\",\"issue\":\"$FISSUE\",\"user_id\":\"u5\"}") || fail "feedback with issue"
[ "$(echo "$FB3" | j 'd["issue_id"] is not None')" = "True" ] || fail "feedback issue link: $FB3"
FB4=$(curl -sf -X POST "$VAKA/ingest/v1/feedback" -H "x-vaka-key: $KEY" -H 'x-forwarded-for: 198.51.100.7' -H 'content-type: application/json' -d '{"message":"Unknown issue","issue":"NOPE-999"}') || fail "feedback with unknown issue"
[ "$(echo "$FB4" | j 'd["issue_id"]')" = "None" ] || fail "feedback unknown issue linked: $FB4"
# GDPR
GD=$(curl -sf "${A[@]}" -X DELETE "$VAKA/api/v1/projects/$SLUG/users/u1")
echo "$GD" | j 'd["errors"]' | grep -q 1 || fail "gdpr: $GD"
[ "$(echo "$GD" | j 'd["feedback"]')" = "1" ] || fail "gdpr feedback: $GD"
echo "SMOKE OK"
