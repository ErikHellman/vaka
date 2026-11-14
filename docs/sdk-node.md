# Node.js SDK

Package `@hellsoft/vaka-node` (source: `sdks/node`). A thin layer on the standard OpenTelemetry Node SDK:
traces and logs go to Vaka's [OTLP endpoint](opentelemetry.md), and the SDK adds what
OpenTelemetry doesn't do well: crash capture, breadcrumbs, per-request user and tags, source
context, release tagging and feature flags. Errors go to the [ingest API](ingest.md) with the
active trace id, so an agent can move from an issue to its trace and back. Node.js 22.15+.

Agent-followable install guide: [`/docs/install/node`]({{ENDPOINT}}/docs/install/node).

```sh
npm install @hellsoft/vaka-node
node --import @hellsoft/vaka-node/register server.js   # reads VAKA_KEY, VAKA_ENDPOINT, VAKA_RELEASE, ...
```

or in code, before the framework is imported:

```ts
import * as Vaka from '@hellsoft/vaka-node';
Vaka.init({ key: 'vk_...', endpoint: 'https://vaka.example.com', release: '2.4.0' });
```

## Init options

| Option | Default | Meaning |
| --- | --- | --- |
| `key` | `$VAKA_KEY` / `$VAKA_INGEST_KEY` | Ingest key; missing disables the SDK with a warning |
| `endpoint` | `$VAKA_ENDPOINT` | Server URL |
| `release` | `$VAKA_RELEASE`, else `$npm_package_version` | App version (`service.version` on spans) |
| `environment` | `$VAKA_ENVIRONMENT`, else `$NODE_ENV`, else `production` | |
| `serviceName` | `$OTEL_SERVICE_NAME`, else `$npm_package_name` | `service.name`, and the `service` tag on errors |
| `otel` | `true` | Start the OpenTelemetry Node SDK. `false` when you run your own |
| `tracesSampleRate` | `1` (`$VAKA_TRACES_SAMPLE_RATE` with the preload) | Share of new traces recorded; an incoming `traceparent` decision wins |
| `logs` | `true` | Export OpenTelemetry log records |
| `instrumentations` | HTTP only | e.g. `[getNodeAutoInstrumentations()]` |
| `resourceAttributes` | | Extra resource attributes |
| `captureUnhandled` | `true` | Report uncaught exceptions and unhandled rejections |
| `unhandledRejection` | Node's `--unhandled-rejections`, else `strict` | `strict` reports and exits 1, `warn` reports and keeps running, `none` only reports |
| `exitOnUncaught` | `true` | Exit 1 after an uncaught exception (skipped when the app has its own `uncaughtException` handler) |
| `shutdownTimeoutMs` | `2000` | How long a crash waits for delivery |
| `breadcrumbs` | `true` | Console warn/error and outgoing `fetch`/`http` requests |
| `maxBreadcrumbs` | `100` | Per scope |
| `sourceContext` | `true` | Read 5 lines around in-app frames from disk |
| `errorSampleRate` | `1` | Share of non-fatal errors sent |
| `ignoreErrors` | | Strings or regexes matched against the message |
| `shouldCaptureStatus` | `status >= 500` | Which HTTP errors the middleware reports |
| `flagsCacheTtlMs` | `30000` | Flag evaluations are cached per identity and properties |
| `beforeSend` | | Change or drop (return `null`) an item |
| `flushIntervalMs` | `2000` | Batching interval; errors are sent at once |
| `debug` | `false` | Log to stderr (`VAKA_DEBUG=1` with the preload) |

## API

| Call | |
| --- | --- |
| `captureException(err, ctx?)` (alias `captureError`) | Report an error with its `cause` chain. `ctx`: `level`, `tags`, `extra`, `handled`, `fingerprint`, `user`. Returns the event id |
| `captureMessage(msg, level?)` | |
| `addBreadcrumb({ category, message, level, data })` | |
| `setUser(id \| { id } \| null)` | Opt-in. Also sets `enduser.id` on the active span |
| `setTag(k, v)`, `setTags({})`, `setContext(name, obj)` | |
| `withScope(fn)` | Run `fn` with a copy of the current scope |
| `track(name, props?, { userId? })` | Server-side product event |
| `getFlags(ctx?)`, `getFlag(key, fallback?, ctx?)`, `isEnabled(key, ctx?)` | Evaluate flags (see below) |
| `traceId()` | Active trace id |
| `flush(ms?)`, `close(ms?)` | Send queued data; `close` also shuts OpenTelemetry down |
| `requestHandler()`, `errorHandler()`, `fastifyPlugin`, `wrapHandler(fn)` | Middleware |
| `vakaSpanExporter()`, `vakaLogRecordProcessor()`, `VakaScopeSpanProcessor` | For your own NodeSDK |

### Scopes

