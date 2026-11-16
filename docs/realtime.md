# Real-time data

Three ways to get fresh data out of Vaka, from simplest to most immediate.

| Need | Use |
| --- | --- |
| A dashboard that refreshes every few seconds | Poll the REST API, e.g. `GET /api/v1/projects/{p}/analytics/realtime` |
| React to something important (new issue, spike, monitor down) | [Alerts](alerts.md): webhooks, Slack, email, SMS, GitHub |
| Every new error, event or span, as it is stored | The live stream: `GET /api/v1/projects/{p}/stream` (Server-Sent Events) |
| Charts in Grafana | [Grafana](grafana.md) |

New data is written to ClickHouse about once a second, so polling every 5 to 10 seconds
already feels live.

## Polling

`GET /api/v1/projects/{p}/analytics/realtime?minutes=5` returns visitors, pageviews, active
sessions, errors, top pages, referrers, countries and custom events for the last `minutes`
(1 to 60), plus `per_minute_last_30`. Every other endpoint in [api.md](api.md) takes `since`
as `15m`, `24h`, an ISO time or epoch milliseconds, so any of them can back a dashboard panel.

## Live stream (SSE)

```
GET /api/v1/projects/{p}/stream?types=error,event
Authorization: Bearer <token>
Accept: text/event-stream
```

| Parameter | Default | Meaning |
| --- | --- | --- |
| `types` | `error,event` | Comma-separated: `error`, `event`, `span` |
| `event_types` | all | Analytics event types to include: `pageview`, `screen_view`, `custom`, ... |
| `environment` | all | Only this environment |
| `root_spans_only` | `true` | With `span`, send only the root span of each trace |

The response is a standard `text/event-stream`. Every message is JSON:

```
event: ready
data: {"project":"web","types":["error","event"],"event_types":[],"environment":null}

event: error
data: {"event_id":"19cf…","issue_id":"7df0…","timestamp":"2026-11-16T14:03:15.695Z","level":"error","exception_type":"TypeError","exception_value":"x is undefined","release":"web@1.4.0","environment":"production",…}

event: event
data: {"event_id":"339b…","timestamp":"2026-11-16T14:03:15.694Z","type":"pageview","name":"pageview","path":"/pricing","country":"SE","browser":"Chrome",…}

event: span
data: {"trace_id":"…","name":"GET /api/cart","duration_ms":182.4,"status":"ok","http_status":200,…}

event: lagged
data: {"dropped":120}
```

- Messages are sent after the rows are stored, so for an error `GET /api/v1/projects/{p}/events/{event_id}`
  and `GET /api/v1/issues/{issue_id}` work right away for the full details (stack frames,
  breadcrumbs and contexts are not in the stream).
- A comment line (`: ping`) is sent every 15 seconds to keep proxies from closing the connection.
- `lagged` means the client read too slowly and missed `dropped` messages. Catch up with the
  REST API if you need every row.
- There is no replay: a client that reconnects gets only new data. Use REST with `since` to
  fill the gap.
- Each server accepts 200 open streams; past that it answers `503`.
- Read access is enough. Use a read-only token scoped to the project.
- Behind nginx, turn off buffering for this path (`proxy_buffering off;`) or messages arrive in bursts.

### From a script

```sh
curl -N -H "Authorization: Bearer $VAKA_TOKEN" "$VAKA/api/v1/projects/web/stream?types=error"
```

### From a backend (recommended for web dashboards)

Keep the token on your server and relay the stream (or derived numbers) to the browser:

```js
// Node 18+
const res = await fetch(`${VAKA}/api/v1/projects/web/stream?types=error,event`, {
  headers: { authorization: `Bearer ${process.env.VAKA_TOKEN}` },
});
const decoder = new TextDecoderStream();
let buf = "";
for await (const chunk of res.body.pipeThrough(decoder)) {
  buf += chunk;
  let i;
  while ((i = buf.indexOf("\n\n")) >= 0) {
    const block = buf.slice(0, i); buf = buf.slice(i + 2);
    const type = /^event: (.*)$/m.exec(block)?.[1];
    const data = /^data: (.*)$/m.exec(block)?.[1];
    if (type && data) handle(type, JSON.parse(data));
  }
}
```

### From the browser directly

The browser's `EventSource` cannot send an `Authorization` header, so read the stream with
`fetch` as above (or a library such as `@microsoft/fetch-event-source`). For a cross-origin page,
allow its origin with `VAKA_API_CORS_ORIGINS` (see [self-hosting.md](self-hosting.md)). Anyone
who opens the page can read the token, so only do this with a read-only, project-scoped token
on an internal dashboard.

## Webhooks

Alert rules push a signed JSON payload when something happens: `new_issue`, `regression`,
`spike`, `threshold`, `issue_resolved`, `release_created`, `perf_regression`, `new_feedback`,
`monitor_down`, `monitor_up` and more. They are for events worth acting on, not for every row.
See [alerts.md](alerts.md).
