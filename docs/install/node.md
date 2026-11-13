# Install Vaka in a Node.js service

Project: `{{PROJECT}}` · Endpoint: `{{ENDPOINT}}` · Ingest key: `{{KEY}}`

These steps are written to be followed by a coding agent. Do them in order and verify at the end.
Requires Node.js 22.15 or later.

## 1. Install

```sh
npm install vaka-node
```

Optional, for Express, Fastify, Koa, databases, Redis, queues and more in your traces:

```sh
npm install @opentelemetry/auto-instrumentations-node
```

## 2. Initialize before the app loads

OpenTelemetry can only instrument modules loaded after it starts, so Vaka must start first.
Pick one:

**Option A: preload (no code change).** Set the environment and add a flag to the start command:

```sh
export VAKA_KEY={{KEY}}
export VAKA_ENDPOINT={{ENDPOINT}}
export VAKA_RELEASE=REPLACE_WITH_APP_VERSION     # e.g. the git sha or package version
export VAKA_ENVIRONMENT=production
export OTEL_SERVICE_NAME=REPLACE_WITH_SERVICE_NAME
node --import vaka-node/register dist/server.js    # CommonJS apps: --require vaka-node/register
```

In `package.json` scripts this is `"start": "node --import vaka-node/register dist/server.js"`.
Release defaults to `npm_package_version` when started through npm.

**Option B: an `instrument` file.** Create `instrument.mjs` (or `.ts`) and load it with
`--import ./instrument.mjs`, or `require('./instrument')` as the very first line of a CommonJS
entry point:

```ts
import * as Vaka from 'vaka-node';
import { getNodeAutoInstrumentations } from '@opentelemetry/auto-instrumentations-node'; // optional

Vaka.init({
  key: '{{KEY}}',
  endpoint: '{{ENDPOINT}}',
  release: process.env.APP_VERSION,     // must match the release used for source map uploads
  environment: process.env.NODE_ENV,
  serviceName: 'api',
  instrumentations: [getNodeAutoInstrumentations()],   // omit for HTTP only
});
```

If the app already runs its own OpenTelemetry `NodeSDK`, pass `otel: false` and add the Vaka
exporters to it instead (see [sdk-node]({{ENDPOINT}}/docs/sdk-node.md#bring-your-own-opentelemetry)).

## 3. What you get automatically

- Uncaught exceptions and unhandled promise rejections, with stack traces, source lines and
  breadcrumbs (console warnings and errors, outgoing `fetch` and `http` requests). The process
  still exits with code 1 afterwards, as Node does by default.
- Traces for incoming and outgoing HTTP requests (OTLP), with release and environment, and
  OpenTelemetry logs.
- Errors carry the trace id, so `get_trace` shows what the request was doing.

## 4. Add the framework middleware

Express (after `Vaka.init`):

```ts
app.use(Vaka.requestHandler());   // first: isolates breadcrumbs, user and tags per request
// ... routes ...
app.use(Vaka.errorHandler());     // after routes, before your own error handler
```

Fastify:

```ts
await app.register(Vaka.fastifyPlugin);
```

Anything else (plain `http`, serverless): wrap the handler with `Vaka.wrapHandler(fn)`.
Errors with status 500 and above are reported; change it with `shouldCaptureStatus`.

## 5. Optional calls

```ts
Vaka.captureException(err, { tags: { area: 'billing' } });   // handled error
Vaka.captureMessage('Payment provider slow', 'warning');
Vaka.addBreadcrumb({ category: 'job', message: 'import started' });
Vaka.setTag('tenant', 'acme');
Vaka.setUser(req.user.id);                 // opt-in; per request inside the middleware
Vaka.track('order_paid', { amount: 49 });  // server-side product event
if (await Vaka.isEnabled('new_checkout')) { /* ... */ }   // feature flag for the scope's user
```

Call `await Vaka.close()` before a deliberate `process.exit()` so queued data is sent.

## 6. Source maps (TypeScript or bundled code)

Start Node with `--enable-source-maps` so stack traces point at the original `.ts` files; Vaka
then reads source lines from disk where they exist. Nothing to upload.

## 7. Verify

```sh
VAKA_KEY={{KEY}} VAKA_ENDPOINT={{ENDPOINT}} VAKA_RELEASE=test node -e "
const Vaka = require('vaka-node');
Vaka.init({ otel: false });
Vaka.captureMessage('vaka install test', 'info');
Vaka.close().then((ok) => console.log(ok ? 'sent' : 'NOT sent'));"
```

It prints `sent`. Then check `vaka issues -p {{PROJECT}}` (or the MCP tool `list_issues`).
Set `debug: true` (or `VAKA_DEBUG=1` with the preload) to log what the SDK sends.