User, tags, contexts, breadcrumbs and flags live on a scope. Outside a request it is a global
scope; the middleware (and `withScope`/`wrapHandler`) gives every request its own copy via
`AsyncLocalStorage`, so breadcrumbs from one request never show up on another request's error.

### Errors

Error items use `platform: "node"`, frames crash site first with `module` (`express:lib/router`
for packages, the relative path for app files), `in_app: false` for `node_modules` and Node
internals, and `context_line`/`pre_context`/`post_context` read from disk. They carry
`contexts.runtime`, `contexts.os`, `contexts.request` (method, URL without query, route, query
string and a few safe headers; never cookies or `Authorization`), the `server_name` and
`transaction` tags (`GET /users/:id`), `trace_id`, user and flags.

When the SDK reports an error inside a span it sets `vaka.error_captured: true` on that span, so
the server doesn't also turn the span's exception event into a second error.

Uncaught exceptions are sent with `fatal: true`, then the SDK prints the stack, waits up to
`shutdownTimeoutMs` for delivery and exits with code 1, the same outcome as without Vaka.

### Feature flags

```ts
app.get('/checkout', async (req, res) => {
  Vaka.setUser(req.user.id);
  const variant = await Vaka.getFlag('new_checkout', 'control', { properties: { plan: req.user.plan } });
  // ...
});
```

Evaluation uses `POST /ingest/v1/flags` with the scope's user id as `distinct_id` (or pass
`distinctId`). The values are added to the current scope, so errors and events in that request
carry them as exposures, and the active span gets `feature_flag.<key>` attributes. On failure
`getFlag` returns the fallback. See [flags](flags.md).

## User feedback

Forward what users wrote in your own forms (support, contact, in-app chat). It is sent right away
with the scope's user, tags and flags.

```ts
app.post('/support', async (req, res) => {
  const r = await Vaka.captureFeedback({
    message: req.body.message,
    email: req.body.email,           // optional contact
    issue: req.body.issue,           // optional: 'API-12', or eventId from captureException
    ip: req.ip,
    attachments: req.file ? [{ filename: req.file.originalname, data: req.file.buffer }] : [],
  });
  res.json({ ticket: r.shortId });   // 'API-F12'
});
```

Fields: `message` (required), `email`, `name`, `eventId`, `issue`, `userId`, `sessionId`, `url`
(defaults to the request URL), `ip`, `tags`, `attachments` (up to 3 PNG, JPEG or WebP images of
5 MB). It rejects with a `FeedbackError` whose `status` is the HTTP status (429: rate limited).

Link it to a crash with the event id from a capture call, or to an issue by its short id (`API-12`). Pass the end user's IP address: feedback is rate limited per address (10 an hour by default), so without it all feedback from your server shares one limit. Vaka believes the address only from backends on a private network or listed in `VAKA_TRUSTED_PROXIES`. Rejected feedback raises an error with the HTTP status; 429 means a limit was hit. See [user feedback](feedback.md).

## Frameworks

**Express**: `app.use(Vaka.requestHandler())` before routes, `app.use(Vaka.errorHandler())` after.
**Fastify**: `await app.register(Vaka.fastifyPlugin)`. **Koa, Hono, plain `http`, Lambda**: wrap the
handler with `Vaka.wrapHandler(fn)` and call `captureException` in your error middleware. For
spans with route names, add `@opentelemetry/auto-instrumentations-node` (or the single
instrumentation for your framework) to `instrumentations`.

## Bring your own OpenTelemetry

```ts
import { NodeSDK } from '@opentelemetry/sdk-node';
import { BatchSpanProcessor } from '@opentelemetry/sdk-trace';
import * as Vaka from '@hellsoft/vaka-node';

const cfg = { endpoint: 'https://vaka.example.com', key: 'vk_...' };
new NodeSDK({
  spanProcessors: [new Vaka.VakaScopeSpanProcessor(), new BatchSpanProcessor({ exporter: Vaka.vakaSpanExporter(cfg) })],
  logRecordProcessors: [Vaka.vakaLogRecordProcessor(cfg)],
}).start();
Vaka.init({ ...cfg, otel: false, release: '2.4.0' });
```

Or skip the SDK's OpenTelemetry entirely and point standard `OTEL_EXPORTER_OTLP_*` variables at
Vaka ([opentelemetry.md](opentelemetry.md)); `@hellsoft/vaka-node` then only adds error capture.

## ES modules

The preload (`--import @hellsoft/vaka-node/register`) also registers OpenTelemetry's ESM loader hook, so
`import express from 'express'` is instrumented. With an `instrument.mjs` of your own, load it
with `--import` and register the hook yourself if you need ESM instrumentation:
`module.register('@opentelemetry/instrumentation/hook.mjs', import.meta.url)`.

## Source maps

Use `node --enable-source-maps`: stacks are already mapped to the original files, and source
context comes from those files.
