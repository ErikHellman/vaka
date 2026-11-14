# @hellsoft/vaka-bundler-plugin

[![npm](https://img.shields.io/npm/v/@hellsoft/vaka-bundler-plugin)](https://www.npmjs.com/package/@hellsoft/vaka-bundler-plugin)

Uploads JavaScript source maps to [Vaka](https://vaka.hellsoft.se) after Vite, Rollup and webpack
builds, so stack traces resolve to original files and lines. No dependencies, Node 18+.

```sh
npm install --save-dev @hellsoft/vaka-bundler-plugin
```

```js
// vite.config.js
import { vakaVite } from '@hellsoft/vaka-bundler-plugin';
export default { build: { sourcemap: 'hidden' }, plugins: [vakaVite({ deleteAfterUpload: true })] };

// webpack.config.js
const { VakaWebpackPlugin } = require('@hellsoft/vaka-bundler-plugin');
module.exports = { devtool: 'hidden-source-map', plugins: [new VakaWebpackPlugin({ deleteAfterUpload: true })] };
```

Configure with options or environment variables: `url` (`VAKA_URL`), `token` (`VAKA_TOKEN`, write
scope), `project` (`VAKA_PROJECT`), `release` (`VAKA_RELEASE`, must match the SDK's `release`).
App code gets the release as `__VAKA_RELEASE__`.

| Option | Default | |
| --- | --- | --- |
| `urlPrefix` | Vite `base` / webpack `output.publicPath`, else `/` | URL path the files are served from |
| `deleteAfterUpload` | `false` | Remove `.map` files from the output after upload |
| `failOnError` | `true` when `CI` is set | Fail the build on a missing config or failed upload |
| `dryRun` | `false` | Print what would be uploaded |
| `defineRelease` | `true` | Define `__VAKA_RELEASE__` |

`uploadSourcemaps(dir, options)` uploads every `*.map` under a directory for other build tools.
