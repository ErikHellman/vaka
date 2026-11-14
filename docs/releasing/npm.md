# Releasing the npm packages

Three packages, published from their own folders:

| Package | Folder | Build |
| --- | --- | --- |
| `@hellsoft/vaka-browser` | `sdks/js` | `npm ci && npm test` (builds `dist/`) |
| `@hellsoft/vaka-bundler-plugin` | `sdks/js-bundler` | none (plain JS), `npm test` |
| `@hellsoft/vaka-node` | `sdks/node` | `npm ci && npm test` (builds `dist/`) |

All are `MIT OR Apache-2.0` and public, under the `@hellsoft` scope. `files` in each `package.json` limits what is
published; `npm pack --dry-run` shows the exact list.

## One-time setup

1. Create an npm account with two-factor authentication, and own the `@hellsoft` scope: either
   your npm username is `hellsoft`, or create a free organization named `hellsoft` on npmjs.com
   (Add Organization) and be a member with publish rights.
   `npm view @hellsoft/vaka-browser` should answer 404 before the first publish.
2. First publish from a laptop (trusted publishing can only be set up for a package that exists):
   `npm login`, then follow "Publish by hand" below.
3. For later releases, set up **trusted publishing** for each package on npmjs.com
   (package → Settings → Trusted publisher → GitHub Actions): repository `ErikHellman/vaka`,
   workflow `publish-npm.yml`. The workflow then publishes with OpenID Connect and provenance,
   with no token stored anywhere. (Alternative: an npm automation token as the `NPM_TOKEN`
   repository secret; the workflow uses it when present.)

## Publish with the workflow

After the release tag exists:

1. Actions → **Publish npm packages** → Run workflow.
2. `ref`: the tag, e.g. `v0.3.0`. `packages`: `all`, or one of `@hellsoft/vaka-browser`,
   `@hellsoft/vaka-bundler-plugin`, `@hellsoft/vaka-node`. `dry_run`: try `true` first.
3. The job runs each package's tests, checks that `package.json` has the tag's version, and runs
   `npm publish --access public --provenance`. Versions already on npm are skipped, so re-running
   after a partial failure is safe.

## Publish by hand

```sh
git checkout v0.3.0
for d in sdks/js sdks/js-bundler sdks/node; do
  (cd "$d" && { [ -f package-lock.json ] && npm ci || true; } && npm test && npm publish --access public)
done
```

`npm publish` runs from the folder, so `dist/` must be freshly built (the `test` script builds
it). Check the result with `npm view @hellsoft/vaka-node version` and a clean install:

```sh
mkdir /tmp/try && cd /tmp/try && npm init -y >/dev/null && npm install @hellsoft/vaka-node @hellsoft/vaka-browser @hellsoft/vaka-bundler-plugin
node -e "require('@hellsoft/vaka-node'); import('@hellsoft/vaka-browser').then(() => console.log('ok'))"
```

## Pre-releases and mistakes

- Pre-release: version `0.3.0-rc.1` and `npm publish --tag next` (so `latest` doesn't move).
- A bad release: `npm deprecate @hellsoft/vaka-node@0.3.0 "use 0.3.1"` and publish a fix.
  `npm unpublish` only works within 72 hours and blocks the version number forever; prefer
  deprecating.

## After the first publish

Update `docs/install/javascript.md`, `sdks/js-bundler/README.md` and the release notes that
say the packages are not on npm yet.
