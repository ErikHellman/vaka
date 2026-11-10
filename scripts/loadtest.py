#!/usr/bin/env python3
"""Load test: N projects sending pageviews, sessions and errors concurrently.

Usage: VAKA=http://localhost:8080 VAKA_TOKEN=vt_... python3 scripts/loadtest.py [projects] [seconds] [threads]
Creates (or reuses) projects named load-1..load-N, then sends batches of mixed items and
reports throughput, ingest latency, and query latency while under load.
"""
import http.client, json, os, random, sys, threading, time, urllib.parse, uuid

BASE = os.environ.get("VAKA", "http://localhost:8080")
TOKEN = os.environ["VAKA_TOKEN"]
N = int(sys.argv[1]) if len(sys.argv) > 1 else 30
SECONDS = int(sys.argv[2]) if len(sys.argv) > 2 else 60
THREADS = int(sys.argv[3]) if len(sys.argv) > 3 else 16
u = urllib.parse.urlparse(BASE)

def conn():
    return http.client.HTTPConnection(u.hostname, u.port or 80, timeout=30)

def api(method, path, body=None):
    c = conn()
    c.request(method, path, json.dumps(body) if body else None, {"Authorization": f"Bearer {TOKEN}", "Content-Type": "application/json"})
    r = c.getresponse()
    data = r.read()
    return r.status, json.loads(data or b"{}")

keys = {}
_, existing = api("GET", "/api/v1/projects")
for p in existing.get("projects", []):
    if p["slug"].startswith("load-"):
        keys[p["slug"]] = p["ingest_keys"][0] if p.get("ingest_keys") else p.get("ingest_key")
for i in range(1, N + 1):
    slug = f"load-{i}"
    if slug not in keys:
        _, p = api("POST", "/api/v1/projects", {"name": f"Load {i}", "platform": random.choice(["javascript", "android", "ios"]), "slug": slug})
        keys[slug] = p["ingest_key"]
slugs = sorted(keys)[:N]

PATHS = ["/", "/pricing", "/docs", "/blog/a", "/blog/b", "/signup", "/login", "/app"]
UAS = [
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:132.0) Gecko/20100101 Firefox/132.0",
]
ERRORS = [("TypeError", "x is undefined", "checkout"), ("RangeError", "bad index", "render"), ("Error", "timeout after 3000ms", "fetchUser")]

def item():
    r = random.random()
    rel = random.choice(["1.0.0", "1.1.0", "1.2.0"])
    if r < 0.85:
        return {"type": "event", "name": "pageview" if random.random() < 0.9 else "signup",
                "url": "https://site.example" + random.choice(PATHS), "referrer": random.choice(["", "https://news.ycombinator.com/", "https://google.com/"]), "release": rel}
    if r < 0.97:
        return {"type": "session", "session_id": str(uuid.uuid4()), "status": random.choice(["exited"] * 9 + ["crashed"]), "release": rel,
                "anon_id": f"inst-{random.randint(1, 5000)}", "duration_ms": random.randint(1000, 600000)}
    t, v, fn = random.choice(ERRORS)
    return {"type": "error", "release": rel, "platform": "javascript", "exceptions": [{"type": t, "value": v, "stacktrace": [
        {"function": fn, "filename": "app.js", "abs_path": "https://site.example/assets/app.js", "lineno": random.randint(1, 3), "colno": 10, "in_app": True}]}],
        "breadcrumbs": [{"category": "ui.click", "message": "button#buy"}] * 20}

stats = {"items": 0, "requests": 0, "errors": 0, "lat": [], "status": {}}
lock = threading.Lock()
deadline = time.time() + SECONDS

def worker():
    c = conn()
    while time.time() < deadline:
        slug = random.choice(slugs)
        batch = {"sdk": {"name": "loadtest", "version": "1"}, "items": [item() for _ in range(random.randint(5, 30))]}
        body = json.dumps(batch)
        hdr = {"X-Vaka-Key": keys[slug], "Content-Type": "application/json", "User-Agent": random.choice(UAS),
               "X-Forwarded-For": f"10.{random.randint(0,255)}.{random.randint(0,255)}.{random.randint(1,254)}"}
        t0 = time.time()
        try:
            c.request("POST", "/ingest/v1/batch", body, hdr)
            r = c.getresponse(); r.read()
            ok = r.status == 202
            with lock: stats["status"][r.status] = stats["status"].get(r.status, 0) + 1
        except Exception:
            ok = False; c = conn()
        dt = time.time() - t0
        with lock:
            stats["requests"] += 1
            stats["lat"].append(dt)
            if ok: stats["items"] += len(batch["items"])
            else: stats["errors"] += 1

qlat = []
def querier():
    while time.time() < deadline:
        slug = random.choice(slugs)
        for path in [f"/api/v1/projects/{slug}/issues?since=1h", f"/api/v1/projects/{slug}/analytics/summary?since=1h",
                     f"/api/v1/projects/{slug}/analytics/breakdown?dimension=path&since=1h", f"/api/v1/projects/{slug}/releases/health"]:
            t0 = time.time(); s, _ = api("GET", path); qlat.append((time.time() - t0, s))
        time.sleep(0.5)

ts = [threading.Thread(target=worker) for _ in range(THREADS)] + [threading.Thread(target=querier)]
print(f"{len(slugs)} projects, {THREADS} threads, {SECONDS}s against {BASE}")
start = time.time()
for t in ts: t.start()
for t in ts: t.join()
el = time.time() - start
lat = sorted(stats["lat"]); ql = sorted(x for x, _ in qlat)
pct = lambda a, p: a[min(len(a) - 1, int(len(a) * p))] * 1000 if a else 0
print(f"ingest: {stats['items']} items in {stats['requests']} requests, {stats['items']/el:.0f} items/s, {stats['requests']/el:.0f} req/s, failed {stats['errors']}")
print("statuses:", stats["status"])
print(f"ingest latency ms: p50 {pct(lat,.5):.1f} p95 {pct(lat,.95):.1f} p99 {pct(lat,.99):.1f}")
print(f"query latency ms under load ({len(ql)} queries, non-200: {sum(1 for _, s in qlat if s != 200)}): p50 {pct(ql,.5):.1f} p95 {pct(ql,.95):.1f} max {pct(ql,1):.1f}")
