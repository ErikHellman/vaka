# Ingest protocol (v1)

SDKs send data to `POST {{ENDPOINT}}/ingest/v1/batch`. This page is the contract every SDK
implements; any language can use it directly.

## Authentication

Send the project's ingest key (starts with `vk_`) in the `X-Vaka-Key` header. Ingest keys
are write-only and safe to embed in apps. For `navigator.sendBeacon`, which cannot set
headers, use `?key=vk_...` in the URL instead.

Optional: `X-Vaka-Sdk: name/version` on the single-kind endpoints below.

## Request

```http
POST /ingest/v1/batch
X-Vaka-Key: vk_...
Content-Type: application/json
Content-Encoding: gzip          (optional)

{
  "sdk": {"name": "vaka-android", "version": "0.1.0"},
  "items": [ {"type": "error", ...}, {"type": "session", ...}, {"type": "event", ...} ]
}
```

- At most 1000 items and 20 MB (after decompression) per request.
- The body is parsed as JSON regardless of `Content-Type` (so `text/plain` beacons work).
- Single-kind shortcuts accept one item or an array, without `type`:
  `POST /ingest/v1/errors`, `/ingest/v1/sessions`, `/ingest/v1/events`.

## Response

`202 Accepted` with `{"accepted": 3, "rejected": [{"index": 1, "error": "..."}]}`. Invalid items
are reported individually and never fail the whole batch. Other statuses:

| Status | Meaning | SDK behaviour |
| --- | --- | --- |
| 400 | Body is not JSON, or no item was valid | Drop the batch, log the `error` message |
| 401 | Missing, unknown or revoked key | Stop sending until the app restarts |
| 413 | Too many items | Split the batch |
| 429 / 503 | Queue full or temporarily unavailable | Retry with exponential backoff (max ~5 min) |

Errors always come back as `{"error": "human-readable explanation"}`.

## Common fields

Timestamps (`timestamp`, `started`) accept RFC 3339 strings (`"2026-11-10T08:00:00.123Z"`),
unix seconds (float) or unix milliseconds. Missing timestamps default to the time of receipt.

`release` should be the app version as users see it (`"2.3.1"` or `"2.3.1+45"`), and it must
match the `release` used when uploading debug files. `environment` defaults to `production`.

## Error item

```json
{
  "type": "error",
  "event_id": "1f0c7c4e-0b1a-4b7e-9e7a-0d7b5d3f1a2b",
  "timestamp": "2026-11-10T08:00:00.123Z",
  "platform": "android",
  "level": "error",
  "fatal": false,
  "handled": true,
  "release": "2.3.1",
  "environment": "production",
  "message": "optional free-text message (used when there is no exception)",
  "exceptions": [
    {
      "type": "java.lang.IllegalStateException",
      "value": "Cart is empty",
      "module": "com.example.cart",
      "stacktrace": [
        {"function": "checkout", "module": "com.example.cart.CartViewModel", "filename": "CartViewModel.kt", "lineno": 42, "in_app": true},
        {"function": "onClick", "module": "com.example.ui.CartFragment", "filename": "CartFragment.kt", "lineno": 88, "in_app": true}
      ]
    }
  ],
  "breadcrumbs": [
    {"timestamp": "2026-11-10T07:59:58Z", "category": "navigation", "message": "Cart", "level": "info", "data": {}}
  ],
  "tags": {"flavor": "free"},
  "contexts": {"app": {"build": "45"}, "runtime": {"name": "Android", "version": "15"}},
  "user": {"id": "user-123"},
  "anon_id": "random-install-id",
  "session_id": "uuid of the current session",
  "device": {"model": "Pixel 9", "os_name": "Android", "os_version": "15", "app_version": "2.3.1"},
  "url": "https://example.com/cart",
  "debug_images": [],
  "fingerprint": ["optional", "custom", "grouping", "key"]
}
```

Rules:

