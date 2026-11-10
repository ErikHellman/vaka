# Send data to Vaka over HTTP (any language)

Project: `{{PROJECT}}` · Endpoint: `{{ENDPOINT}}` · Ingest key: `{{KEY}}`

There is no SDK for this platform yet. Vaka's ingest protocol is plain JSON over HTTP, so a
small client is enough. The full contract is at `{{ENDPOINT}}/docs/ingest.md`; this page is the
short version for a coding agent writing that client.

## 1. Report an error

```sh
curl -X POST {{ENDPOINT}}/ingest/v1/batch \
  -H 'X-Vaka-Key: {{KEY}}' -H 'Content-Type: application/json' \
  -d '{
    "sdk": {"name": "my-client", "version": "0.1.0"},
    "items": [{
      "type": "error",
      "platform": "go",
      "release": "1.4.2",
      "environment": "production",
      "level": "error",
      "exceptions": [{
        "type": "ValueError",
        "value": "invalid amount",
        "stacktrace": [
          {"function": "parseAmount", "filename": "billing.go", "lineno": 42, "in_app": true},
          {"function": "main", "filename": "main.go", "lineno": 10, "in_app": true}
        ]
      }],
      "breadcrumbs": [{"category": "http", "message": "GET /invoice/7", "level": "info"}],
      "tags": {"region": "eu"}
    }]
  }'
```

Rules that matter:

- `stacktrace` frames are ordered **crash site first** (`frames[0]` is where it failed).
- Mark your own code with `in_app: true`; grouping uses the top in-app frames.
- The server answers `202 Accepted`. Treat `429` and `5xx` as retryable with backoff; drop on other `4xx`.
- Batch items (up to 1000 per request) and send at least every few seconds; flush on shutdown.

## 2. Sessions (crash-free rates)

Send a session item when a process/app session starts and again when it ends:

```json
{"type": "session", "session_id": "<uuid>", "status": "ok", "release": "1.4.2",
 "started": "2026-11-10T08:00:00Z", "anon_id": "<stable random install id>"}
```

End it with the same `session_id` and `status` set to `exited`, `crashed` or `errored`, plus `duration_ms`.

## 3. Analytics events

```json
{"type": "event", "name": "signup", "props": {"plan": "pro"}, "release": "1.4.2"}
```

Use `"name": "pageview"` with `url` for web pages and `"name": "screen_view"` with
`props.screen` for app screens.

## 4. Verify

`vaka send-test-event` (with `VAKA_URL={{ENDPOINT}} VAKA_KEY={{KEY}}`) checks connectivity.
Then send an error from the new client and look for it with
`curl -H "Authorization: Bearer $VAKA_TOKEN" {{ENDPOINT}}/api/v1/projects/{{PROJECT}}/issues`
or the MCP tool `list_issues`.
