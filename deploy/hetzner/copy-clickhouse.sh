#!/usr/bin/env bash
# Copies Vaka's ClickHouse tables from another ClickHouse into this host's compose ClickHouse.
# Run from the repository root on the new host, with the new stack running (Vaka creates the
# tables on start). The source is reached over its HTTP port, e.g. for Fly.io:
#   fly proxy 18123:8123 -a vaka-clickhouse &
#   SRC_PASSWORD=<old CLICKHOUSE_PASSWORD> deploy/hetzner/copy-clickhouse.sh
#
# Columns are matched by name, so differing column order (tables altered over time) is fine.
# Run it once, after the old server stopped receiving traffic: re-running duplicates rows.
set -euo pipefail

SRC_URL=${SRC_URL:-http://127.0.0.1:18123}
SRC_USER=${SRC_USER:-vaka}
SRC_PASSWORD=${SRC_PASSWORD:?set SRC_PASSWORD to the source ClickHouse password}
DB=${CH_DATABASE:-vaka}
TABLES=${TABLES:-errors sessions events spans logs monitor_checks}
[ -f .env ] || { echo "run from the directory with docker-compose.yml and .env" >&2; exit 1; }
DST_PASSWORD=$(grep -E '^CLICKHOUSE_PASSWORD=' .env | cut -d= -f2-)

src() { curl -sS --fail -u "$SRC_USER:$SRC_PASSWORD" --data-binary "$1" "$SRC_URL/"; }
dst() { docker compose exec -T clickhouse clickhouse-client --user vaka --password "$DST_PASSWORD" --query "$1"; }
dstq() { dst "$1" </dev/null; }

src "SELECT 1" >/dev/null || { echo "cannot reach source ClickHouse at $SRC_URL" >&2; exit 1; }

for t in $TABLES; do
  # Columns the destination accepts on insert, in its own order (skips MATERIALIZED/ALIAS).
  cols=$(dstq "SELECT arrayStringConcat(groupArray(concat('\`', name, '\`')), ',') FROM (SELECT name FROM system.columns WHERE database = '$DB' AND table = '$t' AND default_kind IN ('', 'DEFAULT') ORDER BY position)")
  [ -n "$cols" ] || { echo "$t: missing on destination, skipped"; continue; }
  # Only columns the source also has; missing ones get their defaults.
  have=$(src "SELECT name FROM system.columns WHERE database = '$DB' AND table = '$t' FORMAT TSV")
  [ -n "$have" ] || { echo "$t: missing on source, skipped"; continue; }
  sel=$(tr ',' '\n' <<<"$cols" | while read -r c; do if grep -qx "${c//\`/}" <<<"$have"; then printf '%s,' "$c"; fi; done)
  sel=${sel%,}
  before=$(dstq "SELECT count() FROM $DB.$t")
  printf '%-15s copying... ' "$t"
  src "SELECT $sel FROM $DB.$t FORMAT Native" | dst "INSERT INTO $DB.$t ($sel) FORMAT Native"
  n_src=$(src "SELECT count() FROM $DB.$t")
  n_dst=$(dstq "SELECT count() FROM $DB.$t")
  echo "source $n_src, destination $before -> $n_dst"
done
echo "Done. Destination counts include rows ingested here since the cutover."
