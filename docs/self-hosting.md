# Self-hosting

Vaka is one Rust binary (`vaka-server`) plus ClickHouse. Metadata (users, projects, issues,
tokens, alert rules) lives in SQLite or Postgres. One instance comfortably serves 20 to 30 apps
and websites on a 2 vCPU / 4 GB machine.

## docker compose

```sh
cp .env.example .env
docker compose up -d                       # SQLite metadata (default)
docker compose --profile postgres up -d    # with Postgres; also set VAKA_DB_URL
```

The compose file runs `ghcr.io/erikhellman/vaka:latest` and `clickhouse/clickhouse-server:24.8`
with a low-memory ClickHouse profile. Data is kept in the `vaka-data`, `clickhouse-data` and
`postgres-data` volumes.

Put a TLS-terminating reverse proxy (Caddy, nginx, Traefik, Cloudflare) in front of port 8080
and set `VAKA_PUBLIC_URL` to the public HTTPS URL. Example Caddyfile:

```
vaka.example.com {
    reverse_proxy localhost:8080
}
```

## Fly.io

The repository ships a ready Fly.io setup: `fly.toml` for the server (app `vaka`) and
`deploy/fly/clickhouse/` for a private ClickHouse app (`vaka-clickhouse`, no public ports,
reached at `vaka-clickhouse.internal:8123`). Metadata stays in SQLite on a volume.
Change the app names, region and `VAKA_PUBLIC_URL` in both files for your own deployment.

```sh
fly apps create vaka && fly apps create vaka-clickhouse
fly volumes create vaka_data -a vaka -r arn -s 3
fly volumes create clickhouse_data -a vaka-clickhouse -r arn -s 10
PW=$(openssl rand -hex 24)
fly secrets set -a vaka-clickhouse --stage CLICKHOUSE_PASSWORD=$PW
fly secrets set -a vaka --stage VAKA_CLICKHOUSE_PASSWORD=$PW \
  VAKA_ADMIN_EMAIL=you@example.com VAKA_ADMIN_PASSWORD=...
fly deploy -c deploy/fly/clickhouse/fly.toml --ha=false
fly deploy --ha=false
```

Both apps run a single machine each (volumes are per machine); use `--ha=false` so Fly
does not create a second one. Volume snapshots are taken daily by Fly.

## Configuration

All settings are environment variables.

| Variable | Default | Meaning |
| --- | --- | --- |
| `VAKA_PUBLIC_URL` | `http://localhost:8080` | Public URL, used in links, alerts and install guides |
| `VAKA_LISTEN` | `0.0.0.0:8080` | Listen address |
| `VAKA_ADMIN_EMAIL`, `VAKA_ADMIN_PASSWORD` | | First owner account, created on start when no users exist. With email configured the password is optional |
| `VAKA_DB_URL` | `sqlite://$VAKA_DATA_DIR/vaka.db?mode=rwc` | `sqlite://...` or `postgres://user:pass@host/db` |
| `VAKA_DATA_DIR` | `./data` (`/data` in Docker) | SQLite database and uploaded debug files |
| `VAKA_CLICKHOUSE_URL` | `http://localhost:8123` | ClickHouse HTTP interface |
| `VAKA_CLICKHOUSE_USER` / `_PASSWORD` / `_DATABASE` | `default` / empty / `vaka` | ClickHouse credentials |
| `VAKA_DEFAULT_RETENTION_DAYS` | `90` | Retention for new projects |
| `VAKA_RESEND_API_KEY` | | Enables email alerts, invite emails and email-code login via Resend |
| `VAKA_LLM_BASE_URL` | | Optional: OpenAI-compatible API (e.g. `https://api.openai.com/v1`) that writes the opening paragraph of digests ([anomalies.md](anomalies.md#the-narrative)) |
| `VAKA_LLM_MODEL` | | Model for digest narratives (needed with `VAKA_LLM_BASE_URL`) |
| `VAKA_LLM_API_KEY` | | Bearer key for that API (optional for local servers) |
| `VAKA_EMAIL_FROM` | `Vaka <vaka@localhost>` | Sender for emails |
| `VAKA_PASSWORD_LOGIN` | `true` | Set to `false` to allow only email-code login (needs `VAKA_RESEND_API_KEY`) |
| `VAKA_GEOIP_DB` | | Optional MaxMind/DB-IP country `.mmdb` file |
| `VAKA_S3_BUCKET` | | Store debug files in an S3-compatible bucket (see below) |
| `VAKA_S3_ENDPOINT` | `https://s3.$REGION.amazonaws.com` | Endpoint for R2, MinIO, Garage, ... |
| `VAKA_S3_REGION` | `us-east-1` | Signing region (`auto` for Cloudflare R2) |
| `VAKA_S3_ACCESS_KEY_ID` / `VAKA_S3_SECRET_ACCESS_KEY` | `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` | Credentials |
| `VAKA_S3_PREFIX` | `vaka/` | Key prefix inside the bucket |
| `VAKA_S3_PATH_STYLE` | `true` with a custom endpoint, else `false` | Path-style (`endpoint/bucket/key`) or virtual-hosted URLs |

### Admin login

The admin UI always supports email and password. When `VAKA_RESEND_API_KEY` is set it also
offers **Email me a sign-in code**: a 6-digit one-time code, valid for 10 minutes, with 5 tries,
and at most one new code per minute. Set `VAKA_PASSWORD_LOGIN=false` to turn password login
off and use codes only. Without email configured that setting is ignored, so you can't lock
yourself out.

Country lookup uses `CF-IPCountry` (Cloudflare) or similar proxy headers when present, otherwise
the GeoIP database if configured.

Vaka creates the ClickHouse database and tables and runs metadata migrations on start.

### S3 storage for debug files

By default source maps, mappings and debug symbols live in `VAKA_DATA_DIR/artifacts`. With
`VAKA_S3_BUCKET` set, every upload is also written to the bucket, and the local directory becomes a
cache: files missing locally are downloaded on first use. That lets the server run on an ephemeral
disk (Fly.io machines, Kubernetes pods) or as several instances. On start the server checks the
bucket (creating it if it does not exist and the credentials allow it) and copies local files that
are not in it yet, so switching an existing install over needs no manual step.

```sh
# Cloudflare R2
VAKA_S3_BUCKET=vaka-debug-files
VAKA_S3_ENDPOINT=https://<account-id>.r2.cloudflarestorage.com
VAKA_S3_REGION=auto
VAKA_S3_ACCESS_KEY_ID=...  VAKA_S3_SECRET_ACCESS_KEY=...
```

## Running without Docker

```sh
cargo build --release -p vaka-server -p vaka
VAKA_CLICKHOUSE_URL=http://localhost:8123 ./target/release/vaka-server
```

## Backups

- Metadata: back up the SQLite file in `VAKA_DATA_DIR` (`sqlite3 vaka.db ".backup b.db"`) or
  `pg_dump` the Postgres database.
- Debug files: back up `VAKA_DATA_DIR/artifacts`, or rely on the bucket when S3 storage is on.
- Events: ClickHouse `BACKUP DATABASE vaka TO Disk(...)`, or accept losing raw events since
  they expire with retention anyway.

## Upgrades

Pull the new image and restart: `docker compose pull && docker compose up -d`. Migrations run
automatically. Read the release notes for anything that needs manual steps.

## Health and monitoring

`GET /health` returns `{"ok": true, "db": true, "clickhouse": true, "version": "..."}` and
503 when a dependency is down. Logs go to stdout; set `VAKA_LOG=debug` (tracing filter syntax) for more.
