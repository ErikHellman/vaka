# Self-hosting

Vaka is one Rust binary (`vaka-server`) plus ClickHouse. Metadata (users, projects, issues,
tokens, alert rules) lives in SQLite or Postgres. One instance comfortably serves 20 to 30 apps
and websites on a 2 vCPU / 4 GB machine.

## Docker Compose

The repository's `docker-compose.yml` runs the server (`ghcr.io/erikhellman/vaka`) and ClickHouse
(`clickhouse/clickhouse-server:24.8` with a low-memory profile). Everything is configured in `.env`,
so the compose file itself never needs editing:

```sh
git clone https://github.com/ErikHellman/vaka && cd vaka
cp .env.example .env    # set VAKA_PUBLIC_URL, VAKA_ADMIN_EMAIL, VAKA_ADMIN_PASSWORD, CLICKHOUSE_PASSWORD
docker compose up -d
```

Optional services are turned on with `COMPOSE_PROFILES` in `.env`:

| Setting in `.env` | Effect |
| --- | --- |
| `COMPOSE_PROFILES=caddy` | Adds [Caddy](https://caddyserver.com) on ports 80 and 443 with an automatic Let's Encrypt certificate for `VAKA_DOMAIN` |
| `COMPOSE_PROFILES=postgres` | Adds Postgres for metadata; also set `VAKA_DB_URL=postgres://vaka:<POSTGRES_PASSWORD>@postgres:5432/vaka` |
| `VAKA_VERSION=0.3.0` | Pins the server image (default `latest`); `VAKA_IMAGE` changes the image itself |
| `VAKA_PORT=127.0.0.1:8080` | Host port for plain HTTP; bind it to localhost when a proxy sits in front |
| `CLICKHOUSE_VERSION`, `POSTGRES_PASSWORD` | ClickHouse image tag and Postgres password |

A production server on a single VM with HTTPS, Postgres and a pinned version:

```sh
# .env
VAKA_PUBLIC_URL=https://vaka.example.com
VAKA_ADMIN_EMAIL=you@example.com
VAKA_ADMIN_PASSWORD=...
CLICKHOUSE_PASSWORD=...            # openssl rand -hex 24
COMPOSE_PROFILES=caddy,postgres
VAKA_DOMAIN=vaka.example.com
VAKA_PORT=127.0.0.1:8080
VAKA_VERSION=0.3.0
POSTGRES_PASSWORD=...
VAKA_DB_URL=postgres://vaka:...@postgres:5432/vaka
```

Point the domain's DNS at the host, open ports 80 and 443, and run `docker compose up -d`. Data is
kept in the `vaka-data`, `clickhouse-data`, `postgres-data` and `caddy-data` volumes.

To use your own reverse proxy (nginx, Traefik, Cloudflare Tunnel) instead of Caddy, leave the
profile off and proxy to port 8080. `deploy/docker/Caddyfile` shows the minimal config.

Running ClickHouse somewhere that can't mount config files (Render, Railway, Kubernetes)? Build
`deploy/clickhouse/Dockerfile`, which bakes the low-memory profile into the image.

## Fly.io

`deploy/fly/fly.sh` creates and deploys a public server app and a private ClickHouse app from
templates, so the only setting you need is your app name:

```sh
cp deploy/fly/fly.env.example deploy/fly/fly.env   # set VAKA_FLY_APP and VAKA_ADMIN_EMAIL
deploy/fly/fly.sh launch
```

See [Deploy on Fly.io](deploy-fly.md) for all settings, custom domains and upgrades.

## Configuration

All settings are environment variables.

| Variable | Default | Meaning |
| --- | --- | --- |
| `VAKA_PUBLIC_URL` | `http://localhost:8080` | Public URL, used in links, alerts and install guides |
| `VAKA_LISTEN` | `0.0.0.0:8080` | Listen address |
| `VAKA_ADMIN_EMAIL`, `VAKA_ADMIN_PASSWORD` | | First owner account, created on start when no users exist. With email configured the password is optional |
| `VAKA_DB_URL` | `sqlite://$VAKA_DATA_DIR/vaka.db?mode=rwc` | `sqlite://...` or `postgres://user:pass@host/db` |
| `VAKA_READ_ONLY` | `false` | Start in read-only maintenance mode (see [Moving from SQLite to Postgres](#moving-from-sqlite-to-postgres)) |
| `VAKA_DATA_DIR` | `./data` (`/data` in Docker) | SQLite database and uploaded debug files |
| `VAKA_CLICKHOUSE_URL` | `http://localhost:8123` | ClickHouse HTTP interface |
| `VAKA_CLICKHOUSE_USER` / `_PASSWORD` / `_DATABASE` | `default` / empty / `vaka` | ClickHouse credentials |
| `VAKA_DEFAULT_RETENTION_DAYS` | `90` | Retention for new projects |
| `VAKA_RESEND_API_KEY` | | Enables email alerts, invite emails and email-code login via Resend |
| `VAKA_LLM_BASE_URL` | | Optional: OpenAI-compatible API (e.g. `https://api.openai.com/v1`) that writes the opening paragraph of digests ([anomalies.md](anomalies.md#the-narrative)) and tags feedback with sentiment and topics ([feedback.md](feedback.md)) |
| `VAKA_LLM_MODEL` | | Model for digest narratives and feedback tagging (needed with `VAKA_LLM_BASE_URL`) |
| `VAKA_LLM_API_KEY` | | Bearer key for that API (optional for local servers) |
| `VAKA_FEEDBACK_PER_IP_HOUR` / `VAKA_FEEDBACK_PER_SESSION_HOUR` | `10` / `5` | Feedback rate limits per project |
| `VAKA_TRUSTED_PROXIES` | loopback and private ranges | Comma-separated addresses or CIDRs whose `X-Forwarded-For` / `X-Real-IP` headers are believed (`none` for nobody). Requests from anywhere else are counted by their own address. Add your proxy's range when it reaches Vaka from a public address (e.g. Cloudflare in front of a public server), and the addresses of backends that forward their users' feedback |
| `VAKA_SMS_PROVIDER` | `46elks` | SMS alert provider: `46elks`, `twilio` or `sinch` ([alerts.md](alerts.md#sms)) |
| `VAKA_46ELKS_USERNAME`, `VAKA_46ELKS_PASSWORD` | | 46elks API credentials |
| `VAKA_TWILIO_ACCOUNT_SID`, `VAKA_TWILIO_AUTH_TOKEN` | | Twilio credentials |
| `VAKA_SINCH_SERVICE_PLAN_ID`, `VAKA_SINCH_API_TOKEN`, `VAKA_SINCH_REGION` | `us` | Sinch credentials and region |
| `VAKA_SMS_FROM` | `Vaka` | SMS sender (a number, or up to 11 letters where the provider allows it) |
| `VAKA_SMS_MAX_PER_DAY` | `100` | SMS deliveries allowed per 24 hours across the server (0 = unlimited) |
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

Pull the new image and restart: `docker compose pull && docker compose up -d` (bump
`VAKA_VERSION` in `.env` first if you pinned one). On Fly.io, run `deploy/fly/fly.sh deploy server`.
Migrations run automatically. Read the release notes for anything that needs manual steps.

## Moving from SQLite to Postgres

Start on SQLite and move to Postgres later, for example to run several instances or use a
managed database. Only metadata moves (users, projects, issues, tokens, alert rules, flags,
feedback): events, crashes, sessions and traces stay in ClickHouse. The metadata database is
small, so a copy takes seconds, and the switch costs a few seconds of read-only admin with no
lost events.

`vaka-server migrate-db --to <postgres url>` copies everything from the current database
(`VAKA_DB_URL`, or the SQLite file in `VAKA_DATA_DIR`; `--from` overrides it). It creates the
Postgres schema, copies every table in one transaction from a consistent snapshot, resets
sequences and checks the row count of every table. Re-running it needs `--replace`, which
overwrites the target with the source again. With Docker Compose, add the `postgres` profile
first (`COMPOSE_PROFILES=postgres` and `POSTGRES_PASSWORD` in `.env`, then `docker compose up -d`):

1. **Copy while live**, to check that the copy works. The server keeps running:

   ```sh
   docker compose exec vaka vaka-server migrate-db --to postgres://vaka:$POSTGRES_PASSWORD@postgres:5432/vaka
   ```

2. **Turn on read-only mode.** Metadata writes (admin edits, issue state changes, new tokens,
   alert rules, logins) are refused with a 503 explaining why, and background jobs pause.
   Events, sessions, spans and logs keep flowing into ClickHouse. Error and feedback items need
   metadata writes (issue grouping), so ingest answers them with `503` and `Retry-After`, and SDKs
   send them again after the switch. The call returns once error events already in the queue are
   stored. Use an admin session or an API token with write access to all projects:

   ```sh
   curl -X PUT https://vaka.example.com/api/v1/admin/read-only \
     -H "authorization: Bearer $VAKA_TOKEN" -H 'content-type: application/json' -d '{"enabled": true}'
   ```

3. **Copy again and switch.** The second copy should report `ok` for every table and no changes in
   the source. Then set `VAKA_DB_URL` and restart; the new server starts writable:

   ```sh
   docker compose exec vaka vaka-server migrate-db --replace --to postgres://vaka:$POSTGRES_PASSWORD@postgres:5432/vaka
   # .env: VAKA_DB_URL=postgres://vaka:...@postgres:5432/vaka
   docker compose up -d
   ```

To abandon the move, send `{"enabled": false}` to the same endpoint and keep running on SQLite.
`GET /api/v1/admin/read-only` shows the current state, and `/health` includes `"read_only"`.
Keep the SQLite file until you are happy with Postgres: switching back is the same steps with
`--from` and `--to` swapped. Setting `VAKA_READ_ONLY=true` starts a server in read-only mode.

## Health and monitoring

`GET /health` returns `{"ok": true, "db": true, "clickhouse": true, "read_only": false, "version": "..."}` and
503 when a dependency is down. Logs go to stdout; set `VAKA_LOG=debug` (tracing filter syntax) for more.
