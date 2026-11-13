# JavaScript browser SDK

Package `vaka-browser` (source: `sdks/js`). About 3 KB gzipped, no dependencies.
Also served by every Vaka instance at `{{ENDPOINT}}/sdk/vaka.js`.

Agent-followable install guide: [`/docs/install/javascript`]({{ENDPOINT}}/docs/install/javascript)
(or `GET /api/v1/projects/{p}/install/javascript` for one with the project's key filled in).

## Init options

| Option | Default | Meaning |
| --- | --- | --- |
| `key` | required | Project ingest key (`vk_...`) |
| `endpoint` | required (script tag: script origin) | Vaka server URL |
| `release` | | App version; must match the source map upload |
| `environment` | `production` | |
| `autoPageviews` | `true` | Pageview on load and on SPA navigation |
| `hashRouting` | `false` | Treat hash changes as navigation |
| `captureErrors` | `true` | Uncaught errors and unhandled rejections |
| `breadcrumbs` | `true` | Clicks, navigation, console warnings/errors, and fetch requests (`http` with method, URL, status and duration) |
| `sessions` | `true` | A per-tab session id (in `sessionStorage`, rotated after 30 idle minutes) on every item, for session timelines. No cookies, nothing shared across tabs or sites |
| `tracesSampleRate` | `0` | Fraction of page loads to trace: a `pageload` span, an `http.client` span per fetch and a `web.vitals` span (LCP, INP, CLS, FCP, TTFB) when the page is hidden. Errors on traced pages carry the trace id |
| `tracePropagationTargets` | same origin | URL prefixes (or RegExps) whose fetches get a W3C `traceparent` header so your backend can continue the trace |
| `errorSampleRate` | `1` | Fraction of errors to send |
| `ignoreLocalhost` | `false` | Send nothing from localhost |
| `beforeSend` | | `(item) => item \| null` to edit or drop data |
| `debug` | `false` | Log to the console |

Script tag attributes: `data-key`, `data-endpoint`, `data-release`, `data-environment`,
`data-pageviews="false"`, `data-hash="true"`, `data-errors="false"`,
`data-ignore-localhost="true"`, `data-traces-sample-rate="0.1"`, `data-debug="true"`.

## API

```ts
init(options)
pageview(url?, props?)                  // manual pageview (when autoPageviews is false)
track(name, props?)                     // custom event
captureError(error, { level?, tags?, extra?, handled?, fingerprint? }) // returns event id
captureMessage(message, level?)
addBreadcrumb({ category?, message?, level?, data? })
setTag(key, value)
identify(userId | null)                 // opt-in
flush()                                 // send queued items now
const span = startSpan({ name, op?, attributes?, root? }) // time an operation
span.setAttribute(k, v); span.setMeasurement(k, n); span.end(status?)
traceId()                               // current trace id on traced pages
```

Data is batched and sent every few seconds, and with `navigator.sendBeacon` when the page is hidden.

## Source maps

Build with source maps and upload them per release with the `vaka-bundler-plugin` package
(`vakaVite()` for Vite and Rollup, `new VakaWebpackPlugin()` for webpack) or with
`vaka upload sourcemaps` (see [CLI](cli.md)). The plugin uses the bundler's `base` /
`output.publicPath` as the URL prefix, so names match what browsers load.
Frames are resolved to original files, lines and function names server-side, including
source context lines when the map contains `sourcesContent`.

## Frameworks

- **React**: errors in render are caught by React; report them from an error boundary's
  `componentDidCatch(error)` with `captureError(error)`.
- **Vue**: `app.config.errorHandler = (err) => Vaka.captureError(err)`.
- **Next.js / SSR**: call `init` in a client component or `instrumentation-client.ts`.
