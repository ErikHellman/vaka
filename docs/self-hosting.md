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

## Configuration

All settings are environment variables.

| Variable | Default | Meaning |
| --- | --- | --- |
| `VAKA_PUBLIC_URL` | `http://localhost:8080` | Public URL, used in links, alerts and install guides |
| `VAKA_LISTEN` | `0.0.0.0:8080` | Listen address |
| `VAKA_ADMIN_EMAIL`, `VAKA_ADMIN_PASSWORD` | | First owner account, created on start when no users exist |
| `VAKA_DB_URL` | `sqlite://$VAKA_DATA_DIR/vaka.db?mode=rwc` | `sqlite://...` or `postgres://user:pass@host/db` |
| `VAKA_DATA_DIR` | `./data` (`/data` in Docker) | SQLite database and uploaded debug files |
| `VAKA_CLICKHOUSE_URL` | `http://localhost:8123` | ClickHouse HTTP interface |
| `VAKA_CLICKHOUSE_USER` / `_PASSWORD` / `_DATABASE` | `default` / empty / `vaka` | ClickHouse credentials |
| `VAKA_DEFAULT_RETENTION_DAYS` | `90` | Retention for new projects |
| `VAKA_RESEND_API_KEY` | | Enables email alerts and invite emails via Resend |
| `VAKA_EMAIL_FROM` | `Vaka <vaka@localhost>` | Sender for emails |
| `VAKA_GEOIP_DB` | | Optional MaxMind/DB-IP country `.mmdb` file |

Country lookup uses `CF-IPCountry` (Cloudflare) or similar proxy headers when present, otherwise
the GeoIP database if configured.

Vaka creates the ClickHouse database and tables and runs metadata migrations on start.

## Running without Docker

```sh
cargo build --release -p vaka-server -p vaka
VAKA_CLICKHOUSE_URL=http://localhost:8123 ./target/release/vaka-server
```

## Backups

- Metadata: back up the SQLite file in `VAKA_DATA_DIR` (`sqlite3 vaka.db ".backup b.db"`) or
  `pg_dump` the Postgres database.
- Debug files: back up `VAKA_DATA_DIR/artifacts`.
- Events: ClickHouse `BACKUP DATABASE vaka TO Disk(...)`, or accept losing raw events since
  they expire with retention anyway.

## Upgrades

Pull the new image and restart: `docker compose pull && docker compose up -d`. Migrations run
automatically. Read the release notes for anything that needs manual steps.

## Health and monitoring

`GET /health` returns `{"ok": true, "db": true, "clickhouse": true, "version": "..."}` and
503 when a dependency is down. Logs go to stdout; set `VAKA_LOG=debug` (tracing filter syntax) for more.
