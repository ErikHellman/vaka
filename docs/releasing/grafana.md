# Releasing the Grafana plugin

The plugin `hellsoft-vaka-datasource` lives in `integrations/grafana`. A release is a signed zip
attached to a GitHub release tagged `grafana/vX.Y.Z` (not `vX.Y.Z`, which are the server's tags).
The Grafana plugin catalog installs from that zip after Grafana has reviewed it.

## One-time setup

1. **grafana.com org.** Sign in at grafana.com and create (or use) the org with the slug
   `hellsoft`. The plugin id prefix must match it; changing the slug means changing the plugin id.
2. **Access policy token.** grafana.com → My Account → Security → Access Policies: create a policy
   for the `hellsoft` org with the scope `plugins:write`, then a token for it. Save it as the
   repository secret `GRAFANA_ACCESS_POLICY_TOKEN` (Settings → Secrets and variables → Actions).
3. **Private signature (optional, before catalog approval).** To load the plugin on your own
   Grafana without `allow_loading_unsigned_plugins`, set the repository variable
   `GRAFANA_SIGN_ROOT_URLS` to that Grafana's root URL(s), comma-separated, e.g.
   `https://grafana.example.com/`. The release is then signed *private* for those URLs only.
   Remove the variable for catalog releases: the catalog signs community plugins itself.

## Steps

1. Bump `version` in `integrations/grafana/package.json` (and `npm install --package-lock-only`),
   add a `## X.Y.Z` section to `integrations/grafana/CHANGELOG.md`. Merge to `main`.
2. Run **Release Grafana plugin** (Actions → Release Grafana plugin → Run workflow) on `main` with
   the version. It checks the version and changelog, runs the tests, builds the frontend and the
   backend for Linux, macOS and Windows (amd64 and arm64), signs (when the token is set), zips,
   runs Grafana's plugin validator, tags `grafana/vX.Y.Z` and creates the GitHub release with the
   zip and its SHA1/MD5.
3. **Catalog submission** (first release, and every release after): grafana.com → My Plugins →
   Submit a new plugin (later: Submit update). Give the zip URL from the release
   (`https://github.com/ErikHellman/vaka/releases/download/grafana/vX.Y.Z/hellsoft-vaka-datasource-X.Y.Z.zip`),
   its SHA1, and the source URL `https://github.com/ErikHellman/vaka/tree/grafana/vX.Y.Z/integrations/grafana`.
   Reviewers may ask for a way to test: point them at `integrations/grafana/tests/mock-vaka.mjs`
   and the docker compose setup in the plugin README, or give them a read token for a demo project.
4. Once approved, Grafana signs it as a *community* plugin and it appears in the catalog
   (`grafana cli plugins install hellsoft-vaka-datasource`).

## Checking a release locally

```sh
cd integrations/grafana
npm ci && npm run build && mage -v buildAll
cp -r dist hellsoft-vaka-datasource && zip -qr plugin.zip hellsoft-vaka-datasource && rm -r hellsoft-vaka-datasource
docker run --pull=always -v $PWD/plugin.zip:/archive.zip grafana/plugin-validator-cli /archive.zip
```
