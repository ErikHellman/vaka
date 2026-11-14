# Vaka on a single VM (Hetzner CX23)

Files for running Vaka with Docker Compose on one small VM: 2 vCPU, 4 GB RAM, 40 GB disk
(Hetzner CX23 or similar), Ubuntu 24.04.

| File | Purpose |
| --- | --- |
| `setup.sh` | One-time host setup as root: admin user, key-only SSH, ufw, swap, auto security updates, Docker, checkout in `/opt/vaka`, generated `.env` |
| `compose.yml` | Compose overrides: ClickHouse capped at 2 GB (server limit ~1.6 GB), Vaka at 1 GB, 1 GB per dashboard query |
| `clickhouse-memory.xml` | ClickHouse memory limit, loaded after `deploy/clickhouse/low-memory.xml` |
| `copy-clickhouse.sh` | Copies all events from another ClickHouse (e.g. the Fly.io one) into this host's |

## 1. Prepare the server

As root on the new server (the setup copies root's SSH keys to the new user):

```sh
curl -fsSLO https://raw.githubusercontent.com/ErikHellman/vaka/main/deploy/hetzner/setup.sh
VAKA_USER=erik VAKA_DOMAIN=vaka.example.com VAKA_ADMIN_EMAIL=you@example.com bash setup.sh
```

Then, **from a new terminal** (keep the root session open until this works):

```sh
ssh erik@<server-ip>
cd /opt/vaka && nano .env      # check the values; see .env.example for everything else
```

Point `VAKA_DOMAIN`'s DNS (A and AAAA) at the server, then:

```sh
docker compose pull && docker compose up -d
curl -s localhost:8080/health && curl -sI https://$VAKA_DOMAIN/health | head -1
```

Caddy gets the Let's Encrypt certificate on first request once DNS resolves. In the Hetzner console
it is worth turning on Backups and adding a Cloud Firewall with the same rules as ufw (22, 80, 443).

**Disk.** Events expire per project (`VAKA_DEFAULT_RETENTION_DAYS=90` for new ones). Watch
`df -h /` and `docker system df`; lower retention in the admin UI if the disk passes ~70%.

## 2. Move an existing Fly.io instance

The metadata (SQLite on the server volume) and debug files move during a short read-only window;
events in ClickHouse are copied afterwards. Nothing is lost: during the window events keep flowing,
and error and feedback items get `503` + `Retry-After`, which SDKs resend.

Below, `vaka` / `vaka-clickhouse` are the Fly app names, and commands marked **(server)** run in
`/opt/vaka` on the new host.

**Before the switch**

```sh
# (laptop) values the new server must keep. Copy them into the server's .env:
fly ssh console -a vaka -C 'printenv VAKA_SECRET_KEY'            # if set: VAKA_SECRET_KEY=...
fly ssh console -a vaka -C 'printenv VAKA_CLICKHOUSE_PASSWORD'   # old ClickHouse password, for step 6
# (server) flyctl, to fetch files and reach the old ClickHouse
curl -L https://fly.io/install.sh | sh && ~/.fly/bin/fly auth login
```

If the instance uses a custom domain, lower its DNS TTL to 60 s a day ahead and give the new
server a temporary name for step 1. If SDKs use `https://<app>.fly.dev`, give the new server its
own domain: step 5 turns the Fly app into a forwarder so those apps keep working.

**The switch** (a few minutes)

1. **Read-only on Fly** (an admin API token with write access to all projects):

   ```sh
   curl -X PUT https://vaka.fly.dev/api/v1/admin/read-only -H "authorization: Bearer $VAKA_TOKEN" \
     -H 'content-type: application/json' -d '{"enabled": true}'
   ```

2. **Export** a consistent copy of the metadata and the debug files on Fly:

   ```sh
   # (server)
   F=~/.fly/bin/fly
   $F ssh console -a vaka -C 'vaka-server migrate-db --replace --from sqlite:///data/vaka.db --to sqlite:///data/export.db?mode=rwc'
   $F ssh console -a vaka -C 'tar czf /data/export-artifacts.tgz -C /data artifacts'   # skip with S3 storage
   rm -f vaka.db artifacts.tgz
   $F ssh sftp get -a vaka /data/export.db vaka.db
   $F ssh sftp get -a vaka /data/export-artifacts.tgz artifacts.tgz
   ```

3. **Import** into the new server's volume and start it:

   ```sh
   # (server)
   docker compose stop vaka
   docker compose run --rm --no-deps -u 0 -v "$PWD:/import:ro" --entrypoint sh vaka -c \
     'rm -f /data/vaka.db*; cp /import/vaka.db /data/vaka.db; [ -f /import/artifacts.tgz ] && tar xzf /import/artifacts.tgz -C /data; chown -R 10001 /data'
   docker compose up -d && curl -s localhost:8080/health
   ```

   Log in at the new URL with your existing account to check.

4. **Custom domain:** switch its A/AAAA records to the server (and set `VAKA_DOMAIN`/`VAKA_PUBLIC_URL`
   to it, then `docker compose up -d`). Wait for the TTL to pass and Fly's logs to go quiet, then
   stop the old server: `fly scale count 0 -a vaka`.

5. **`*.fly.dev` URL:** replace the Fly server with a forwarder to the new host instead:

   ```sh
   # (laptop, in the repo)
   fly deploy -a vaka -c deploy/fly/forward/fly.toml -e VAKA_FORWARD_TO=vaka.example.com
   ```

   If `fly deploy` refuses because of the old volume, `fly machine destroy` the old machine first
   (the volume is kept). Forwarded requests reach Vaka from Fly's address, so country lookup and the
   per-client rate limit see Fly, not the user: move SDKs to the new URL when convenient.

6. **Copy events** once the old server gets no traffic:

   ```sh
   # (server)
   ~/.fly/bin/fly proxy 18123:8123 -a vaka-clickhouse &
   SRC_PASSWORD=<old ClickHouse password> deploy/hetzner/copy-clickhouse.sh
   kill %1
   ```

   It copies each table once and prints source and destination counts. Do not run it twice.

**Afterwards.** Keep the Fly apps (scaled down) for a few days as a fallback; then
`fly apps destroy vaka-clickhouse` (and `vaka`, once no SDK uses the fly.dev URL). To back out
before step 4/5, send `{"enabled": false}` to the read-only endpoint on Fly.