- **Frames are ordered crash site first**: `stacktrace[0]` is where the error was raised.
- `exceptions` is the cause chain, outermost first. A single `exception` object is also accepted.
- Mark library/framework frames `in_app: false`. Grouping uses the top in-app frames.
- Breadcrumbs: send at most the last 100.
- Frame fields: `function`, `module` (JVM class / package), `filename`, `abs_path` (full URL or path;
  used to match JS source maps), `lineno`, `colno`, `in_app`, `instruction_addr` (native, hex string),
  `package` (native image path), `context_line`, `pre_context`, `post_context`.
- `level`: `fatal`, `error`, `warning` or `info`.

### Native crashes (iOS, Rust, C/C++)

Send raw addresses plus the loaded images; the server symbolicates with uploaded debug files.

```json
"exceptions": [{"type": "EXC_BAD_ACCESS", "value": "SIGSEGV at 0x0", "stacktrace": [
  {"instruction_addr": "0x0000000104a3c1f4", "in_app": true},
  {"instruction_addr": "0x0000000104a3b0e8"}
]}],
"debug_images": [
  {"type": "macho", "debug_id": "6a8e2b3c-1f0d-3e4a-9b8c-7d6e5f4a3b2c", "code_file": "/private/var/containers/.../MyApp",
   "image_addr": "0x0000000104a20000", "image_size": 1048576}
]
```

- `debug_id`: Mach-O `LC_UUID`; for ELF send the GNU build id as `code_id` (hex) and the server
  derives the debug id. PE/PDB: GUID + age.
- If the SDK can already resolve function names in-process, send them in `function` as well;
  server symbolication adds file and line when debug files exist.

### ANRs and app hangs

Report a hung main thread as an ordinary error item:

- `tags.mechanism`: `"anr"` (Android, main thread blocked; default threshold 5 s) or `"hang"` (iOS, default 2 s).
- `exceptions[0].type`: `"ANR"` or `"AppHang"`; `value` like `"Application not responding for 5000 ms"`.
- `stacktrace`: the main thread's stack at detection, crash site first.
- `fatal: false`, `handled: false`. If the app is killed while hung, end that session with status `abnormal`.

Release health reports ANR/hang rates per release from these events (`anr_rate_sessions` is the
share of sessions with at least one; it needs `session_id` on the event).

### JVM / Android with R8

Send obfuscated class and method names as they appear (`module` = class, `function` = method,
`lineno`). The server deobfuscates them with the mapping uploaded for the same `release`.

### JavaScript

Send `abs_path` (the script URL), `lineno` and `colno` from the minified stack. The server
applies the source map uploaded for that file and `release`.

## Session item (release health)

```json
{"type": "session", "session_id": "uuid", "status": "ok", "started": "...", "timestamp": "...",
 "duration_ms": 5400, "errors": 0, "release": "2.3.1", "environment": "production",
 "user_id": "optional", "anon_id": "random-install-id"}
```

- Send an update when a session starts (`status: "ok"`), and when it ends: `exited` (normal),
  `crashed` (the app crashed), `errored` (handled errors happened), or `abnormal` (killed, ANR).
- Updates for the same `session_id` replace earlier ones. A crash on the previous run is reported
  on the next launch as a `crashed` update for that previous session.
- Mobile: a session starts when the app comes to the foreground and ends after 30 seconds in the
  background.

## Event item (analytics)

```json
{"type": "event", "name": "pageview", "url": "https://example.com/pricing?utm_source=hn",
 "referrer": "https://news.ycombinator.com/", "props": {"plan": "pro"}}
```

- `name`: `pageview` (web), `screen_view` (mobile, with `screen`), or any custom event name.
- Web: `url` and `referrer`; the server derives path, hostname, UTM tags, browser, OS and device
  from the URL and `User-Agent`, and country from the IP (the IP itself is never stored).
- Mobile: `screen`, `device`, `release`, and `anon_id` (a random id generated on install and kept
  in app storage; it is hashed server-side before storage).
- `props`: flat string/number/bool values, at most 64 keys.
- `user_id` only after the app calls `identify()` (opt-in).

## Privacy guarantees

- Web visitors are counted with a daily rotating salted hash of IP + User-Agent + project; the salt
  is deleted after a day, so visitors cannot be tracked across days.
- Error payloads are scrubbed of passwords, tokens, emails and card numbers (configurable).
- See [privacy.md](privacy.md).
