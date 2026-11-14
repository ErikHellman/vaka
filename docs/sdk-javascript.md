# JavaScript browser SDK

Package `@hellsoft/vaka-browser` (source: `sdks/js`). About 3 KB gzipped, no dependencies.
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
| `flags` | `false` | Load feature flags on init (and after `identify`); their values are attached to every item |
| `distinctId` | session id | Stable id for flag bucketing before `identify()` |
| `errorSampleRate` | `1` | Fraction of errors to send |
| `ignoreLocalhost` | `false` | Send nothing from localhost |
| `beforeSend` | | `(item) => item \| null` to edit or drop data |
| `debug` | `false` | Log to the console |

Script tag attributes: `data-key`, `data-endpoint`, `data-release`, `data-environment`,
`data-pageviews="false"`, `data-hash="true"`, `data-errors="false"`,
`data-ignore-localhost="true"`, `data-traces-sample-rate="0.1"`, `data-flags="true"`, `data-debug="true"`.

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
await loadFlags(props?)                 // (re)load flags; with `flags: true` this runs on init
getFlag('pricing', 'control')           // true/false or the variant key
isEnabled('new_checkout')
onFlags((flags) => ...)
await captureFeedback({ message, email?, name?, eventId?, tags?, attachments? }) // { id, short_id }
lastEventId()                           // id of the last error sent, for feedback about it
onUnhandledError((eventId) => ...)      // after an uncaught error is captured; returns unsubscribe
```

Data is batched and sent every few seconds, and with `navigator.sendBeacon` when the page is hidden.

## User feedback

The widget is a separate module (about 5 KB gzipped), so apps that don't use it don't load it:

```ts
import { installFeedback, showFeedback } from '@hellsoft/vaka-browser/feedback';

installFeedback({ crashPrompt: true });
showFeedback();                       // open the dialog from your own link
showFeedback({ eventId, crash: true }); // ask about a specific error
```

| Option | Default | Meaning |
| --- | --- | --- |
| `button` | `true` | Floating feedback button |
| `position` | `bottom-right` | Or `bottom-left` |
| `crashPrompt` | `false` | Ask "what happened?" after an uncaught error, at most once a minute; the answer is linked to that crash |
| `screenshot` | `true` | Offer a screenshot of the page (the user can remove it) |
| `askEmail` | `true` | Show the optional email field |
| `email` | | Prefill the email, e.g. for signed-in users |
| `accent` | `#2563eb` | Button and highlight color |
| `zIndex` | | Stacking order of the widget |
| `strings` | English | Override any text: `button`, `title`, `crashTitle`, `crashIntro`, `placeholder`, `emailLabel`, `screenshotLabel`, `submit`, `cancel`, `sending`, `thanks`, `error` |
| `onSubmit` | | `(result) => ...` after sending |

Script tag: `<script defer src="{{ENDPOINT}}/sdk/vaka-feedback.js" data-crash-prompt="true"></script>`
after `vaka.js`, with `data-button`, `data-position`, `data-crash-prompt`, `data-screenshot`,
`data-ask-email`, `data-accent` and `data-auto="false"`. Add `data-vaka-mask` to elements that
must not appear in screenshots. See [feedback](feedback.md).

## Source maps

Build with source maps and upload them per release with the `@hellsoft/vaka-bundler-plugin` package
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
