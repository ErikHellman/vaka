# Install Vaka in a web app (JavaScript)

Project: `{{PROJECT}}` · Endpoint: `{{ENDPOINT}}` · Ingest key: `{{KEY}}`

These steps are written to be followed by a coding agent. Do them in order and verify at the end.

## 1. Pick an install method

**Option A: script tag** (static sites, server-rendered apps, anything without a bundler).
Add this to every page's `<head>`:

```html
<script defer src="{{ENDPOINT}}/sdk/vaka.js"
        data-key="{{KEY}}"
        data-release="REPLACE_WITH_APP_VERSION"></script>
```

Optional attributes: `data-environment="production"`, `data-hash="true"` (hash routing),
`data-pageviews="false"`, `data-errors="false"`, `data-ignore-localhost="true"`, `data-debug="true"`.
The global `window.Vaka` exposes the same API as the npm package.

**Option B: npm package** ([`@hellsoft/vaka-browser`](https://www.npmjs.com/package/@hellsoft/vaka-browser), for apps built
with Vite, webpack, Next.js, etc.).

```sh
npm install @hellsoft/vaka-browser
```

Initialise once, as early as possible in the client entry point (e.g. `main.ts`):

```ts
import * as Vaka from '@hellsoft/vaka-browser';

Vaka.init({
  key: '{{KEY}}',
  endpoint: '{{ENDPOINT}}',
  release: import.meta.env.VITE_APP_VERSION, // any string; must match the source map upload
  environment: import.meta.env.MODE,
});
```

For server-side rendering frameworks, call `init` only in the browser (`typeof window !== 'undefined'`).

## 2. What you get automatically

- Uncaught errors and unhandled promise rejections, with breadcrumbs (clicks, navigation,
  console warnings/errors, failed `fetch`).
- Cookieless pageviews, including SPA navigation (`history.pushState`/`popstate`).
- A session per page load, so crash-free rates per release work.

## 3. Optional calls

```ts
Vaka.track('signup', { plan: 'pro' });     // custom event (props: string/number/boolean)
Vaka.captureError(err, { tags: { area: 'checkout' } });  // handled error
Vaka.captureMessage('Payment provider slow', 'warning');
Vaka.addBreadcrumb({ category: 'cart', message: 'Added item 42' });
Vaka.setTag('tenant', 'acme');
Vaka.identify('user-123');                 // opt-in only; Vaka.identify(null) clears it
```

Do not call `identify` with emails or names unless the app's privacy policy allows it.

Performance tracing and feature flags are off by default. Turn them on in `init` (or with
`data-traces-sample-rate="0.1"` and `data-flags="true"` on the script tag):

```ts
Vaka.init({
  // ...key, endpoint, release as above
  tracesSampleRate: 0.1,   // trace 10% of page loads: page load, fetch spans and Web Vitals
  flags: true,             // load feature flags on init; values are attached to every item
});
if (Vaka.isEnabled('new_checkout')) { /* ... */ }
```

See [performance]({{ENDPOINT}}/docs/performance.md) and [feature flags]({{ENDPOINT}}/docs/flags.md).

## 4. Upload source maps (production builds)

With Vite, Rollup or webpack, use the bundler plugin: it uploads the maps after each production
build and defines `__VAKA_RELEASE__` for app code. It needs an API token with write access.
The package is [`@hellsoft/vaka-bundler-plugin`](https://www.npmjs.com/package/@hellsoft/vaka-bundler-plugin).

```sh
npm install --save-dev @hellsoft/vaka-bundler-plugin
```

```js
// vite.config.js
import { vakaVite } from '@hellsoft/vaka-bundler-plugin';
export default {
  build: { sourcemap: 'hidden' },               // maps without a sourceMappingURL comment
  plugins: [vakaVite({ deleteAfterUpload: true })],
};

// webpack.config.js
const { VakaWebpackPlugin } = require('@hellsoft/vaka-bundler-plugin');
module.exports = { devtool: 'hidden-source-map', plugins: [new VakaWebpackPlugin({ deleteAfterUpload: true })] };
```

Then pass `release: __VAKA_RELEASE__` to `init` and build with
`VAKA_URL={{ENDPOINT}} VAKA_TOKEN=vt_... VAKA_PROJECT={{PROJECT}} VAKA_RELEASE="$APP_VERSION"` set
(or pass `url`, `token`, `project`, `release` as plugin options). Without a token the plugin
skips the upload with a warning, so local builds keep working; on CI (`CI` set) a failed upload
fails the build.

Other bundlers: generate source maps (Vite: `build.sourcemap: true`; webpack: `devtool: 'source-map'`)
and upload them after each build with the `vaka` CLI:

```sh
export VAKA_URL={{ENDPOINT}} VAKA_TOKEN=vt_... VAKA_PROJECT={{PROJECT}}
vaka upload sourcemaps dist --release "$APP_VERSION" --url-prefix /assets
```

`--url-prefix` is the URL path the minified files are served from. `--release` must equal the
`release` passed to `init`. You may delete the `.map` files from the deployed output afterwards.

## 5. Verify

1. Load the app in a browser and run `Vaka.captureMessage('vaka install test')` in the console
   (script tag) or trigger a test error.
2. Check with the API: `curl -H "Authorization: Bearer $VAKA_TOKEN" {{ENDPOINT}}/api/v1/projects/{{PROJECT}}/issues`,
   or the MCP tool `list_issues`. The event appears within a few seconds.
3. Pageviews appear in `analytics_summary` / `GET /api/v1/projects/{{PROJECT}}/analytics/summary`.

If nothing arrives: enable `debug: true`, check the browser network tab for
`/ingest/v1/batch` (expect 202), and make sure a Content-Security-Policy allows
`connect-src {{ENDPOINT}}` (and `script-src` for option A).
