# CLI

`vaka` uploads debug files from build pipelines and gives a quick terminal view of issues.

## Install

```sh
cargo install --git https://github.com/ErikHellman/vaka vaka
```

Prebuilt binaries for Linux, macOS and Windows are attached to each GitHub release, and the
Docker image contains `vaka` too (`docker compose exec vaka vaka --help`).

## Configuration

| Flag | Env var | Meaning |
| --- | --- | --- |
| `--url` | `VAKA_URL` | Server URL (default `http://localhost:8080`) |
| `--token` | `VAKA_TOKEN` | API token (`vt_...`); uploads need write scope |
| `-p, --project` | `VAKA_PROJECT` | Project slug or id |
| `--key` | `VAKA_KEY` | Ingest key, only for `send-test-event` |
| `--json` | | Print raw JSON responses |

## Upload debug files

```sh
# JavaScript: every *.map under dist, served from /assets
vaka upload sourcemaps dist --release 1.4.0 --url-prefix /assets

# Android: R8/ProGuard mapping
vaka upload proguard app/build/outputs/mapping/release/mapping.txt --release 2.3.1

# iOS/macOS: dSYM bundles, or a folder containing them (matched by UUID, no release needed)
vaka upload dsym build/MyApp.xcarchive/dSYMs

# Native (Rust, C/C++): ELF with debug info, Mach-O, PDB (matched by build id / debug id)
vaka upload debug-files target/release/myapp
```

`--release` must equal the `release` the SDK reports. Source maps are matched by the URL path of
the minified file, so `--url-prefix` must be the path the files are served from (for example
`/assets` when `dist/app.js` is served at `https://example.com/assets/app.js`).

## Record release commits

```sh
# In CI, after tagging: commits since the previous tag (or the last 100), with changed files
vaka releases set-commits --release 1.4.0
vaka releases set-commits --release 1.4.0 --range v1.3.0..v1.4.0 --repo-url https://github.com/acme/shop
vaka releases commits --release 1.4.0
```

Issues first seen in that release then list `suspect_commits`: commits that changed files in the
stack trace. The checkout must have enough history for the range (`fetch-depth: 0` in GitHub Actions).

## Ownership rules

```sh
vaka ownership set .vaka/OWNERS   # replace the project's rules with a file from the repo
vaka ownership show
```

See [REST API: Ownership](api.md#ownership) for the rule format.

## Query

```sh
vaka projects
vaka issues -p web --since 7d --sort events     # --status open|resolved|ignored|all
vaka issue WEB-12                                # latest event with stack trace
vaka resolve WEB-12 --release 1.4.1
vaka health -p web --release 1.4.0              # crash-free sessions and users
```

## Verify an installation

```sh
VAKA_URL=https://vaka.example.com VAKA_KEY=vk_... vaka send-test-event
```

Sends a test error with the ingest key and prints the server's response.
