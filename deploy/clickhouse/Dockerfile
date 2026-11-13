# ClickHouse with Vaka's low-memory profile baked in, for platforms that can't bind-mount
# config files (Render, Railway, Kubernetes without ConfigMaps, ...):
#   docker build -t vaka-clickhouse deploy/clickhouse
# Set CLICKHOUSE_USER, CLICKHOUSE_PASSWORD and CLICKHOUSE_DEFAULT_ACCESS_MANAGEMENT=1 when running it,
# and persist /var/lib/clickhouse. docker-compose.yml mounts the same file instead.
ARG CLICKHOUSE_VERSION=24.8
FROM clickhouse/clickhouse-server:${CLICKHOUSE_VERSION}
COPY low-memory.xml /etc/clickhouse-server/config.d/low-memory.xml
